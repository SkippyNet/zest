//! Conservative "is it really gone?" checks shared by all scanners.
//!
//! [`zest_core::cmdline`] decides what a registry string points at. This
//! module adds a second line of defence before anything is reported:
//!
//! * absence must be *confirmed* – an "access denied" while probing a path is
//!   not proof that the file is missing;
//! * a path is only missing when its 32/64-bit twin is missing too. Zest
//!   is a 64-bit program, so `%ProgramFiles%` and `%CommonProgramFiles%` in a
//!   32-bit registration expand to the 64-bit folders here, while the
//!   program that owns the registration sees the `(x86)` ones;
//! * packaged-app locations are never judged, because their folders are not
//!   readable for every account;
//! * locations owned by Windows components can be excluded, because Windows
//!   ships registrations for optional features whose files are installed on
//!   demand;
//! * values that should hold exactly one file path must be missing under
//!   both the "plain path" and the "command line" reading.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use winreg::RegKey;
use zest_core::cmdline::{self, Target};
use zest_core::paths;
use zest_core::reg::RegPath;

/// True only when the file system positively reports "not found".
///
/// Any other outcome – the entry exists, access is denied, the name is
/// invalid, the device is not ready – returns false. So does a path that
/// runs through a junction, symbolic link or volume mount point: the entry
/// may only be unreachable because the link target (an external drive, a
/// network location) is offline right now.
pub(crate) fn definitely_absent(path: &Path) -> bool {
    if !matches!(path.try_exists(), Ok(false)) {
        return false;
    }
    // A dangling link is still "something there".
    if std::fs::symlink_metadata(path).is_ok() {
        return false;
    }
    let mut ancestor = path.parent();
    while let Some(dir) = ancestor {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        ancestor = dir.parent();
    }
    true
}

/// Variables whose value differs from account to account.
const PER_USER_VARIABLES: &[&str] = &[
    "%userprofile%",
    "%appdata%",
    "%localappdata%",
    "%temp%",
    "%tmp%",
    "%homepath%",
    "%homedrive%",
    "%homeshare%",
    "%username%",
    "%onedrive",
];

/// True when `text` refers to a per-user environment variable.
///
/// A machine-wide entry written that way resolves to a different file for
/// every account; that it is missing for the current account says nothing
/// about the others.
pub(crate) fn mentions_per_user_variable(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    PER_USER_VARIABLES.iter().any(|var| lower.contains(var))
}

/// True only when the registry positively reports that the key is missing.
pub(crate) fn key_definitely_absent(key: &RegPath) -> bool {
    matches!(key.open(), Err(e) if e.kind() == ErrorKind::NotFound)
}

/// True only when the registry positively reports that the value is missing
/// (either the key itself or the value inside it does not exist).
pub(crate) fn value_definitely_absent(key: &RegPath, name: &str) -> bool {
    match key.open() {
        Ok(k) => matches!(k.get_raw_value(name), Err(e) if e.kind() == ErrorKind::NotFound),
        Err(e) => e.kind() == ErrorKind::NotFound,
    }
}

/// True for locations inside the packaged-app store, which ordinary accounts
/// cannot inspect reliably.
pub(crate) fn is_packaged(path: &Path) -> bool {
    let text = path.to_string_lossy().to_lowercase();
    text.contains("\\windowsapps\\") || text.contains("\\systemapps\\")
}

/// Returns the part of `path` below `root` (original spelling), if `path`
/// lies inside `root`. The comparison ignores case.
fn relative_to(path: &Path, root: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('/', "\\");
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('\\');
    if text.len() <= root.len() || !text.is_char_boundary(root.len()) {
        return None;
    }
    let (head, rest) = text.split_at(root.len());
    if head.to_lowercase() != root.to_lowercase() {
        return None;
    }
    rest.strip_prefix('\\').map(str::to_owned)
}

/// True for anything inside the Windows directory.
pub(crate) fn is_under_windows(path: &Path) -> bool {
    relative_to(path, &paths::windows_dir()).is_some_and(|rel| !rel.is_empty())
}

