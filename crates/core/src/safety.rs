//! The hard safety guard.
//!
//! Every deletion performed by Zest – whether it comes from a built-in
//! cleaning rule, a custom include, the duplicate finder or the shredder –
//! passes through [`ensure_deletable`]. The guard is deliberately independent
//! of the rules: even a buggy or malicious rule cannot remove a critical
//! location.
//!
//! The guard is a pure function of the path (no file system access), which
//! keeps it fast and makes it exhaustively testable.

use crate::{Error, Result, paths};
use std::path::Path;
use std::sync::OnceLock;

/// Sub-trees of the Windows directory in which deletion is permitted.
/// Everything else below `%SystemRoot%` is off limits.
const WINDOWS_ALLOWED_TREES: &[&str] = &[
    "temp",
    "prefetch",
    "logs",
    "minidump",
    "livekernelreports",
    "debug",
    "panther",
    "cbstemp",
    "downloaded program files",
    "softwaredistribution\\download",
    "softwaredistribution\\datastore\\logs",
    "system32\\logfiles",
    "system32\\wbem\\logs",
    "system32\\config\\systemprofile\\appdata\\local\\temp",
    "system32\\config\\systemprofile\\appdata\\local\\microsoft\\windows\\inetcache",
    "serviceprofiles\\localservice\\appdata\\local\\temp",
    "serviceprofiles\\networkservice\\appdata\\local\\temp",
    "serviceprofiles\\localservice\\appdata\\local\\fontcache",
    "serviceprofiles\\networkservice\\appdata\\local\\microsoft\\windows\\deliveryoptimization\\cache",
];

/// Folders (relative to the Windows directory, `""` = the directory itself)
/// whose *direct* log-like files may be deleted.
const WINDOWS_LOG_FILE_DIRS: &[&str] = &["", "inf", "security\\logs"];

/// Extensions considered log-like for [`WINDOWS_LOG_FILE_DIRS`].
const LOG_LIKE_EXTENSIONS: &[&str] = &["log", "etl", "dmp", "tmp", "old", "bak"];

/// Names directly under any drive root that must never be touched.
const DRIVE_ROOT_PROTECTED: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "recovery",
    "boot",
    "efi",
    "documents and settings",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "bootmgr",
    "bootnxt",
    "$winreagent",
    "$sysreset",
];

/// Folders directly inside a user profile that may be cleaned *inside* but
/// never removed themselves.
const PROFILE_PROTECTED_CHILDREN: &[&str] = &[
    "appdata",
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "music",
    "videos",
    "favorites",
    "contacts",
    "links",
    "saved games",
    "searches",
    "3d objects",
    "onedrive",
    "ntuser.dat",
];

struct Roots {
    windows: String,
    users: String,
    exact: Vec<String>,
    trees: Vec<String>,
}

fn roots() -> &'static Roots {
    static ROOTS: OnceLock<Roots> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let norm = |p: std::path::PathBuf| normalize(&p).unwrap_or_default();
        let program_files = norm(paths::program_files());
        let program_files_x86 = norm(paths::program_files_x86());
        let program_data = norm(paths::program_data());
        let mut exact = vec![
            program_files.clone(),
            program_files_x86.clone(),
            program_data.clone(),
            format!("{program_files}\\common files"),
            format!("{program_files_x86}\\common files"),
            format!("{program_data}\\microsoft"),
            format!("{program_data}\\microsoft\\windows"),
            format!("{program_data}\\packages"),
        ];
        exact.retain(|p| !p.is_empty() && !p.starts_with('\\'));
        let mut trees = vec![
            format!("{program_files}\\windowsapps"),
            format!("{program_files}\\windows defender"),
            format!("{program_files}\\windows security"),
            format!("{program_data}\\microsoft\\windows defender"),
            format!("{program_data}\\microsoft\\crypto"),
        ];
        trees.retain(|p| !p.starts_with('\\'));
        Roots {
            windows: norm(paths::windows_dir()),
            users: norm(paths::users_dir()),
            exact,
            trees,
        }
    })
}

