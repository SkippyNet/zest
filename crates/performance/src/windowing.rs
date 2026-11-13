//! Private window enumeration: which processes own application windows, and
//! which process the user is working with right now.

use crate::monitor::WindowState;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow,
    GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, PostMessageW, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

/// Result of one pass over the desktop's top-level windows.
#[derive(Clone, Debug, Default)]
pub(crate) struct WindowScan {
    /// Best window state per process id (absent = no application window).
    pub states: HashMap<u32, WindowState>,
    /// Process ids behind the foreground window (usually one).
    pub foreground_pids: Vec<u32>,
}

/// Enumeration callback that appends each window to the `Vec<HWND>` whose
/// address is passed in `lparam`.
///
/// # Safety
///
/// Must only be handed to `EnumWindows`/`EnumChildWindows` together with a
/// pointer to a live `Vec<HWND>` as the `LPARAM`.
unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the address of the `Vec<HWND>` owned by the caller
    // of EnumWindows/EnumChildWindows, which outlives the enumeration.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    BOOL(1)
}

fn top_level_windows() -> Vec<HWND> {
    let mut list: Vec<HWND> = Vec::with_capacity(512);
    // SAFETY: the callback only touches `list`, which lives until the call returns.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut list as *mut Vec<HWND> as isize));
    }
    list
}

fn child_windows(parent: HWND) -> Vec<HWND> {
    let mut list: Vec<HWND> = Vec::new();
    // SAFETY: as in `top_level_windows`.
    unsafe {
        let _ = EnumChildWindows(
            Some(parent),
            Some(collect),
            LPARAM(&mut list as *mut Vec<HWND> as isize),
        );
    }
    list
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out pointer; an invalid window simply yields pid 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 96];
    // SAFETY: the buffer length is passed implicitly through the slice.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.clamp(0, buf.len() as i32) as usize])
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: the attribute is a 4-byte integer and the buffer is exactly that.
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    }
    .is_ok();
    ok && cloaked != 0
}

/// Maps the placeholder window Windows shows for a frozen program back to
/// the program's real window. The export is resolved at run time because it
/// is not part of the documented API surface.
fn hung_window_from_ghost(ghost: HWND) -> Option<HWND> {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{s, w};
    type HungWindowFromGhostWindow = unsafe extern "system" fn(HWND) -> HWND;
    static ENTRY: OnceLock<Option<usize>> = OnceLock::new();

    let address = *ENTRY.get_or_init(|| {
        // SAFETY: user32.dll is always loaded in a process that enumerates
        // windows; the lookup has no side effects.
        unsafe {
            let module = GetModuleHandleW(w!("user32.dll")).ok()?;
            GetProcAddress(module, s!("HungWindowFromGhostWindow")).map(|f| f as usize)
        }
    });
    let address = address?;
    // SAFETY: the export has the signature `HWND (HWND)`; the address was
    // obtained from the loader for exactly this export.
    let real = unsafe {
        let func: HungWindowFromGhostWindow = std::mem::transmute(address);
        func(ghost)
    };
    (!real.0.is_null()).then_some(real)
}

/// The process ids an application window really belongs to.
///
/// Packaged apps draw inside a frame owned by a shared host process; the app
/// itself owns a child window. Frozen programs are represented by a ghost
/// window owned by the compositor.
fn owner_pids(hwnd: HWND) -> Vec<u32> {
    let base = window_pid(hwnd);
    match class_name(hwnd).as_str() {
        "ApplicationFrameWindow" => {
            let mut hosted: Vec<u32> = child_windows(hwnd)
                .into_iter()
                .map(window_pid)
                .filter(|&pid| pid != 0 && pid != base)
                .collect();
            hosted.sort_unstable();
            hosted.dedup();
            if hosted.is_empty() {
                vec![base]
            } else {
                hosted
            }
        }
        "Ghost" => match hung_window_from_ghost(hwnd) {
            Some(real) => vec![window_pid(real)],
            None => vec![base],
        },
        _ => vec![base],
    }
    .into_iter()
    .filter(|&pid| pid != 0)
    .collect()
}

fn is_app_window(hwnd: HWND) -> bool {
    // SAFETY: plain queries on a window handle; stale handles return zero.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null());
        if owned && ex_style & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        GetWindowTextLengthW(hwnd) > 0
    }
}

/// Scans the desktop once.
pub(crate) fn scan() -> WindowScan {
    let mut scan = WindowScan::default();
    // SAFETY: no arguments; returns null when no window has the focus.
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.0.is_null() {
        scan.foreground_pids = owner_pids(foreground);
    }

    for hwnd in top_level_windows() {
        if !is_app_window(hwnd) {
            continue;
        }
        // SAFETY: plain query on a window handle.
        let minimized = unsafe { IsIconic(hwnd) }.as_bool();
        let state = if hwnd == foreground {
            WindowState::Foreground
        } else if minimized || is_cloaked(hwnd) {
            WindowState::Background
        } else {
            WindowState::Visible
        };
        for pid in owner_pids(hwnd) {
            let slot = scan.states.entry(pid).or_insert(WindowState::None);
            if state > *slot {
                *slot = state;
            }
        }
    }

    // The foreground window may be a popup that is not an "app window".
    for pid in &scan.foreground_pids {
        scan.states.insert(*pid, WindowState::Foreground);
    }
    scan
}

/// Asks every top-level window of the given processes to close, the same way
/// clicking the window's close button would. Returns how many were asked.
pub(crate) fn request_close(pids: &HashSet<u32>) -> usize {
    let mut asked = 0;
    for hwnd in top_level_windows() {
        if pids.contains(&window_pid(hwnd)) {
            // SAFETY: posting a message never blocks; a stale handle fails harmlessly.
            if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok() {
                asked += 1;
            }
        }
    }
    asked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_is_consistent() {
        let scan = scan();
        // Every foreground pid is reported in the foreground state.
        for pid in &scan.foreground_pids {
            assert_eq!(scan.states.get(pid), Some(&WindowState::Foreground));
        }
        assert!(scan.states.values().all(|s| *s != WindowState::None));
        assert!(!scan.states.contains_key(&0));
    }

    #[test]
    fn closing_nothing_asks_nobody() {
        assert_eq!(request_close(&HashSet::new()), 0);
        // A pid that owns no windows.
        assert_eq!(request_close(&HashSet::from([0x7FFF_FFF0])), 0);
    }
}