/// Folders below `Program Files` that belong to Windows itself.
const WINDOWS_PROGRAM_DIRS: &[&str] = &[
    "windows nt",
    "windows media player",
    "windows photo viewer",
    "windows mail",
    "windows defender",
    "windows defender advanced threat protection",
    "windows security",
    "windows sidebar",
    "windows portable devices",
    "windows multimedia platform",
    "windowspowershell",
    "internet explorer",
    "common files\\system",
    "common files\\microsoft shared\\dao",
    "common files\\microsoft shared\\ink",
    "common files\\microsoft shared\\msinfo",
    "common files\\microsoft shared\\stationery",
    "common files\\microsoft shared\\textconv",
    "common files\\microsoft shared\\triedit",
    "common files\\microsoft shared\\vgx",
];

/// True for locations owned by Windows components: the Windows directory
/// and the Windows-owned folders below `Program Files`.
///
/// Windows keeps registrations (classes, firewall rules, shared file
/// counters, ...) for optional features whose files are only installed on
/// demand. Scanners for those kinds of registrations refuse to judge these
/// locations.
pub(crate) fn is_windows_component(path: &Path) -> bool {
    if is_under_windows(path) {
        return true;
    }
    [paths::program_files(), paths::program_files_x86()]
        .iter()
        .filter_map(|root| relative_to(path, root))
        .map(|rel| rel.to_lowercase())
        .any(|rel| {
            WINDOWS_PROGRAM_DIRS.iter().any(|dir| {
                rel.strip_prefix(dir)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
            })
        })
}

/// The same location as seen by a program of the other bitness:
/// `Program Files` <-> `Program Files (x86)` and `System32` <-> `SysWOW64`.
pub(crate) fn bitness_twins(path: &Path) -> Vec<PathBuf> {
    let mut twins = Vec::new();
    let program_files = paths::program_files();
    let program_files_x86 = paths::program_files_x86();
    if let Some(rel) = relative_to(path, &program_files) {
        twins.push(program_files_x86.join(rel));
    } else if let Some(rel) = relative_to(path, &program_files_x86) {
        twins.push(program_files.join(rel));
    }
    let windows = paths::windows_dir();
    for (from, to) in [
        ("System32", "SysWOW64"),
        ("Sysnative", "SysWOW64"),
        ("SysWOW64", "System32"),
    ] {
        if let Some(rel) = relative_to(path, &windows.join(from)) {
            twins.push(windows.join(to).join(rel));
        }
    }
    twins.retain(|twin| !paths::same_path(twin, path));
    twins
}

/// True only when `path` and all of its [`bitness_twins`] are positively
/// reported as "not found".
pub(crate) fn confirmed_absent(path: &Path) -> bool {
    definitely_absent(path)
        && bitness_twins(path)
            .iter()
            .all(|twin| definitely_absent(twin))
}

/// Turns a [`Target`] into the missing path, but only when the absence is
/// confirmed and the location is one Zest is willing to judge.
pub(crate) fn confirmed(target: Target) -> Option<PathBuf> {
    match target {
        Target::Missing(path) if !is_packaged(&path) && confirmed_absent(&path) => Some(path),
        _ => None,
    }
}

/// Drops missing paths that live in Windows-owned locations.
pub(crate) fn outside_windows(missing: Option<PathBuf>) -> Option<PathBuf> {
    missing.filter(|path| !is_windows_component(path))
}

/// The missing path named by a plain path string, if it is confirmed missing.
pub(crate) fn missing_path(raw: &str) -> Option<PathBuf> {
    confirmed(cmdline::check_path(raw))
}

/// The missing target of a command line, if it is confirmed missing.
pub(crate) fn missing_command(raw: &str) -> Option<PathBuf> {
    confirmed(cmdline::check_command(raw))
}

/// For values that should contain exactly one file path but sometimes carry
/// trailing arguments: both the "whole string is a path" and the "command
/// line" reading must agree that the target is missing.
pub(crate) fn missing_file(raw: &str) -> Option<PathBuf> {
    let path = missing_path(raw)?;
    missing_command(raw)?;
    Some(path)
}

