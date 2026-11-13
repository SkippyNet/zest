//! A hidden child-process runner that can be cancelled, timed out and
//! observed while it runs.
//!
//! `zest_core::process::run_capture` blocks until the child exits, which
//! is not acceptable for package upgrades that can take minutes. This runner
//! polls the child, forwards the latest output line to a callback and kills
//! the child on cancellation or timeout.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zest_core::{Error, IoContext, Progress, Result, process};

/// Captured result of a finished child process.
#[derive(Clone, Debug)]
pub(crate) struct ExecOutput {
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Decoded standard output.
    pub stdout: String,
    /// Decoded standard error.
    pub stderr: String,
}

fn pump(
    mut source: impl Read + Send + 'static,
    sink: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut guard) = sink.lock() {
                        guard.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    })
}

/// Runs `command` without a window.
///
/// * `progress` – polled for cancellation; the child is killed when set.
/// * `timeout` – the child is killed when it runs longer than this.
/// * `on_output` – called from the waiting thread with all output received
///   since the previous call (raw, possibly containing control sequences).
pub(crate) fn run_cancellable(
    mut command: Command,
    progress: &Progress,
    timeout: Option<Duration>,
    on_output: &mut dyn FnMut(&str),
) -> Result<ExecOutput> {
    let program = command.get_program().to_string_lossy().into_owned();
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().ctx(format!("starting {program}"))?;

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let pumps = [
        child.stdout.take().map(|s| pump(s, stdout.clone())),
        child.stderr.take().map(|s| pump(s, stderr.clone())),
    ];

    let started = Instant::now();
    let mut reported = 0usize;
    let mut forward = |reported: &mut usize| {
        let fresh = stdout.lock().ok().and_then(|guard| {
            (guard.len() > *reported).then(|| {
                let text = String::from_utf8_lossy(&guard[*reported..]).into_owned();
                *reported = guard.len();
                text
            })
        });
        if let Some(text) = fresh {
            on_output(&text);
        }
    };

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(e).ctx(format!("waiting for {program}"));
            }
        }
        if progress.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        if timeout.is_some_and(|limit| started.elapsed() > limit) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Command(format!(
                "{program} did not finish within {} seconds",
                timeout.unwrap_or_default().as_secs()
            )));
        }
        forward(&mut reported);
        std::thread::sleep(Duration::from_millis(80));
    };

    for handle in pumps.into_iter().flatten() {
        let _ = handle.join();
    }
    forward(&mut reported);

    let take = |buffer: &Arc<Mutex<Vec<u8>>>| {
        buffer
            .lock()
            .map(|guard| process::decode_console(&guard))
            .unwrap_or_default()
    };
    Ok(ExecOutput {
        code: status.code(),
        stdout: take(&stdout),
        stderr: take(&stderr),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(script: &str) -> Command {
        let mut command = process::hidden_command(process::system_tool("cmd.exe"));
        command.args(["/C", script]);
        command
    }

    #[test]
    fn captures_output_and_exit_code() {
        let mut seen = String::new();
        let out = run_cancellable(
            cmd("echo first& echo second& exit 7"),
            &Progress::new(),
            Some(Duration::from_secs(30)),
            &mut |text| seen.push_str(text),
        )
        .unwrap();
        assert_eq!(out.code, Some(7));
        assert!(out.stdout.contains("first") && out.stdout.contains("second"));
        assert!(seen.contains("second"), "callback saw: {seen:?}");
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn cancellation_kills_the_child() {
        let progress = Progress::new();
        let canceller = progress.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            canceller.cancel();
        });
        let started = Instant::now();
        // `ping -n 30` would run for about half a minute if left alone.
        let mut command = process::hidden_command(process::system_tool("ping.exe"));
        command.args(["-n", "30", "127.0.0.1"]);
        let result = run_cancellable(command, &progress, None, &mut |_| {});
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn timeout_kills_the_child() {
        let mut command = process::hidden_command(process::system_tool("ping.exe"));
        command.args(["-n", "30", "127.0.0.1"]);
        let result = run_cancellable(
            command,
            &Progress::new(),
            Some(Duration::from_millis(300)),
            &mut |_| {},
        );
        assert!(matches!(result, Err(Error::Command(ref m)) if m.contains("did not finish")));
    }

    #[test]
    fn missing_program_is_an_error() {
        let command = process::hidden_command(r"C:\zest-no-such-dir\nothing.exe");
        let result = run_cancellable(command, &Progress::new(), None, &mut |_| {});
        assert!(result.is_err());
    }
}
