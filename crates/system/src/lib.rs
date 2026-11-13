//! Startup manager, uninstaller and System Restore tools for Zest.
//!
//! * [`startup`] – everything that starts automatically: Run keys, Startup
//!   folders, scheduled tasks, services and Explorer context-menu handlers,
//!   with reversible enable/disable and backed-up deletion.
//! * [`apps`] – installed programs (classic installers and Store packages),
//!   uninstall / repair, stale entry removal and a strict leftover scan.
//! * [`restore`] – System Restore points: list, create and delete.
//! * [`shortcut`] – reading and writing `.lnk` shortcuts.
//!
//! Every enumeration is read-only. Every change either uses the mechanism
//! Windows itself uses (so it shows up in Task Manager / Settings and can be
//! undone there) or is preceded by a backup.

#![cfg(windows)]
#![warn(missing_docs)]

pub mod apps;
mod com;
pub mod restore;
pub mod shortcut;
pub mod startup;
#[cfg(test)]
mod test_support;
mod util;

use serde::{Deserialize, Serialize};

/// Whether something belongs to the signed-in user or to the whole PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    /// Applies to the current user only; changing it needs no special rights.
    User,
    /// Applies to every user of this PC; changing it needs administrator rights.
    Machine,
}

impl Scope {
    /// Short label for tables, e.g. `Current user`.
    pub fn label(self) -> &'static str {
        match self {
            Scope::User => "Current user",
            Scope::Machine => "All users",
        }
    }
}
