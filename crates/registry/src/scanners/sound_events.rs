//! Sound events.
//!
//! `AppEvents\Schemes\Apps\<app>\<event>\.Current` holds the sound file
//! played for an event as its default value. When that file is confirmed
//! missing, the fix clears the assignment (deletes the default value) and
//! keeps the event itself. Bare file names, which the system resolves on its
//! own, are never judged.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::reg::RegPath;

const APPS_KEY: &str = r"AppEvents\Schemes\Apps";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    scan_root(&RegPath::hkcu(APPS_KEY), progress, out);
}

/// Scans an `AppEvents\Schemes\Apps`-shaped key.
pub(crate) fn scan_root(apps: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = apps.open() else {
        return;
    };
    for app in root.enum_keys().flatten() {
        let Ok(app_key) = root.open_subkey_with_flags(&app, KEY_READ) else {
            continue;
        };
        for event in app_key.enum_keys().flatten() {
            if progress.is_cancelled() {
                return;
            }
            let Ok(current) =
                app_key.open_subkey_with_flags(format!("{event}\\.Current"), KEY_READ)
            else {
                continue;
            };
            let Some(raw) = check::raw_string_value(&current, "") else {
                continue;
            };
            let file = raw.trim().trim_matches('"').trim();
            if file.is_empty() || !file.contains(['\\', '/']) {
                continue;
            }
            let Some(path) = check::missing_path(file) else {
                continue;
            };
            let current_path = apps.child(&app).child(&event).child(".Current");
            out.push(Issue::for_value(
                Category::SoundEvents,
                &current_path,
                "",
                "Sound event refers to a missing sound file",
                format!("{app} / {event} -> {}", path.display()),
                Confidence::Safe,
                vec![
                    Condition::ValueEquals {
                        key: current_path.clone(),
                        name: String::new(),
                        data: raw,
                    },
                    Condition::PathMissing(path),
                ],
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, gone};

    #[test]
    fn missing_sound_files() {
        let scratch = Scratch::new("sounds");
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("ding.wav");
        std::fs::write(&wav, b"RIFF").unwrap();

        let apps = scratch.key("Apps");
        let current = |app: &str, event: &str| apps.child(app).child(event).child(".Current");
        scratch.set(
            &current(".Default", "Notify"),
            "",
            &wav.display().to_string(),
        );
        scratch.set(&current(".Default", "Broken"), "", &gone("broken.wav"));
        scratch.set(&current("SomeApp", "Alert"), "", &gone("alert.wav"));
        scratch.set(&current("SomeApp", "Silent"), "", "");
        scratch.set(&current("SomeApp", "Bare"), "", "chimes.wav");
        // Only the active assignment matters.
        scratch.set(
            &apps.child("SomeApp").child("Alert").child(".Default"),
            "",
            &gone("default.wav"),
        );

        let mut out = Vec::new();
        scan_root(&apps, &Progress::new(), &mut out);
        let mut found: Vec<String> = out.iter().map(|i| i.detail.clone()).collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                format!(".Default / Broken -> {}", gone("broken.wav")),
                format!("SomeApp / Alert -> {}", gone("alert.wav")),
            ],
            "{out:#?}"
        );
        // The fix targets the default value of the `.Current` key.
        assert!(out.iter().all(|i| i.value.as_deref() == Some("")
            && i.key.leaf() == ".Current"
            && i.still_valid()));
    }
}
