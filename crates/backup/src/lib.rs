//! Backups for Zest: versioned file backups and full system images.
//!
//! # File backups
//!
//! A [`Repository`] lives in a folder on the drive the user picks. Each run
//! of [`create_snapshot`] records the chosen folders as they are at that
//! moment. File contents are stored in chunks named by their hash, so a file
//! that did not change, a copy of a file, or the unchanged part of a large
//! file takes no extra room in later snapshots, and everything is compressed.
//!
//! * [`create_snapshot`] takes a snapshot; unchanged files are not read again.
//! * [`Repository::list_snapshots`] and [`Snapshot`] list and browse them.
//! * [`restore`] brings back a snapshot, a folder or single files, to the
//!   original place or elsewhere, and never overwrites without being told to.
//! * [`verify`] reads stored data back and checks it against its hashes.
//! * [`delete_snapshot`], [`prune`] and [`collect_garbage`] remove snapshots
//!   and reclaim the space only they used.
//!
//! Links and junctions are never followed, online-only cloud files are not
//! downloaded, and a file that cannot be read is reported rather than
//! failing the run. A snapshot becomes visible only when its record is
//! complete, so an interrupted run leaves the repository as it was.
//!
//! # System images
//!
//! [`image`] drives the imaging engine built into Windows to capture whole
//! drives, including everything needed to start the PC. Such an image is
//! restored from the Windows Recovery Environment.

#![cfg(windows)]

mod create;
pub mod exclude;
pub mod image;
mod maintain;
mod manifest;
mod repo;
mod restore;

pub use create::{BackupOptions, BackupReport, create_snapshot};
pub use exclude::{DEFAULT_EXCLUDES, Excludes, split_patterns};
pub use maintain::{
    DamagedFile, GcReport, PruneReport, RepoStats, VerifyReport, VerifyScope, collect_garbage,
    delete_snapshot, discard_damaged, prune, reference_counts, stats, verify,
};
pub use manifest::{
    Entry, EntryKind, MANIFEST_FORMAT, Manifest, ObjectId, RootKind, SkipReason, Skipped,
    SnapshotInfo, SourceRoot,
};
pub use repo::{CHUNK_SIZE, CONFIG_FILE, REPO_DIR_NAME, RepoLock, Repository, is_snapshot_id};
pub use restore::{
    BrowseEntry, Overwrite, RestoreFailure, RestoreOptions, RestoreReport, RestoreTarget,
    Selection, Snapshot, filetime_to_system, restore,
};
