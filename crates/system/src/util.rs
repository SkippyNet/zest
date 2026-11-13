//! Small private helpers shared by the modules of this crate.

use chrono::NaiveDateTime;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use zest_core::fileinfo::{VersionInfo, version_info};
use zest_core::reg::RegPath;
use zest_core::{Error, paths};

/// `E_ACCESSDENIED` as an unsigned HRESULT.
const E_ACCESSDENIED: u32 = 0x8007_0005;

/// NUL terminated UTF-16 copy of `s`.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// NUL terminated UTF-16 copy of an OS string (paths).
pub(crate) fn wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Decodes a UTF-16 buffer up to its first NUL.
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Decodes a NUL terminated UTF-16 string behind a raw pointer.
///
/// # Safety
/// `ptr` must be null or point at a NUL terminated UTF-16 string that stays
/// valid for the duration of the call.
pub(crate) unsafe fn from_wide_ptr(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

/// True when a publisher string names Microsoft.
pub(crate) fn is_microsoft(publisher: Option<&str>) -> bool {
    publisher.is_some_and(|p| p.to_lowercase().contains("microsoft"))
}

/// Maps a Windows API failure, turning "access denied" into
/// [`Error::NeedsElevation`] with a description of the attempted `action`.
pub(crate) fn win_err(e: windows::core::Error, action: &str) -> Error {
    if e.code().0 as u32 == E_ACCESSDENIED {
        Error::NeedsElevation(action.to_owned())
    } else {
        Error::Windows(format!(
            "could not {action}: {} (0x{:08X})",
            e.message(),
            e.code().0 as u32
        ))
    }
}

/// Maps an I/O failure (registry, files), turning "permission denied" into
/// [`Error::NeedsElevation`].
pub(crate) fn io_err(e: std::io::Error, action: &str) -> Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        Error::NeedsElevation(action.to_owned())
    } else {
        Error::Io {
            context: format!("could not {action}"),
            source: e,
        }
    }
}

/// Converts an OLE automation date (days since 1899-12-30, local time) into
/// a timestamp. Dates before the year 2000 mean "never" in the Task
/// Scheduler API and yield `None`.
pub(crate) fn ole_date_to_local(days: f64) -> Option<NaiveDateTime> {
    if !days.is_finite() || days < 36_526.0 {
        // 36526 = 2000-01-01
        return None;
    }
    let epoch = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let millis = (days * 86_400_000.0).round() as i64;
    epoch.checked_add_signed(chrono::Duration::milliseconds(millis))
}

/// The current time as a Windows FILETIME value (100 ns ticks since 1601).
pub(crate) fn filetime_now() -> u64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (unix.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(unix.subsec_nanos() / 100)
}

/// Security identifier of the signed-in user (cached).
pub(crate) fn current_user_sid() -> Option<&'static str> {
    static SID: OnceLock<Option<String>> = OnceLock::new();
    SID.get_or_init(|| {
        paths::UserDirs::all()
            .into_iter()
            .find(|u| u.is_current)
            .and_then(|u| u.sid)
    })
    .as_deref()
}

/// Resolves indirect resource strings such as `@shell32.dll,-8506` or
/// `@{PackageFullName?ms-resource://...}` to their display text.
/// Returns `None` when `text` is not indirect or cannot be resolved.
pub(crate) fn load_indirect_string(text: &str) -> Option<String> {
    use windows::Win32::UI::Shell::SHLoadIndirectString;
    use windows::core::PCWSTR;

    if !text.starts_with('@') {
        return None;
    }
    let source = wide(text);
    let mut out = [0u16; 1024];
    // SAFETY: `source` is NUL terminated; `out` is a valid writable buffer.
    unsafe { SHLoadIndirectString(PCWSTR(source.as_ptr()), &mut out, None) }.ok()?;
    let resolved = from_wide(&out).trim().to_owned();
    (!resolved.is_empty() && !resolved.starts_with('@')).then_some(resolved)
}

