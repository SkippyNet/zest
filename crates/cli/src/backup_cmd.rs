//! The `backup` commands: versioned file backups from the command line.

use clap::{Subcommand, ValueEnum};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};
use zest_engine::Settings;
use zest_engine::backup::{
    self, BackupOptions, Overwrite, Repository, RestoreOptions, RestoreTarget, Selection, Snapshot,
    SnapshotInfo, VerifyScope,
};
use zest_engine::core::Progress;
use zest_engine::core::fmt::{
    format_bytes, format_count, format_duration, format_timestamp, plural,
};

type CliResult = Result<(), String>;

/// What to do with a file that is already there and differs.
#[derive(Clone, Copy, ValueEnum)]
pub enum OverwriteChoice {
    /// Leave the existing file alone and report it.
    Skip,
    /// Restore under a new name beside the existing file.
    KeepBoth,
    /// Replace the existing file with the backed-up version.
    Replace,
}

#[derive(Subcommand)]
pub enum BackupAction {
    /// Back up folders. Without arguments, uses the folders, destination and
    /// exclusions saved in the app.
    Create {
        /// Folders or files to back up.
        sources: Vec<PathBuf>,
        /// Folder or drive that holds the backups.
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Leave out names or paths matching this pattern (repeatable).
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        /// A note stored with the backup.
        #[arg(long, default_value = "")]
        label: String,
        /// Afterwards keep only this many backups (0 keeps all).
        #[arg(long)]
        keep_last: Option<u32>,
        /// Read every file again instead of trusting size and date.
        #[arg(long)]
        reread: bool,
    },
    /// List the backups in a destination (changes nothing).
    List {
        /// Folder or drive that holds the backups.
        #[arg(long)]
        dest: Option<PathBuf>,
    },
    /// Restore a backup, or parts of it.
    Restore {
        /// Folder or drive that holds the backups.
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Which backup, as shown by `backup list`. Defaults to the newest.
        #[arg(long, default_value = "latest")]
        snapshot: String,
        /// Restore into this folder.
        #[arg(
            long,
            conflicts_with = "original",
            required_unless_present = "original"
        )]
        to: Option<PathBuf>,
        /// Restore to where the files were backed up from.
        #[arg(long)]
        original: bool,
        /// What to do when a different file is already there.
        #[arg(long, value_enum, default_value = "skip")]
        overwrite: OverwriteChoice,
        /// Restore only this part: a source name such as `Documents`, or a
        /// path below it such as `Documents\Letters` (repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// Read stored data back and check it (changes nothing).
    Verify {
        /// Folder or drive that holds the backups.
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Check only this backup. Defaults to all of them.
        #[arg(long)]
        snapshot: Option<String>,
    },
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn destination(given: Option<PathBuf>, settings: &Settings) -> Result<PathBuf, String> {
    given
        .or_else(|| settings.backup.destination.clone())
        .ok_or_else(|| "no destination: pass --dest or choose one in the app".to_owned())
}

fn taken(info: &SnapshotInfo) -> String {
    format_timestamp(UNIX_EPOCH + Duration::from_secs(info.created.max(0) as u64))
}

fn resolve_snapshot(repo: &Repository, wanted: &str) -> Result<String, String> {
    if wanted.eq_ignore_ascii_case("latest") {
        repo.snapshot_ids()
            .map_err(text)?
            .pop()
            .ok_or_else(|| "there are no backups in this destination yet".to_owned())
    } else {
        Ok(wanted.to_owned())
    }
}

/// Turns `Documents\Letters` into the source it names and the path below it.
fn resolve_selection(snapshot: &Snapshot, paths: &[String]) -> Result<Vec<Selection>, String> {
    let roots = snapshot.roots();
    paths
        .iter()
        .map(|given| {
            let normalized = given.replace('/', "\\");
            let trimmed = normalized.trim_matches('\\');
            let (name, rest) = trimmed.split_once('\\').unwrap_or((trimmed, ""));
            let root = roots
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
                    format!(
                        "{given:?} does not start with a source of this backup ({})",
                        names.join(", ")
                    )
                })?;
            Ok(Selection {
                root: root.root,
                path: rest.to_owned(),
            })
        })
        .collect()
}

