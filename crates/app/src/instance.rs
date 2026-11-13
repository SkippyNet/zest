//! Named mutexes that tell the app's processes about each other.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::core::HSTRING;

/// Held by the main window while it is open.
pub const APP_MUTEX: &str = "Local\\ZestAppRunning";
/// Held by the background agent.
pub const AGENT_MUTEX: &str = "Local\\ZestAgentRunning";

/// Releases the mutex when dropped.
pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateMutexW and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Claims `name` for this process. Returns `None` when another process of
/// this user session already holds it.
pub fn acquire(name: &str) -> Option<Guard> {
    // SAFETY: the name is a valid NUL terminated string for the call.
    unsafe {
        let handle = CreateMutexW(None, false, &HSTRING::from(name)).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return None;
        }
        Some(Guard(handle))
    }
}

/// True when some process holds `name`.
pub fn exists(name: &str) -> bool {
    // SAFETY: the handle, when one is returned, is closed immediately.
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}
