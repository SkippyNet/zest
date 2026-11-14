//! Shared foundation for every Zest crate.
//!
//! Nothing in here knows about a particular feature. It provides the safety
//! rails and plumbing the feature crates build on:
//!
//! * [`safety`] – the hard guard that refuses to delete critical locations.
//! * [`delete`] – normal, recycle-bin and multi-pass secure deletion.
//! * [`reg`] – registry paths plus `.reg` backups taken before any change.
//! * [`paths`] – per-user and system folder discovery (multi-user aware).
//! * [`progress`] / [`task`] – cancellable background work with progress.
//! * [`process`] – windowless child processes and process enumeration.
//! * [`elevation`] – administrator detection and relaunch.
//! * [`cmdline`] – conservative resolution of registry command lines.
//! * [`fileinfo`] – publisher/description lookup from version resources.
//! * [`fastdir`] / [`walk`] – bulk directory listing and link-safe tree walks.

#![cfg(windows)]

pub mod cmdline;
pub mod delete;
pub mod elevation;
pub mod error;
pub mod fastdir;
pub mod fileinfo;
pub mod fmt;
pub mod logging;
pub mod paths;
pub mod process;
pub mod progress;
pub mod reg;
pub mod safety;
pub mod task;
pub mod walk;

pub use error::{Error, IoContext, Result};
pub use progress::Progress;
pub use task::Task;

/// Product identity. Change the name here (and the package names) to rebrand.
pub mod brand {
    /// Display name of the product.
    pub const NAME: &str = "Zest";
    /// Short marketing line shown beside the name.
    pub const TAGLINE: &str = "Peel off the junk. Squeeze out the speed";
    /// Folder name used under `%APPDATA%` and inside Task Scheduler.
    pub const DATA_DIR_NAME: &str = "Zest";
    /// File name of the graphical executable.
    pub const GUI_EXE: &str = "zest.exe";
    /// File name of the command line executable.
    pub const CLI_EXE: &str = "zest-cli.exe";
    /// Semantic version of this build.
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
    /// GitHub repository as `owner/name`; releases published there are what
    /// the built-in update check looks at.
    pub const REPO: &str = "SkippyNet/zest";
    /// Source code and issue tracker.
    pub const REPO_URL: &str = "https://github.com/SkippyNet/zest";
    /// Product website.
    pub const WEBSITE_URL: &str = "https://skippynet.github.io/zest/";
    /// Where problems are reported.
    pub const ISSUES_URL: &str = "https://github.com/SkippyNet/zest/issues";
}