pub fn run(json: bool, action: BackupAction) -> CliResult {
    let settings = Settings::load();
    match action {
        BackupAction::Create {
            sources,
            dest,
            excludes,
            label,
            keep_last,
            reread,
        } => {
            let dest = destination(dest, &settings)?;
            let options = BackupOptions {
                sources: if sources.is_empty() {
                    settings.backup.sources.clone()
                } else {
                    sources
                },
                excludes: if excludes.is_empty() {
                    settings.backup.excludes.clone()
                } else {
                    excludes
                },
                label,
                reread_all: reread,
                ..BackupOptions::default()
            };
            let repo = Repository::open_or_create(&dest).map_err(text)?;
            let report =
                backup::create_snapshot(&repo, &options, &Progress::new()).map_err(text)?;
            let keep = keep_last.unwrap_or(settings.backup.keep_last) as usize;
            let pruned = backup::prune(&repo, keep, &Progress::new()).map_err(text)?;
            if json {
                println!(
                    "{}",
                    json!({
                        "snapshot": report.snapshot,
                        "read_files": report.read_files,
                        "skipped": report.skipped,
                        "removed_snapshots": pruned.removed,
                        "elapsed_ms": report.elapsed.as_millis() as u64,
                    })
                );
                return Ok(());
            }
            let s = &report.snapshot;
            println!(
                "Backup {} written to {} in {}.",
                s.id,
                repo.root().display(),
                format_duration(report.elapsed)
            );
            println!(
                "{}, {}. {} read, {} unchanged. {} of new data, stored in {}.",
                plural(s.files, "file"),
                format_bytes(s.bytes),
                format_count(report.read_files),
                format_count(s.reused_files),
                format_bytes(s.new_bytes),
                format_bytes(s.stored_bytes)
            );
            for item in report.skipped.iter().take(20) {
                println!("  left out: {} ({})", item.path, item.reason.label());
            }
            if report.skipped.len() > 20 {
                println!(
                    "  … and {} more (use --json for all)",
                    report.skipped.len() - 20
                );
            }
            if !pruned.removed.is_empty() {
                println!(
                    "{} removed, {} freed.",
                    plural(pruned.removed.len() as u64, "old backup"),
                    format_bytes(pruned.gc.bytes_freed)
                );
            }
            Ok(())
        }
        BackupAction::List { dest } => {
            let dest = destination(dest, &settings)?;
            let repo = Repository::open(&dest).map_err(text)?;
            let snapshots = repo.list_snapshots().map_err(text)?;
            let stats = backup::stats(&repo).map_err(text)?;
            if json {
                println!(
                    "{}",
                    json!({
                        "snapshots": snapshots,
                        "stored_bytes": stats.stored_bytes,
                        "logical_bytes": stats.logical_bytes,
                        "objects": stats.objects,
                    })
                );
                return Ok(());
            }
            println!(
                "{:<20} {:<17} {:>9} {:>10} {:>10}  CONTENTS",
                "BACKUP", "TAKEN", "FILES", "SIZE", "ADDED"
            );
            for s in &snapshots {
                let contents = if s.label.is_empty() {
                    s.sources.join(", ")
                } else {
                    format!("{} ({})", s.label, s.sources.join(", "))
                };
                println!(
                    "{:<20} {:<17} {:>9} {:>10} {:>10}  {}",
                    s.id,
                    taken(s),
                    format_count(s.files),
                    format_bytes(s.bytes),
                    format_bytes(s.stored_bytes),
                    contents
                );
            }
            println!(
                "\n{}: {} of versions kept in {}. Nothing was changed.",
                plural(stats.snapshots, "backup"),
                format_bytes(stats.logical_bytes),
                format_bytes(stats.stored_bytes)
            );
            Ok(())
        }
        BackupAction::Restore {
            dest,
            snapshot,
            to,
            original,
            overwrite,
            paths,
        } => {
            let dest = destination(dest, &settings)?;
            let repo = Repository::open(&dest).map_err(text)?;
            let id = resolve_snapshot(&repo, &snapshot)?;
            let loaded = Snapshot::load(&repo, &id).map_err(text)?;
            let target = match (to, original) {
                (Some(folder), false) => RestoreTarget::Folder(folder),
                (None, true) => RestoreTarget::Original,
                _ => return Err("pass either --to <folder> or --original".to_owned()),
            };
            let options = RestoreOptions {
                target,
                overwrite: match overwrite {
                    OverwriteChoice::Skip => Overwrite::Skip,
                    OverwriteChoice::KeepBoth => Overwrite::KeepBoth,
                    OverwriteChoice::Replace => Overwrite::Replace,
                },
                selection: resolve_selection(&loaded, &paths)?,
            };
            let report =
                backup::restore(&repo, &loaded, &options, &Progress::new()).map_err(text)?;
            if json {
                let failed: Vec<_> = report
                    .failed
                    .iter()
                    .map(|f| json!({"path": f.path, "reason": f.reason}))
                    .collect();
                println!(
                    "{}",
                    json!({
                        "snapshot": id,
                        "files": report.files,
                        "bytes": report.bytes,
                        "unchanged": report.unchanged,
                        "kept_existing": report.kept_existing,
                        "renamed": report.renamed,
                        "failed": failed,
                    })
                );
            } else {
                println!(
                    "{} restored from backup {} ({}), {} already in place.",
                    plural(report.files, "file"),
                    id,
                    format_bytes(report.bytes),
                    format_count(report.unchanged)
                );
                for path in &report.kept_existing {
                    println!("  left alone, a different file is there: {path}");
                }
                for path in &report.renamed {
                    println!("  restored beside the existing file as: {path}");
                }
                for failure in &report.failed {
                    println!("  FAILED {}: {}", failure.path, failure.reason);
                }
            }
            if report.failed.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{} could not be restored",
                    plural(report.failed.len() as u64, "file")
                ))
            }
        }
        BackupAction::Verify { dest, snapshot } => {
            let dest = destination(dest, &settings)?;
            let repo = Repository::open(&dest).map_err(text)?;
            let scope = match snapshot {
                Some(wanted) => VerifyScope::Snapshot(resolve_snapshot(&repo, &wanted)?),
                None => VerifyScope::All,
            };
            let report = backup::verify(&repo, &scope, &Progress::new()).map_err(text)?;
            if json {
                let damaged: Vec<_> = report
                    .damaged_files
                    .iter()
                    .map(|f| json!({"snapshot": f.snapshot, "path": f.path}))
                    .collect();
                println!(
                    "{}",
                    json!({
                        "ok": report.is_ok(),
                        "snapshots": report.snapshots,
                        "objects": report.objects,
                        "bytes": report.bytes,
                        "missing": report.missing,
                        "damaged": report.damaged,
                        "damaged_file_count": report.damaged_file_count,
                        "damaged_files": damaged,
                        "unreadable_snapshots": report.unreadable_snapshots,
                    })
                );
            } else {
                println!(
                    "{} checked: {} stored pieces, {} of file contents.",
                    plural(report.snapshots, "backup"),
                    format_count(report.objects),
                    format_bytes(report.bytes)
                );
                for (id, why) in &report.unreadable_snapshots {
                    println!("  backup {id} cannot be read: {why}");
                }
                for file in report.damaged_files.iter().take(50) {
                    println!("  damaged in backup {}: {}", file.snapshot, file.path);
                }
                if report.damaged_file_count > 50 {
                    println!(
                        "  … and {} more (use --json for the list)",
                        report.damaged_file_count - 50
                    );
                }
            }
            if report.is_ok() {
                if !json {
                    println!("Everything is intact. Nothing was changed.");
                }
                Ok(())
            } else {
                Err(format!(
                    "{} missing and {} damaged stored pieces affect {}",
                    report.missing,
                    report.damaged,
                    plural(report.damaged_file_count, "file")
                ))
            }
        }
    }
}
