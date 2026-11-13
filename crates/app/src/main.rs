//! Zest – a fast, private PC cleaner and optimizer for Windows.
//!
//! This is the graphical application. The same executable also runs two
//! window-less modes: `--agent` (the notification-area agent behind smart
//! cleaning) and `--scheduled-clean` (started by the Task Scheduler). The
//! full command line interface lives in `zest-cli`.

#![cfg(windows)]
// No console window for the GUI.
#![windows_subsystem = "windows"]

mod agent;
mod app;
mod brand_icon;
mod demo;
mod instance;
mod job;
mod pages;
mod theme;
mod widgets;

use app::{App, Launch};
use eframe::egui;
use pages::Page;
use zest_engine::core::{brand, logging};
use zest_engine::{Settings, autostart, schedule};

/// Passed when the app restarts itself as administrator, so the new process
/// waits for the old one to close instead of handing over to it.
pub const RESTARTED_ARG: &str = "--restarted";

fn parse_args() -> Launch {
    let mut launch = Launch::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--page" => launch.page = args.next().and_then(|s| Page::from_slug(&s)),
            "--screenshots" => launch.screenshots = args.next().map(Into::into),
            "--scan" => launch.screenshot_scan = true,
            "--demo" => launch.demo = true,
            "--theme" => {
                launch.theme = match args.next().as_deref() {
                    Some("light") => Some(zest_engine::settings::Theme::Light),
                    Some("dark") => Some(zest_engine::settings::Theme::Dark),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    launch
}

/// Brings an already running main window to the front.
fn focus_existing_window() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    use windows::core::HSTRING;
    // SAFETY: plain window lookups; a missing window simply yields an error.
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, &HSTRING::from(brand::NAME)) {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == autostart::AGENT_ARG) {
        logging::init("agent", false);
        logging::log_panics();
        std::process::exit(agent::run());
    }
    if args.iter().any(|a| a == schedule::SCHEDULED_ARG) {
        logging::init("scheduled", false);
        logging::log_panics();
        std::process::exit(agent::scheduled_clean());
    }

    logging::init("zest", false);
    logging::log_panics();
    let launch = parse_args();

    // One main window per user session (screenshot runs are exempt so they
    // can be taken while the app is open).
    let _instance = if launch.screenshots.is_none() {
        let restarted = args.iter().any(|a| a == RESTARTED_ARG);
        let mut guard = instance::acquire(instance::APP_MUTEX);
        // After "Restart as administrator" the previous window needs a moment to close.
        let mut attempts = 0;
        while guard.is_none() && restarted && attempts < 50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            guard = instance::acquire(instance::APP_MUTEX);
            attempts += 1;
        }
        match guard {
            Some(guard) => Some(guard),
            None => {
                focus_existing_window();
                return Ok(());
            }
        }
    } else {
        None
    };

    let settings = Settings::load();
    let icon = egui::IconData {
        rgba: brand_icon::render(64),
        width: 64,
        height: 64,
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(brand::NAME)
        .with_app_id(brand::NAME)
        .with_icon(icon)
        .with_inner_size([settings.window.width, settings.window.height])
        .with_min_inner_size([980.0, 640.0]);
    if settings.window.maximized && launch.screenshots.is_none() {
        viewport = viewport.with_maximized(true);
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        brand::NAME,
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, launch)))),
    )
}
