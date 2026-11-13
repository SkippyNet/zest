//! Run at startup.
//!
//! * `Run` / `RunOnce` values whose program is confirmed missing.
//! * Leftover on/off switches under `Explorer\StartupApproved`: a switch is a
//!   leftover when no startup value of that name exists in *any* startup key
//!   (for `Run` / `Run32`), or no file of that name exists in *any* startup
//!   folder (for `StartupFolder`).

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use std::path::PathBuf;
use winreg::enums::RegType;
use zest_core::reg::{RegPath, RegRoot};
use zest_core::{Progress, paths};

const CURRENT_VERSION: &str = r"Microsoft\Windows\CurrentVersion";

/// Every key a startup switch could belong to.
fn all_startup_keys() -> Vec<RegPath> {
    let mut keys = Vec::new();
    for software in super::software_roots() {
        for name in ["Run", "RunOnce", r"Policies\Explorer\Run"] {
            keys.push(software.child(&format!(r"{CURRENT_VERSION}\{name}")));
        }
    }
    keys
}

/// The per-user and all-users startup folders (configured and default).
fn startup_folders() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |dir: PathBuf| {
        if dir.is_absolute() && !dirs.iter().any(|d| paths::same_path(d, &dir)) {
            dirs.push(dir);
        }
    };
    let shell_folders = format!(r"Software\{CURRENT_VERSION}\Explorer\User Shell Folders");
    if let Some(dir) = RegPath::hkcu(shell_folders.clone()).read_string("Startup") {
        add(PathBuf::from(paths::expand_env(dir.trim())));
    }
    if let Some(dir) = RegPath::hklm(shell_folders).read_string("Common Startup") {
        add(PathBuf::from(paths::expand_env(dir.trim())));
    }
    let relative = r"Microsoft\Windows\Start Menu\Programs\Startup";
    add(paths::UserDirs::current().roaming.join(relative));
    add(paths::program_data().join(relative));
    dirs
}

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for name in ["Run", "RunOnce"] {
        let sub_key = format!(r"{CURRENT_VERSION}\{name}");
        for run_key in super::distinct_children(super::software_roots(), &sub_key) {
            scan_run_key(&run_key, progress, out);
        }
    }

    let startup_keys = all_startup_keys();
    let folders = startup_folders();
    for root in [RegRoot::Hklm, RegRoot::Hkcu] {
        let approved = RegPath::new(
            root,
            format!(r"Software\{CURRENT_VERSION}\Explorer\StartupApproved"),
        );
        for name in ["Run", "Run32"] {
            scan_approved_run(&approved.child(name), &startup_keys, progress, out);
        }
        scan_approved_folder(&approved.child("StartupFolder"), &folders, progress, out);
    }
}

