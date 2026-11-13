//! Running uninstallers and maintaining `Uninstall` registrations.

use super::{AppSource, InstalledApp, store};
use crate::Scope;
use crate::util::{io_err, wide, wide_os};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use windows::core::PCWSTR;
use zest_core::reg::RegBackup;
use zest_core::reg::RegPath;
use zest_core::{Error, Progress, Result, cmdline, elevation, paths, process};

/// Windows Installer: "restart required to complete".
const ERROR_SUCCESS_REBOOT_REQUIRED: u32 = 3010;
/// Windows Installer: "restart initiated".
const ERROR_SUCCESS_REBOOT_INITIATED: u32 = 1641;
/// HRESULT for `ERROR_CANCELLED` (the elevation prompt was declined).
const HR_CANCELLED: u32 = 0x8007_04C7;
/// HRESULT for `ERROR_FILE_NOT_FOUND`.
const HR_FILE_NOT_FOUND: u32 = 0x8007_0002;
/// HRESULT for `ERROR_PATH_NOT_FOUND`.
const HR_PATH_NOT_FOUND: u32 = 0x8007_0003;
/// How often running uninstallers are polled.
const POLL: Duration = Duration::from_millis(200);
/// How long to keep waiting for helper processes once the registration has
/// disappeared (uninstallers like to open a feedback page at the end).
const GRACE_AFTER_REMOVAL: Duration = Duration::from_secs(2);

/// A program to start, with its argument text passed through verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchCommand {
    /// The executable (absolute path, or a bare name Windows can resolve).
    pub program: String,
    /// Raw argument text.
    pub arguments: String,
    /// True when the command is expected to run without asking questions.
    pub quiet: bool,
}

impl LaunchCommand {
    /// The command as one line, for logs and confirmation dialogs.
    pub fn display(&self) -> String {
        let program = if self.program.contains(' ') {
            format!("\"{}\"", self.program)
        } else {
            self.program.clone()
        };
        if self.arguments.trim().is_empty() {
            program
        } else {
            format!("{program} {}", self.arguments.trim())
        }
    }
}

/// What happened when an uninstaller (or repair) was run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallOutcome {
    /// The command that was run (empty for Store packages).
    pub command: String,
    /// Exit code of the uninstaller, when it could be observed.
    pub exit_code: Option<u32>,
    /// True when the program is no longer registered afterwards. `false`
    /// usually means the user cancelled the uninstaller or it failed.
    pub entry_removed: bool,
    /// True when the uninstaller asked for a restart.
    pub reboot_required: bool,
    /// True when the uninstaller ran without user interaction.
    pub ran_quietly: bool,
    /// True when waiting was cancelled through the progress handle (the
    /// uninstaller itself keeps running).
    pub wait_cancelled: bool,
}

fn msiexec() -> String {
    paths::system32_dir()
        .join("msiexec.exe")
        .display()
        .to_string()
}

fn split(command: &str) -> Result<(String, String)> {
    let (program, arguments) = cmdline::split_program(command);
    if program.trim().is_empty() {
        return Err(Error::Invalid(format!("\"{command}\" is not a command")));
    }
    Ok((program, arguments))
}

