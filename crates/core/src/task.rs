//! Background tasks with progress, cancellation and panic isolation.

use crate::Progress;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Slot<T> = Arc<Mutex<Option<std::result::Result<T, String>>>>;

/// A unit of work running on its own thread.
///
/// The owner polls [`Task::try_take`] (typically once per UI frame). A panic
/// inside the worker is caught and surfaced as an `Err(String)` rather than
/// taking the whole application down.
pub struct Task<T> {
    progress: Progress,
    slot: Slot<T>,
    started: Instant,
    finished_after: Arc<Mutex<Option<Duration>>>,
}

impl<T: Send + 'static> Task<T> {
    /// Spawns `work` on a new named thread.
    pub fn spawn(name: &str, work: impl FnOnce(Progress) -> T + Send + 'static) -> Self {
        Self::spawn_with_notify(name, work, || {})
    }

    /// Like [`Task::spawn`], additionally calling `notify` when the work ends
    /// (used by the GUI to request a repaint).
    pub fn spawn_with_notify(
        name: &str,
        work: impl FnOnce(Progress) -> T + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
    ) -> Self {
        let progress = Progress::new();
        let slot: Slot<T> = Arc::new(Mutex::new(None));
        let finished_after = Arc::new(Mutex::new(None));
        let started = Instant::now();

        let thread_progress = progress.clone();
        let thread_slot = slot.clone();
        let thread_finished = finished_after.clone();
        let thread_name = name.to_owned();
        let spawn_result = std::thread::Builder::new()
            .name(format!("zest-{name}"))
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    work(thread_progress)
                }))
                .map_err(|payload| {
                    let msg = panic_message(payload.as_ref());
                    log::error!("task '{thread_name}' panicked: {msg}");
                    format!("internal error: {msg}")
                });
                if let Ok(mut guard) = thread_finished.lock() {
                    *guard = Some(started.elapsed());
                }
                if let Ok(mut guard) = thread_slot.lock() {
                    *guard = Some(outcome);
                }
                notify();
            });

        if let Err(e) = spawn_result
            && let Ok(mut guard) = slot.lock()
        {
            *guard = Some(Err(format!("could not start background thread: {e}")));
        }

        Task {
            progress,
            slot,
            started,
            finished_after,
        }
    }

    /// True once the result is ready to be taken.
    pub fn is_finished(&self) -> bool {
        self.slot.lock().map(|g| g.is_some()).unwrap_or(true)
    }

    /// Takes the result if the work has finished. Returns `None` while it is
    /// still running (and after the result was already taken).
    pub fn try_take(&mut self) -> Option<std::result::Result<T, String>> {
        self.slot.lock().ok()?.take()
    }

    /// Blocks until the work has finished and returns its result.
    pub fn join(mut self) -> std::result::Result<T, String> {
        loop {
            if let Some(result) = self.try_take() {
                return result;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Progress handle shared with the worker.
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Requests cancellation of the work.
    pub fn cancel(&self) {
        self.progress.cancel();
    }

    /// Time since the task started (frozen once it finishes).
    pub fn elapsed(&self) -> Duration {
        self.finished_after
            .lock()
            .ok()
            .and_then(|g| *g)
            .unwrap_or_else(|| self.started.elapsed())
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_value() {
        let task = Task::spawn("test", |p| {
            p.set_total(1);
            p.inc(1);
            41 + 1
        });
        assert_eq!(task.join(), Ok(42));
    }

    #[test]
    fn panic_becomes_error() {
        let task: Task<()> = Task::spawn("test-panic", |_| panic!("boom"));
        let err = task.join().unwrap_err();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn cancellation_is_visible_to_worker() {
        let task = Task::spawn("test-cancel", |p| {
            while !p.is_cancelled() {
                std::thread::sleep(Duration::from_millis(2));
            }
            "stopped"
        });
        task.cancel();
        assert_eq!(task.join(), Ok("stopped"));
    }
}
