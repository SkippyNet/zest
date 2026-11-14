//! Looking after a repository: checking it, removing old snapshots and
//! reclaiming the space they held.

use crate::manifest::{EntryKind, Manifest, ObjectId};
use crate::repo::{ObjectState, Repository};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use zest_core::{Error, Progress, Result};

/// Most damaged files listed by name in a [`VerifyReport`].
const MAX_DAMAGED_LISTED: usize = 5_000;

/// What to check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyScope {
    /// Every snapshot.
    All,
    /// One snapshot.
    Snapshot(String),
}

/// A backed-up file whose stored contents are missing or damaged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DamagedFile {
    /// Snapshot the file belongs to.
    pub snapshot: String,
    /// Original location of the file.
    pub path: String,
}

/// Result of a check.
#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    /// Snapshots that were checked.
    pub snapshots: u64,
    /// Stored objects that were read and checked.
    pub objects: u64,
    /// Bytes of file contents those objects hold.
    pub bytes: u64,
    /// Objects that snapshots refer to but that are gone.
    pub missing: u64,
    /// Objects whose contents no longer match their hash.
    pub damaged: u64,
    /// Files affected by missing or damaged objects (at most a few thousand
    /// are listed; see `damaged_file_count` for the full number).
    pub damaged_files: Vec<DamagedFile>,
    /// Number of files affected in total.
    pub damaged_file_count: u64,
    /// Snapshots whose record could not be read: `(id, reason)`.
    pub unreadable_snapshots: Vec<(String, String)>,
    /// The stored objects whose contents are provably wrong (see
    /// [`discard_damaged`]).
    pub damaged_objects: Vec<ObjectId>,
}

impl VerifyReport {
    /// True when everything that was checked is intact.
    pub fn is_ok(&self) -> bool {
        self.missing == 0 && self.damaged == 0 && self.unreadable_snapshots.is_empty()
    }
}

/// Result of a clean-up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Stored objects removed because no snapshot uses them any more.
    pub objects_removed: u64,
    /// Disk space given back.
    pub bytes_freed: u64,
}

/// Result of removing old snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Snapshots that were removed, oldest first.
    pub removed: Vec<String>,
    /// The clean-up that followed.
    pub gc: GcReport,
}

/// Figures about a repository.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepoStats {
    /// Number of snapshots.
    pub snapshots: u64,
    /// Size of the files in the newest snapshot.
    pub latest_bytes: u64,
    /// Size of the files of all snapshots added together.
    pub logical_bytes: u64,
    /// Space the repository's stored data takes on disk.
    pub stored_bytes: u64,
    /// Number of stored objects.
    pub objects: u64,
    /// When the oldest snapshot was taken (seconds since the Unix epoch).
    pub oldest: Option<i64>,
    /// When the newest snapshot was taken.
    pub newest: Option<i64>,
}

impl RepoStats {
    /// How many times larger the backed-up versions are than what they take
    /// on disk: the combined effect of sharing unchanged data between
    /// snapshots and compression. `None` for an empty repository.
    pub fn savings_ratio(&self) -> Option<f64> {
        (self.stored_bytes > 0 && self.logical_bytes > 0)
            .then(|| self.logical_bytes as f64 / self.stored_bytes as f64)
    }
}

fn original_path(manifest: &Manifest, root: u32, rel: &str) -> String {
    let base = manifest
        .roots
        .get(root as usize)
        .map(|r| r.path.as_str())
        .unwrap_or_default();
    if rel.is_empty() {
        base.to_owned()
    } else {
        format!("{}\\{rel}", base.trim_end_matches('\\'))
    }
}

