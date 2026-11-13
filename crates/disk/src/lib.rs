//! Disk tools for Zest.
//!
//! * [`drives`] – drive letters, capacities, device kind and SSD detection.
//! * [`mod@analyze`] – the disk analyzer: totals, categories, extensions, the
//!   largest files and a pruned folder tree, from one fast parallel pass.
//! * [`duplicates`] – the duplicate finder with safe, verified removal.
//! * [`empty`] – the empty folder finder.
//! * [`wipe`] – free-space wiping, whole-drive wiping and file shredding.
//! * [`categories`] – the file type categories shared by the above.
//!
//! Every scanner is read-only, never follows junctions or symbolic links,
//! never opens cloud-only placeholder files, reports into a
//! [`zest_core::Progress`] and stops promptly when it is cancelled.
//! Everything that deletes goes through `zest_core::delete` and thereby
//! through the global safety guard.
//!
//! All long-running functions block the calling thread and do their work on
//! an internal thread pool; call them from a background task.

#![cfg(windows)]
#![warn(missing_docs)]

pub mod analyze;
pub mod categories;
pub mod drives;
pub mod duplicates;
pub mod empty;
pub mod wipe;

mod fastdir;
mod pool;

pub use analyze::{
    Analysis, AnalyzeOptions, CategoryStat, ExtensionStat, FileEntry, FolderNode, analyze,
    find_large_files,
};
pub use categories::FileCategory;
pub use drives::{DriveInfo, DriveKind, drive_info, list_drives, list_local_drives};
pub use duplicates::{
    DupFile, DuplicateDeleteReport, DuplicateGroup, DuplicateOptions, DuplicateReport,
    KeepStrategy, delete_duplicates, find_duplicates, select_for_removal,
};
pub use empty::{EmptyFolderOptions, delete_empty_folders, find_empty_folders};
pub use wipe::{
    ContentWipeReport, DriveWipeReport, WipeReport, check_drive_wipe_allowed, shred,
    wipe_entire_drive, wipe_folder_contents, wipe_free_space,
};
