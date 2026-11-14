//! Running a [`Tool`]: hidden, with its output captured as it arrives.

use super::decode::{StreamDecoder, Transcript};
use super::{Action, ExitCodes, Tool};
use serde::Serialize;
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use zest_core::{Error, IoContext, Progress, Result, elevation, process};

/// How a finished tool run is to be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Verdict {
    /// Finished without problems.
    Ok,
    /// Finished; a restart completes it.
    RestartNeeded,
    /// Finished, and found something the user should look at.
    Attention,
    /// Windows refused because administrator rights are missing.
    NeedsAdmin,
    /// The tool reported an error.
    Failed,
    /// Stopped by the user.
    Cancelled,
}

/// The result of running a tool to the end.
#[derive(Clone, Debug, Serialize)]
pub struct ToolRun {
    pub verdict: Verdict,
    /// Exit code, when the tool exited by itself.
    pub code: Option<i32>,
    /// One sentence that says how it went.
    pub summary: String,
    /// Everything the tool wrote.
    pub output: String,
    /// How long it ran.
    pub elapsed: Duration,
}

/// Runs a command tool until it exits and returns what it reported.
///
/// `on_output` is called whenever new output has arrived, with the log so
/// far; `progress` receives the percentage the tool prints and can cancel
/// the run. Tools that need administrator rights are not even started
/// without them. Consoles are not run this way; see [`open`].
pub fn run(
    tool: &Tool,
    progress: &Progress,
    on_output: &mut dyn FnMut(&Transcript),
) -> Result<ToolRun> {
    run_as(tool, elevation::is_elevated(), progress, on_output)
}

/// [`run`] with the elevation state passed in.
pub(crate) fn run_as(
    tool: &Tool,
    elevated: bool,
    progress: &Progress,
    on_output: &mut dyn FnMut(&Transcript),
) -> Result<ToolRun> {
    let Action::Command { codes, .. } = tool.action else {
        return Err(Error::Invalid(format!(
            "{} opens a window and cannot be run for output",
            tool.title
        )));
    };
    if tool.needs_admin && !elevated {
        return Err(Error::NeedsElevation(format!(
            "run \u{201c}{}\u{201d}",
            tool.title
        )));
    }
    if !tool.is_available() {
        return Err(Error::NotFound(tool.program().display().to_string()));
    }
    log::info!("running Windows tool: {}", tool.command_line());
    let finished = run_program(
        &tool.program(),
        &tool.arguments(),
        codes,
        progress,
        on_output,
    )?;
    log::info!(
        "{} finished: {:?} (exit code {:?}) after {:.0?}",
        tool.id,
        finished.verdict,
        finished.code,
        finished.elapsed
    );
    Ok(finished)
}

enum Piece {
    Out(Vec<u8>),
    Err(Vec<u8>),
}

