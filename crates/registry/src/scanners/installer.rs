//! Installer references.
//!
//! `Installer\Folders` lists folders created by installer packages so they
//! can be removed again later. The value *name* is the folder path. Entries
//! for folders that are already gone serve no purpose.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use zest_core::Progress;
use zest_core::reg::RegPath;

const FOLDERS_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\Folders";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    scan_key(&RegPath::hklm(FOLDERS_KEY), progress, out);
}

/// Scans one `Installer\Folders`-shaped key.
pub(crate) fn scan_key(key: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (index, (name, _)) in handle.enum_values().flatten().enumerate() {
        if index % 64 == 0 && progress.is_cancelled() {
            return;
        }
        if name.trim().is_empty() {
            continue;
        }
        if let Some(path) = check::missing_path(&name) {
            out.push(Issue::for_value(
                Category::Installer,
                key,
                &name,
                "Installer reference to a folder that no longer exists",
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
    use crate::testutil::{Scratch, gone_dir};

    #[test]
    fn folders_that_are_gone() {
        let scratch = Scratch::new("installer");
        let tmp = tempfile::tempdir().unwrap();
        let key = scratch.key("Folders");
        scratch.set(&key, &format!("{}\\", tmp.path().display()), "");
        scratch.set(&key, &format!("{}\\Old App\\", gone_dir()), "");
        scratch.set(&key, &format!("{}\\Old App\\Data\\", gone_dir()), "1");
        scratch.set(&key, r"\\server\share\Folder\", "");

        let mut out = Vec::new();
        scan_key(&key, &Progress::new(), &mut out);
        assert_eq!(out.len(), 2, "{out:#?}");
        assert!(out.iter().all(|i| i.value.is_some() && i.still_valid()));

        // The folder is created again before the fix runs.
        let revived = tmp.path().join("Revived");
        let key2 = scratch.key("Folders2");
        scratch.set(&key2, &format!("{}\\", revived.display()), "");
        let mut out2 = Vec::new();
        scan_key(&key2, &Progress::new(), &mut out2);
        assert_eq!(out2.len(), 1);
        std::fs::create_dir_all(&revived).unwrap();
        assert!(!out2[0].still_valid());
    }
}