/// Works out the command that uninstalls a classic program.
///
/// * Windows Installer products are removed with `msiexec /x {code}`; when
///   `quiet`, `/quiet /norestart` is added if no elevation prompt will be
///   needed, otherwise `/passive /norestart` (progress bar only) so Windows
///   can still ask for elevation.
/// * Other programs use their `QuietUninstallString` when `quiet` and one
///   exists, otherwise their `UninstallString`. If the installer provides no
///   quiet command the normal interactive uninstaller is returned with
///   `quiet == false`.
///
/// Store packages have no command; they are removed by the package manager.
pub fn uninstall_command(app: &InstalledApp, quiet: bool) -> Result<LaunchCommand> {
    if app.source == AppSource::Store {
        return Err(Error::Invalid(
            "Store apps are removed by the package manager, not by a command".to_owned(),
        ));
    }
    if let (true, Some(code)) = (app.is_msi, &app.product_code) {
        let mut arguments = format!("/x {code}");
        if quiet {
            let silent_possible = elevation::is_elevated() || app.scope == Scope::User;
            arguments.push_str(if silent_possible {
                " /quiet /norestart"
            } else {
                " /passive /norestart"
            });
        }
        return Ok(LaunchCommand {
            program: msiexec(),
            arguments,
            quiet,
        });
    }

    if quiet && let Some(command) = &app.quiet_uninstall_string {
        let (program, arguments) = split(command)?;
        return Ok(LaunchCommand {
            program,
            arguments,
            quiet: true,
        });
    }
    let Some(command) = &app.uninstall_string else {
        return Err(Error::NotFound(format!(
            "an uninstaller for \"{}\"",
            app.name
        )));
    };
    let (program, mut arguments) = split(command)?;
    // Installers built with the common "_is1" convention accept standard
    // silent switches even when they register no quiet command.
    let supports_silent_switches = app
        .key
        .as_ref()
        .is_some_and(|key| key.leaf().to_lowercase().ends_with("_is1"));
    let mut runs_quietly = false;
    if quiet && supports_silent_switches {
        if !arguments.to_lowercase().contains("silent") {
            arguments = format!("{arguments} /VERYSILENT /SUPPRESSMSGBOXES /NORESTART")
                .trim()
                .to_owned();
        }
        runs_quietly = true;
    }
    Ok(LaunchCommand {
        program,
        arguments,
        quiet: runs_quietly,
    })
}

/// Works out the command that repairs (or changes) a classic program.
pub fn repair_command(app: &InstalledApp) -> Result<LaunchCommand> {
    if app.source == AppSource::Store {
        return Err(Error::Invalid(
            "Store apps cannot be repaired here".to_owned(),
        ));
    }
    if let (true, Some(code)) = (app.is_msi, &app.product_code) {
        return Ok(LaunchCommand {
            program: msiexec(),
            arguments: format!("/f {code}"),
            quiet: false,
        });
    }
    let Some(command) = &app.modify_path else {
        return Err(Error::NotFound(format!(
            "a repair command for \"{}\"",
            app.name
        )));
    };
    let (program, arguments) = split(command)?;
    Ok(LaunchCommand {
        program,
        arguments,
        quiet: false,
    })
}

/// Owned process handle from `ShellExecuteExW`.
struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by ShellExecuteExW and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Starts a program the way Explorer would (so programs that need
/// administrator rights show the elevation prompt).
fn launch(command: &LaunchCommand, show_window: bool) -> Result<Option<ProcessHandle>> {
    const SW_HIDE: i32 = 0;
    const SW_SHOWNORMAL: i32 = 1;

    let file = wide(&command.program);
    let parameters = wide(&command.arguments);
    let directory = Path::new(&command.program)
        .parent()
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .map(|dir| wide_os(dir.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        lpDirectory: directory
            .as_ref()
            .map_or(PCWSTR::null(), |dir| PCWSTR(dir.as_ptr())),
        nShow: if show_window { SW_SHOWNORMAL } else { SW_HIDE },
        ..Default::default()
    };
    // SAFETY: `info` is fully initialised and every string outlives the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| match e.code().0 as u32 {
        HR_CANCELLED => Error::Cancelled,
        HR_FILE_NOT_FOUND | HR_PATH_NOT_FOUND => {
            Error::NotFound(format!("the program {}", command.program))
        }
        code => Error::Windows(format!(
            "could not start {}: {} (0x{code:08X})",
            command.program,
            e.message()
        )),
    })?;
    Ok((!info.hProcess.is_invalid()).then_some(ProcessHandle(info.hProcess)))
}

