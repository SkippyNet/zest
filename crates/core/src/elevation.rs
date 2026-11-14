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

/// The Windows account signed in to this desktop, when the process runs as a
/// different one.
///
/// That happens when a standard user starts the program "as administrator"
/// and types another account's password: the elevated process then belongs to
/// that administrator, and everything that is "per user" (temporary files,
/// browser profiles, `HKEY_CURRENT_USER`) is the administrator's, not that of
/// the person at the keyboard. Returns `None` when both are the same account
/// or when Windows does not say who is signed in.
pub fn desktop_account_if_other() -> Option<String> {
    static OTHER: OnceLock<Option<String>> = OnceLock::new();
    OTHER
        .get_or_init(|| {
            let session = current_session_id()?;
            // LogonUI records who is signed in to each session here.
            let key = crate::reg::RegPath::hklm(format!(
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\LogonUI\SessionData\{session}"
            ));
            let signed_in = key.read_string("LoggedOnSAMUser")?;
            let process = format!(
                r"{}\{}",
                std::env::var("USERDOMAIN").ok()?,
                std::env::var("USERNAME").ok()?
            );
            other_account(&signed_in, &process)
        })
        .clone()
}

/// Compares two `DOMAIN\name` account names; returns the bare name of
/// `signed_in` when they differ.
fn other_account(signed_in: &str, process: &str) -> Option<String> {
    let (signed_in, process) = (signed_in.trim(), process.trim());
    if signed_in.is_empty()
        || process.is_empty()
        || signed_in.to_lowercase() == process.to_lowercase()
    {
        return None;
    }
    let name = signed_in.rsplit(r"\").next().unwrap_or(signed_in);
    (!name.is_empty()).then(|| name.to_owned())
}

/// The Terminal Services session this process belongs to.
fn current_session_id() -> Option<u32> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenSessionId};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: standard token query into a correctly sized u32; the handle is
    // closed before returning.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut session = 0u32;
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenSessionId,
            Some(&mut session as *mut u32 as *mut core::ffi::c_void),
            size_of::<u32>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok.then_some(session)
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn same_account_in_any_spelling_is_not_other() {
        assert_eq!(other_account(r"DESK\Alex", r"DESK\Alex"), None);
        assert_eq!(other_account(r"DESK\Alex", r"desk\alex"), None);
        assert_eq!(other_account(r" DESK\Alex ", r"DESK\Alex"), None);
    }

    #[test]
    fn a_different_account_is_named() {
        assert_eq!(
            other_account(r"DESK\Alex", r"DESK\Admin"),
            Some("Alex".to_owned())
        );
        assert_eq!(
            other_account(r"AzureAD\Sam", r"DESK\Admin"),
            Some("Sam".to_owned())
        );
    }

    #[test]
    fn missing_information_never_warns() {
        assert_eq!(other_account("", r"DESK\Admin"), None);
        assert_eq!(other_account(r"DESK\Alex", ""), None);
    }

    #[test]
    fn this_process_runs_as_the_signed_in_account() {
        // Tests are started from the developer's or the CI runner's own
        // session, so the answer is "same account" or "unknown" (services).
        assert!(current_session_id().is_some());
        if std::env::var("GITHUB_ACTIONS").is_err() {
            assert_eq!(desktop_account_if_other(), None);
        }
    }
}
