//! A tiny file logger (no dependencies beyond `log`).
//!
//! Logs go to `<data dir>\logs\<component>.log`. The file is restarted once it
//! grows past [`MAX_LOG_BYTES`], keeping one previous generation.

use crate::paths;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Size at which the log file is rotated.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

struct FileLogger {
    file: Mutex<Option<File>>,
    level: log::LevelFilter,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Third-party crates (graphics stack and friends) are chatty at info
        // level; keep only their warnings and errors.
        if record.level() > log::Level::Warn && !record.target().starts_with("zest") {
            return;
        }
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        if let Some(file) = guard.as_mut() {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let _ = writeln!(
                file,
                "{now} {:<5} [{}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.flush();
        }
    }
}

/// Routes panics into the log (in addition to the default handler), so a
/// crash of a windowed program, which has no console, still leaves a trace.
pub fn log_panics() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_owned());
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        log::error!("panic at {location}: {message}");
        log::logger().flush();
        default_hook(info);
    }));
}

/// Path of the log file for `component`.
pub fn log_path(component: &str) -> PathBuf {
    paths::logs_dir().join(format!("{component}.log"))
}

/// Installs the file logger. Safe to call more than once; later calls are
/// ignored. Failure to open the file disables logging rather than erroring.
pub fn init(component: &str, verbose: bool) {
    let path = log_path(component);
    let file = paths::ensure_dir(&paths::logs_dir()).ok().and_then(|_| {
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
            let _ = std::fs::rename(&path, path.with_extension("old.log"));
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
    });
    let level = if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let logger = FileLogger {
        file: Mutex::new(file),
        level,
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
        log::info!(
            "{} {} started ({component}, elevated: {})",
            crate::brand::NAME,
            crate::brand::VERSION,
            crate::elevation::is_elevated()
        );
    }
}
