//! Applying fixes: re-validate, back up, then delete.

use crate::types::Issue;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zest_core::reg::{self, RegBackup};
use zest_core::{Error, Progress, Result, elevation};

/// Outcome of a [`fix`] run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixReport {
    /// Number of entries removed.
    pub fixed: usize,
    /// Ids of the removed entries, so callers can drop them from their list.
    pub fixed_ids: Vec<String>,
    /// Entries left alone because removing them needs administrator rights.
    pub skipped_needs_admin: usize,
    /// Entries left alone because the PC changed since the scan: the entry
    /// is already gone, its data was rewritten, or the missing file is back.
    pub skipped_changed: usize,
    /// Entries that could not be backed up or removed: `(issue id, error)`.
    pub failed: Vec<(String, String)>,
    /// The backup written before the first deletion, if one was requested
    /// and there was anything to back up.
    pub backup_file: Option<PathBuf>,
    /// True when the run was cancelled before every entry was processed.
    pub cancelled: bool,
}

impl FixReport {
    /// Total number of entries that were not removed.
    pub fn not_fixed(&self) -> usize {
        self.skipped_needs_admin + self.skipped_changed + self.failed.len()
    }
}

/// Removes the given issues from the registry.
///
/// The run has three strict phases:
///
/// 1. **Re-validate.** Every issue is checked again with
///    [`Issue::still_valid`]; entries that changed since the scan are
///    skipped. Machine-wide entries are skipped when the process is not
///    elevated.
/// 2. **Back up.** When `backup` is given, every entry about to be removed
///    is exported into it and the backup is **saved to disk before anything
///    is deleted**. If the backup cannot be written the function returns an
///    error and the registry is untouched.
/// 3. **Delete**, through [`zest_core::reg::delete_key`] /
///    [`zest_core::reg::delete_value`], so the protected-key guard
///    applies to every removal.
///
/// Individual failures never abort the run; they are listed in the report.
/// `progress` counts processed issues and can cancel the run between two
/// deletions (the backup may then contain entries that were not removed,
/// which is harmless: restoring it rewrites identical data).
///
/// To undo a run, pass [`FixReport::backup_file`] to
/// [`zest_core::reg::import_reg_file`].
pub fn fix(
    issues: &[Issue],
    mut backup: Option<&mut RegBackup>,
    progress: &Progress,
) -> Result<FixReport> {
    let mut report = FixReport::default();
    let elevated = elevation::is_elevated();

    progress.set_done(0);
    progress.set_total(issues.len() as u64);
    progress.set_message("Checking registry entries");

    // Phase 1 + 2a: decide what will be removed and capture it.
    let mut planned: Vec<&Issue> = Vec::with_capacity(issues.len());
    for issue in issues {
        if progress.is_cancelled() {
            report.cancelled = true;
            return Ok(report);
        }
        if issue.requires_admin && !elevated {
            report.skipped_needs_admin += 1;
            progress.inc(1);
            continue;
        }
        if issue.value.is_none() && reg::is_protected_key(&issue.key) {
            report.failed.push((
                issue.id.clone(),
                format!("{} is a protected key", issue.key),
            ));
            progress.inc(1);
            continue;
        }
        if !issue.still_valid() {
            report.skipped_changed += 1;
            progress.inc(1);
            continue;
        }
        if let Some(backup) = backup.as_deref_mut() {
            let captured = match &issue.value {
                None => backup.add_key(&issue.key),
                Some(name) => backup.add_value(&issue.key, name),
            };
            if let Err(e) = captured {
                report
                    .failed
                    .push((issue.id.clone(), format!("could not back up: {e}")));
                progress.inc(1);
                continue;
            }
        }
        planned.push(issue);
    }

    // Phase 2b: the backup must be on disk before the first deletion.
    if let Some(backup) = backup.as_deref()
        && !planned.is_empty()
    {
        progress.set_message("Saving registry backup");
        report.backup_file = backup.save()?;
    }

    // Phase 3: delete.
    progress.set_message("Removing invalid registry entries");
    for issue in planned {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let removed = match &issue.value {
            None => reg::delete_key(&issue.key, None),
            Some(name) => reg::delete_value(&issue.key, name, None),
        };
        match removed {
            Ok(()) => {
                report.fixed += 1;
                report.fixed_ids.push(issue.id.clone());
            }
            Err(Error::NeedsElevation(_)) => report.skipped_needs_admin += 1,
            Err(e) => {
                log::warn!("registry fix failed for {}: {e}", issue.location());
                report.failed.push((issue.id.clone(), e.to_string()));
            }
        }
        progress.inc(1);
    }

    log::info!(
        "registry fix: {} fixed, {} need admin, {} changed, {} failed",
        report.fixed,
        report.skipped_needs_admin,
        report.skipped_changed,
        report.failed.len()
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanners::{app_paths, obsolete_software, startup};
    use crate::testutil::{Scratch, existing_file, gone};
    use crate::types::{Category, Condition, Confidence};
    use zest_core::reg::{RegPath, import_reg_file};

    /// Builds a scratch tree with one key issue and two value issues.
    fn scenario(tag: &str) -> (Scratch, Vec<Issue>) {
        let scratch = Scratch::new(tag);
        let app_paths_key = scratch.key("App Paths");
        scratch.set(&app_paths_key.child("dead.exe"), "", &gone("dead.exe"));
        scratch.set(&app_paths_key.child("dead.exe"), "Path", &gone(""));
        scratch.set(
            &app_paths_key.child("dead.exe").child("Sub"),
            "Deep",
            "kept in backup",
        );
        scratch.set(&app_paths_key.child("alive.exe"), "", &existing_file());

        let run = scratch.key("Run");
        scratch.set(&run, "DeadOne", &format!("\"{}\"", gone("one.exe")));
        scratch.set(&run, "DeadTwo", &format!("\"{}\" /tray", gone("two.exe")));
        scratch.set(&run, "Alive", &format!("\"{}\"", existing_file()));

        let progress = Progress::new();
        let mut issues = Vec::new();
        app_paths::scan_key(&app_paths_key, &progress, &mut issues);
        startup::scan_run_key(&run, &progress, &mut issues);
        assert_eq!(issues.len(), 3, "{issues:#?}");
        (scratch, issues)
    }

    #[test]
    fn fixes_with_backup_and_restores() {
        let (scratch, issues) = scenario("fix-roundtrip");
        let tmp = tempfile::tempdir().unwrap();
        let mut backup = RegBackup::at(tmp.path().join("fix.reg"));
        let progress = Progress::new();

        let report = fix(&issues, Some(&mut backup), &progress).unwrap();
        assert_eq!(report.fixed, 3, "{report:#?}");
        assert_eq!(report.fixed_ids.len(), 3);
        assert_eq!(report.not_fixed(), 0);
        assert!(!report.cancelled);
        assert_eq!(progress.done(), 3);
        let saved = report.backup_file.clone().expect("backup file");
        assert!(saved.is_file());
        assert_eq!(backup.len(), 3);

        let app_paths_key = scratch.path.child("App Paths");
        let run = scratch.path.child("Run");
        assert!(!app_paths_key.child("dead.exe").exists());
        assert!(app_paths_key.child("alive.exe").exists());
        assert_eq!(run.read_string("DeadOne"), None);
        assert_eq!(run.read_string("DeadTwo"), None);
        assert!(run.read_string("Alive").is_some());

        // Fixing the same issues again changes nothing.
        let again = fix(&issues, None, &Progress::new()).unwrap();
        assert_eq!((again.fixed, again.skipped_changed), (0, 3));

        // The backup restores exactly what was removed.
        import_reg_file(&saved).unwrap();
        assert_eq!(
            app_paths_key.child("dead.exe").read_string("").as_deref(),
            Some(gone("dead.exe").as_str())
        );
        assert_eq!(
            app_paths_key
                .child("dead.exe")
                .child("Sub")
                .read_string("Deep")
                .as_deref(),
            Some("kept in backup")
        );
        assert_eq!(
            run.read_string("DeadTwo").as_deref(),
            Some(format!("\"{}\" /tray", gone("two.exe")).as_str())
        );
        // After the restore the issues are valid once more.
        assert!(issues.iter().all(Issue::still_valid));
    }

    #[test]
    fn entries_that_changed_since_the_scan_are_skipped() {
        let (scratch, issues) = scenario("fix-changed");
        let run = scratch.path.child("Run");
        // One program is reinstalled and rewrites its startup command...
        scratch.set(&run, "DeadOne", &format!("\"{}\"", existing_file()));
        // ...and one entry is removed by something else.
        run.open_write().unwrap().delete_value("DeadTwo").unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let mut backup = RegBackup::at(tmp.path().join("fix.reg"));
        let report = fix(&issues, Some(&mut backup), &Progress::new()).unwrap();
        assert_eq!(report.fixed, 1, "{report:#?}");
        assert_eq!(report.skipped_changed, 2);
        assert_eq!(backup.len(), 1);
        // The rewritten value survived.
        assert_eq!(
            run.read_string("DeadOne").as_deref(),
            Some(format!("\"{}\"", existing_file()).as_str())
        );
    }

    #[test]
    fn nothing_is_deleted_when_the_backup_cannot_be_saved() {
        let (scratch, issues) = scenario("fix-nobackup");
        let tmp = tempfile::tempdir().unwrap();
        // A file where a folder is needed makes the save fail.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let mut backup = RegBackup::at(blocker.join("sub").join("fix.reg"));

        let result = fix(&issues, Some(&mut backup), &Progress::new());
        assert!(result.is_err());
        assert!(scratch.path.child("App Paths").child("dead.exe").exists());
        assert!(scratch.path.child("Run").read_string("DeadOne").is_some());
        assert!(issues.iter().all(Issue::still_valid));
    }

    #[test]
    fn cancellation_before_start_changes_nothing() {
        let (_scratch, issues) = scenario("fix-cancel");
        let progress = Progress::new();
        progress.cancel();
        let report = fix(&issues, None, &progress).unwrap();
        assert!(report.cancelled);
        assert_eq!(report.fixed, 0);
        assert!(issues.iter().all(Issue::still_valid));
    }

    #[test]
    fn machine_wide_issues_need_elevation() {
        // A fabricated machine-wide issue for a key that does not exist:
        // unelevated it is counted as "needs admin" without being touched;
        // elevated it is "changed" because the key is not there. Either way
        // nothing is modified.
        let issue = Issue::for_key(
            Category::AppPaths,
            &RegPath::hklm(r"SOFTWARE\ZestTests\NoSuchKey\Nothing"),
            "t",
            "d",
            Confidence::Safe,
            vec![],
        );
        assert!(issue.requires_admin);
        let report = fix(&[issue], None, &Progress::new()).unwrap();
        assert_eq!(report.fixed, 0);
        if elevation::is_elevated() {
            assert_eq!(report.skipped_changed, 1);
        } else {
            assert_eq!(report.skipped_needs_admin, 1);
        }
    }

    #[test]
    fn protected_keys_are_refused() {
        // First-level keys are structural by shape. This one does not exist,
        // so the test cannot touch real data even if the guard regressed.
        let structural = RegPath::hkcu("ZestTestsNoSuchTopLevelKey");
        assert!(!structural.exists());
        assert!(reg::is_protected_key(&structural));
        let issue = Issue::for_key(
            Category::ObsoleteSoftware,
            &structural,
            "t",
            "d",
            Confidence::Safe,
            vec![],
        );
        let report = fix(&[issue], None, &Progress::new()).unwrap();
        assert_eq!(report.fixed, 0);
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].1.contains("protected"),
            "{:?}",
            report.failed
        );
    }

    #[test]
    fn empty_vendor_key_is_removed_only_while_empty() {
        let scratch = Scratch::new("fix-vendor");
        let software = scratch.key("Software");
        scratch.key(r"Software\EmptyOne");
        scratch.key(r"Software\EmptyTwo");
        let mut issues = Vec::new();
        obsolete_software::scan_empty_vendor_keys(&software, &Progress::new(), &mut issues);
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().all(|i| matches!(
            i.conditions.as_slice(),
            [Condition::KeyShape {
                sub_keys: 0,
                values: 0,
                ..
            }]
        )));

        scratch.set(&software.child("EmptyTwo"), "NowInUse", "1");
        let report = fix(&issues, None, &Progress::new()).unwrap();
        assert_eq!((report.fixed, report.skipped_changed), (1, 1));
        assert!(!software.child("EmptyOne").exists());
        assert_eq!(
            software
                .child("EmptyTwo")
                .read_string("NowInUse")
                .as_deref(),
            Some("1")
        );
    }
}
