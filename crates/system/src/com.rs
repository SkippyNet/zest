//! COM apartment management for worker threads.

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Initialises COM on the current thread for as long as the guard lives.
///
/// The functions of this crate are called from arbitrary worker threads.
/// Some of those may already have joined an apartment (possibly a
/// single-threaded one); that is tolerated and left untouched. Declare the
/// guard *before* any COM object so it is dropped after them.
pub(crate) struct ComGuard {
    uninitialize: bool,
}

impl ComGuard {
    /// Joins the multithreaded apartment unless the thread already has one.
    pub(crate) fn new() -> Self {
        // SAFETY: plain COM initialisation; balanced in `Drop` only when this
        // call actually added a reference (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard {
            // RPC_E_CHANGED_MODE means the thread is already in a different
            // apartment, which works just as well for everything used here.
            uninitialize: hr.is_ok(),
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
