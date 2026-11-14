//! Directory enumeration and path helpers for the disk scanners.
//!
//! The enumerator itself lives in [`zest_core::fastdir`] so that every part
//! of the product reads folders the same fast way; this module adds the
//! path bookkeeping the scanners in this crate share.

use std::path::{Path, PathBuf};

pub(crate) use zest_core::fastdir::{
    ATTR_HIDDEN, ATTR_READ_ONLY, ATTR_SYSTEM, RawEntry, absolute, filetime_to_system, read_dir_raw,
    system_to_filetime,
};

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
