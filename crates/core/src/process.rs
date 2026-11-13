//! Child processes without console flashes, plus process enumeration.

use crate::{Error, IoContext, Result, paths};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Full path of a tool in `System32`, e.g. `schtasks.exe`.
///
/// Always launching system tools by absolute path avoids picking up a
/// same-named executable from the current directory or `PATH`.
pub fn system_tool(name: &str) -> PathBuf {
    paths::system32_dir().join(name)
}

/// A [`Command`] that never opens a console window and has no stdin.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(Stdio::null());
    cmd
}

/// Captured result of a finished child process.
#[derive(Clone, Debug)]
pub struct CommandOutput {
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Decoded standard output.
    pub stdout: String,
    /// Decoded standard error.
    pub stderr: String,
}

impl CommandOutput {
    /// True when the process exited with code zero.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs a program hidden and captures its decoded output.
pub fn run_capture<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<CommandOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let output = hidden_command(program)
        .args(args)
        .output()
        .ctx(format!("starting {}", program.to_string_lossy()))?;
    Ok(CommandOutput {
        code: output.status.code(),
        stdout: decode_console(&output.stdout),
        stderr: decode_console(&output.stderr),
    })
}

/// Runs a program hidden and returns its stdout, failing on a non-zero exit.
pub fn run_checked<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let name = program.as_ref().to_string_lossy().into_owned();
    let output = run_capture(program, args)?;
    if output.success() {
        Ok(output.stdout)
    } else {
        let detail = if output.stderr.trim().is_empty() {
            output.stdout.trim().to_owned()
        } else {
            output.stderr.trim().to_owned()
        };
        Err(Error::Command(format!(
            "{name} exited with code {}: {detail}",
            output.code.map_or("?".to_owned(), |c| c.to_string())
        )))
    }
}

/// Decodes console output: UTF-16 (with BOM), then UTF-8, then the OEM code page.
pub fn decode_console(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(body) {
        Ok(s) => s.to_owned(),
        Err(_) => decode_oem(body),
    }
}

fn decode_oem(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::{
        CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar,
    };
    if bytes.is_empty() {
        return String::new();
    }
    // SAFETY: both buffers are valid for the lengths passed.
    unsafe {
        let needed = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if needed <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(
            CP_OEMCP,
            MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
            bytes,
            Some(&mut wide),
        );
        wide.truncate(written.max(0) as usize);
        String::from_utf16_lossy(&wide)
    }
}

/// One running process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    /// Process identifier.
    pub pid: u32,
    /// Identifier of the parent process.
    pub parent_pid: u32,
    /// Executable file name, e.g. `chrome.exe`.
    pub name: String,
}

/// Snapshot of all running processes.
pub fn list_processes() -> Vec<ProcessEntry> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    // SAFETY: the snapshot handle is valid until closed; `entry.dwSize` is set
    // as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                out.push(ProcessEntry {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    name: String::from_utf16_lossy(&entry.szExeFile[..len]),
                });
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// Lower-cased executable names of all running processes.
pub fn running_process_names() -> HashSet<String> {
    list_processes()
        .into_iter()
        .map(|p| p.name.to_lowercase())
        .collect()
}

/// True when a process with this executable name (case-insensitive) is running.
pub fn is_process_running(exe_name: &str) -> bool {
    let wanted = exe_name.to_lowercase();
    list_processes()
        .iter()
        .any(|p| p.name.to_lowercase() == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_itself_and_the_system() {
        let me = std::process::id();
        let all = list_processes();
        assert!(all.iter().any(|p| p.pid == me));
        assert!(running_process_names().contains("explorer.exe") || !all.is_empty());
        assert!(!is_process_running("definitely-not-running-zest-test.exe"));
    }

    #[test]
    fn captures_output_without_a_window() {
        let out = run_capture(system_tool("cmd.exe"), ["/C", "echo hello"]).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.trim(), "hello");
        let err = run_checked(system_tool("cmd.exe"), ["/C", "exit 3"]).unwrap_err();
        assert!(
            matches!(err, Error::Command(ref m) if m.contains("code 3")),
            "{err}"
        );
    }

    #[test]
    fn decoding() {
        assert_eq!(decode_console(b"plain"), "plain");
        assert_eq!(decode_console(&[0xEF, 0xBB, 0xBF, b'o', b'k']), "ok");
        assert_eq!(decode_console(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        // 0x81 is not valid UTF-8 on its own; must fall back without panicking.
        assert!(!decode_console(&[b'a', 0x81, b'b']).is_empty());
    }
}