/// Normalises a path for comparison: lower case, back slashes, no verbatim
/// prefix, no trailing separator (except for drive roots).
///
/// Returns `None` for paths that are relative or contain `.`/`..` components,
/// both of which the guard treats as unsafe.
pub fn normalize(path: &Path) -> Option<String> {
    let mut s = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(rest) = s.strip_prefix("\\\\?\\unc\\") {
        s = format!("\\\\{rest}");
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        s = rest.to_owned();
    } else if s.starts_with("\\\\.\\") {
        // Device paths (\\.\PhysicalDrive0, ...) are never valid deletion targets.
        return None;
    }

    let is_unc = s.starts_with("\\\\");
    let bytes = s.as_bytes();
    let is_drive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    if !is_unc && !is_drive {
        return None;
    }

    let body = if is_unc { &s[2..] } else { &s[3..] };
    let mut parts: Vec<&str> = Vec::new();
    for part in body.split('\\') {
        match part {
            "" => continue,
            "." | ".." => return None,
            // Windows silently strips trailing dots and spaces from names,
            // so "windows." would alias "windows". Refuse such names.
            p if p.ends_with('.') || p.ends_with(' ') => return None,
            p => parts.push(p),
        }
    }

    let mut out = if is_unc {
        String::from("\\\\")
    } else {
        s[..3].to_owned()
    };
    out.push_str(&parts.join("\\"));
    Some(out)
}

fn is_under(path: &str, root: &str) -> bool {
    !root.is_empty()
        && path.len() > root.len()
        && path.starts_with(root)
        && path.as_bytes()[root.len()] == b'\\'
}

fn extension(path: &str) -> &str {
    let name = path.rsplit('\\').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) => &name[i + 1..],
        None => "",
    }
}

/// Returns true when `path` must not be deleted, renamed or overwritten.
pub fn is_protected(path: &Path) -> bool {
    let Some(p) = normalize(path) else {
        return true;
    };
    let roots = roots();

    // UNC: require at least \\server\share\something.
    if let Some(body) = p.strip_prefix("\\\\") {
        return body.split('\\').filter(|c| !c.is_empty()).count() < 3;
    }

    // Drive roots.
    if p.len() <= 3 {
        return true;
    }

    // Critical names directly under a drive root, and everything below them.
    let after_root = &p[3..];
    let first = after_root.split('\\').next().unwrap_or("");
    if DRIVE_ROOT_PROTECTED.contains(&first) {
        return true;
    }

    // The Windows directory: allow-list only.
    if p == roots.windows {
        return true;
    }
    if is_under(&p, &roots.windows) {
        let rel = &p[roots.windows.len() + 1..];
        let in_allowed_tree = WINDOWS_ALLOWED_TREES.iter().any(|tree| {
            rel.len() > tree.len() && rel.starts_with(tree) && rel.as_bytes()[tree.len()] == b'\\'
        });
        if in_allowed_tree {
            return false;
        }
        if rel == "memory.dmp" {
            return false;
        }
        let (dir, _file) = match rel.rfind('\\') {
            Some(i) => (&rel[..i], &rel[i + 1..]),
            None => ("", rel),
        };
        let log_like =
            WINDOWS_LOG_FILE_DIRS.contains(&dir) && LOG_LIKE_EXTENSIONS.contains(&extension(rel));
        return !log_like;
    }

    // User profiles: the profile folder and its well-known children.
    if p == roots.users {
        return true;
    }
    if is_under(&p, &roots.users) {
        let rel = &p[roots.users.len() + 1..];
        let parts: Vec<&str> = rel.split('\\').collect();
        return match parts.as_slice() {
            [_profile] => true,
            [_profile, child] => {
                PROFILE_PROTECTED_CHILDREN.contains(child)
                    || child.starts_with("ntuser")
                    || child.starts_with("onedrive")
            }
            [_profile, "appdata", area] => matches!(*area, "local" | "roaming" | "locallow"),
            [_profile, "appdata", "local", "microsoft"]
            | [_profile, "appdata", "roaming", "microsoft"]
            | [_profile, "appdata", "local", "packages"]
            | [_profile, "appdata", "local", "programs"]
            | [_profile, "appdata", "local", "microsoft", "windows"]
            | [_profile, "appdata", "roaming", "microsoft", "windows"] => true,
            _ => false,
        };
    }

    if roots.exact.contains(&p) {
        return true;
    }
    if roots.trees.iter().any(|t| p == *t || is_under(&p, t)) {
        return true;
    }

    false
}

