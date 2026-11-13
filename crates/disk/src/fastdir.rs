//! Allocation-free directory enumeration.
//!
//! `std::fs::read_dir` allocates a path and a name for every entry. When a
//! scan touches millions of files that dominates the CPU profile, so the
//! scanners in this crate read the raw find data instead: one buffer per
//! directory, names borrowed as UTF-16 slices, large-fetch enabled so the
//! kernel returns many entries per call.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

pub(crate) const ATTR_READ_ONLY: u32 = 0x1;
pub(crate) const ATTR_HIDDEN: u32 = 0x2;
pub(crate) const ATTR_SYSTEM: u32 = 0x4;
pub(crate) const ATTR_DIRECTORY: u32 = 0x10;
pub(crate) const ATTR_REPARSE_POINT: u32 = 0x400;
pub(crate) const ATTR_OFFLINE: u32 = 0x1000;
pub(crate) const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub(crate) const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// Bit that marks a reparse tag as a "name surrogate": the entry stands in
/// for another location (junction, symbolic link, mount point).
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

/// Offset between the Windows epoch (1601) and the Unix epoch, in 100 ns ticks.
const FILETIME_UNIX_OFFSET: u64 = 116_444_736_000_000_000;

/// One directory entry, borrowed from the enumeration buffer.
pub(crate) struct RawEntry<'a> {
    /// File name as UTF-16 (no terminator).
    pub name: &'a [u16],
    /// `FILE_ATTRIBUTE_*` bits.
    pub attributes: u32,
    /// Logical size in bytes (zero for directories).
    pub size: u64,
    /// Last write time in FILETIME ticks.
    pub modified: u64,
    /// Reparse tag (only meaningful when the reparse attribute is set).
    pub reparse_tag: u32,
}

impl RawEntry<'_> {
    /// True for junctions and symbolic links. This is the same test the
    /// standard library applies for `FileType::is_symlink`, and therefore
    /// agrees with `zest_core::walk::is_link`.
    pub fn is_link(&self) -> bool {
        self.attributes & ATTR_REPARSE_POINT != 0
            && self.reparse_tag & REPARSE_TAG_NAME_SURROGATE != 0
    }

    /// True for directories (including links to directories; test
    /// [`RawEntry::is_link`] first).
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    /// True when the contents live in the cloud and reading would download them.
    pub fn is_cloud_placeholder(&self) -> bool {
        self.attributes & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
    }

    /// The file name as an owned OS string (lossless).
    pub fn os_name(&self) -> OsString {
        OsString::from_wide(self.name)
    }

    /// Full path of this entry inside `dir`.
    pub fn path_in(&self, dir: &Path) -> PathBuf {
        dir.join(self.os_name())
    }
}

/// Converts FILETIME ticks to a [`SystemTime`]. Returns `None` for times
/// before 1970 or zero.
pub(crate) fn filetime_to_system(ticks: u64) -> Option<SystemTime> {
    let since_unix = ticks.checked_sub(FILETIME_UNIX_OFFSET)?;
    if ticks == 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::new(
        since_unix / 10_000_000,
        ((since_unix % 10_000_000) * 100) as u32,
    ))
}

/// Converts a [`SystemTime`] to FILETIME ticks (zero when out of range).
pub(crate) fn system_to_filetime(time: SystemTime) -> u64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            FILETIME_UNIX_OFFSET + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100
        }
        Err(_) => 0,
    }
}

/// Builds the NUL-terminated `\\?\...\*` search pattern for a directory.
///
/// The verbatim prefix lifts the 260 character limit. The caller must pass
/// an absolute path without `.`/`..` components (see [`absolute`]).
fn search_pattern(dir: &Path) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    let raw: Vec<u16> = dir.as_os_str().encode_wide().collect();
    let verbatim: &[u16] = &[BACKSLASH, BACKSLASH, b'?' as u16, BACKSLASH];
    let mut out: Vec<u16> = Vec::with_capacity(raw.len() + 12);
    if raw.starts_with(verbatim) {
        out.extend_from_slice(&raw);
    } else if raw.starts_with(&[BACKSLASH, BACKSLASH]) {
        // \\server\share\dir -> \\?\UNC\server\share\dir
        out.extend_from_slice(verbatim);
        out.extend("UNC\\".encode_utf16());
        out.extend_from_slice(&raw[2..]);
    } else {
        out.extend_from_slice(verbatim);
        out.extend_from_slice(&raw);
    }
    if out.last() != Some(&BACKSLASH) {
        out.push(BACKSLASH);
    }
    out.push(b'*' as u16);
    out.push(0);
    out
}

struct FindGuard(HANDLE);

impl Drop for FindGuard {
    fn drop(&mut self) {
        // SAFETY: the handle came from FindFirstFileExW and is closed once.
        unsafe {
            let _ = FindClose(self.0);
        }
    }
}

