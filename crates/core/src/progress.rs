//! A cheap, cloneable progress + cancellation handle.
//!
//! Engines receive a [`Progress`] and report into it; front ends keep a clone
//! and poll it from the UI thread. No channels, no locking on the hot path.

use crate::{Error, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    done: AtomicU64,
    total: AtomicU64,
    bytes: AtomicU64,
    cancelled: AtomicBool,
    message: Mutex<String>,
}

/// Shared progress state for one long running operation.
#[derive(Clone, Default)]
pub struct Progress(Arc<Inner>);

impl Progress {
    /// Creates a fresh, indeterminate progress handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the number of work units expected. Zero means "indeterminate".
    pub fn set_total(&self, total: u64) {
        self.0.total.store(total, Ordering::Relaxed);
    }

    /// Adds to the number of work units expected.
    pub fn add_total(&self, extra: u64) {
        self.0.total.fetch_add(extra, Ordering::Relaxed);
    }

    /// Marks `n` more work units as finished.
    pub fn inc(&self, n: u64) {
        self.0.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Overrides the number of finished work units.
    pub fn set_done(&self, done: u64) {
        self.0.done.store(done, Ordering::Relaxed);
    }

    /// Adds to the running byte counter (bytes found, hashed, freed, ...).
    pub fn add_bytes(&self, n: u64) {
        self.0.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Work units finished so far.
    pub fn done(&self) -> u64 {
        self.0.done.load(Ordering::Relaxed)
    }

    /// Work units expected in total (zero when unknown).
    pub fn total(&self) -> u64 {
        self.0.total.load(Ordering::Relaxed)
    }

    /// Value of the running byte counter.
    pub fn bytes(&self) -> u64 {
        self.0.bytes.load(Ordering::Relaxed)
    }

    /// Completed fraction in `0.0..=1.0`, or `None` while indeterminate.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total();
        if total == 0 {
            None
        } else {
            Some((self.done() as f64 / total as f64).clamp(0.0, 1.0) as f32)
        }
    }

    /// Sets the short status line shown to the user.
    pub fn set_message(&self, message: impl Into<String>) {
        if let Ok(mut guard) = self.0.message.lock() {
            *guard = message.into();
        }
    }

    /// Current status line.
    pub fn message(&self) -> String {
        self.0.message.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Requests cancellation. Engines notice at their next [`Progress::check`].
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
    }

    /// True once cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }

    /// Returns [`Error::Cancelled`] once cancellation has been requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Resets counters and message, keeping the handle shared.
    pub fn reset(&self) {
        self.0.done.store(0, Ordering::Relaxed);
        self.0.total.store(0, Ordering::Relaxed);
        self.0.bytes.store(0, Ordering::Relaxed);
        self.0.cancelled.store(false, Ordering::Relaxed);
        self.set_message(String::new());
    }
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Progress")
            .field("done", &self.done())
            .field("total", &self.total())
            .field("bytes", &self.bytes())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_and_cancel() {
        let p = Progress::new();
        assert_eq!(p.fraction(), None);
        p.set_total(4);
        p.inc(1);
        assert_eq!(p.fraction(), Some(0.25));
        assert!(p.check().is_ok());
        let clone = p.clone();
        clone.cancel();
        assert!(matches!(p.check(), Err(Error::Cancelled)));
        p.reset();
        assert!(!p.is_cancelled());
        assert_eq!(p.done(), 0);
    }
}
