//! Software updater and driver updater for Zest.
//!
//! Both halves of this crate deliberately delegate the actual downloading and
//! installing to components that ship with Windows, so Zest never hosts,
//! mirrors or repackages anybody's installer:
//!
//! * [`software`] drives the **Windows Package Manager** (`winget`). Every
//!   update comes from the publisher-verified package source, with the hash
//!   checks and installer handling the package manager already performs.
//! * [`drivers`] lists the drivers in use and asks **Windows Update** for
//!   newer ones. Windows Update is the only driver source: its drivers are
//!   signed and matched to the exact hardware by Microsoft, which is the safe
//!   choice for something that runs in the kernel.
//!
//! Listing and searching never change the system and need no special rights.
//! Installing does; see the documentation of each function.

#![cfg(windows)]
#![warn(missing_docs)]

mod com;
mod exec;

pub mod drivers;
pub mod software;
