//! Registry cleaner for Zest: finds and safely removes invalid registry entries.
//!
//! # Workflow
//!
//! ```no_run
//! use zest_core::Progress;
//! use zest_registry::{RegBackup, ScanOptions, fix, scan};
//!
//! # fn main() -> zest_core::Result<()> {
//! // 1. Scan (read-only).
//! let issues = scan(&ScanOptions::default(), &Progress::new())?;
//!
//! // 2. Let the user review; `selected_by_default` is the suggested tick state.
//! let selected: Vec<_> = issues.into_iter().filter(|i| i.selected_by_default()).collect();
//!
//! // 3. Fix with a backup that is written before anything is removed.
//! let mut backup = RegBackup::new("registry-clean");
//! let report = fix(&selected, Some(&mut backup), &Progress::new())?;
//!
//! // 4. Undo, if ever needed.
//! if let Some(file) = &report.backup_file {
//!     zest_registry::import_reg_file(file)?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Safety model
//!
//! * **Conservative detection.** An entry is reported only when its target
//!   is *confirmed* missing on a local fixed drive. Network paths, removable
//!   drives, unexpanded variables, script hosts, packaged-app locations and
//!   anything that answers "access denied" are never reported.
//! * **Physical hives.** Class registrations are reported against
//!   `HKLM\SOFTWARE\Classes` or `HKCU\Software\Classes`, never the merged
//!   `HKCR` view, so backups restore exactly what was removed.
//! * **Re-validation.** Every [`Issue`] carries the [`Condition`]s that made
//!   it an issue. [`fix`] re-checks them immediately before removal and
//!   skips entries that changed since the scan.
//! * **Backup first.** [`fix`] saves the `.reg` backup to disk before the
//!   first deletion and aborts without changes if that fails.
//! * **Guarded deletion.** All removals go through `zest_core::reg`, whose
//!   protected-key guard refuses structural keys regardless of what a
//!   scanner reports.

#![cfg(windows)]

mod check;
mod fixing;
mod physical;
mod scanners;
mod scanning;
#[cfg(test)]
mod testutil;
mod types;

pub use fixing::{FixReport, fix};
pub use scanning::{count_by_category, scan};
pub use types::{Category, Condition, Confidence, Issue, ScanOptions};
pub use zest_core::reg::{
    BackupInfo, RegBackup, RegPath, RegRoot, import_reg_file, list_backups, registry_backups_dir,
};
