//! Application layer of Zest.
//!
//! The feature crates each do one job. This crate ties them to the things an
//! application needs around them: persistent [`settings`], a cleaning
//! [`history`], one-call [`ops`] that turn settings into engine runs, the
//! Windows integration for scheduled cleaning ([`schedule`]), the
//! background agent ([`autostart`], [`smart`]) and the check for new versions
//! of Zest itself ([`app_update`]).

#![cfg(windows)]

pub mod app_update;
pub mod autostart;
pub mod health;
pub mod history;
pub mod ops;
pub mod schedule;
pub mod settings;
pub mod smart;

pub use history::{History, HistoryEntry, RunKind};
pub use settings::Settings;

// The feature engines, re-exported so front ends depend on one crate.
pub use zest_backup as backup;
pub use zest_cleaner as cleaner;
pub use zest_core as core;
pub use zest_disk as disk;
pub use zest_hardware as hardware;
pub use zest_performance as performance;
pub use zest_registry as registry;
pub use zest_system as system;
pub use zest_tweaks as tweaks;
pub use zest_updates as updates;