/// Reads stored data back and checks every object against its hash.
///
/// This is the test that the backup can actually be restored: an object that
/// is missing or altered is reported together with the files it belongs to.
/// `progress` counts objects.
pub fn verify(repo: &Repository, scope: &VerifyScope, progress: &Progress) -> Result<VerifyReport> {
    let ids = match scope {
        VerifyScope::All => repo.snapshot_ids()?,
        VerifyScope::Snapshot(id) => vec![id.clone()],
    };
    let mut report = VerifyReport::default();
    let mut manifests = Vec::new();
    for id in &ids {
        progress.check()?;
        match repo.load_manifest(id) {
            Ok(manifest) => manifests.push(manifest),
            Err(Error::NotFound(what)) if matches!(scope, VerifyScope::Snapshot(_)) => {
                return Err(Error::NotFound(what));
            }
            Err(e) => report
                .unreadable_snapshots
                .push((id.clone(), e.to_string())),
        }
    }
    report.snapshots = manifests.len() as u64;

    let mut wanted: HashSet<ObjectId> = HashSet::new();
    for manifest in &manifests {
        for entry in &manifest.entries {
            wanted.extend(entry.chunks.iter().copied());
        }
    }
    let wanted: Vec<ObjectId> = wanted.into_iter().collect();
    progress.set_total(wanted.len() as u64);
    progress.set_message("Checking stored data");

    let states: Vec<(ObjectId, Option<ObjectState>)> = wanted
        .par_iter()
        .map(|id| {
            if progress.is_cancelled() {
                return (*id, None);
            }
            let state = repo.object_state(id);
            if let ObjectState::Intact(len) = state {
                progress.add_bytes(len);
            }
            progress.inc(1);
            (*id, Some(state))
        })
        .collect();
    progress.check()?;

    let mut bad: HashSet<ObjectId> = HashSet::new();
    for (id, state) in states {
        match state {
            Some(ObjectState::Intact(len)) => {
                report.objects += 1;
                report.bytes += len;
            }
            Some(ObjectState::Missing) => {
                report.missing += 1;
                bad.insert(id);
            }
            Some(ObjectState::Damaged) => {
                report.objects += 1;
                report.damaged += 1;
                report.damaged_objects.push(id);
                bad.insert(id);
            }
            Some(ObjectState::Unreadable) => {
                report.objects += 1;
                report.damaged += 1;
                bad.insert(id);
            }
            None => {}
        }
    }
    if !bad.is_empty() {
        for manifest in &manifests {
            for entry in &manifest.entries {
                if entry.kind == EntryKind::File && entry.chunks.iter().any(|id| bad.contains(id)) {
                    report.damaged_file_count += 1;
                    if report.damaged_files.len() < MAX_DAMAGED_LISTED {
                        report.damaged_files.push(DamagedFile {
                            snapshot: manifest.info.id.clone(),
                            path: original_path(manifest, entry.root, &entry.path),
                        });
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Removes the stored objects a check found to be damaged, so that the next
/// backup stores them again from the files on this PC (where those still
/// exist unchanged). A damaged object cannot be restored from, so nothing of
/// value is lost. Each object is checked once more before it is removed.
/// Returns how many were removed.
pub fn discard_damaged(repo: &Repository, report: &VerifyReport) -> Result<u64> {
    let _lock = repo.lock()?;
    let mut removed = 0;
    for id in &report.damaged_objects {
        if repo.object_state(id) == ObjectState::Damaged && repo.remove_object(id).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        log::info!("removed {removed} damaged objects from the backup");
    }
    Ok(removed)
}

/// Removes stored objects that no snapshot refers to, and leftovers of
/// interrupted writes.
///
/// Nothing is removed unless every snapshot record can be read: an unreadable
/// record might be the only thing pointing at data that is still wanted.
pub fn collect_garbage(repo: &Repository, progress: &Progress) -> Result<GcReport> {
    let _lock = repo.lock()?;
    collect_garbage_locked(repo, progress)
}

fn collect_garbage_locked(repo: &Repository, progress: &Progress) -> Result<GcReport> {
    progress.set_message("Reclaiming space");
    let mut used: HashSet<ObjectId> = HashSet::new();
    for id in repo.snapshot_ids()? {
        progress.check()?;
        let manifest = repo.load_manifest(&id).map_err(|e| {
            Error::Other(format!(
                "Nothing was cleaned up because snapshot {id} cannot be read ({e}). Remove that snapshot first."
            ))
        })?;
        for entry in &manifest.entries {
            used.extend(entry.chunks.iter().copied());
        }
    }
    let unused: Vec<ObjectId> = repo
        .list_objects()?
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| !used.contains(id))
        .collect();
    progress.set_total(unused.len() as u64);

    let mut report = GcReport {
        objects_removed: 0,
        bytes_freed: repo.clear_tmp(),
    };
    for id in unused {
        progress.check()?;
        match repo.remove_object(&id) {
            Ok(size) => {
                report.objects_removed += 1;
                report.bytes_freed += size;
            }
            Err(e) => log::warn!("could not remove unused backup data: {e}"),
        }
        progress.inc(1);
    }
    Ok(report)
}

/// Removes one snapshot and reclaims the space only it was using. Files that
/// other snapshots share with it stay.
pub fn delete_snapshot(repo: &Repository, id: &str, progress: &Progress) -> Result<GcReport> {
    let _lock = repo.lock()?;
    repo.remove_snapshot_files(id)?;
    log::info!("snapshot {id} removed");
    collect_garbage_locked(repo, progress)
}

/// Keeps the newest `keep_last` snapshots and removes the rest, then
/// reclaims space. `keep_last` of zero keeps everything.
pub fn prune(repo: &Repository, keep_last: usize, progress: &Progress) -> Result<PruneReport> {
    let _lock = repo.lock()?;
    let ids = repo.snapshot_ids()?;
    if keep_last == 0 || ids.len() <= keep_last {
        return Ok(PruneReport::default());
    }
    let mut report = PruneReport::default();
    for id in &ids[..ids.len() - keep_last] {
        progress.check()?;
        repo.remove_snapshot_files(id)?;
        report.removed.push(id.clone());
    }
    log::info!("removed {} old snapshots", report.removed.len());
    report.gc = collect_garbage_locked(repo, progress)?;
    Ok(report)
}

/// Figures about the repository, from the snapshot summaries and the sizes of
/// the stored objects.
pub fn stats(repo: &Repository) -> Result<RepoStats> {
    let snapshots = repo.list_snapshots()?;
    let objects = repo.list_objects()?;
    Ok(RepoStats {
        snapshots: snapshots.len() as u64,
        latest_bytes: snapshots.first().map_or(0, |s| s.bytes),
        logical_bytes: snapshots.iter().map(|s| s.bytes).sum(),
        stored_bytes: objects.iter().map(|(_, size)| size).sum(),
        objects: objects.len() as u64,
        oldest: snapshots.last().map(|s| s.created),
        newest: snapshots.first().map(|s| s.created),
    })
}

/// How many snapshots refer to each stored object. Used by tests and
/// diagnostics to reason about sharing.
#[doc(hidden)]
pub fn reference_counts(repo: &Repository) -> Result<HashMap<ObjectId, u64>> {
    let mut counts: HashMap<ObjectId, u64> = HashMap::new();
    for id in repo.snapshot_ids()? {
        let manifest = repo.load_manifest(&id)?;
        let unique: HashSet<ObjectId> = manifest
            .entries
            .iter()
            .flat_map(|e| e.chunks.iter().copied())
            .collect();
        for object in unique {
            *counts.entry(object).or_default() += 1;
        }
    }
    Ok(counts)
}
