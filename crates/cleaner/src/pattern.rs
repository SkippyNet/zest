//! Wildcard matching and wildcard path expansion.

use std::path::{Path, PathBuf};
use zest_core::walk::is_link;

/// Case-insensitive wildcard match. `*` matches any run of characters
/// (including none), `?` matches exactly one character.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((star_p, star_t)) = star {
            pi = star_p + 1;
            ti = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// True when `text` contains a wildcard character.
pub fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?'])
}

/// A `;`-separated list of file name wildcards, e.g. `*.log;*.etl`.
/// An empty specification (or `*`) matches every name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamePattern {
    parts: Vec<String>,
}

impl NamePattern {
    /// Parses a specification such as `*.tmp;~*`.
    pub fn new(spec: &str) -> Self {
        let parts: Vec<String> = spec
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty() && *p != "*" && *p != "*.*")
            .map(str::to_owned)
            .collect();
        // "*" anywhere in the list makes the whole pattern match everything.
        if spec.split(';').any(|p| matches!(p.trim(), "*" | "*.*")) {
            return NamePattern { parts: Vec::new() };
        }
        NamePattern { parts }
    }

    /// True when the pattern accepts every name.
    pub fn matches_all(&self) -> bool {
        self.parts.is_empty()
    }

    /// Tests a file name (not a full path).
    pub fn matches(&self, name: &str) -> bool {
        self.parts.is_empty() || self.parts.iter().any(|p| wildcard_match(p, name))
    }
}

/// Expands wildcards in any component of `path` into the paths that exist.
///
/// Links are never traversed for wildcard components, so a pattern such as
/// `Packages\*\AC\Temp` cannot wander through a junction. A path without
/// wildcards is returned as-is when it exists.
pub fn expand_glob(path: &Path) -> Vec<PathBuf> {
    let text = path.to_string_lossy();
    if !has_wildcard(&text) {
        return if path.exists() {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    }

    let mut components = path.components();
    let mut current: Vec<PathBuf> = Vec::new();
    // The prefix (drive + root) can never contain wildcards.
    let mut root = PathBuf::new();
    for comp in components.by_ref() {
        use std::path::Component;
        match comp {
            Component::Prefix(_) | Component::RootDir => root.push(comp.as_os_str()),
            other => {
                current.push(root.clone());
                current = step(current, &other.as_os_str().to_string_lossy());
                break;
            }
        }
    }
    for comp in components {
        if current.is_empty() {
            break;
        }
        current = step(current, &comp.as_os_str().to_string_lossy());
    }
    current
}

fn step(bases: Vec<PathBuf>, component: &str) -> Vec<PathBuf> {
    let mut next = Vec::new();
    for base in bases {
        if has_wildcard(component) {
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !wildcard_match(component, &name.to_string_lossy()) {
                    continue;
                }
                match entry.metadata() {
                    Ok(meta) if !is_link(&meta) => next.push(base.join(name)),
                    _ => {}
                }
            }
        } else {
            let candidate = base.join(component);
            if candidate.exists() {
                next.push(candidate);
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*", "anything.txt"));
        assert!(wildcard_match("*.LOG", "setup.log"));
        assert!(wildcard_match("thumbcache_*.db", "thumbcache_256.db"));
        assert!(wildcard_match("f?le.*", "file.txt"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(wildcard_match("*.tar.gz", "x.tar.gz"));
        assert!(!wildcard_match("*.log", "setup.log.bak"));
        assert!(!wildcard_match("f?le", "fle"));
        assert!(!wildcard_match("abc", "abcd"));
        assert!(!wildcard_match("", "x"));
        assert!(wildcard_match("", ""));
        assert!(wildcard_match("a**b", "ab"));
        assert!(wildcard_match("ÄB*", "äbc"));
    }

    #[test]
    fn name_patterns() {
        let all = NamePattern::new("*");
        assert!(all.matches_all() && all.matches("x"));
        assert!(NamePattern::new("").matches("anything"));
        assert!(NamePattern::new("*.log; *").matches_all());
        let logs = NamePattern::new("*.log; *.etl ;");
        assert!(logs.matches("A.LOG"));
        assert!(logs.matches("trace.etl"));
        assert!(!logs.matches("notes.txt"));
    }

    #[test]
    fn glob_expansion() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for pkg in ["App.One_abc", "App.Two_def", "Other"] {
            std::fs::create_dir_all(root.join("Packages").join(pkg).join("AC").join("Temp"))
                .unwrap();
        }
        std::fs::create_dir_all(root.join("Packages").join("NoTemp_xyz").join("AC")).unwrap();

        let mut found = expand_glob(&root.join("Packages").join("App.*").join("AC").join("Temp"));
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with(r"App.One_abc\AC\Temp"));

        assert_eq!(
            expand_glob(&root.join("Packages").join("*").join("AC").join("Temp")).len(),
            3
        );
        assert_eq!(
            expand_glob(&root.join("Packages").join("*").join("AC")).len(),
            4
        );
        assert_eq!(expand_glob(&root.join("Packages").join("Other")).len(), 1);
        assert!(expand_glob(&root.join("Missing").join("*")).is_empty());
        assert!(expand_glob(&root.join("Packages").join("Nope")).is_empty());
    }

    #[test]
    fn glob_does_not_traverse_links() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("Cache")).unwrap();
        let holder = tmp.path().join("holder");
        std::fs::create_dir_all(&holder).unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(holder.join("link"))
            .arg(&real)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(expand_glob(&holder.join("*").join("Cache")).is_empty());
    }
}
