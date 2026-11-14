//! Windows privacy settings and built-in repair tools for Zest.
//!
//! * [`privacy`] – a catalog of documented Windows privacy settings, with the
//!   current state of each and reversible switching.
//! * [`tools`] – the maintenance and repair commands that ship with Windows,
//!   run hidden with their output captured.
//!
//! Nothing here invents its own tweak: every registry value in the privacy
//! catalog is one Microsoft documents, and every tool is a program in
//! `System32`.

#![cfg(windows)]

pub mod privacy;
pub mod tools;
