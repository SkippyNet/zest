//! Fonts.
//!
//! Each value maps a font name to a file. Bare file names are resolved the
//! way the system resolves them (the Fonts folder first); absolute paths are
//! checked as they are. A font is reported only when the file is confirmed
//! missing from every place it could be loaded from.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use std::path::PathBuf;
use winreg::enums::RegType;
use zest_core::reg::RegPath;
use zest_core::{Progress, paths};

const SUB_KEY: &str = r"Microsoft\Windows NT\CurrentVersion\Fonts";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    let windows = paths::windows_dir();
    let dirs = [
        windows.join("Fonts"),
        windows.join("System32"),
        windows.clone(),
    ];
    for software in [RegPath::hklm("SOFTWARE"), RegPath::hkcu("Software")] {
        scan_key(&software.child(SUB_KEY), &dirs, progress, out);
    }
}

/// Scans one `Fonts`-shaped key. Bare file names are looked up in `font_dirs`.
pub(crate) fn scan_key(
    key: &RegPath,
    font_dirs: &[PathBuf],
    progress: &Progress,
    out: &mut Vec<Issue>,
) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (name, value) in handle.enum_values().flatten() {
        if progress.is_cancelled() {
            return;
        }
        if name.is_empty() || !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
            continue;
        }
        let Some(raw) = check::decode_string(&value) else {
            continue;
        };
        let file = raw.trim().trim_matches('"').trim();
        if file.is_empty() || file.contains(['*', '?', '<', '>', '|', '%']) {
            continue;
        }

        let missing: Vec<PathBuf> = if file.contains(['\\', '/']) {
            match check::missing_path(file) {
                Some(path) => vec![path],
                None => continue,
            }
        } else {
            let candidates: Vec<PathBuf> = font_dirs.iter().map(|dir| dir.join(file)).collect();
            if candidates.is_empty() || !candidates.iter().all(|c| check::definitely_absent(c)) {
                continue;
            }
            candidates
        };

        let detail = format!("{name} -> {}", missing[0].display());
        let mut conditions = vec![Condition::ValueEquals {
            key: key.clone(),
            name: name.clone(),
            data: raw,
        }];
        conditions.extend(missing.into_iter().map(Condition::PathMissing));
        out.push(Issue::for_value(
            Category::Fonts,
            key,
            &name,
            "Font file no longer exists",
            detail,
            Confidence::Safe,
            conditions,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, gone};

    #[test]
    fn bare_names_and_full_paths() {
        let scratch = Scratch::new("fonts");
        let tmp = tempfile::tempdir().unwrap();
        let fonts_dir = tmp.path().join("Fonts");
        let fallback_dir = tmp.path().join("System");
        std::fs::create_dir_all(&fonts_dir).unwrap();
        std::fs::create_dir_all(&fallback_dir).unwrap();
        std::fs::write(fonts_dir.join("present.ttf"), b"font").unwrap();
        std::fs::write(fallback_dir.join("fallback.fon"), b"font").unwrap();
        let installed = tmp.path().join("user-installed.otf");
        std::fs::write(&installed, b"font").unwrap();

        let key = scratch.key("Fonts");
        scratch.set(&key, "Present (TrueType)", "present.ttf");
        scratch.set(&key, "Fallback", "fallback.fon");
        scratch.set(&key, "Missing (TrueType)", "missing.ttf");
        scratch.set(
            &key,
            "User Font (OpenType)",
            &installed.display().to_string(),
        );
        scratch.set(&key, "Gone Font (OpenType)", &gone("gone.otf"));
        scratch.set(&key, "Blank", "");
        scratch.set_dword(&key, "NotAString", 7);

        let dirs = [fonts_dir.clone(), fallback_dir];
        let mut out = Vec::new();
        scan_key(&key, &dirs, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().filter_map(|i| i.value.as_deref()).collect();
        found.sort();
        assert_eq!(
            found,
            vec!["Gone Font (OpenType)", "Missing (TrueType)"],
            "{out:#?}"
        );
        assert!(out.iter().all(Issue::still_valid));

        // Restoring the file revives the font.
        let missing = out
            .iter()
            .find(|i| i.value.as_deref() == Some("Missing (TrueType)"))
            .unwrap();
        std::fs::write(fonts_dir.join("missing.ttf"), b"font").unwrap();
        assert!(!missing.still_valid());
    }
}
