//! COM apartment handling for worker threads.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use zest_core::{Error, Progress, Result};

/// Initialises COM on the current thread and undoes it when dropped.
///
/// A thread that was already initialised with a different apartment model is
/// accepted as-is: COM is usable there, it just must not be uninitialised by
/// us.
pub(crate) struct ComGuard {
    uninitialize: bool,
}

impl ComGuard {
    /// Joins the multithreaded apartment (or reuses the existing apartment).
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: plain COM initialisation; balanced by `Drop` only when this
        // call succeeded (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(ComGuard { uninitialize: true })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(ComGuard {
                uninitialize: false,
            })
        } else {
            Err(windows::core::Error::from(hr).into())
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninitialize {
            // SAFETY: balances the successful CoInitializeEx in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs `work` on a dedicated thread that owns its own COM apartment and
/// waits for the result.
///
/// COM interface pointers never leave that thread; `work` must return plain
/// data. With `abandon_on_cancel` the caller gets [`Error::Cancelled`] as soon
/// as cancellation is requested, while the worker is left to finish in the
/// background (only appropriate for read-only work). Without it the call
/// always waits and `work` is expected to check for cancellation itself.
pub(crate) fn run_on_com_thread<T, F>(
    name: &str,
    progress: &Progress,
    abandon_on_cancel: bool,
    work: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Progress) -> Result<T> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let worker_progress = progress.clone();
    std::thread::Builder::new()
        .name(format!("zest-{name}"))
        .spawn(move || {
            let result = ComGuard::new().and_then(|_com| work(&worker_progress));
            let _ = tx.send(result);
        })
        .map_err(|e| Error::other(format!("could not start background thread: {e}")))?;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {
                if abandon_on_cancel && progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::other("the background worker stopped unexpectedly"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_can_be_nested_and_repeated() {
        let outer = ComGuard::new().unwrap();
        let inner = ComGuard::new().unwrap();
        drop(inner);
        drop(outer);
        let again = ComGuard::new().unwrap();
        drop(again);
    }

    #[test]
    fn worker_returns_plain_data() {
        let progress = Progress::new();
        let value = run_on_com_thread("test", &progress, true, |p| {
            p.set_message("working");
            Ok(21 * 2)
        })
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(progress.message(), "working");
    }

    #[test]
    fn cancellation_abandons_read_only_work() {
        let progress = Progress::new();
        progress.cancel();
        let result: Result<()> = run_on_com_thread("test-cancel", &progress, true, |_| {
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        });
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn panicking_worker_is_reported_not_propagated() {
        let progress = Progress::new();
        let result: Result<()> =
            run_on_com_thread("test-panic", &progress, false, |_| panic!("boom"));
        assert!(result.is_err());
    }
}
