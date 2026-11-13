//! Administrator detection and elevation.

use crate::{Error, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;

/// True when the current process runs with administrator rights.
pub fn is_elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(query_elevation)
}

fn query_elevation() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: standard token query; the handle is closed before returning and
    // the output buffer is a correctly sized TOKEN_ELEVATION.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Quotes one argument following the rules of the Microsoft C runtime, so it
/// survives the round trip through a Windows command line.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Joins arguments into a single command line string.
pub fn join_args<S: AsRef<str>>(args: &[S]) -> String {
    args.iter()
        .map(|a| quote_arg(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Starts `exe` with administrator rights (shows the UAC prompt).
///
/// Returns [`Error::Cancelled`] when the user declines the prompt.
pub fn run_elevated<S: AsRef<str>>(exe: &Path, args: &[S], show_window: bool) -> Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    use windows::core::PCWSTR;

    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params = wide(std::ffi::OsStr::new(&join_args(args)));
    let show = if show_window { SW_SHOWNORMAL } else { SW_HIDE };
    // SAFETY: all strings are NUL terminated and outlive the call.
    let instance = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            show,
        )
    };
    // ShellExecute reports success with a value greater than 32.
    let code = instance.0 as usize;
    if code > 32 {
        Ok(())
    } else if code == 5 {
        // SE_ERR_ACCESSDENIED: the UAC prompt was declined.
        Err(Error::Cancelled)
    } else {
        Err(Error::Windows(format!(
            "could not start {} elevated (code {code})",
            exe.display()
        )))
    }
}

/// Restarts the current program with administrator rights, passing `args`.
/// The caller should exit after this returns `Ok`.
pub fn relaunch_elevated<S: AsRef<str>>(args: &[S]) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::other(format!("cannot locate the running program: {e}")))?;
    run_elevated(&exe, args, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_arg("simple"), "simple");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("two words"), "\"two words\"");
        assert_eq!(
            quote_arg(r"C:\Program Files\x\"),
            r#""C:\Program Files\x\\""#
        );
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(join_args(&["--auto", "a b"]), r#"--auto "a b""#);
    }

    #[test]
    fn elevation_query_does_not_panic() {
        let first = is_elevated();
        assert_eq!(first, is_elevated());
    }
}
