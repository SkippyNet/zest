//! Discovery of system folders, per-user folders and Zest's own data folders.

use crate::{IoContext, Result, brand};
use std::path::{Path, PathBuf};
use winreg::RegKey;
use winreg::enums::HKEY_LOCAL_MACHINE;

/// Environment variable that redirects [`data_dir`] (used by tests and portable setups).
pub const DATA_DIR_ENV: &str = "ZEST_DATA_DIR";

/// Marker file that switches the app to portable mode when placed beside the executable.
pub const PORTABLE_MARKER: &str = "zest.portable";

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Root of the system drive, e.g. `C:\`.
pub fn system_drive() -> PathBuf {
    let mut drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
    if !drive.ends_with('\\') {
        drive.push('\\');
    }
    PathBuf::from(drive)
}

/// The Windows directory, e.g. `C:\Windows`.
pub fn windows_dir() -> PathBuf {
    env_path("SystemRoot")
        .or_else(|| env_path("windir"))
        .unwrap_or_else(|| system_drive().join("Windows"))
}

/// `C:\Windows\System32`.
pub fn system32_dir() -> PathBuf {
    windows_dir().join("System32")
}

/// `C:\ProgramData`.
pub fn program_data() -> PathBuf {
    env_path("ProgramData").unwrap_or_else(|| system_drive().join("ProgramData"))
}

/// `C:\Program Files` (64-bit programs).
pub fn program_files() -> PathBuf {
    env_path("ProgramW6432")
        .or_else(|| env_path("ProgramFiles"))
        .unwrap_or_else(|| system_drive().join("Program Files"))
}

/// `C:\Program Files (x86)` (32-bit programs).
pub fn program_files_x86() -> PathBuf {
    env_path("ProgramFiles(x86)").unwrap_or_else(|| system_drive().join("Program Files (x86)"))
}

/// The folder that contains all user profiles, e.g. `C:\Users`.
pub fn users_dir() -> PathBuf {
    let from_registry = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList")
        .and_then(|k| k.get_value::<String, _>("ProfilesDirectory"))
        .ok()
        .map(|raw| PathBuf::from(expand_env(&raw)));
    from_registry
        .filter(|p| p.is_absolute())
        .or_else(|| env_path("USERPROFILE").and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| system_drive().join("Users"))
}

/// Expands `%VARIABLE%` references using the process environment.
///
/// Unknown variables are left untouched, matching Windows behaviour.
pub fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The well-known folders of one Windows user account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserDirs {
    /// Account folder name (not necessarily the login name).
    pub name: String,
    /// Security identifier, when known.
    pub sid: Option<String>,
    /// `C:\Users\<name>`
    pub profile: PathBuf,
    /// `AppData\Local`
    pub local: PathBuf,
    /// `AppData\Roaming`
    pub roaming: PathBuf,
    /// `AppData\LocalLow`
    pub local_low: PathBuf,
    /// The user's temp folder.
    pub temp: PathBuf,
    /// True for the account running this process.
    pub is_current: bool,
}

impl UserDirs {
    /// Folders of the account running this process.
    pub fn current() -> Self {
        let profile = env_path("USERPROFILE").unwrap_or_else(|| users_dir().join("Default"));
        let mut dirs = Self::for_profile(profile, None);
        if let Some(local) = env_path("LOCALAPPDATA") {
            dirs.local = local;
        }
        if let Some(roaming) = env_path("APPDATA") {
            dirs.roaming = roaming;
        }
        dirs.temp = dirs.local.join("Temp");
        dirs.is_current = true;
        dirs
    }

    /// Derives the standard folder layout for an arbitrary profile folder.
    pub fn for_profile(profile: PathBuf, sid: Option<String>) -> Self {
        let name = profile
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let app_data = profile.join("AppData");
        let local = app_data.join("Local");
        UserDirs {
            name,
            sid,
            temp: local.join("Temp"),
            local,
            roaming: app_data.join("Roaming"),
            local_low: app_data.join("LocalLow"),
            profile,
            is_current: false,
        }
    }

    /// Every real (interactive) user profile on this PC, current user first.
    ///
    /// Service and system accounts are skipped. Profiles whose folder is
    /// missing are skipped as well.
    pub fn all() -> Vec<UserDirs> {
        let current = Self::current();
        let mut users = vec![current.clone()];
        let Ok(list) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList")
        else {
            return users;
        };
        for sid in list.enum_keys().flatten() {
            // S-1-5-21-* are local/domain accounts; S-1-12-1-* are Entra ID accounts.
            if !(sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-12-1-")) {
                continue;
            }
            let Ok(key) = list.open_subkey(&sid) else {
                continue;
            };
            let Ok(raw) = key.get_value::<String, _>("ProfileImagePath") else {
                continue;
            };
            let profile = PathBuf::from(expand_env(&raw));
            if !profile.is_dir() {
                continue;
            }
            if same_path(&profile, &current.profile) {
                users[0].sid = Some(sid);
                continue;
            }
            users.push(Self::for_profile(profile, Some(sid)));
        }
        users
    }
}

/// Case-insensitive path comparison ignoring trailing separators.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Folder containing the running executable.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// True when a `zest.portable` marker sits beside the executable.
pub fn is_portable() -> bool {
    exe_dir().join(PORTABLE_MARKER).exists()
}

/// Where Zest keeps settings, history, logs and backups.
///
/// Resolution order: `ZEST_DATA_DIR`, portable mode (`<exe>\data`), then
/// `%APPDATA%\Zest`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env_path(DATA_DIR_ENV) {
        return dir;
    }
    if is_portable() {
        return exe_dir().join("data");
    }
    env_path("APPDATA")
        .unwrap_or_else(|| UserDirs::current().roaming)
        .join(brand::DATA_DIR_NAME)
}

/// Folder for backups (registry exports, removed startup entries, ...).
pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

/// Folder for log files.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Creates `dir` (and parents) if needed and returns it.
pub fn ensure_dir(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).ctx(format!("creating folder {}", dir.display()))?;
    Ok(dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_and_keeps_unknown() {
        let windir = std::env::var("SystemRoot").unwrap();
        assert_eq!(expand_env(r"%SystemRoot%\Temp"), format!(r"{windir}\Temp"));
        assert_eq!(expand_env("%NoSuchVar_Zest%\\x"), "%NoSuchVar_Zest%\\x");
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("plain"), "plain");
    }

    #[test]
    fn current_user_is_first_and_flagged() {
        let all = UserDirs::all();
        assert!(!all.is_empty());
        assert!(all[0].is_current);
        assert!(all[0].profile.is_absolute());
        assert!(all.iter().skip(1).all(|u| !u.is_current));
    }

    #[test]
    fn profile_layout() {
        let u = UserDirs::for_profile(PathBuf::from(r"C:\Users\Alice"), None);
        assert_eq!(u.name, "Alice");
        assert_eq!(u.local, PathBuf::from(r"C:\Users\Alice\AppData\Local"));
        assert_eq!(u.temp, PathBuf::from(r"C:\Users\Alice\AppData\Local\Temp"));
    }

    #[test]
    fn path_equality_is_lenient() {
        assert!(same_path(
            Path::new(r"C:\Users\Bob\"),
            Path::new("c:/users/bob")
        ));
        assert!(!same_path(
            Path::new(r"C:\Users\Bob"),
            Path::new(r"C:\Users\Bobby")
        ));
    }
}
