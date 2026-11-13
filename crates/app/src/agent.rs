//! The background agent: a notification-area icon that carries out smart
//! cleaning and keeps sleep rules applied while the main window is closed.
//!
//! It is the same executable started with `--agent`. It has no window of its
//! own, uses no graphics stack and spends its time waiting on a five second
//! timer.

use std::cell::RefCell;
use std::time::SystemTime;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, IDI_APPLICATION, LoadIconW, MF_SEPARATOR,
    MF_STRING, MSG, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
    SetTimer, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
    WINDOW_EX_STYLE, WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};
use zest_engine::cleaner::scan;
use zest_engine::core::fmt::format_bytes;
use zest_engine::core::{Progress, brand};
use zest_engine::performance::control::default_journal_path;
use zest_engine::performance::{ControlState, Sampler, SleepRules, apply_rules};
use zest_engine::settings::SmartAction;
use zest_engine::smart::{self, SmartEvent, SmartMonitor};
use zest_engine::{RunKind, Settings, ops};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const TIMER_ID: usize = 1;
const TICK_MS: u32 = 5000;
/// Sent by the shell when the user clicks a notification balloon.
const NIN_BALLOONUSERCLICK: u32 = 0x0405;

const MENU_OPEN: usize = 1;
const MENU_CLEAN: usize = 2;
const MENU_EXIT: usize = 3;

/// What clicking the current notification does.
#[derive(Clone)]
enum ClickAction {
    OpenClean,
    CleanBrowser(String),
}

struct Agent {
    hwnd: HWND,
    settings: Settings,
    settings_stamp: Option<SystemTime>,
    monitor: SmartMonitor,
    sampler: Sampler,
    control: ControlState,
    on_click: Option<ClickAction>,
    busy: bool,
    taskbar_created: u32,
}

thread_local! {
    static AGENT: RefCell<Option<Agent>> = const { RefCell::new(None) };
}

fn wide(text: &str, out: &mut [u16]) {
    let units: Vec<u16> = text.encode_utf16().take(out.len() - 1).collect();
    out[..units.len()].copy_from_slice(&units);
    out[units.len()] = 0;
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

fn add_icon(hwnd: HWND) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = TRAY_MESSAGE;
    // SAFETY: plain Win32 calls with valid arguments. Resource id 1 is the
    // icon embedded by the build script; the stock icon is the fallback.
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        data.hIcon = LoadIconW(Some(module.into()), PCWSTR(std::ptr::without_provenance(1)))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
        wide(
            &format!("{} is watching for junk", brand::NAME),
            &mut data.szTip,
        );
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

fn notify(hwnd: HWND, title: &str, message: &str) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_INFO;
    wide(title, &mut data.szInfoTitle);
    wide(message, &mut data.szInfo);
    // SAFETY: `data` is fully initialised and refers to our own icon.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn open_app(page: Option<&str>) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(exe);
    if let Some(page) = page {
        command.args(["--page", page]);
    }
    let _ = command.spawn();
}

/// True while the main window is open (it holds a named mutex).
fn app_is_open() -> bool {
    crate::instance::exists(crate::instance::APP_MUTEX)
}

impl Agent {
    fn reload_settings(&mut self) {
        let stamp = std::fs::metadata(Settings::path())
            .and_then(|m| m.modified())
            .ok();
        if stamp != self.settings_stamp {
            self.settings_stamp = stamp;
            self.settings = Settings::load();
        }
    }

    fn clean(
        &mut self,
        only: Option<std::collections::HashSet<String>>,
        junk_only: bool,
    ) -> (u64, u64) {
        let report = match only {
            Some(rules) => {
                let analysis = ops::analyze(&self.settings, Some(&rules), &Progress::new());
                ops::run_clean(&analysis, RunKind::Smart, false, &Progress::new())
            }
            None => ops::auto_clean(&self.settings, RunKind::Smart, junk_only, &Progress::new()),
        };
        self.monitor.note_cleaned();
        (report.bytes, report.items)
    }

    fn tick(&mut self) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.reload_settings();
        if !self.settings.smart.enabled {
            // Switched off in Options: leave quietly.
            // SAFETY: destroying our own window from its own thread.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            self.busy = false;
            return;
        }

        // Sleep rules: applied here only while the main window is closed, so
        // the two never manage the same processes at once.
        let rules: SleepRules = self.settings.extra("sleep_rules");
        if !rules.is_empty()
            && !app_is_open()
            && let Ok(snapshot) = self.sampler.sample()
        {
            apply_rules(&rules, &snapshot, &mut self.control);
        }

