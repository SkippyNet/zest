//! Application paths.
//!
//! `App Paths\<name>.exe` lets a program be started by its bare name. The
//! default value is the full path of the executable; when that file is
//! confirmed missing the whole entry is dead. Entries for programs in
//! Windows-owned locations are never reported.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::reg::RegPath;

const SUB_KEY: &str = r"Microsoft\Windows\CurrentVersion\App Paths";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for app_paths in super::distinct_children(super::software_roots(), SUB_KEY) {
        scan_key(&app_paths, progress, out);
    }
}

/// Scans the entries directly below an `App Paths`-shaped key.
pub(crate) fn scan_key(app_paths: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = app_paths.open() else {
        return;
    };
    for name in root.enum_keys().flatten() {
        if progress.is_cancelled() {
            return;
        }
        let Ok(entry) = root.open_subkey_with_flags(&name, KEY_READ) else {
            continue;
        };
        let Some(raw) = check::raw_string_value(&entry, "") else {
            continue;
        };
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        // Programs that ship with Windows may be optional features.
        let Some(path) = check::outside_windows(check::missing_file(text)) else {
            continue;
        };
        let entry_path = app_paths.child(&name);
        out.push(Issue::for_key(
            Category::AppPaths,
            &entry_path,
            "Application path points to a missing program",
            format!("{name} -> {}", path.display()),
            Confidence::Safe,
            vec![
                Condition::ValueEquals {
                    key: entry_path.clone(),
                    name: String::new(),
                    data: raw,
                },
                Condition::PathMissing(path),
            ],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};

    #[test]
    fn dead_entries_only() {
        let scratch = Scratch::new("app-paths");
        let root = scratch.key("App Paths");
        scratch.set(&root.child("dead.exe"), "", &gone("dead.exe"));
        scratch.set(
            &root.child("quoted.exe"),
            "",
            &format!("\"{}\"", gone("quoted.exe")),
        );
        scratch.set(&root.child("alive.exe"), "", &existing_file());
        scratch.set(&root.child("bare.exe"), "", "bare.exe");
        scratch.set(&root.child("nodefault.exe"), "Path", &gone(""));
        scratch.set(
            &root.child("network.exe"),
            "",
            r"\\server\share\network.exe",
        );
        scratch.set(
            &root.child("variable.exe"),
            "",
            r"%ZestNoSuchVariable%\variable.exe",
        );

        let mut out = Vec::new();
        scan_key(&root, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().map(|i| i.key.leaf()).collect();
        found.sort();
        assert_eq!(found, vec!["dead.exe", "quoted.exe"], "{out:#?}");
        assert!(out.iter().all(|i| i.value.is_none() && i.still_valid()));

        // The program comes back under a new location.
        scratch.set(&root.child("dead.exe"), "", &existing_file());
        assert!(
            !out.iter()
                .find(|i| i.key.leaf() == "dead.exe")
                .unwrap()
                .still_valid()
        );
    }
}
