//! Backs a folder up, checks the backup, restores it elsewhere and compares
//! the result with the original, byte for byte.
//!
//! ```text
//! cargo run -p zest-backup --example backup_roundtrip -- <source> <backup-dir> <restore-dir>
//! ```
//!
//! The source is only read. Everything is written below the two other
//! folders, which should be new or empty.

use std::path::{Path, PathBuf};
use zest_backup::{
    BackupOptions, DEFAULT_EXCLUDES, Overwrite, Repository, RestoreOptions, RestoreTarget,
    Snapshot, VerifyScope, create_snapshot, restore, stats, verify,
};
use zest_core::Progress;
use zest_core::fmt::{format_bytes, format_count};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let [source, backup_dir, restore_dir] = args.as_slice() else {
        eprintln!("usage: backup_roundtrip <source> <backup-dir> <restore-dir>");
        std::process::exit(2);
    };

    let repo = Repository::open_or_create(backup_dir)?;
    let options = BackupOptions {
        sources: vec![source.clone()],
        excludes: DEFAULT_EXCLUDES.iter().map(|p| (*p).to_owned()).collect(),
        ..BackupOptions::default()
    };

    for round in 1..=2 {
        let report = create_snapshot(&repo, &options, &Progress::new())?;
        let s = &report.snapshot;
        println!(
            "run {round}: snapshot {} - {} files, {} in {:.2?}; read {}, reused {}, new data {} stored as {}, skipped {}",
            s.id,
            format_count(s.files),
            format_bytes(s.bytes),
            report.elapsed,
            format_count(report.read_files),
            format_count(s.reused_files),
            format_bytes(s.new_bytes),
            format_bytes(s.stored_bytes),
            s.skipped,
        );
        for item in report.skipped.iter().take(5) {
            println!("  skipped: {} ({})", item.path, item.reason.label());
        }
    }

    let figures = stats(&repo)?;
    println!(
        "repository: {} snapshots, {} objects, {} on disk for {} of versions ({:.1}x)",
        figures.snapshots,
        format_count(figures.objects),
        format_bytes(figures.stored_bytes),
        format_bytes(figures.logical_bytes),
        figures.savings_ratio().unwrap_or(1.0),
    );

    let check = verify(&repo, &VerifyScope::All, &Progress::new())?;
    println!(
        "verify: {} objects, {} checked, {}",
        format_count(check.objects),
        format_bytes(check.bytes),
        if check.is_ok() {
            "all intact"
        } else {
            "PROBLEMS FOUND"
        }
    );

    let latest = repo
        .list_snapshots()?
        .into_iter()
        .next()
        .ok_or("no snapshot was written")?;
    let snapshot = Snapshot::load(&repo, &latest.id)?;
    let restored = restore(
        &repo,
        &snapshot,
        &RestoreOptions {
            target: RestoreTarget::Folder(restore_dir.clone()),
            overwrite: Overwrite::Skip,
            selection: Vec::new(),
        },
        &Progress::new(),
    )?;
    println!(
        "restore: {} files, {}, {} failed",
        format_count(restored.files),
        format_bytes(restored.bytes),
        restored.failed.len()
    );

    let root_name = &snapshot.roots()[0].name;
    let (compared, different) = compare(&snapshot, source, &restore_dir.join(root_name));
    println!("compare: {compared} files compared, {different} different");
    if !check.is_ok() || !restored.failed.is_empty() || different > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Compares every file of the snapshot's first source with its restored copy.
fn compare(snapshot: &Snapshot, source: &Path, restored: &Path) -> (u64, u64) {
    let (mut compared, mut different) = (0, 0);
    for entry in &snapshot.manifest().entries {
        if entry.root != 0 || entry.kind != zest_backup::EntryKind::File {
            continue;
        }
        let (original, copy) = if entry.path.is_empty() {
            (source.to_path_buf(), restored.to_path_buf())
        } else {
            (source.join(&entry.path), restored.join(&entry.path))
        };
        compared += 1;
        match (std::fs::read(&original), std::fs::read(&copy)) {
            (Ok(a), Ok(b)) if a == b => {}
            _ => {
                different += 1;
                println!("  differs: {}", entry.path);
            }
        }
    }
    (compared, different)
}
