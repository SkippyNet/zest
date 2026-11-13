//! A background job owned by a page: a [`Task`] plus the glue to repaint the
//! window when it reports progress or finishes.

use eframe::egui;
use std::time::Duration;
use zest_engine::core::{Progress, Task};

/// At most one running background task and its result hand-over.
pub struct Job<T> {
    task: Option<Task<T>>,
}

impl<T> Default for Job<T> {
    fn default() -> Self {
        Job { task: None }
    }
}

impl<T: Send + 'static> Job<T> {
    /// Starts `work` on a background thread. A job that is already running
    /// is left alone (the new request is ignored).
    pub fn start(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        work: impl FnOnce(Progress) -> T + Send + 'static,
    ) {
        if self.task.is_some() {
            return;
        }
        let repaint = ctx.clone();
        self.task = Some(Task::spawn_with_notify(name, work, move || {
            repaint.request_repaint()
        }));
        ctx.request_repaint();
    }

    /// True while the work is in flight.
    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    /// Progress of the running work.
    pub fn progress(&self) -> Option<&Progress> {
        self.task.as_ref().map(Task::progress)
    }

    /// Time since the job started.
    pub fn elapsed(&self) -> Duration {
        self.task.as_ref().map(Task::elapsed).unwrap_or_default()
    }

    /// Asks the running work to stop.
    pub fn cancel(&self) {
        if let Some(task) = &self.task {
            task.cancel();
        }
    }

    /// Returns the result exactly once, when the work has finished. While it
    /// is still running this keeps the window repainting so progress shows.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Result<T, String>> {
        let task = self.task.as_mut()?;
        match task.try_take() {
            Some(result) => {
                self.task = None;
                Some(result)
            }
            None => {
                ctx.request_repaint_after(Duration::from_millis(50));
                None
            }
        }
    }
}
