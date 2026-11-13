//! Help files.
//!
//! The `Help` and `HTML Help` keys map a help file name (value name) to the
//! folder it lives in (value data). An entry whose file is confirmed missing
//! from that folder is reported, unless the folder is a Windows-owned
//! location.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use std::path::PathBuf;
use winreg::enums::RegType;
use zest_core::Progress;
use zest_core::cmdline;
use zest_core::reg::RegPath;

const SUB_KEYS: [&str; 2] = [r"Microsoft\Windows\Help", r"Microsoft\Windows\HTML Help"];

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for sub_key in SUB_KEYS {
        let machine = [
            RegPath::hklm("SOFTWARE"),
            RegPath::hklm(r"SOFTWARE\WOW6432Node"),
        ];
        for key in super::distinct_children(machine, sub_key) {
            scan_key(&key, progress, out);
        }
    }
}

/// Scans one `Help`-shaped key.
pub(crate) fn scan_key(key: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (name, value) in handle.enum_values().flatten() {
        if progress.is_cancelled() {
            return;
        }
        let file_name = name.trim();
        if file_name.is_empty() || !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ)
        {
            continue;
        }
        let Some(raw) = check::decode_string(&value) else {
            continue;
        };
        let folder = cmdline::normalize_path_text(raw.trim().trim_matches('"'));
        if folder.trim().is_empty() {
            continue;
        }
        // An absolute value name replaces the folder, as the system does.
        let full = PathBuf::from(folder.trim()).join(file_name);
        // Help files of optional Windows components are not leftovers.
        let Some(path) = check::outside_windows(check::missing_path(&full.display().to_string()))
        else {
            continue;
        };
        out.push(Issue::for_value(
            Category::HelpFiles,
            key,
            &name,
            "Help file no longer exists",
            path.display().to_string(),
            Confidence::Safe,
            vec![
                Condition::ValueEquals {
                    key: key.clone(),
                    name: name.clone(),
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
    use crate::testutil::{Scratch, gone_dir};

    #[test]
    fn file_is_looked_up_in_its_folder() {
        let scratch = Scratch::new("help");
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("present.chm"), b"help").unwrap();
        let folder = tmp.path().display().to_string();

        let key = scratch.key("HTML Help");
        scratch.set(&key, "present.chm", &folder);
        scratch.set(&key, "missing.chm", &folder);
        scratch.set(&key, "gone.chm", &gone_dir());
        scratch.set(&key, "nofolder.chm", "");
        scratch.set(&key, "remote.chm", r"\\server\share\help");
        scratch.set_dword(&key, "number.chm", 1);

        let mut out = Vec::new();
        scan_key(&key, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().filter_map(|i| i.value.as_deref()).collect();
        found.sort();
        assert_eq!(found, vec!["gone.chm", "missing.chm"], "{out:#?}");
        assert!(out.iter().all(Issue::still_valid));

        std::fs::write(tmp.path().join("missing.chm"), b"help").unwrap();
        let missing = out
            .iter()
            .find(|i| i.value.as_deref() == Some("missing.chm"))
            .unwrap();
        assert!(!missing.still_valid());
    }
}
