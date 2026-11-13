//! Matching of user exclusions ("never clean this").

use crate::model::Exclusion;
use crate::pattern::NamePattern;
use std::path::Path;
use zest_core::reg::RegPath;

fn norm_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn is_same_or_under(path: &str, root: &str) -> bool {
    path == root
        || (path.len() > root.len()
            && path.starts_with(root)
            && path.as_bytes()[root.len()] == b'\\')
}

/// A compiled set of [`Exclusion`]s.
#[derive(Clone, Debug, Default)]
pub struct Excluder {
    paths: Vec<String>,
    patterns: Vec<(String, NamePattern)>,
    registry: Vec<String>,
}

impl Excluder {
    /// Compiles the user's exclusion list.
    pub fn new(exclusions: &[Exclusion]) -> Self {
        let mut out = Excluder::default();
        for exclusion in exclusions {
            match exclusion {
                Exclusion::Path(path) => {
                    let norm = norm_path(path);
                    if !norm.is_empty() {
                        out.paths.push(norm);
                    }
                }
                Exclusion::Pattern { dir, pattern } => {
                    let norm = norm_path(dir);
                    if !norm.is_empty() {
                        out.patterns.push((norm, NamePattern::new(pattern)));
                    }
                }
                Exclusion::Registry(key) => {
                    // Accept both the short and the long hive spelling.
                    let norm = RegPath::parse(key)
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| key.trim().trim_end_matches('\\').to_owned())
                        .to_lowercase();
                    if !norm.is_empty() {
                        out.registry.push(norm);
                    }
                }
            }
        }
        out
    }

    /// True when there are no exclusions at all.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.patterns.is_empty() && self.registry.is_empty()
    }

    /// True when a file (or folder) must not be touched.
    pub fn excludes_path(&self, path: &Path) -> bool {
        if self.paths.is_empty() && self.patterns.is_empty() {
            return false;
        }
        let norm = norm_path(path);
        if self.paths.iter().any(|p| is_same_or_under(&norm, p)) {
            return true;
        }
        if !self.patterns.is_empty() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return self.patterns.iter().any(|(dir, pattern)| {
                is_same_or_under(&norm, dir) && norm != *dir && pattern.matches(&name)
            });
        }
        false
    }

    /// True when deleting the whole folder `dir` could remove something
    /// excluded, i.e. an exclusion lies inside (or is) the folder. Such
    /// folders must be cleaned file by file.
    pub fn touches_dir(&self, dir: &Path) -> bool {
        let norm = norm_path(dir);
        self.paths
            .iter()
            .any(|p| is_same_or_under(p, &norm) || is_same_or_under(&norm, p))
            || self
                .patterns
                .iter()
                .any(|(d, _)| is_same_or_under(d, &norm) || is_same_or_under(&norm, d))
    }

    /// True when a registry key (or anything below an excluded key) must not be touched.
    pub fn excludes_key(&self, key: &RegPath) -> bool {
        if self.registry.is_empty() {
            return false;
        }
        let norm = key.to_string().to_lowercase();
        self.registry.iter().any(|r| is_same_or_under(&norm, r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn paths_and_patterns() {
        let ex = Excluder::new(&[
            Exclusion::Path(PathBuf::from(r"C:\Keep\Folder\")),
            Exclusion::Path(PathBuf::from(r"C:\Temp\important.txt")),
            Exclusion::Pattern {
                dir: PathBuf::from(r"C:\Logs"),
                pattern: "*.keep;audit*.log".to_owned(),
            },
        ]);
        assert!(!ex.is_empty());
        assert!(ex.excludes_path(Path::new(r"c:\keep\folder")));
        assert!(ex.excludes_path(Path::new(r"C:\Keep\Folder\sub\file.bin")));
        assert!(!ex.excludes_path(Path::new(r"C:\Keep\FolderOther\file.bin")));
        assert!(ex.excludes_path(Path::new(r"C:/Temp/IMPORTANT.TXT")));
        assert!(!ex.excludes_path(Path::new(r"C:\Temp\other.txt")));
        assert!(ex.excludes_path(Path::new(r"C:\Logs\deep\x.keep")));
        assert!(ex.excludes_path(Path::new(r"C:\Logs\audit-2025.log")));
        assert!(!ex.excludes_path(Path::new(r"C:\Logs\app.log")));
        assert!(!ex.excludes_path(Path::new(r"C:\Elsewhere\x.keep")));
    }

    #[test]
    fn folder_intersection() {
        let ex = Excluder::new(&[Exclusion::Path(PathBuf::from(r"C:\Cache\App\keep.db"))]);
        assert!(ex.touches_dir(Path::new(r"C:\Cache")));
        assert!(ex.touches_dir(Path::new(r"C:\Cache\App")));
        assert!(!ex.touches_dir(Path::new(r"C:\Cache\Other")));
        let ex = Excluder::new(&[Exclusion::Path(PathBuf::from(r"C:\Cache"))]);
        assert!(ex.touches_dir(Path::new(r"C:\Cache\App")));
    }

    #[test]
    fn registry_keys() {
        let ex = Excluder::new(&[
            Exclusion::Registry(r"HKEY_CURRENT_USER\Software\Keep".to_owned()),
            Exclusion::Registry(r"HKLM\SOFTWARE\Vendor\".to_owned()),
        ]);
        assert!(ex.excludes_key(&RegPath::hkcu(r"Software\Keep")));
        assert!(ex.excludes_key(&RegPath::hkcu(r"software\keep\Sub")));
        assert!(!ex.excludes_key(&RegPath::hkcu(r"Software\KeepNot")));
        assert!(ex.excludes_key(&RegPath::hklm(r"SOFTWARE\Vendor\App")));
        assert!(!Excluder::default().excludes_key(&RegPath::hkcu("Software")));
    }
}
