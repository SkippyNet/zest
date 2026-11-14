//! Performance optimizer for Zest: background app control and memory tools.
//!
//! The crate answers three questions and acts on the answers reversibly:
//!
//! * **What is running, and what does it cost?** – [`monitor`] samples the
//!   whole system cheaply and groups processes into the programs a person
//!   would recognise.
//! * **What may be touched?** – [`classify`] is the hard deny-list: Windows
//!   components, services, security software, other users' programs, this
//!   program and whatever the user is working in are never controlled.
//! * **How can background programs be quietened?** – [`control`] offers
//!   efficiency mode, suspension, working-set trimming and "end task", plus
//!   standing [`control::SleepRule`]s for a background agent. Everything is
//!   tracked so it can be undone, even after a crash.
//!
//! [`power`] adds power plans, a few read-only Windows settings and a
//! hardware summary. [`boost`] combines the two into a temporary push for
//! performance that undoes itself.
//!
//! # Example
//!
//! ```no_run
//! use zest_performance::monitor::Sampler;
//! use std::time::Duration;
//!
//! let mut sampler = Sampler::new();
//! sampler.sample()?; // the first tick only establishes a baseline
//! std::thread::sleep(Duration::from_secs(1));
//! let snapshot = sampler.sample()?;
//! for app in snapshot.recommended() {
//!     println!("{} uses {:.1}% CPU in the background", app.name, app.cpu_percent);
//! }
//! # Ok::<(), zest_core::Error>(())
//! ```

#![cfg(windows)]
#![warn(missing_docs)]

pub mod boost;
pub mod classify;
pub mod control;
pub mod monitor;
pub mod power;

mod security;
mod sys;
mod windowing;

pub use boost::{Boost, BoostEnd, BoostStart, PlanOutcome, PowerPlans, SystemPowerPlans};
pub use classify::{AppKind, BackgroundImpact, ProtectionReason, is_protected};
pub use control::{
    ActionFailure, ActionReport, ControlEvent, ControlEventKind, ControlState, SleepMode,
    SleepOrigin, SleepRule, SleepRules, SleepingApp, TrimReport, WakeReason, apply_rules,
    terminate, trim_all, trim_memory,
};
pub use monitor::{
    AppUsage, MemoryInfo, ProcessOwner, ProcessUsage, Sampler, SamplerOptions, SystemSnapshot,
    WindowState,
};
pub use power::{PowerPlan, PowerStatus, SystemSummary, SystemTweaks};