fn pump(
    mut source: impl Read + Send + 'static,
    wrap: fn(Vec<u8>) -> Piece,
    to: mpsc::Sender<Piece>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to.send(wrap(buffer[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// How long output may keep trickling in after the tool itself has exited
/// (a helper process it started can hold the pipe open).
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(400);

/// Runs any program hidden, capturing and decoding its output.
pub(crate) fn run_program<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    codes: ExitCodes,
    progress: &Progress,
    on_output: &mut dyn FnMut(&Transcript),
) -> Result<ToolRun> {
    let started = Instant::now();
    let mut child: Child = process::hidden_command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ctx(format!("starting {}", program.display()))?;

    // Helper processes the tool starts are stopped together with it.
    let group = ProcessGroup::around(&child);

    let (sender, receiver) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        pump(stdout, Piece::Out, sender.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        pump(stderr, Piece::Err, sender.clone());
    }
    drop(sender);

    let mut out = StreamDecoder::default();
    let mut err = StreamDecoder::default();
    let mut log = Transcript::default();
    let mut cancelled = false;
    let mut exited_at: Option<Instant> = None;
    progress.set_total(1000);

    loop {
        match receiver.recv_timeout(Duration::from_millis(80)) {
            Ok(piece) => {
                let text = match piece {
                    Piece::Out(bytes) => out.push(&bytes),
                    Piece::Err(bytes) => err.push(&bytes),
                };
                if !text.is_empty() {
                    log.push_str(&text);
                    if let Some(fraction) = log.fraction() {
                        progress.set_done((fraction * 1000.0) as u64);
                    }
                    on_output(&log);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Both pipes are closed: nothing more can arrive.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if progress.is_cancelled() && !cancelled {
            cancelled = true;
            if let Some(group) = &group {
                group.terminate();
            }
            let _ = child.kill();
        }
        if exited_at.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            exited_at = Some(Instant::now());
        }
        if exited_at.is_some_and(|at| at.elapsed() > DRAIN_AFTER_EXIT) {
            break;
        }
    }
    let status = child
        .wait()
        .ctx(format!("waiting for {}", program.display()))?;

    let rest = out.finish() + &err.finish();
    if !rest.is_empty() {
        log.push_str(&rest);
        on_output(&log);
    }

    let code = status.code();
    let (verdict, summary) = if cancelled {
        (Verdict::Cancelled, "Stopped before it finished.".to_owned())
    } else {
        interpret(codes, code)
    };
    if matches!(verdict, Verdict::Ok | Verdict::RestartNeeded) {
        progress.set_done(1000);
    }
    Ok(ToolRun {
        verdict,
        code,
        summary,
        output: log.text(),
        elapsed: started.elapsed(),
    })
}

/// A job object holding a tool and every process it starts.
struct ProcessGroup(windows::Win32::Foundation::HANDLE);

impl ProcessGroup {
    /// Puts `child` into a new group. `None` when Windows refuses, in which
    /// case only the tool itself can be stopped.
    fn around(child: &Child) -> Option<ProcessGroup> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        use windows::core::PCWSTR;
        // SAFETY: the child handle is valid while `child` is alive, and the
        // job handle is closed exactly once (here or in `drop`).
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            if AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
                let _ = CloseHandle(job);
                return None;
            }
            Some(ProcessGroup(job))
        }
    }

    /// Ends every process in the group.
    fn terminate(&self) {
        use windows::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the handle is a job object owned by `self`.
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Closing the handle leaves processes that are still running alone.
        // SAFETY: the handle is owned by `self` and not used afterwards.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

// Windows error codes the tools pass through as exit codes.
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_ELEVATION_REQUIRED: i32 = 740;
const ERROR_CANCELLED: i32 = 1223;
const ERROR_SUCCESS_REBOOT_REQUIRED: i32 = 3010;
/// `CBS_E_SOURCE_MISSING`: the files needed for a repair could not be found.
const CBS_E_SOURCE_MISSING: i32 = 0x800F_081Fu32 as i32;

/// Reads an exit code the way the tool documents it.
pub(crate) fn interpret(codes: ExitCodes, code: Option<i32>) -> (Verdict, String) {
    let Some(code) = code else {
        return (
            Verdict::Failed,
            "The tool stopped without reporting a result.".to_owned(),
        );
    };
    let generic_failure = || {
        (
            Verdict::Failed,
            format!(
                "The tool reported error {code} (0x{:08X}). Its output has the details.",
                code as u32
            ),
        )
    };
    match codes {
        ExitCodes::Plain => match code {
            0 => (Verdict::Ok, "Finished.".to_owned()),
            ERROR_ACCESS_DENIED | ERROR_ELEVATION_REQUIRED => needs_admin(),
            _ => generic_failure(),
        },
        ExitCodes::Dism => match code {
            0 => (Verdict::Ok, "Finished.".to_owned()),
            ERROR_SUCCESS_REBOOT_REQUIRED => (
                Verdict::RestartNeeded,
                "Finished. Restart the PC to complete it.".to_owned(),
            ),
            ERROR_ACCESS_DENIED | ERROR_ELEVATION_REQUIRED => needs_admin(),
            ERROR_INVALID_PARAMETER => (
                Verdict::Failed,
                "This version of Windows does not accept the command (error 87).".to_owned(),
            ),
            ERROR_CANCELLED => (Verdict::Cancelled, "Stopped before it finished.".to_owned()),
            CBS_E_SOURCE_MISSING => (
                Verdict::Failed,
                "Windows could not find the files it needs for the repair. Check the internet connection and try again.".to_owned(),
            ),
            _ => generic_failure(),
        },
        // https://learn.microsoft.com/windows-server/administration/windows-commands/chkdsk
        ExitCodes::Chkdsk => match code {
            0 => (Verdict::Ok, "No errors were found.".to_owned()),
            1 => (Verdict::Ok, "Errors were found and fixed.".to_owned()),
            2 => (
                Verdict::Ok,
                "Finished. Some clean-up was done, or skipped because this was only a scan."
                    .to_owned(),
            ),
            3 => (
                Verdict::Attention,
                "The drive could not be checked, or it has errors this scan does not fix. The output has the details.".to_owned(),
            ),
            _ => generic_failure(),
        },
        // sfc does not document its exit codes; what it found is in the text.
        ExitCodes::Sfc => match code {
            0 => (
                Verdict::Ok,
                "Finished. The output says what was found.".to_owned(),
            ),
            ERROR_ACCESS_DENIED | ERROR_ELEVATION_REQUIRED => needs_admin(),
            _ => (
                Verdict::Attention,
                format!("Finished with code {code}. The output says what was found."),
            ),
        },
    }
}

fn needs_admin() -> (Verdict, String) {
    (
        Verdict::NeedsAdmin,
        "Windows refused: this tool needs administrator rights.".to_owned(),
    )
}

/// Opens a console tool for the user and returns at once.
///
/// Windows shows its own consent prompt for consoles that need
/// administrator rights; declining it yields [`Error::Cancelled`].
pub fn open(tool: &Tool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    if !matches!(tool.action, Action::Open { .. }) {
        return Err(Error::Invalid(format!(
            "{} is run for its output, not opened",
            tool.title
        )));
    }
    if !tool.is_available() {
        return Err(Error::NotFound(tool.program().display().to_string()));
    }
    let wide = |s: &OsStr| -> Vec<u16> { s.encode_wide().chain(Some(0)).collect() };
    let verb = wide(OsStr::new("open"));
    let file = wide(tool.program().as_os_str());
    let arguments = wide(OsStr::new(&elevation::join_args(&tool.arguments())));
    let folder = wide(zest_core::paths::system32_dir().as_os_str());
    log::info!("opening Windows console: {}", tool.command_line());
    // SAFETY: all strings are NUL terminated and outlive the call.
    let instance = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(arguments.as_ptr()),
            PCWSTR(folder.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute reports success with a value greater than 32.
    let code = instance.0 as usize;
    if code > 32 {
        Ok(())
    } else if code == 5 {
        // SE_ERR_ACCESSDENIED: the consent prompt was declined.
        Err(Error::Cancelled)
    } else {
        Err(Error::Windows(format!(
            "could not open {} (code {code})",
            tool.title
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn cmd() -> std::path::PathBuf {
        process::system_tool("cmd.exe")
    }

    fn run_cmd(script: &str, codes: ExitCodes) -> ToolRun {
        run_program(
            &cmd(),
            &["/d", "/c", script],
            codes,
            &Progress::new(),
            &mut |_| {},
        )
        .unwrap()
    }

    #[test]
    fn output_and_exit_code_are_captured() {
        let run = run_cmd("echo first& echo second& exit /b 0", ExitCodes::Plain);
        assert_eq!(run.verdict, Verdict::Ok);
        assert_eq!(run.code, Some(0));
        assert_eq!(run.output, "first\nsecond");
        assert_eq!(run.summary, "Finished.");
    }

    #[test]
    fn standard_error_is_part_of_the_log() {
        let run = run_cmd("echo problem 1>&2& exit /b 7", ExitCodes::Plain);
        assert_eq!(run.verdict, Verdict::Failed);
        assert_eq!(run.code, Some(7));
        assert!(run.output.contains("problem"), "{:?}", run.output);
        assert!(run.summary.contains("error 7"), "{}", run.summary);
    }

    #[test]
    fn utf16_output_is_decoded_like_the_system_file_checker_writes_it() {
        // `cmd /u` writes UTF-16 without a byte order mark, as sfc.exe does.
        let run = run_program(
            &cmd(),
            &["/d", "/u", "/c", "echo Verification 100% complete."],
            ExitCodes::Sfc,
            &Progress::new(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(run.output, "Verification 100% complete.");
        assert_eq!(run.verdict, Verdict::Ok);
    }

    #[test]
    fn output_is_streamed_and_progress_follows_the_percentage() {
        let progress = Progress::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        let run = run_program(
            &cmd(),
            &["/d", "/c", "echo Stage: 25%& echo Stage: 50%"],
            ExitCodes::Plain,
            &progress,
            &mut |log| sink.lock().unwrap().push(log.text()),
        )
        .unwrap();
        assert_eq!(run.verdict, Verdict::Ok);
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "the sink was called");
        assert_eq!(seen.last().unwrap(), "Stage: 25%\nStage: 50%");
        assert_eq!(progress.total(), 1000);
        assert_eq!(progress.done(), 1000, "a clean finish completes the bar");
    }

    #[test]
    fn a_running_tool_can_be_cancelled() {
        let progress = Progress::new();
        let canceller = progress.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            canceller.cancel();
        });
        // Waits for a signal nobody sends; it would take 60 seconds.
        let signal = format!("ZestToolsTest{}", std::process::id());
        let run = run_program(
            &process::system_tool("waitfor.exe"),
            &["/t", "60", signal.as_str()],
            ExitCodes::Plain,
            &progress,
            &mut |_| {},
        )
        .unwrap();
        stopper.join().unwrap();
        assert_eq!(run.verdict, Verdict::Cancelled);
        assert!(run.elapsed < Duration::from_secs(20), "{:?}", run.elapsed);
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let result = run_program(
            Path::new(r"C:\Windows\System32\zest-no-such-tool.exe"),
            &[] as &[&str],
            ExitCodes::Plain,
            &Progress::new(),
            &mut |_| {},
        );
        assert!(result.is_err());
    }

    #[test]
    fn tools_that_need_administrator_rights_are_not_started_without_them() {
        let tool = Tool::find("sfc-repair").unwrap();
        let mut called = false;
        let result = run_as(tool, false, &Progress::new(), &mut |_| called = true);
        assert!(
            matches!(result, Err(Error::NeedsElevation(_))),
            "{result:?}"
        );
        assert!(!called);
    }

    #[test]
    fn consoles_cannot_be_run_and_commands_cannot_be_opened() {
        let console = Tool::find("event-viewer").unwrap();
        let result = run_as(console, true, &Progress::new(), &mut |_| {});
        assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");

        let command = Tool::find("dns-flush").unwrap();
        assert!(matches!(open(command), Err(Error::Invalid(_))));
    }

    #[test]
    fn dism_exit_codes_are_read_as_windows_errors() {
        let verdict = |code| interpret(ExitCodes::Dism, Some(code)).0;
        assert_eq!(verdict(0), Verdict::Ok);
        assert_eq!(verdict(3010), Verdict::RestartNeeded);
        assert_eq!(verdict(740), Verdict::NeedsAdmin);
        assert_eq!(verdict(5), Verdict::NeedsAdmin);
        assert_eq!(verdict(87), Verdict::Failed);
        assert_eq!(verdict(1223), Verdict::Cancelled);
        assert_eq!(verdict(CBS_E_SOURCE_MISSING), Verdict::Failed);
        let (_, message) = interpret(ExitCodes::Dism, Some(CBS_E_SOURCE_MISSING));
        assert!(message.contains("internet"), "{message}");
        let (failed, message) = interpret(ExitCodes::Dism, Some(-2146498529));
        assert_eq!(failed, Verdict::Failed);
        assert!(message.contains("internet"), "0x800F081F as a signed code");
        let (_, message) = interpret(ExitCodes::Dism, Some(1726));
        assert!(
            message.contains("1726") && message.contains("0x000006BE"),
            "{message}"
        );
    }

    #[test]
    fn chkdsk_exit_codes_follow_the_documented_table() {
        let read = |code| interpret(ExitCodes::Chkdsk, Some(code));
        assert_eq!(read(0), (Verdict::Ok, "No errors were found.".to_owned()));
        assert_eq!(read(1).0, Verdict::Ok);
        assert_eq!(read(2).0, Verdict::Ok);
        assert_eq!(read(3).0, Verdict::Attention);
        assert_eq!(read(9).0, Verdict::Failed);
    }

    #[test]
    fn sfc_and_plain_exit_codes() {
        assert_eq!(interpret(ExitCodes::Sfc, Some(0)).0, Verdict::Ok);
        assert_eq!(interpret(ExitCodes::Sfc, Some(1)).0, Verdict::Attention);
        assert_eq!(interpret(ExitCodes::Sfc, Some(740)).0, Verdict::NeedsAdmin);
        assert_eq!(interpret(ExitCodes::Plain, Some(0)).0, Verdict::Ok);
        assert_eq!(interpret(ExitCodes::Plain, Some(1)).0, Verdict::Failed);
        assert_eq!(interpret(ExitCodes::Plain, Some(5)).0, Verdict::NeedsAdmin);
        assert_eq!(interpret(ExitCodes::Plain, None).0, Verdict::Failed);
    }
}
