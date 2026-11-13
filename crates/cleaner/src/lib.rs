//! Junk and privacy cleaner for Zest.
//!
//! The crate is organised around three steps:
//!
//! 1. [`Catalog::build`] discovers what can be cleaned on this PC: Windows
//!    itself, every browser profile, and the applications that are present.
//! 2. [`scan`] analyses the enabled rules and returns the exact list of
//!    files, registry entries and operations involved. Nothing is modified.
//! 3. [`clean`] removes precisely what the scan listed.
//!
//! Every file deletion goes through `zest_core::delete`, and therefore
//! through the hard safety guard in `zest_core::safety`.
//!
//! ```no_run
//! use zest_cleaner::{Catalog, CleanOptions, clean, scan};
//! use zest_core::{Progress, paths::UserDirs};
//!
//! let catalog = Catalog::build(&[UserDirs::current()], &[]);
//! let opts = CleanOptions {
//!     enabled: catalog.default_selection(),
//!     min_age_hours: 24,
//!     ..Default::default()
//! };
//! let found = scan(&catalog, &opts, &Progress::new());
//! println!("{} bytes can be freed", found.bytes);
//! let report = clean(&catalog, &found, &opts, true, &Progress::new()); // dry run
//! assert!(report.dry_run);
//! ```

#![cfg(windows)]

pub mod browsers;
mod catalog;
mod clean;
pub mod cookies;
pub mod exclude;
pub mod extensions;
mod model;
pub mod pattern;
mod scan;
mod special;
pub mod sqlite;

pub use catalog::{Ctx, MachineDirs};
pub use clean::clean;
pub use model::*;
pub use scan::scan;
pub use special::recycle_bin_usage;