fn io_error(e: &windows::core::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// Calls `visit` for every entry of `dir` except `.` and `..`.
///
/// Returns an error when the directory cannot be opened (access denied,
/// vanished, ...). An empty directory is not an error.
pub(crate) fn read_dir_raw(
    dir: &Path,
    visit: &mut dyn FnMut(&RawEntry<'_>),
) -> std::io::Result<()> {
    const DOT: u16 = b'.' as u16;
    let pattern = search_pattern(dir);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL terminated and outlives the call; `data` is a
    // correctly sized WIN32_FIND_DATAW as required by FindExInfoBasic.
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (&raw mut data).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match handle {
        Ok(handle) => FindGuard(handle),
        Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(()),
        Err(e) => return Err(io_error(&e)),
    };

    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        let is_dots = name == [DOT] || name == [DOT, DOT];
        if !is_dots {
            visit(&RawEntry {
                name,
                attributes: data.dwFileAttributes,
                size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                modified: (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32)
                    | u64::from(data.ftLastWriteTime.dwLowDateTime),
                reparse_tag: data.dwReserved0,
            });
        }
        // SAFETY: `handle` is a live find handle and `data` is valid for writes.
        match unsafe { FindNextFileW(handle.0, &mut data) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(()),
            Err(e) => return Err(io_error(&e)),
        }
    }
}

/// Makes a path absolute and lexically clean (`.`/`..` resolved, forward
/// slashes converted) without touching the file system.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Lower-cased, separator-normalised form of a path used for comparisons.
pub(crate) fn path_key(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(rest) = s.strip_prefix("\\\\?\\unc\\") {
        s = format!("\\\\{rest}");
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        s = rest.to_owned();
    }
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// True when `path_key` lies inside (or equals) `root_key`.
pub(crate) fn key_is_under(path_key: &str, root_key: &str) -> bool {
    if path_key == root_key {
        return true;
    }
    let root = root_key.trim_end_matches('\\');
    path_key.len() > root.len()
        && path_key.starts_with(root)
        && path_key.as_bytes()[root.len()] == b'\\'
}

/// Cleans a list of scan roots: absolute paths, duplicates and nested roots
/// removed (scanning `C:\` and `C:\Users` would count everything twice).
pub(crate) fn normalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut cleaned: Vec<(String, PathBuf)> = Vec::new();
    for root in roots {
        let abs = absolute(root);
        let key = path_key(&abs);
        if cleaned.iter().any(|(k, _)| key_is_under(&key, k)) {
            continue;
        }
        cleaned.retain(|(k, _)| !key_is_under(k, &key));
        cleaned.push((key, abs));
    }
    cleaned.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn enumerates_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), vec![1u8; 1234]).unwrap();
        fs::write(tmp.path().join("b.bin"), b"").unwrap();

        let mut seen = Vec::new();
        read_dir_raw(tmp.path(), &mut |e| {
            seen.push((
                e.os_name().to_string_lossy().into_owned(),
                e.is_dir(),
                e.size,
                e.is_link(),
            ));
        })
        .unwrap();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("a.txt".to_owned(), false, 1234, false),
                ("b.bin".to_owned(), false, 0, false),
                ("sub".to_owned(), true, 0, false),
            ]
        );
    }

    #[test]
    fn empty_and_missing_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let mut count = 0;
        read_dir_raw(tmp.path(), &mut |_| count += 1).unwrap();
        assert_eq!(count, 0);
        assert!(read_dir_raw(&tmp.path().join("nope"), &mut |_| {}).is_err());
    }

    #[test]
    fn junctions_are_reported_as_links() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(tmp.path().join("link"))
            .arg(&target)
            .output()
            .unwrap();
        assert!(out.status.success());
        let mut links = Vec::new();
        read_dir_raw(tmp.path(), &mut |e| {
            if e.is_link() {
                links.push(e.os_name());
            }
        })
        .unwrap();
        assert_eq!(links, vec![OsString::from("link")]);
    }

    #[test]
    fn long_paths_are_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        for _ in 0..12 {
            deep.push("a-rather-long-directory-name-to-exceed-max-path");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("leaf.dat"), b"12345").unwrap();
        assert!(deep.as_os_str().len() > 300);
        let mut found = None;
        read_dir_raw(&deep, &mut |e| found = Some((e.os_name(), e.size))).unwrap();
        assert_eq!(found, Some((OsString::from("leaf.dat"), 5)));
    }

    #[test]
    fn timestamps_round_trip_and_match_std() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.txt");
        fs::write(&file, b"x").unwrap();
        let std_time = fs::metadata(&file).unwrap().modified().unwrap();
        let mut raw = 0;
        read_dir_raw(tmp.path(), &mut |e| raw = e.modified).unwrap();
        assert_eq!(filetime_to_system(raw), Some(std_time));
        assert_eq!(system_to_filetime(std_time), raw);
        assert_eq!(filetime_to_system(0), None);
    }

    #[test]
    fn root_normalisation() {
        let roots = normalize_roots(&[
            PathBuf::from(r"C:\Users\Someone\Docs"),
            PathBuf::from(r"C:\Users"),
            PathBuf::from(r"c:\users\"),
            PathBuf::from(r"D:\Data"),
            PathBuf::from(r"C:\UsersBackup"),
        ]);
        let keys: Vec<String> = roots.iter().map(|p| path_key(p)).collect();
        assert_eq!(keys, vec![r"c:\users", r"d:\data", r"c:\usersbackup"]);
        assert!(key_is_under(r"c:\users\a", r"c:\"));
        assert!(key_is_under(r"c:\users\a", r"c:\users"));
        assert!(!key_is_under(r"c:\usersx", r"c:\users"));
        assert_eq!(path_key(Path::new(r"\\?\C:\Temp\")), r"c:\temp");
        assert_eq!(path_key(Path::new(r"C:\")), r"c:\");
    }
}