/// Reads a string value, trimmed. Empty strings count as absent.
pub(crate) fn string_value(key: &RegKey, name: &str) -> Option<String> {
    let text = key.get_value::<String, _>(name).ok()?;
    let text = text.trim().trim_matches('\0').trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Reads the raw (untrimmed) string data of a value, exactly as a later
/// [`crate::Condition::ValueEquals`] check will read it.
pub(crate) fn raw_string_value(key: &RegKey, name: &str) -> Option<String> {
    key.get_value::<String, _>(name).ok()
}

/// Decodes the string data of an already enumerated value, exactly as
/// [`raw_string_value`] would read it (saves a second registry round trip).
pub(crate) fn decode_string(value: &winreg::RegValue) -> Option<String> {
    use winreg::types::FromRegValue;
    String::from_reg_value(value).ok()
}

/// Reads a `REG_DWORD` value.
pub(crate) fn dword_value(key: &RegKey, name: &str) -> Option<u32> {
    key.get_value::<u32, _>(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, gone, gone_dir};

    #[test]
    fn absence_must_be_positive() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("here.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(!definitely_absent(&file));
        assert!(definitely_absent(&tmp.path().join("gone.txt")));
        assert!(definitely_absent(Path::new(&gone("x.dll"))));
        // Invalid names are "unknown", not "absent".
        assert!(!definitely_absent(Path::new("C:\\bad|name?\\x.dll")));
    }

    #[test]
    fn paths_through_links_are_never_confirmed_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("external-drive");
        let link = tmp.path().join("link");
        std::fs::create_dir_all(&target).unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink failed");
        // Unlink the junction even if an assertion below fails.
        struct Unlink(PathBuf);
        impl Drop for Unlink {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let _unlink = Unlink(link.clone());

        // Live link, file not there: could be anything, so not confirmed.
        assert!(!definitely_absent(&link.join("app.exe")));
        assert!(!definitely_absent(&link.join("sub").join("app.exe")));
        // The same file addressed directly is confirmed missing.
        assert!(definitely_absent(&target.join("app.exe")));

        // The "drive" goes offline: the link dangles.
        std::fs::remove_dir(&target).unwrap();
        assert!(!definitely_absent(&link.join("app.exe")));
        assert!(!definitely_absent(&link));
    }

    #[test]
    fn per_user_variables() {
        assert!(mentions_per_user_variable(r"%LOCALAPPDATA%\Vendor\app.exe"));
        assert!(mentions_per_user_variable(
            r#""%AppData%\Vendor\app.exe" /tray"#
        ));
        assert!(mentions_per_user_variable(r"%UserProfile%\tool.exe"));
        assert!(mentions_per_user_variable(r"%OneDriveConsumer%\x.exe"));
        assert!(!mentions_per_user_variable(
            r"%ProgramFiles%\Vendor\app.exe"
        ));
        assert!(!mentions_per_user_variable(r"%SystemRoot%\system32\x.dll"));
        assert!(!mentions_per_user_variable(r"C:\Users\someone\app.exe"));
    }

    #[test]
    fn registry_absence() {
        let scratch = Scratch::new("check");
        scratch.set(&scratch.path, "Present", "1");
        assert!(!key_definitely_absent(&scratch.path));
        assert!(key_definitely_absent(&scratch.path.child("Nope")));
        assert!(!value_definitely_absent(&scratch.path, "Present"));
        assert!(value_definitely_absent(&scratch.path, "Absent"));
        assert!(value_definitely_absent(
            &scratch.path.child("Nope"),
            "Anything"
        ));
    }

    #[test]
    fn packaged_and_windows_locations() {
        assert!(is_packaged(Path::new(
            r"C:\Program Files\WindowsApps\Vendor.App_1.0\app.exe"
        )));
        assert!(!is_packaged(Path::new(r"C:\Program Files\Vendor\app.exe")));
        let windows = paths::windows_dir();
        assert!(is_under_windows(&windows.join(r"System32\x.dll")));
        assert!(is_under_windows(Path::new(
            &windows
                .join(r"system32\X.DLL")
                .display()
                .to_string()
                .to_uppercase()
        )));
        assert!(!is_under_windows(&windows));
        assert!(!is_under_windows(Path::new(&format!(
            "{}Old\\x.dll",
            windows.display()
        ))));
    }

    #[test]
    fn windows_component_locations() {
        let pf = paths::program_files();
        let pf86 = paths::program_files_x86();
        assert!(is_windows_component(
            &paths::system32_dir().join("p2phost.exe")
        ));
        assert!(is_windows_component(
            &paths::windows_dir().join(r"ehome\ehshell.exe")
        ));
        assert!(is_windows_component(
            &pf.join(r"Windows NT\Accessories\wordpad.exe")
        ));
        assert!(is_windows_component(
            &pf86.join(r"Windows Media Player\wmplayer.exe")
        ));
        assert!(is_windows_component(
            &pf.join(r"Common Files\System\Ole DB\msdaora.dll")
        ));
        assert!(is_windows_component(
            &pf86.join(r"COMMON FILES\Microsoft Shared\DAO\dao360.dll")
        ));
        // Third-party and look-alike locations are not Windows components.
        assert!(!is_windows_component(&pf.join(r"Vendor\app.exe")));
        assert!(!is_windows_component(
            &pf.join(r"Windows NT Tools\tool.exe")
        ));
        assert!(!is_windows_component(
            &pf.join(r"Common Files\Microsoft Shared\OFFICE16\mso.dll")
        ));
        assert!(!is_windows_component(
            &pf.join(r"Common Files\SystemTools\x.dll")
        ));
        assert!(!is_windows_component(Path::new(&gone("x.dll"))));

        assert_eq!(
            outside_windows(Some(pf.join(r"Vendor\app.exe"))),
            Some(pf.join(r"Vendor\app.exe"))
        );
        assert_eq!(
            outside_windows(Some(paths::system32_dir().join("gone.dll"))),
            None
        );
        assert_eq!(outside_windows(None), None);
    }

    #[test]
    fn twins_of_the_other_bitness() {
        let pf = paths::program_files();
        let pf86 = paths::program_files_x86();
        assert_eq!(
            bitness_twins(&pf.join(r"Common Files\Vendor\Lib.dll")),
            vec![pf86.join(r"Common Files\Vendor\Lib.dll")]
        );
        assert_eq!(
            bitness_twins(&pf86.join(r"Vendor\App.exe")),
            vec![pf.join(r"Vendor\App.exe")]
        );
        let windows = paths::windows_dir();
        assert_eq!(
            bitness_twins(&windows.join(r"System32\x.dll")),
            vec![windows.join(r"SysWOW64\x.dll")]
        );
        assert_eq!(
            bitness_twins(&windows.join(r"SysWOW64\sub\x.dll")),
            vec![windows.join(r"System32\sub\x.dll")]
        );
        assert!(bitness_twins(Path::new(&gone("x.dll"))).is_empty());
        assert!(bitness_twins(&pf).is_empty());
    }

    #[test]
    fn a_file_that_exists_for_the_other_bitness_is_not_missing() {
        // Find a real file that exists below "Program Files (x86)" but not at
        // the same relative place below "Program Files". A 32-bit
        // registration that names it through %ProgramFiles% expands to the
        // 64-bit folder in this process and must still not count as missing.
        let pf = paths::program_files();
        let pf86 = paths::program_files_x86();
        let common86 = pf86.join("Common Files");
        let Ok(entries) = std::fs::read_dir(&common86) else {
            return;
        };
        let mut candidate = None;
        'outer: for dir in entries.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if !path.is_file() {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&pf86) else {
                    continue;
                };
                if definitely_absent(&pf.join(rel)) {
                    candidate = Some(rel.to_path_buf());
                    break 'outer;
                }
            }
        }
        let Some(rel) = candidate else {
            return;
        };
        let as_seen_here = pf.join(&rel);
        assert!(definitely_absent(&as_seen_here));
        assert!(
            !confirmed_absent(&as_seen_here),
            "{}",
            as_seen_here.display()
        );
        assert_eq!(missing_path(&as_seen_here.display().to_string()), None);
        assert_eq!(missing_file(&as_seen_here.display().to_string()), None);
    }

    #[test]
    fn missing_helpers_agree_with_reality() {
        let shell32 = paths::system32_dir().join("shell32.dll");
        assert_eq!(missing_path(&shell32.display().to_string()), None);
        assert_eq!(
            missing_path(&gone("lib.dll")),
            Some(PathBuf::from(gone("lib.dll")))
        );
        assert_eq!(
            missing_command(&format!("\"{}\" /run", gone("app.exe"))),
            Some(PathBuf::from(gone("app.exe")))
        );
        // Packaged locations are never judged.
        assert_eq!(
            missing_path(r"C:\Program Files\WindowsApps\Zest.NoSuch_1.0\app.exe"),
            None
        );
        // A path followed by an argument: the plain reading says "missing",
        // the command reading sees the real file. They disagree, so no report.
        let with_arg = format!("{} -Embedding", shell32.display());
        assert_eq!(missing_file(&with_arg), None);
        let spaced = format!(r"{}\Sub Dir\lib.dll", gone_dir());
        assert_eq!(missing_file(&spaced), Some(PathBuf::from(&spaced)));
    }
}