/// When the process behind `handle` was created, as FILETIME ticks.
fn handle_creation_time(handle: HANDLE) -> Option<u64> {
    let mut created = FILETIME::default();
    let mut ignored = [FILETIME::default(); 3];
    let [exited, kernel, user] = &mut ignored;
    // SAFETY: valid handle and four valid out pointers.
    unsafe { GetProcessTimes(handle, &mut created, exited, kernel, user) }.ok()?;
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// When the process with identifier `pid` was created, if it can be queried.
fn creation_time(pid: u32) -> Option<u64> {
    // SAFETY: the handle is closed by `ProcessHandle` after the query.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let handle = ProcessHandle(handle);
    handle_creation_time(handle.0)
}

/// Waits for a started program *and the helper processes it spawns*.
///
/// Many uninstallers copy themselves to a temporary folder, start that copy
/// and exit immediately, so waiting for the first process alone would return
/// far too early. Returns the exit code of the first process (when seen) and
/// whether the wait was cancelled.
fn wait_for_tree(
    handle: &ProcessHandle,
    progress: &Progress,
    still_registered: &dyn Fn() -> bool,
) -> (Option<u32>, bool) {
    // SAFETY: the handle is valid for the lifetime of `handle`.
    let root_pid = unsafe { GetProcessId(handle.0) };
    let root_started = handle_creation_time(handle.0);
    let mut exit_code: Option<u32> = None;
    let mut tracked: HashSet<u32> = HashSet::from([root_pid]);
    let mut unrelated: HashSet<u32> = HashSet::new();
    let mut removed_at: Option<Instant> = None;

    loop {
        if progress.is_cancelled() {
            return (exit_code, true);
        }
        if exit_code.is_none() {
            // SAFETY: valid handle; a zero timeout only polls.
            let finished = unsafe { WaitForSingleObject(handle.0, 0) } == WAIT_OBJECT_0;
            if finished {
                let mut code = 0u32;
                // SAFETY: valid handle and out pointer.
                if unsafe { GetExitCodeProcess(handle.0, &mut code) }.is_ok() {
                    exit_code = Some(code);
                } else {
                    exit_code = Some(0);
                }
            }
        }

        // Carry the set of live processes forward: anything that was tracked
        // and is still alive, plus anything started by a tracked process.
        // Membership is re-derived from each snapshot so that an exited
        // process's identifier is forgotten after one round.
        let snapshot = process::list_processes();
        let mut alive: HashSet<u32> = snapshot
            .iter()
            .filter(|p| p.pid != 0 && tracked.contains(&p.pid))
            .map(|p| p.pid)
            .collect();
        loop {
            let mut added = false;
            for p in &snapshot {
                if p.pid == 0 || alive.contains(&p.pid) || unrelated.contains(&p.pid) {
                    continue;
                }
                if !(tracked.contains(&p.parent_pid) || alive.contains(&p.parent_pid)) {
                    continue;
                }
                // Parent identifiers are never cleared when a parent exits
                // and get reused, so a long-running process can *look* like
                // a child. A real child was started after the uninstaller.
                let is_child = match (root_started, creation_time(p.pid)) {
                    (Some(root), Some(child)) => child >= root,
                    _ => true,
                };
                if is_child {
                    alive.insert(p.pid);
                    added = true;
                } else {
                    unrelated.insert(p.pid);
                }
            }
            if !added {
                break;
            }
        }
        if exit_code.is_some() {
            alive.remove(&root_pid);
        }

        if exit_code.is_some() {
            if alive.is_empty() {
                return (exit_code, false);
            }
            if !still_registered() {
                let since = *removed_at.get_or_insert_with(Instant::now);
                if since.elapsed() >= GRACE_AFTER_REMOVAL {
                    return (exit_code, false);
                }
            }
        }
        tracked = alive;
        if exit_code.is_none() {
            tracked.insert(root_pid);
        }
        std::thread::sleep(POLL);
    }
}

/// True while the program is still registered as installed.
pub fn is_still_installed(app: &InstalledApp) -> bool {
    match app.source {
        AppSource::Registry => app.key.as_ref().is_some_and(RegPath::exists),
        AppSource::Store => app
            .package_full_name
            .as_deref()
            .is_some_and(store::is_installed),
    }
}

fn run(
    app: &InstalledApp,
    command: LaunchCommand,
    progress: &Progress,
    show_window: bool,
) -> Result<UninstallOutcome> {
    progress.set_message(format!("Running {}", command.display()));
    log::info!("starting \"{}\" for \"{}\"", command.display(), app.name);
    let handle = launch(&command, show_window)?;
    let (exit_code, wait_cancelled) = match &handle {
        Some(handle) => wait_for_tree(handle, progress, &|| is_still_installed(app)),
        None => (None, false),
    };
    Ok(UninstallOutcome {
        command: command.display(),
        exit_code,
        entry_removed: !is_still_installed(app),
        reboot_required: matches!(
            exit_code,
            Some(ERROR_SUCCESS_REBOOT_REQUIRED | ERROR_SUCCESS_REBOOT_INITIATED)
        ),
        ran_quietly: command.quiet,
        wait_cancelled,
    })
}

fn uninstall_impl(
    app: &InstalledApp,
    quiet: bool,
    progress: &Progress,
    show_window: bool,
) -> Result<UninstallOutcome> {
    if !app.can_uninstall {
        return Err(Error::Other(format!(
            "\"{}\" does not offer a way to uninstall it",
            app.name
        )));
    }
    if app.source == AppSource::Store {
        let Some(full_name) = &app.package_full_name else {
            return Err(Error::Invalid("the app has no package name".to_owned()));
        };
        progress.set_message(format!("Removing {}", app.name));
        store::remove(full_name)?;
        return Ok(UninstallOutcome {
            command: String::new(),
            exit_code: Some(0),
            entry_removed: !is_still_installed(app),
            reboot_required: false,
            ran_quietly: true,
            wait_cancelled: false,
        });
    }
    let command = uninstall_command(app, quiet)?;
    run(app, command, progress, show_window)
}

/// Uninstalls a program by running its own uninstaller and waiting for it
/// (and the helper processes it starts) to finish.
///
/// * `quiet` asks for an unattended uninstall where the installer supports
///   one; see [`uninstall_command`]. Check [`UninstallOutcome::ran_quietly`].
/// * Cancelling `progress` stops the *waiting*; an uninstaller that is
///   already running is never killed.
/// * Declining the elevation prompt yields [`Error::Cancelled`].
///
/// Always check [`UninstallOutcome::entry_removed`]: a user can cancel the
/// uninstaller's own dialog, in which case the program is still installed.
pub fn uninstall(app: &InstalledApp, quiet: bool, progress: &Progress) -> Result<UninstallOutcome> {
    uninstall_impl(app, quiet, progress, true)
}

/// Starts the program's repair / change command and waits for it to finish.
pub fn repair(app: &InstalledApp, progress: &Progress) -> Result<UninstallOutcome> {
    if !app.can_repair {
        return Err(Error::Other(format!(
            "\"{}\" does not offer a repair command",
            app.name
        )));
    }
    let command = repair_command(app)?;
    run(app, command, progress, true)
}

/// True when `key` sits directly below an `Uninstall` key.
fn is_uninstall_entry(key: &RegPath) -> bool {
    key.parent()
        .is_some_and(|parent| parent.leaf().eq_ignore_ascii_case("Uninstall"))
        && !key.leaf().is_empty()
}

fn registration(app: &InstalledApp) -> Result<&RegPath> {
    match &app.key {
        Some(key) if is_uninstall_entry(key) => Ok(key),
        Some(key) => Err(Error::Other(format!(
            "refused to modify {key}: not an uninstall registration"
        ))),
        None => Err(Error::Invalid(format!(
            "\"{}\" is not registered in the registry",
            app.name
        ))),
    }
}

/// Removes a stale registration from the installed-programs list without
/// running any uninstaller. The key is exported to `backup` first.
///
/// Use this for entries whose uninstaller is gone. It does not remove any
/// files.
pub fn remove_entry(app: &InstalledApp, backup: Option<&mut RegBackup>) -> Result<()> {
    let key = registration(app)?;
    zest_core::reg::delete_key(key, backup)
}

/// Changes the name an entry is listed under. The old name is exported to
/// `backup` first.
pub fn rename_entry(
    app: &InstalledApp,
    new_name: &str,
    backup: Option<&mut RegBackup>,
) -> Result<()> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err(Error::Invalid("the new name is empty".to_owned()));
    }
    let key = registration(app)?;
    let action = format!("rename \"{}\"", app.name);
    let handle = key.open_write().map_err(|e| io_err(e, &action))?;
    if let Some(backup) = backup
        && handle.get_raw_value("DisplayName").is_ok()
    {
        backup.add_value(key, "DisplayName")?;
    }
    handle
        .set_value("DisplayName", &new_name)
        .map_err(|e| io_err(e, &action))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{add_entry, fake_uninstall_root};
    use super::super::{ListOptions, list_location};
    use super::*;
    use crate::test_support::Scratch;

    fn cmd() -> String {
        paths::system32_dir().join("cmd.exe").display().to_string()
    }

    fn base_app() -> InstalledApp {
        InstalledApp {
            id: "win32|test".to_owned(),
            source: AppSource::Registry,
            name: "Test App".to_owned(),
            publisher: None,
            version: None,
            install_date: None,
            install_date_estimated: false,
            size_bytes: None,
            install_location: None,
            uninstall_string: None,
            quiet_uninstall_string: None,
            modify_path: None,
            is_system_component: false,
            is_update: false,
            is_msi: false,
            product_code: None,
            icon_path: None,
            key: None,
            is_64bit: true,
            scope: Scope::User,
            requires_admin: false,
            can_uninstall: true,
            can_repair: true,
            package_full_name: None,
            package_family_name: None,
            about_url: None,
        }
    }

    #[test]
    fn msi_commands() {
        let mut app = base_app();
        app.is_msi = true;
        app.product_code = Some("{11111111-2222-3333-4444-555555555555}".to_owned());
        app.uninstall_string = Some("MsiExec.exe /I{11111111-2222-3333-4444-555555555555}".into());

        let normal = uninstall_command(&app, false).unwrap();
        assert!(
            normal
                .program
                .to_lowercase()
                .ends_with(r"\system32\msiexec.exe")
        );
        assert_eq!(
            normal.arguments,
            "/x {11111111-2222-3333-4444-555555555555}"
        );
        assert!(!normal.quiet);

        // Per-user product: fully silent is always possible.
        let quiet = uninstall_command(&app, true).unwrap();
        assert_eq!(
            quiet.arguments,
            "/x {11111111-2222-3333-4444-555555555555} /quiet /norestart"
        );
        assert!(quiet.quiet);

        // Machine-wide product without elevation: keep the elevation prompt possible.
        app.scope = Scope::Machine;
        let machine = uninstall_command(&app, true).unwrap();
        if elevation::is_elevated() {
            assert!(machine.arguments.ends_with("/quiet /norestart"));
        } else {
            assert!(machine.arguments.ends_with("/passive /norestart"));
        }

        let repair = repair_command(&app).unwrap();
        assert_eq!(
            repair.arguments,
            "/f {11111111-2222-3333-4444-555555555555}"
        );
    }

    #[test]
    fn classic_commands() {
        let mut app = base_app();
        assert!(matches!(
            uninstall_command(&app, false),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(repair_command(&app), Err(Error::NotFound(_))));

        app.uninstall_string = Some(r#""C:\Program Files\My App\uninstall.exe" /remove"#.into());
        let normal = uninstall_command(&app, false).unwrap();
        assert_eq!(normal.program, r"C:\Program Files\My App\uninstall.exe");
        assert_eq!(normal.arguments, "/remove");
        assert!(!normal.quiet);
        assert_eq!(
            normal.display(),
            r#""C:\Program Files\My App\uninstall.exe" /remove"#
        );

        // No quiet command registered: the interactive one is used and flagged.
        let fallback = uninstall_command(&app, true).unwrap();
        assert_eq!(fallback.arguments, "/remove");
        assert!(!fallback.quiet);

        app.quiet_uninstall_string =
            Some(r#""C:\Program Files\My App\uninstall.exe" /remove /S"#.into());
        let quiet = uninstall_command(&app, true).unwrap();
        assert_eq!(quiet.arguments, "/remove /S");
        assert!(quiet.quiet);
        // The quiet command is only used when asked for.
        assert_eq!(uninstall_command(&app, false).unwrap().arguments, "/remove");

        // Unquoted path with spaces (nothing on disk): cut at the extension.
        app.quiet_uninstall_string = None;
        app.uninstall_string = Some(r"C:\No Such Dir\My App\unins000.exe".into());
        let unquoted = uninstall_command(&app, false).unwrap();
        assert_eq!(unquoted.program, r"C:\No Such Dir\My App\unins000.exe");
        assert_eq!(unquoted.arguments, "");

        // "_is1" registrations accept standard silent switches.
        app.key = Some(RegPath::hkcu(r"Software\X\Uninstall\{APP}_is1"));
        let silent = uninstall_command(&app, true).unwrap();
        assert_eq!(silent.arguments, "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART");
        assert!(silent.quiet);

        app.modify_path = Some(r"C:\No Such Dir\setup.exe /modify".into());
        let repair = repair_command(&app).unwrap();
        assert_eq!(repair.program, r"C:\No Such Dir\setup.exe");
        assert_eq!(repair.arguments, "/modify");

        app.uninstall_string = Some("   ".into());
        assert!(matches!(
            uninstall_command(&app, false),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn store_apps_have_no_command() {
        let mut app = base_app();
        app.source = AppSource::Store;
        assert!(matches!(
            uninstall_command(&app, false),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(repair_command(&app), Err(Error::Invalid(_))));
        // Without a package name nothing can be removed.
        assert!(uninstall_impl(&app, true, &Progress::new(), false).is_err());
        assert!(!is_still_installed(&app));
    }

    #[test]
    fn entries_that_cannot_be_uninstalled_are_refused() {
        let mut app = base_app();
        app.can_uninstall = false;
        app.can_repair = false;
        app.uninstall_string = Some("x.exe".into());
        assert!(matches!(
            uninstall_impl(&app, false, &Progress::new(), false),
            Err(Error::Other(_))
        ));
        assert!(matches!(
            repair(&app, &Progress::new()),
            Err(Error::Other(_))
        ));
    }

    #[test]
    fn runs_a_fake_uninstaller_end_to_end() {
        let scratch = Scratch::new("apps-uninstall");
        let loc = fake_uninstall_root(&scratch);
        let key = add_entry(&loc, "FakeApp");
        key.set_value("DisplayName", &"Fake App").unwrap();
        // The "uninstaller" deletes the fake registration, like a real one would.
        let entry = loc.key.child("FakeApp");
        key.set_value(
            "UninstallString",
            // The tool path is deliberately unquoted: `cmd /c` strips the
            // outer quotes of a command that starts with one.
            &format!(
                "\"{}\" /c {} delete \"{}\" /f",
                cmd(),
                process::system_tool("reg.exe").display(),
                entry.full_name()
            ),
        )
        .unwrap();
        drop(key);

        let app = list_location(&loc, &ListOptions::default()).remove(0);
        assert!(is_still_installed(&app));
        let outcome = uninstall_impl(&app, false, &Progress::new(), false).unwrap();
        assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
        assert!(outcome.entry_removed, "{outcome:?}");
        assert!(!outcome.reboot_required && !outcome.wait_cancelled && !outcome.ran_quietly);
        assert!(!is_still_installed(&app));
        assert!(list_location(&loc, &ListOptions::default()).is_empty());
    }

    #[test]
    fn a_cancelled_uninstaller_leaves_the_entry() {
        let scratch = Scratch::new("apps-uninstall-cancel");
        let loc = fake_uninstall_root(&scratch);
        let key = add_entry(&loc, "Stubborn");
        key.set_value("DisplayName", &"Stubborn").unwrap();
        key.set_value("UninstallString", &format!("\"{}\" /c exit 3010", cmd()))
            .unwrap();
        drop(key);
        let app = list_location(&loc, &ListOptions::default()).remove(0);
        let outcome = uninstall_impl(&app, false, &Progress::new(), false).unwrap();
        assert_eq!(outcome.exit_code, Some(3010));
        assert!(!outcome.entry_removed);
        assert!(outcome.reboot_required);
    }

    #[test]
    fn waits_for_helper_processes() {
        // The first process exits at once; the helper it started lives on
        // for about two seconds. The wait must cover the helper.
        let command = LaunchCommand {
            program: cmd(),
            arguments: "/c start \"\" /b cmd.exe /c \"ping -n 3 127.0.0.1 >nul\"".to_owned(),
            quiet: true,
        };
        let handle = launch(&command, false).unwrap().expect("a process handle");
        let started = Instant::now();
        let (code, cancelled) = wait_for_tree(&handle, &Progress::new(), &|| true);
        assert_eq!(code, Some(0));
        assert!(!cancelled);
        assert!(
            started.elapsed() >= Duration::from_millis(1200),
            "returned after {:?} without waiting for the helper",
            started.elapsed()
        );
    }

    #[test]
    fn waiting_can_be_cancelled() {
        let command = LaunchCommand {
            program: cmd(),
            arguments: "/c ping -n 4 127.0.0.1 >nul".to_owned(),
            quiet: true,
        };
        let handle = launch(&command, false).unwrap().expect("a process handle");
        let progress = Progress::new();
        progress.cancel();
        let started = Instant::now();
        let (_, cancelled) = wait_for_tree(&handle, &progress, &|| true);
        assert!(cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn missing_program_is_reported() {
        let drive = paths::system_drive();
        let command = LaunchCommand {
            program: format!(r"{}Zest No Such Dir\uninstall.exe", drive.display()),
            arguments: String::new(),
            quiet: false,
        };
        assert!(matches!(launch(&command, false), Err(Error::NotFound(_))));
    }

    #[test]
    fn process_creation_times() {
        // Our own process exists and was created before "now".
        let mine = creation_time(std::process::id()).expect("own process is queryable");
        assert!(mine > 0 && mine <= crate::util::filetime_now());
        // A process we start is younger than we are - the ordering the
        // helper-process tracking relies on to tell real children apart.
        let command = LaunchCommand {
            program: cmd(),
            arguments: "/c ping -n 2 127.0.0.1 >nul".to_owned(),
            quiet: true,
        };
        let handle = launch(&command, false).unwrap().expect("a process handle");
        let child = handle_creation_time(handle.0).expect("child is queryable");
        assert!(child >= mine);
        // SAFETY: the handle is valid until `handle` is dropped.
        let child_pid = unsafe { GetProcessId(handle.0) };
        assert_eq!(creation_time(child_pid), Some(child));
        let _ = wait_for_tree(&handle, &Progress::new(), &|| true);
        // Identifiers that are not in use cannot be queried.
        assert_eq!(creation_time(0), None);
    }

    #[test]
    fn removes_and_renames_fake_entries_with_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::new("apps-entry");
        let loc = fake_uninstall_root(&scratch);
        let key = add_entry(&loc, "Stale");
        key.set_value("DisplayName", &"Stale App").unwrap();
        key.set_value("UninstallString", &r"C:\gone\uninstall.exe")
            .unwrap();
        drop(key);
        let app = list_location(&loc, &ListOptions::default()).remove(0);

        let mut backup = RegBackup::at(tmp.path().join("apps.reg"));
        rename_entry(&app, "  Stale App (old)  ", Some(&mut backup)).unwrap();
        let renamed = list_location(&loc, &ListOptions::default()).remove(0);
        assert_eq!(renamed.name, "Stale App (old)");
        assert_eq!(backup.len(), 1);
        assert!(matches!(
            rename_entry(&app, "   ", None),
            Err(Error::Invalid(_))
        ));

        remove_entry(&renamed, Some(&mut backup)).unwrap();
        assert!(list_location(&loc, &ListOptions::default()).is_empty());
        assert_eq!(backup.len(), 2);
        // Removing what is already gone is fine.
        remove_entry(&renamed, None).unwrap();

        // The backup restores the entry (the later export wins: renamed name).
        let saved = backup.save().unwrap().unwrap();
        zest_core::reg::import_reg_file(&saved).unwrap();
        let restored = list_location(&loc, &ListOptions::default()).remove(0);
        assert_eq!(restored.name, "Stale App (old)");
    }

    #[test]
    fn only_uninstall_registrations_are_touched() {
        let mut app = base_app();
        assert!(matches!(remove_entry(&app, None), Err(Error::Invalid(_))));
        app.key = Some(RegPath::hkcu(r"Software\ZestTests\NotAnUninstallKey\Thing"));
        assert!(matches!(remove_entry(&app, None), Err(Error::Other(_))));
        assert!(matches!(
            rename_entry(&app, "x", None),
            Err(Error::Other(_))
        ));
        assert!(is_uninstall_entry(&RegPath::hklm(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\App"
        )));
        assert!(!is_uninstall_entry(&RegPath::hklm(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
        )));
    }
}
