//! Missing shared DLLs.
//!
//! `SharedDLLs` holds one value per shared file: the value *name* is the
//! file path, the data is a reference count used by legacy installers. A
//! counter for a file that no longer exists is dead weight. Counters for
//! files in Windows-owned locations are never reported.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use zest_core::Progress;
use zest_core::reg::RegPath;

const SUB_KEY: &str = r"Microsoft\Windows\CurrentVersion\SharedDLLs";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    let machine = [
        RegPath::hklm("SOFTWARE"),
        RegPath::hklm(r"SOFTWARE\WOW6432Node"),
    ];
    for key in super::distinct_children(machine, SUB_KEY) {
        scan_key(&key, progress, out);
    }
}

/// Scans one `SharedDLLs`-shaped key.
pub(crate) fn scan_key(key: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (name, _) in handle.enum_values().flatten() {
        if progress.is_cancelled() {
            return;
        }
        if name.trim().is_empty() {
            continue;
        }
        // Windows ships counters for optional components (for example old
        // framework versions); those are not leftovers of removed software.
        if let Some(path) = check::outside_windows(check::missing_path(&name)) {
            out.push(Issue::for_value(
                Category::SharedDlls,
                key,
                &name,
                "Shared file no longer exists",
                path.display().to_string(),
                Confidence::Safe,
                vec![Condition::PathMissing(path)],
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};

    #[test]
    fn reports_only_confirmed_missing_files() {
        let scratch = Scratch::new("shared-dlls");
        let key = scratch.key("SharedDLLs");
        scratch.set_dword(&key, &gone("old.dll"), 1);
        scratch.set_dword(&key, &existing_file(), 3);
        scratch.set_dword(&key, "bare-name.dll", 1);
        scratch.set_dword(&key, r"\\server\share\remote.dll", 1);
        scratch.set_dword(&key, r"B:\floppy.dll", 1);
        // A counter Windows ships for an optional component.
        let optional = zest_core::paths::windows_dir()
            .join(r"Microsoft.NET\Framework\v0.0.1\zest-test.tlb")
            .display()
            .to_string();
        scratch.set_dword(&key, &optional, 1);

        let mut out = Vec::new();
        scan_key(&key, &Progress::new(), &mut out);
        assert_eq!(out.len(), 1, "{out:#?}");
        let issue = &out[0];
        assert_eq!(issue.category, Category::SharedDlls);
        assert_eq!(issue.value.as_deref(), Some(gone("old.dll").as_str()));
        assert_eq!(issue.detail, gone("old.dll"));
        assert_eq!(issue.confidence, Confidence::Safe);
        assert!(issue.still_valid());
    }

    #[test]
    fn missing_key_is_not_an_error() {
        let scratch = Scratch::new("shared-dlls-none");
        let mut out = Vec::new();
        scan_key(
            &scratch.path.child("DoesNotExist"),
            &Progress::new(),
            &mut out,
        );
        assert!(out.is_empty());
    }
}
