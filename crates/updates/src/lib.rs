//! Software updater, driver updater and Zest's own update check.
//!
//! The first two halves of this crate deliberately delegate the actual
//! downloading and installing to components that ship with Windows, so Zest
//! never hosts, mirrors or repackages anybody's installer:
//!
//! * [`software`] drives the **Windows Package Manager** (`winget`). Every
//!   update comes from the publisher-verified package source, with the hash
//!   checks and installer handling the package manager already performs. On
//!   PCs without the package manager, the same public catalog is read
//!   directly to at least show which programs are out of date.
//! * [`drivers`] lists the drivers in use and asks **Windows Update** for
//!   newer ones. Windows Update is the only driver source: its drivers are
//!   signed and matched to the exact hardware by Microsoft, which is the safe
//!   choice for something that runs in the kernel.
//! * [`selfupdate`] keeps Zest itself current from the releases of its
//!   public repository, verifying every download against its published
//!   checksum.
//!
//! Listing and searching never change the system and need no special rights.
//! Installing does; see the documentation of each function.
//!
//! # Network use
//!
//! Zest itself makes exactly two kinds of connection, both anonymous HTTPS
//! requests that carry nothing but the Zest version: the check for a newer
//! Zest release, and the download of the public package catalog on PCs that
//! lack the package manager. Both can be switched off.

#![cfg(windows)]
#![warn(missing_docs)]

mod com;
mod exec;
mod net;

pub mod drivers;
pub mod selfupdate;
pub mod software;

pub use net::NetError;