        for event in self.monitor.tick(&self.settings) {
            match event {
                SmartEvent::JunkOverThreshold { bytes, .. } => {
                    match self.settings.smart.junk_action {
                        SmartAction::Clean => {
                            let (freed, _) = self.clean(None, true);
                            notify(
                                self.hwnd,
                                brand::NAME,
                                &format!("Cleaned up {} of junk.", format_bytes(freed)),
                            );
                            self.on_click = None;
                        }
                        SmartAction::Notify => {
                            notify(
                                self.hwnd,
                                brand::NAME,
                                &format!(
                                    "{} of junk can be removed. Click to review it.",
                                    format_bytes(bytes)
                                ),
                            );
                            self.on_click = Some(ClickAction::OpenClean);
                        }
                        SmartAction::Off => {}
                    }
                }
                SmartEvent::BrowserClosed { group_id, name } => {
                    let rules = smart::browser_rules(&self.settings, &group_id);
                    if rules.is_empty() {
                        continue;
                    }
                    match self.settings.smart.browser_action {
                        SmartAction::Clean => {
                            let (freed, _) = self.clean(Some(rules), false);
                            notify(
                                self.hwnd,
                                brand::NAME,
                                &format!("{name} cleaned: {} freed.", format_bytes(freed)),
                            );
                            self.on_click = None;
                        }
                        SmartAction::Notify => {
                            // Only offer when there is something to clean.
                            let analysis =
                                ops::analyze(&self.settings, Some(&rules), &Progress::new());
                            if analysis.scan.items > 0 {
                                notify(
                                    self.hwnd,
                                    brand::NAME,
                                    &format!(
                                        "{name} was closed. Click to clean {} of its data.",
                                        format_bytes(analysis.scan.bytes)
                                    ),
                                );
                                self.on_click = Some(ClickAction::CleanBrowser(group_id));
                            }
                        }
                        SmartAction::Off => {}
                    }
                }
            }
        }
        self.busy = false;
    }

    fn balloon_clicked(&mut self) {
        match self.on_click.take() {
            Some(ClickAction::OpenClean) => open_app(Some("clean")),
            Some(ClickAction::CleanBrowser(group_id)) => {
                let rules = smart::browser_rules(&self.settings, &group_id);
                let (freed, _) = self.clean(Some(rules), false);
                notify(
                    self.hwnd,
                    brand::NAME,
                    &format!("{} freed.", format_bytes(freed)),
                );
            }
            None => {}
        }
    }

    fn show_menu(&mut self) {
        // SAFETY: standard popup-menu sequence on our own window; the menu
        // handle is destroyed before returning.
        let choice = unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open Zest"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLEAN, w!("Clean junk now"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Stop watching"));
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            // Required so the menu closes when the user clicks elsewhere.
            let _ = SetForegroundWindow(self.hwnd);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                point.x,
                point.y,
                Some(0),
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            choice.0 as usize
        };
        match choice {
            MENU_OPEN => open_app(None),
            MENU_CLEAN => {
                let (bytes, _) = smart::junk_size(&self.settings);
                if bytes == 0 {
                    notify(
                        self.hwnd,
                        brand::NAME,
                        "There is no junk to remove right now.",
                    );
                } else {
                    let (freed, _) = self.clean(None, true);
                    notify(
                        self.hwnd,
                        brand::NAME,
                        &format!("{} freed.", format_bytes(freed)),
                    );
                }
            }
            MENU_EXIT => {
                // SAFETY: destroying our own window from its own thread.
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
                }
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = AGENT.with(|cell| {
        // A nested message (for example from the menu loop) must not borrow twice.
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(agent) = guard.as_mut() else {
            return false;
        };
        match message {
            WM_TIMER if wparam.0 == TIMER_ID => {
                agent.tick();
                true
            }
            TRAY_MESSAGE => {
                match lparam.0 as u32 & 0xFFFF {
                    WM_LBUTTONUP => open_app(None),
                    WM_RBUTTONUP => agent.show_menu(),
                    NIN_BALLOONUSERCLICK => agent.balloon_clicked(),
                    _ => {}
                }
                true
            }
            m if m == agent.taskbar_created && m != 0 => {
                // Explorer restarted: put the icon back.
                add_icon(hwnd);
                true
            }
            _ => false,
        }
    });
    if handled {
        return LRESULT(0);
    }
    match message {
        WM_DESTROY => {
            // SAFETY: removing our own icon and ending our own message loop.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_COMMAND => LRESULT(0),
        // SAFETY: forwarding unhandled messages is the documented contract.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Runs the agent until the user stops it or smart cleaning is switched off.
/// Returns the process exit code.
pub fn run() -> i32 {
    // One agent per user session.
    let Some(_guard) = crate::instance::acquire(crate::instance::AGENT_MUTEX) else {
        return 0;
    };
    let settings = Settings::load();
    if !settings.smart.enabled {
        return 0;
    }
    log::info!("agent started");

    // SAFETY: registers a window class and creates a hidden window owned by
    // this thread; all strings are static and NUL terminated.
    let hwnd = unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: w!("ZestAgentWindow"),
            ..Default::default()
        };
        RegisterClassW(&class);
        match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ZestAgentWindow"),
            w!("Zest agent"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(e) => {
                log::error!("agent window could not be created: {e}");
                return 1;
            }
        }
    };

    add_icon(hwnd);
    // SAFETY: registering a well-known shell message name.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    AGENT.with(|cell| {
        *cell.borrow_mut() = Some(Agent {
            hwnd,
            settings,
            settings_stamp: std::fs::metadata(Settings::path())
                .and_then(|m| m.modified())
                .ok(),
            monitor: SmartMonitor::new(),
            sampler: Sampler::new(),
            control: ControlState::with_journal(default_journal_path("agent")),
            on_click: None,
            busy: false,
            taskbar_created,
        });
    });

    // SAFETY: a standard message loop for the window created above.
    unsafe {
        SetTimer(Some(hwnd), TIMER_ID, TICK_MS, None);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // Dropping the agent wakes anything it put to sleep.
    AGENT.with(|cell| cell.borrow_mut().take());
    log::info!("agent stopped");
    0
}

/// Unattended clean used by the scheduled task. Returns the exit code.
pub fn scheduled_clean() -> i32 {
    let settings = Settings::load();
    let catalog = ops::catalog(&settings);
    let options = ops::clean_options(&settings, &catalog);
    let found = scan(&catalog, &options, &Progress::new());
    let analysis = ops::Analysis {
        catalog,
        options,
        scan: found,
    };
    let report = ops::run_clean(&analysis, RunKind::Scheduled, false, &Progress::new());
    log::info!(
        "scheduled clean: {} freed, {} items, {} in use, {} failed",
        format_bytes(report.bytes),
        report.items,
        report.in_use,
        report.failed
    );
    0
}
