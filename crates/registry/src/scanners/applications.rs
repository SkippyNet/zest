//! Applications ("Open with" registrations).
//!
//! `Applications\<program>\shell\<verb>\command` tells Windows how to open a
//! file with a program. A registration is reported only when it defines at
//! least one command and **every** verb has a command whose program is
//! confirmed missing. Verbs without a command string (drop targets,
//! delegate handlers) make the registration unjudgeable and it is skipped,
//! as are registrations for programs in Windows-owned locations.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use std::path::PathBuf;
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::reg::RegPath;

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for applications in super::distinct_children(super::class_roots(), "Applications") {
        scan_root(&applications, progress, out);
    }
}

/// Scans the program keys directly below an `Applications`-shaped key.
pub(crate) fn scan_root(applications: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = applications.open() else {
        return;
    };
    'apps: for name in root.enum_keys().flatten() {
        if progress.is_cancelled() {
            return;
        }
        let Ok(shell) = root.open_subkey_with_flags(format!("{name}\\shell"), KEY_READ) else {
            continue;
        };
        let app_path = applications.child(&name);
        let mut conditions = Vec::new();
        let mut first_missing: Option<PathBuf> = None;

        for verb in shell.enum_keys().flatten() {
            let Ok(command_key) =
                shell.open_subkey_with_flags(format!("{verb}\\command"), KEY_READ)
            else {
                continue 'apps;
            };
            let Some(raw) = check::raw_string_value(&command_key, "") else {
                continue 'apps;
            };
            // Programs that ship with Windows may be optional features.
            let Some(path) = check::outside_windows(check::missing_command(raw.trim())) else {
                continue 'apps;
            };
            conditions.push(Condition::ValueEquals {
                key: app_path.child("shell").child(&verb).child("command"),
                name: String::new(),
                data: raw,
            });
            conditions.push(Condition::PathMissing(path.clone()));
            first_missing.get_or_insert(path);
        }

        let Some(missing) = first_missing else {
            continue;
        };
        out.push(Issue::for_key(
            Category::Applications,
            &app_path,
            "\"Open with\" entry for a program that no longer exists",
            format!("{name} -> {}", missing.display()),
            Confidence::Safe,
            conditions,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};

    #[test]
    fn every_verb_must_be_dead() {
        let scratch = Scratch::new("applications");
        let root = scratch.key("Applications");
        let command =
            |app: &str, verb: &str| root.child(app).child("shell").child(verb).child("command");

        scratch.set(
            &command("dead.exe", "open"),
            "",
            &format!("\"{}\" \"%1\"", gone("dead.exe")),
        );
        scratch.set(
            &command("dead.exe", "edit"),
            "",
            &format!("\"{}\" /edit \"%1\"", gone("dead.exe")),
        );

        scratch.set(
            &command("alive.exe", "open"),
            "",
            &format!("\"{}\" \"%1\"", existing_file()),
        );

        scratch.set(
            &command("mixed.exe", "open"),
            "",
            &format!("\"{}\" \"%1\"", gone("mixed.exe")),
        );
        scratch.set(
            &command("mixed.exe", "print"),
            "",
            &format!("\"{}\" /p \"%1\"", existing_file()),
        );

        // Script hosts hide their real target.
        scratch.set(
            &command("script.exe", "open"),
            "",
            &format!("cmd.exe /c \"{}\" \"%1\"", gone("script.bat")),
        );

        // A verb without a command string.
        scratch.set(
            &command("droptarget.exe", "open"),
            "",
            &format!("\"{}\" \"%1\"", gone("droptarget.exe")),
        );
        scratch.key(r"Applications\droptarget.exe\shell\play\DropTarget");

        // No shell key at all.
        scratch.set(
            &root.child("metadata.exe"),
            "FriendlyAppName",
            "Metadata only",
        );

        // The placeholder alone is not a program.
        scratch.set(&command("placeholder.exe", "open"), "", "\"%1\" %*");

        let mut out = Vec::new();
        scan_root(&root, &Progress::new(), &mut out);
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].key.leaf(), "dead.exe");
        assert_eq!(out[0].conditions.len(), 4);
        assert!(out[0].still_valid());
    }
}