/// The server (DLL or executable) registered for a COM class below
/// `clsid_root` (normally `HKCR\CLSID`), unexpanded.
pub(crate) fn clsid_server(clsid_root: &RegPath, clsid: &str) -> Option<String> {
    let class = clsid_root.child(clsid);
    ["InprocServer32", "LocalServer32"]
        .iter()
        .find_map(|sub| class.child(sub).read_string(""))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Remembers version-resource lookups so that binaries shared by many
/// entries (service hosts, launchers) are only read once.
#[derive(Default)]
pub(crate) struct InfoCache {
    map: HashMap<PathBuf, Option<VersionInfo>>,
}

impl InfoCache {
    /// Version information of `path`, if it has any.
    pub(crate) fn get(&mut self, path: &Path) -> Option<&VersionInfo> {
        self.map
            .entry(path.to_path_buf())
            .or_insert_with(|| version_info(path))
            .as_ref()
    }

    /// Publisher and friendly description of `path`.
    pub(crate) fn publisher_and_description(
        &mut self,
        path: &Path,
    ) -> (Option<String>, Option<String>) {
        match self.get(path) {
            Some(info) => (info.company.clone(), info.description.clone()),
            None => (None, None),
        }
    }
}

/// Lower-cased text form of a path without trailing separators, for
/// case-insensitive comparisons.
pub(crate) fn norm_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// True when `path` is `root` or lies below it (case-insensitive).
pub(crate) fn is_within(path: &Path, root: &Path) -> bool {
    let path = norm_path(path);
    let root = norm_path(root);
    !root.is_empty()
        && (path == root
            || (path.len() > root.len()
                && path.starts_with(&root)
                && path.as_bytes()[root.len()] == b'\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_round_trip() {
        let w = wide("héllo");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(from_wide(&w), "héllo");
        // SAFETY: `w` is NUL terminated and alive.
        assert_eq!(unsafe { from_wide_ptr(w.as_ptr()) }, "héllo");
        // SAFETY: null is explicitly allowed.
        assert_eq!(unsafe { from_wide_ptr(std::ptr::null()) }, "");
    }

    #[test]
    fn microsoft_detection() {
        assert!(is_microsoft(Some("Microsoft Corporation")));
        assert!(is_microsoft(Some("MICROSOFT WINDOWS")));
        assert!(!is_microsoft(Some("Valve Corporation")));
        assert!(!is_microsoft(None));
    }

    #[test]
    fn ole_dates() {
        assert_eq!(ole_date_to_local(0.0), None);
        // 1999-11-30 is what Task Scheduler reports for "never".
        assert_eq!(ole_date_to_local(36_494.0), None);
        let d = ole_date_to_local(45_000.5).unwrap();
        assert_eq!(d.to_string(), "2023-03-15 12:00:00");
        assert_eq!(ole_date_to_local(f64::NAN), None);
    }

    #[test]
    fn filetime_is_plausible() {
        // 2020-01-01 in FILETIME ticks.
        assert!(filetime_now() > 132_223_104_000_000_000);
    }

    #[test]
    fn path_containment() {
        let root = Path::new(r"C:\Program Files\App");
        assert!(is_within(
            Path::new(r"c:\program files\app\bin\x.exe"),
            root
        ));
        assert!(is_within(Path::new(r"C:\Program Files\App\"), root));
        assert!(!is_within(Path::new(r"C:\Program Files\Apple\x.exe"), root));
        assert!(!is_within(Path::new(r"C:\Program Files"), root));
        assert!(!is_within(Path::new(r"C:\x"), Path::new("")));
    }

    #[test]
    fn indirect_strings() {
        assert_eq!(load_indirect_string("plain text"), None);
        let resolved = load_indirect_string(r"@%SystemRoot%\system32\shell32.dll,-8506");
        assert!(resolved.is_some_and(|s| !s.is_empty()));
        assert_eq!(load_indirect_string(r"@C:\no\such\file.dll,-1"), None);
    }

    #[test]
    fn info_cache_reuses_lookups() {
        let mut cache = InfoCache::default();
        let cmd = paths::system32_dir().join("cmd.exe");
        let (publisher, _) = cache.publisher_and_description(&cmd);
        assert!(is_microsoft(publisher.as_deref()));
        assert!(cache.get(Path::new(r"C:\no\such\file.exe")).is_none());
        assert_eq!(cache.map.len(), 2);
    }
}