/// Fails with [`Error::Protected`] when `path` is guarded.
///
/// On top of the pure [`is_protected`] check this resolves the *parent*
/// folder on disk, so 8.3 short names (`C:\PROGRA~1`) and paths that reach a
/// protected area through a junction are caught as well. The entry itself is
/// deliberately not resolved: deleting a link must remove the link, never
/// its target.
pub fn ensure_deletable(path: &Path) -> Result<()> {
    if is_protected(path) {
        return Err(Error::Protected(path.to_path_buf()));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Error::Protected(path.to_path_buf()));
    };
    if resolved_parent_is_protected(parent, name) {
        return Err(Error::Protected(path.to_path_buf()));
    }
    Ok(())
}

fn resolved_parent_is_protected(parent: &Path, name: &std::ffi::OsStr) -> bool {
    use std::cell::RefCell;
    use std::path::PathBuf;
    thread_local! {
        // Files are deleted folder by folder, so remembering the last parent
        // removes nearly all of the resolution cost.
        static LAST: RefCell<Option<(PathBuf, Option<PathBuf>)>> = const { RefCell::new(None) };
    }
    let resolved = LAST.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some((cached_parent, cached)) = slot.as_ref()
            && cached_parent == parent
        {
            return cached.clone();
        }
        let resolved = std::fs::canonicalize(parent).ok();
        *slot = Some((parent.to_path_buf(), resolved.clone()));
        resolved
    });
    match resolved {
        // A parent that does not exist cannot contain anything to delete; the
        // pure check above has already passed, so there is nothing to add.
        None => false,
        Some(real_parent) => is_protected(&real_parent.join(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn win(rel: &str) -> PathBuf {
        paths::windows_dir().join(rel)
    }

    fn profile(rel: &str) -> PathBuf {
        paths::UserDirs::current().profile.join(rel)
    }

    #[test]
    fn rejects_relative_and_traversal() {
        assert!(is_protected(Path::new("temp\\file.txt")));
        assert!(is_protected(Path::new("")));
        assert!(is_protected(Path::new(
            r"C:\Windows\Temp\..\System32\kernel32.dll"
        )));
        assert!(is_protected(Path::new(r"C:\data\.\x")));
        assert!(is_protected(Path::new(r"\\.\PhysicalDrive0")));
        assert!(is_protected(Path::new(r"C:\Windows.\notepad.exe")));
    }

    #[test]
    fn rejects_drive_and_share_roots() {
        assert!(is_protected(Path::new(r"C:\")));
        assert!(is_protected(Path::new(r"D:\")));
        assert!(is_protected(Path::new(r"\\?\C:\")));
        assert!(is_protected(Path::new(r"\\server\share")));
        assert!(!is_protected(Path::new(r"\\server\share\folder\file.tmp")));
        assert!(is_protected(Path::new(r"C:\$Recycle.Bin")));
        assert!(is_protected(Path::new(r"D:\$RECYCLE.BIN\S-1-5-21\x.txt")));
        assert!(is_protected(Path::new(r"C:\System Volume Information\x")));
        assert!(is_protected(Path::new(r"C:\pagefile.sys")));
        assert!(is_protected(Path::new(r"C:\hiberfil.sys")));
    }

    #[test]
    fn windows_directory_is_allow_list_only() {
        assert!(is_protected(&win("")));
        assert!(is_protected(&win("System32")));
        assert!(is_protected(&win(r"System32\kernel32.dll")));
        assert!(is_protected(&win(r"System32\drivers\etc\hosts")));
        assert!(is_protected(&win(r"SysWOW64\ntdll.dll")));
        assert!(is_protected(&win(r"WinSxS\x\y.dll")));
        assert!(is_protected(&win("explorer.exe")));
        assert!(is_protected(&win("Fonts")));
        assert!(is_protected(&win(r"Fonts\arial.ttf")));
        assert!(is_protected(&win(r"Installer\abc.msi")));
        // Allowed trees: contents yes, the tree root itself no.
        assert!(is_protected(&win("Temp")));
        assert!(!is_protected(&win(r"Temp\foo.tmp")));
        assert!(!is_protected(&win(r"Temp\sub\dir")));
        assert!(!is_protected(&win(r"Prefetch\APP.EXE-1234.pf")));
        assert!(!is_protected(&win(
            r"SoftwareDistribution\Download\abc\file.cab"
        )));
        assert!(is_protected(&win(
            r"SoftwareDistribution\DataStore\DataStore.edb"
        )));
        assert!(!is_protected(&win(r"System32\LogFiles\WMI\x.etl")));
        assert!(!is_protected(&win("MEMORY.DMP")));
        assert!(!is_protected(&win(r"Minidump\010126-1.dmp")));
        // Log-like files directly in permitted folders.
        assert!(!is_protected(&win("WindowsUpdate.log")));
        assert!(!is_protected(&win(r"inf\setupapi.dev.log")));
        assert!(is_protected(&win(r"inf\usbstor.inf")));
        assert!(is_protected(&win("win.ini")));
        // A sibling whose name merely starts with an allowed tree name.
        assert!(is_protected(&win(r"TempEvil\x.tmp")));
    }

    #[test]
    fn verbatim_prefix_is_understood() {
        let verbatim = PathBuf::from(format!(r"\\?\{}", win(r"System32\cmd.exe").display()));
        assert!(is_protected(&verbatim));
        let ok = PathBuf::from(format!(r"\\?\{}", win(r"Temp\x.tmp").display()));
        assert!(!is_protected(&ok));
    }

    #[test]
    fn user_profile_structure_is_protected() {
        assert!(is_protected(&paths::users_dir()));
        assert!(is_protected(&profile("")));
        assert!(is_protected(&profile("Documents")));
        assert!(is_protected(&profile("Desktop")));
        assert!(is_protected(&profile("Downloads")));
        assert!(is_protected(&profile("AppData")));
        assert!(is_protected(&profile(r"AppData\Local")));
        assert!(is_protected(&profile(r"AppData\Roaming")));
        assert!(is_protected(&profile(r"AppData\Local\Microsoft")));
        assert!(is_protected(&profile(r"AppData\Local\Microsoft\Windows")));
        assert!(is_protected(&profile("NTUSER.DAT")));
        assert!(is_protected(&profile("ntuser.dat.LOG1")));
        // Other users' profiles follow the same shape.
        let other = paths::users_dir().join("SomeoneElse");
        assert!(is_protected(&other));
        assert!(is_protected(&other.join("Documents")));
        assert!(!is_protected(&other.join(r"AppData\Local\Temp\x.tmp")));
        // Ordinary content is fine.
        assert!(!is_protected(&profile(r"AppData\Local\Temp\abc.tmp")));
        assert!(!is_protected(&profile(r"Documents\report-copy.docx")));
        assert!(!is_protected(&profile(
            r"AppData\Local\Microsoft\Windows\INetCache\x"
        )));
    }

    #[test]
    fn program_folders() {
        assert!(is_protected(&paths::program_files()));
        assert!(is_protected(&paths::program_files_x86()));
        assert!(is_protected(&paths::program_data()));
        assert!(is_protected(&paths::program_files().join("WindowsApps")));
        assert!(is_protected(
            &paths::program_files().join(r"WindowsApps\Some.App\app.exe")
        ));
        assert!(is_protected(&paths::program_files().join("Common Files")));
        assert!(!is_protected(
            &paths::program_files().join(r"SomeApp\logs\old.log")
        ));
        assert!(!is_protected(
            &paths::program_data().join(r"Microsoft\Windows\WER\ReportQueue\x")
        ));
    }

    #[test]
    fn junction_into_protected_area_is_caught() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("sys");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(paths::system32_dir())
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink failed");
        // Unlink the junction even if an assertion below fails, so the
        // temporary folder never contains a link when it is cleaned up.
        struct Unlink(PathBuf);
        impl Drop for Unlink {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let _unlink = Unlink(link.clone());
        // Lexically harmless, but it really is System32\cmd.exe.
        let sneaky = link.join("cmd.exe");
        assert!(!is_protected(&sneaky));
        assert!(matches!(
            ensure_deletable(&sneaky),
            Err(Error::Protected(_))
        ));
        // The junction itself may be removed (that never touches the target).
        assert!(ensure_deletable(&link).is_ok());
    }

    #[test]
    fn short_names_are_resolved() {
        // C:\PROGRA~1 is the 8.3 alias of Program Files on virtually every system.
        let short = paths::system_drive().join("PROGRA~1");
        if short.exists() {
            let inside = short.join("WindowsApps");
            assert!(matches!(
                ensure_deletable(&inside),
                Err(Error::Protected(_))
            ));
        }
    }

    #[test]
    fn ordinary_paths_are_allowed() {
        assert!(!is_protected(Path::new(r"D:\Backups\old.zip")));
        assert!(!is_protected(Path::new(r"C:\Temp\x")));
        assert!(ensure_deletable(Path::new(r"D:\Backups\old.zip")).is_ok());
        assert!(matches!(
            ensure_deletable(Path::new(r"C:\")),
            Err(Error::Protected(_))
        ));
    }
}