/// Scans one `Run`-shaped key for commands whose program is missing.
pub(crate) fn scan_run_key(key: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (name, value) in handle.enum_values().flatten() {
        if progress.is_cancelled() {
            return;
        }
        if !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
            continue;
        }
        let Some(raw) = check::decode_string(&value) else {
            continue;
        };
        let command = raw.trim();
        if command.is_empty() {
            continue;
        }
        let Some(path) = check::missing_command(command) else {
            continue;
        };
        let shown = if name.is_empty() { "(Default)" } else { &name };
        out.push(Issue::for_value(
            Category::Startup,
            key,
            &name,
            "Startup entry points to a missing program",
            format!("{shown} -> {}", path.display()),
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

/// Scans a `StartupApproved\Run`-shaped key for switches whose startup
/// value does not exist in any of `startup_keys`.
pub(crate) fn scan_approved_run(
    approved: &RegPath,
    startup_keys: &[RegPath],
    progress: &Progress,
    out: &mut Vec<Issue>,
) {
    let Ok(handle) = approved.open() else {
        return;
    };
    if startup_keys.is_empty() {
        return;
    }
    for (name, _) in handle.enum_values().flatten() {
        if progress.is_cancelled() {
            return;
        }
        if name.is_empty() {
            continue;
        }
        if !startup_keys
            .iter()
            .all(|key| check::value_definitely_absent(key, &name))
        {
            continue;
        }
        out.push(Issue::for_value(
            Category::Startup,
            approved,
            &name,
            "Leftover startup switch for an entry that no longer exists",
            name.clone(),
            Confidence::Safe,
            startup_keys
                .iter()
                .map(|key| Condition::ValueMissing {
                    key: key.clone(),
                    name: name.clone(),
                })
                .collect(),
        ));
    }
}

/// Scans a `StartupApproved\StartupFolder`-shaped key for switches whose
/// shortcut does not exist in any of `folders`.
pub(crate) fn scan_approved_folder(
    approved: &RegPath,
    folders: &[PathBuf],
    progress: &Progress,
    out: &mut Vec<Issue>,
) {
    let Ok(handle) = approved.open() else {
        return;
    };
    if folders.is_empty() {
        return;
    }
    for (name, _) in handle.enum_values().flatten() {
        if progress.is_cancelled() {
            return;
        }
        let file = name.trim();
        if file.is_empty() || file.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
            continue;
        }
        let candidates: Vec<PathBuf> = folders.iter().map(|dir| dir.join(file)).collect();
        if !candidates.iter().all(|c| check::definitely_absent(c)) {
            continue;
        }
        out.push(Issue::for_value(
            Category::Startup,
            approved,
            &name,
            "Leftover startup switch for a shortcut that no longer exists",
            name.clone(),
            Confidence::Safe,
            candidates.into_iter().map(Condition::PathMissing).collect(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};

    #[test]
    fn run_values_with_missing_programs() {
        let scratch = Scratch::new("run");
        let run = scratch.key("Run");
        scratch.set(
            &run,
            "Dead",
            &format!("\"{}\" --minimized", gone("dead.exe")),
        );
        scratch.set(
            &run,
            "DeadUnquoted",
            &format!("{} /background", gone("tray.exe")),
        );
        scratch.set(&run, "Alive", &format!("\"{}\"", existing_file()));
        scratch.set(
            &run,
            "Script",
            &format!("cmd.exe /c start {}", gone("x.bat")),
        );
        scratch.set(&run, "Bare", "sometool.exe --flag");
        scratch.set(&run, "Empty", "");
        scratch.set(
            &run,
            "DeadDll",
            &format!("rundll32.exe \"{}\",Start", gone("helper.dll")),
        );
        scratch.set_dword(&run, "Number", 1);

        let mut out = Vec::new();
        scan_run_key(&run, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().filter_map(|i| i.value.as_deref()).collect();
        found.sort();
        assert_eq!(found, vec!["Dead", "DeadDll", "DeadUnquoted"], "{out:#?}");
        assert!(out.iter().all(Issue::still_valid));

        // The program updates itself and rewrites its startup command.
        scratch.set(&run, "Dead", &format!("\"{}\"", existing_file()));
        assert!(
            !out.iter()
                .find(|i| i.value.as_deref() == Some("Dead"))
                .unwrap()
                .still_valid()
        );
    }

    #[test]
    fn approved_switches_without_a_startup_value() {
        let scratch = Scratch::new("approved-run");
        let run_user = scratch.key("RunUser");
        let run_machine = scratch.key("RunMachine");
        let approved = scratch.key("Approved");
        let enabled: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

        scratch.set(&run_user, "InUser", "x");
        scratch.set(&run_machine, "InMachine", "x");
        for name in ["InUser", "InMachine", "Orphan"] {
            scratch.set_binary(&approved, name, &enabled);
        }

        let keys = [
            run_user.clone(),
            run_machine,
            scratch.path.child("NoSuchRunKey"),
        ];
        let mut out = Vec::new();
        scan_approved_run(&approved, &keys, &Progress::new(), &mut out);
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].value.as_deref(), Some("Orphan"));
        assert_eq!(out[0].conditions.len(), 3);
        assert!(out[0].still_valid());

        // The entry is registered again before the fix runs.
        scratch.set(&run_user, "Orphan", "x");
        assert!(!out[0].still_valid());

        // Without any startup key to compare against nothing can be judged.
        let mut none = Vec::new();
        scan_approved_run(&approved, &[], &Progress::new(), &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn approved_switches_without_a_shortcut() {
        let scratch = Scratch::new("approved-folder");
        let tmp = tempfile::tempdir().unwrap();
        let user_dir = tmp.path().join("user");
        let common_dir = tmp.path().join("common");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::create_dir_all(&common_dir).unwrap();
        std::fs::write(user_dir.join("Mine.lnk"), b"lnk").unwrap();
        std::fs::write(common_dir.join("Everyone.lnk"), b"lnk").unwrap();

        let approved = scratch.key("StartupFolder");
        for name in ["Mine.lnk", "Everyone.lnk", "Gone.lnk"] {
            scratch.set_binary(&approved, name, &[2, 0, 0, 0]);
        }

        let folders = [user_dir.clone(), common_dir];
        let mut out = Vec::new();
        scan_approved_folder(&approved, &folders, &Progress::new(), &mut out);
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].value.as_deref(), Some("Gone.lnk"));
        assert!(out[0].still_valid());

        std::fs::write(user_dir.join("Gone.lnk"), b"lnk").unwrap();
        assert!(!out[0].still_valid());
    }

    #[test]
    fn real_locations_are_well_formed() {
        assert_eq!(all_startup_keys().len(), 12);
        let folders = startup_folders();
        assert!(!folders.is_empty());
        assert!(folders.iter().all(|f| f.is_absolute()));
    }
}
