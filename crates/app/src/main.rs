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
mod gpu;
mod instance;
mod job;
mod pages;
mod theme;
mod updater;
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

/// Tells the user why no window appeared. The GUI has no console, so a
/// message box is the only place left to say it.
fn show_fatal(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    log::error!("{message}");
    // SAFETY: both strings are valid, NUL-terminated wide strings for the call.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            &HSTRING::from(brand::NAME),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Starts the app again with Direct3D after OpenGL failed to initialise.
fn restart_with_direct3d(args: &[String]) -> std::io::Result<()> {
    let mut forwarded: Vec<&str> = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
        } else if arg == gpu::OVERRIDE_ARG {
            skip_value = true;
        } else if arg != RESTARTED_ARG {
            forwarded.push(arg);
        }
    }
    std::process::Command::new(std::env::current_exe()?)
        .args(forwarded)
        .args([
            gpu::OVERRIDE_ARG,
            gpu::Backend::Direct3d.name(),
            RESTARTED_ARG,
        ])
        .spawn()
        .map(|_| ())
}

fn main() {
    gpu::mark_start();
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
    let instance = if launch.screenshots.is_none() {
        let restarted = args.iter().any(|a| a == RESTARTED_ARG);
        let mut guard = instance::acquire(instance::APP_MUTEX);
        // After a restart (as administrator, or with other graphics) the
        // previous window needs a moment to close.
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
                return;
            }
        }
    } else {
        None
    };

    // Only the one real window clears away what a finished update of Zest
    // left behind; a second start must not disturb a download in progress.
    if instance.is_some() {
        zest_engine::updates::selfupdate::cleanup_after_update();
    }

    // Screenshot runs leave no trace, so they do not record what worked.
    let graphics = gpu::Plan::for_launch(&args, launch.screenshots.is_none());
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
        wgpu_options: graphics.wgpu_options(),
        ..Default::default()
    };
    let result = eframe::run_native(
        brand::NAME,
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, launch)))),
    );
    let Err(error) = result else {
        return;
    };

    if graphics.should_fall_back() {
        // OpenGL is missing or too old here (remote sessions, virtual
        // machines, PCs without a graphics driver). Direct3D always works.
        log::warn!("OpenGL could not be started ({error}); starting again with Direct3D");
        graphics.note_opengl_failed();
        // Let the new process take over the single-instance lock.
        drop(instance);
        match restart_with_direct3d(&args) {
            Ok(()) => return,
            Err(e) => log::error!("could not start again: {e}"),
        }
    }
    show_fatal(&format!(
        "{} could not open its window.\n\n{error}\n\nGraphics: {}. Starting with \"{} software\" draws without the graphics driver.",
        brand::NAME,
        graphics.backend().name(),
        gpu::OVERRIDE_ARG,
    ));
    std::process::exit(1);
}
