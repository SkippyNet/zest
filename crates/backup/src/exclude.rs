//! Patterns for files and folders a backup leaves out.
//!
//! * A pattern without a backslash is compared with the **name** of each file
//!   and folder: `*.tmp`, `node_modules`, `Thumbs.db`.
//! * A pattern with a backslash is compared with the **full path**:
//!   `C:\Projects\*\target`, `%USERPROFILE%\Downloads`.
//!
//! `*` stands for any run of characters and `?` for exactly one. Upper and
//! lower case are the same. A folder that matches is left out with everything
//! in it.

use std::path::Path;
use zest_core::paths;

/// Left out unless the user removes them: files Windows recreates on its own
/// and scratch files of running programs.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "*.tmp",
    "~$*",
    "Thumbs.db",
    "$RECYCLE.BIN",
    "System Volume Information",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
];

/// A compiled set of exclusion patterns.
#[derive(Clone, Debug, Default)]
pub struct Excludes {
    names: Vec<Vec<char>>,
    paths: Vec<Vec<char>>,
}

impl Excludes {
    /// Compiles patterns. Blank entries are ignored.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Excludes {
        let mut excludes = Excludes::default();
        for pattern in patterns {
            let pattern = pattern.as_ref().trim();
            if pattern.is_empty() {
                continue;
            }
            let normalized = pattern.replace('/', "\\");
            if normalized.contains('\\') {
                let expanded = paths::expand_env(&normalized);
                let trimmed = expanded.trim_end_matches('\\');
                excludes
                    .paths
                    .push(trimmed.to_lowercase().chars().collect());
            } else {
                excludes
                    .names
                    .push(normalized.to_lowercase().chars().collect());
            }
        }
        excludes
    }

    /// True when no pattern was given.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty()
    }

    /// Whether the file or folder at `path`, whose last part is `name`, is
    /// left out.
    pub fn matches(&self, path: &Path, name: &str) -> bool {
        if !self.names.is_empty() {
            let name: Vec<char> = name.to_lowercase().chars().collect();
            if self.names.iter().any(|p| wildcard_match(p, &name)) {
                return true;
            }
        }
        if !self.paths.is_empty() {
            let full: Vec<char> = path
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
                .chars()
                .collect();
            if self.paths.iter().any(|p| wildcard_match(p, &full)) {
                return true;
            }
        }
        false
    }
}

/// Splits what a user typed into a single field into patterns. Entries are
/// separated by semicolons or line breaks.
pub fn split_patterns(text: &str) -> Vec<String> {
    text.split([';', '\n', '\r'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Matches `text` against `pattern` where `*` is any run of characters
/// (including none) and `?` is exactly one character.
fn wildcard_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    // Where to resume after the last `*` when a later character mismatches.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wild(pattern: &str, text: &str) -> bool {
        let p: Vec<char> = pattern.chars().collect();
        let t: Vec<char> = text.chars().collect();
        wildcard_match(&p, &t)
    }

    #[test]
    fn wildcards() {
        assert!(wild("*.tmp", "a.tmp"));
        assert!(wild("*.tmp", ".tmp"));
        assert!(!wild("*.tmp", "a.tmp.bak"));
        assert!(wild("~$*", "~$report.docx"));
        assert!(wild("a?c", "abc"));
        assert!(!wild("a?c", "ac"));
        assert!(wild("*", ""));
        assert!(wild("**", "anything"));
        assert!(wild("a*b*c", "a--b--b--c"));
        assert!(!wild("a*b*c", "a--b--b--"));
        assert!(wild("", ""));
        assert!(!wild("", "x"));
        assert!(wild("über*", "übersicht"));
    }

    #[test]
    fn name_patterns_look_at_the_last_part_only() {
        let ex = Excludes::new(&["*.TMP", "node_modules", "Thumbs.db"]);
        assert!(ex.matches(Path::new(r"C:\work\a.tmp"), "a.tmp"));
        assert!(ex.matches(Path::new(r"C:\work\A.Tmp"), "A.Tmp"));
        assert!(ex.matches(Path::new(r"C:\work\app\node_modules"), "node_modules"));
        assert!(ex.matches(Path::new(r"C:\pics\thumbs.db"), "thumbs.db"));
        assert!(!ex.matches(Path::new(r"C:\node_modules\keep.txt"), "keep.txt"));
        assert!(!ex.matches(Path::new(r"C:\work\a.tmpl"), "a.tmpl"));
    }

    #[test]
    fn path_patterns_look_at_the_whole_path() {
        let ex = Excludes::new(&[r"C:\Projects\*\target", "c:/users/me/downloads/"]);
        assert!(ex.matches(Path::new(r"C:\Projects\zest\target"), "target"));
        assert!(ex.matches(Path::new(r"c:\projects\a\b\target"), "target"));
        assert!(!ex.matches(Path::new(r"D:\Projects\zest\target"), "target"));
        assert!(ex.matches(Path::new(r"C:\Users\me\Downloads"), "Downloads"));
        assert!(!ex.matches(Path::new(r"C:\Users\me\Downloads\x.zip"), "x.zip"));
    }

    #[test]
    fn environment_variables_are_expanded_in_path_patterns() {
        let windir = std::env::var("SystemRoot").unwrap();
        let ex = Excludes::new(&[r"%SystemRoot%\Temp"]);
        assert!(ex.matches(&Path::new(&windir).join("Temp"), "Temp"));
    }

    #[test]
    fn blank_patterns_are_ignored() {
        let ex = Excludes::new(&["", "   "]);
        assert!(ex.is_empty());
        assert!(!ex.matches(Path::new(r"C:\a"), "a"));
        assert_eq!(
            split_patterns("*.tmp; Thumbs.db ;\n node_modules\r\n;;"),
            vec!["*.tmp", "Thumbs.db", "node_modules"]
        );
    }
}
