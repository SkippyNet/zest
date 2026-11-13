//! MUI cache.
//!
//! The shell caches the display name and publisher of every program it has
//! shown. Value names are `<program path>.FriendlyAppName` and
//! `<program path>.ApplicationCompany`. Entries for programs that are
//! confirmed missing are reported; anything else in the key is ignored.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use zest_core::Progress;
use zest_core::reg::RegPath;

const MUI_CACHE_KEY: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache";

const SUFFIXES: [&str; 2] = [".friendlyappname", ".applicationcompany"];

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    scan_key(&RegPath::hkcu(MUI_CACHE_KEY), progress, out);
}

/// Returns the program path of a cache value name, if it has the known shape.
fn program_of(value_name: &str) -> Option<&str> {
    let lower = value_name.to_ascii_lowercase();
    SUFFIXES
        .iter()
        .find(|suffix| lower.ends_with(*suffix))
        .map(|suffix| &value_name[..value_name.len() - suffix.len()])
        .filter(|program| !program.trim().is_empty())
}

/// Scans one `MuiCache`-shaped key.
pub(crate) fn scan_key(key: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (index, (name, _)) in handle.enum_values().flatten().enumerate() {
        if index % 64 == 0 && progress.is_cancelled() {
            return;
        }
        let Some(program) = program_of(&name) else {
            continue;
        };
        if let Some(path) = check::missing_path(program) {
            out.push(Issue::for_value(
                Category::MuiCache,
                key,
                &name,
                "Cached name of a program that no longer exists",
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
    fn value_name_shapes() {
        assert_eq!(
            program_of(r"C:\a\b.exe.FriendlyAppName"),
            Some(r"C:\a\b.exe")
        );
        assert_eq!(
            program_of(r"C:\a\b.exe.ApplicationCompany"),
            Some(r"C:\a\b.exe")
        );
        assert_eq!(
            program_of(r"C:\a\b.exe.friendlyappname"),
            Some(r"C:\a\b.exe")
        );
        assert_eq!(program_of("LangID"), None);
        assert_eq!(program_of(".FriendlyAppName"), None);
        assert_eq!(program_of(r"C:\a\b.exe"), None);
    }

    #[test]
    fn only_entries_for_missing_programs() {
        let scratch = Scratch::new("mui");
        let key = scratch.key("MuiCache");
        let dead = gone("dead.exe");
        scratch.set(&key, &format!("{dead}.FriendlyAppName"), "Dead App");
        scratch.set(&key, &format!("{dead}.ApplicationCompany"), "Dead Corp");
        scratch.set(
            &key,
            &format!("{}.FriendlyAppName", existing_file()),
            "Shell",
        );
        scratch.set(
            &key,
            r"C:\Program Files\WindowsApps\Zest.NoSuch_1.0\app.exe.FriendlyAppName",
            "Packaged",
        );
        scratch.set_binary(&key, "LangID", &[9, 4]);

        let mut out = Vec::new();
        scan_key(&key, &Progress::new(), &mut out);
        assert_eq!(out.len(), 2, "{out:#?}");
        assert!(out.iter().all(|i| i.detail == dead && i.still_valid()));
    }
}
