//! Decides which programs may be put to sleep and how much they cost.
//!
//! The protection rules here are the hard safety rail of the optimizer:
//! nothing they flag is ever throttled, suspended, trimmed or terminated,
//! no matter what a sleep rule or a caller asks for. They are evaluated
//! twice – when a snapshot is taken and again, against live data, right
//! before any action runs.

use crate::monitor::{AppUsage, ProcessOwner, WindowState};
use crate::security;
use crate::sys::{self, OwnedHandle, RawProcess};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Why a program must be left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProtectionReason {
    /// A core Windows component (by name, or flagged critical by the kernel).
    SystemProcess,
    /// Runs in the services session (session 0).
    SystemSession,
    /// Runs under the SYSTEM account or a service account.
    SystemAccount,
    /// Belongs to another signed-in user.
    OtherUser,
    /// A protected process; Windows itself forbids tampering with it.
    ProtectedProcess,
    /// Antivirus, firewall or other security software.
    SecuritySoftware,
    /// Could not be inspected, so nothing is assumed about it.
    Inaccessible,
    /// This program, the program that started it, or the terminal hosting it.
    ThisProgram,
    /// The program the user is working with right now.
    Foreground,
}

impl ProtectionReason {
    /// Short explanation suitable for a tooltip.
    pub fn label(self) -> &'static str {
        match self {
            ProtectionReason::SystemProcess => "Windows system component",
            ProtectionReason::SystemSession => "Background service",
            ProtectionReason::SystemAccount => "Runs under a system account",
            ProtectionReason::OtherUser => "Belongs to another user",
            ProtectionReason::ProtectedProcess => "Protected by Windows",
            ProtectionReason::SecuritySoftware => "Security software",
            ProtectionReason::Inaccessible => "Cannot be inspected",
            ProtectionReason::ThisProgram => "Part of this program",
            ProtectionReason::Foreground => "In use right now",
        }
    }

    /// False only for [`ProtectionReason::Foreground`], which ends as soon
    /// as the user switches to another program.
    pub fn is_permanent(self) -> bool {
        self != ProtectionReason::Foreground
    }
}

/// How much a program costs while it sits in the background.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum BackgroundImpact {
    /// Negligible processor, memory and disk use.
    #[default]
    Low,
    /// Noticeable on a busy or battery-powered PC.
    Medium,
    /// A clear drain on processor time, memory or disk.
    High,
}

impl BackgroundImpact {
    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            BackgroundImpact::Low => "Low",
            BackgroundImpact::Medium => "Medium",
            BackgroundImpact::High => "High",
        }
    }
}

/// Broad role of a program, for grouping in a user interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    /// Has at least one application window (visible, minimized or hidden).
    Windowed,
    /// Runs without any application window.
    #[default]
    Background,
    /// Protected for a permanent reason; shown for information only.
    System,
}

impl AppKind {
    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            AppKind::Windowed => "App",
            AppKind::Background => "Background process",
            AppKind::System => "System",
        }
    }
}

/// Windows components that must never be throttled, suspended or ended.
/// Compared against the lower-cased executable name.
const SYSTEM_NAMES: &[&str] = &[
    "system",
    "system idle process",
    "idle",
    "registry",
    "memory compression",
    "secure system",
    "vmmem",
    "vmmemwsl",
    "ntoskrnl.exe",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "svchost.exe",
    "dwm.exe",
    "fontdrvhost.exe",
    "explorer.exe",
    "sihost.exe",
    "ctfmon.exe",
    "audiodg.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "searchindexer.exe",
    "textinputhost.exe",
    "applicationframehost.exe",
    "dllhost.exe",
    "conhost.exe",
    "spoolsv.exe",
    "wudfhost.exe",
    "dashost.exe",
    "wmiprvse.exe",
    "unsecapp.exe",
    "lockapp.exe",
    "logonui.exe",
    "userinit.exe",
    "consent.exe",
    "taskmgr.exe",
    "backgroundtaskhost.exe",
    "smartscreen.exe",
    "sgrmbroker.exe",
    "wlanext.exe",
    "trustedinstaller.exe",
    "tiworker.exe",
    "msiexec.exe",
    "wuauclt.exe",
    "usoclient.exe",
    "mousocoreworker.exe",
    "werfault.exe",
    "wermgr.exe",
    "vmms.exe",
    "vmwp.exe",
    "vmcompute.exe",
    "wslservice.exe",
    "lsm.exe",
    // Shell surfaces that run in the user's session.
    "shellhost.exe",
    "useroobebroker.exe",
    "dataexchangehost.exe",
    "systemsettingsbroker.exe",
    "settingsynchost.exe",
    // Accessibility tools: freezing these can lock a person out of the PC.
    "osk.exe",
    "tabtip.exe",
    "magnify.exe",
    "narrator.exe",
    // Terminals. A frozen terminal freezes every console program inside it,
    // and when this program is started from a command line the terminal
    // window is not necessarily one of its ancestors.
    "windowsterminal.exe",
    "openconsole.exe",
    "wt.exe",
];

/// Components of the security suite built into Windows.
const BUILT_IN_SECURITY_NAMES: &[&str] = &[
    "msmpeng.exe",
    "nissrv.exe",
    "mpdefendercoreservice.exe",
    "mpcmdrun.exe",
    "mssense.exe",
    "securityhealthservice.exe",
    "securityhealthsystray.exe",
    "securityhealthhost.exe",
    "microsoftsecurityapp.exe",
    "sechealthui.exe",
];

/// Lower-cased folder (ending in a backslash) that holds the shell
/// components Windows ships as packaged system apps.
fn system_apps_dir() -> &'static str {
    use std::sync::OnceLock;
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        format!(
            "{}\\systemapps\\",
            zest_core::paths::windows_dir()
                .to_string_lossy()
                .to_lowercase()
                .trim_end_matches('\\')
        )
    })
}

/// Install folders of the security products registered with Windows
/// (lower-cased, each ending in a backslash). Programs running from these
/// folders are always protected. The first call may take a moment.
pub fn security_product_folders() -> Vec<String> {
    security::product_dirs(true).to_vec()
}

/// Programs whose children are independent programs rather than helpers.
/// Used when grouping processes into apps.
pub(crate) const SHELL_HOSTS: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "windowsterminal.exe",
    "openconsole.exe",
    "wt.exe",
    "bash.exe",
    "wsl.exe",
    "wslhost.exe",
];

/// Returns the reason a program is protected purely because of its name.
pub fn system_process_reason(exe_name: &str) -> Option<ProtectionReason> {
    let name = exe_name.trim().to_lowercase();
    if SYSTEM_NAMES.contains(&name.as_str()) {
        Some(ProtectionReason::SystemProcess)
    } else if BUILT_IN_SECURITY_NAMES.contains(&name.as_str()) {
        Some(ProtectionReason::SecuritySoftware)
    } else {
        None
    }
}

/// True when the executable name alone marks a program as untouchable.
pub fn is_system_process_name(exe_name: &str) -> bool {
    system_process_reason(exe_name).is_some()
}

/// Returns the reason `app` is protected, if any.
///
/// This combines the verdict recorded when the snapshot was taken with a
/// fresh evaluation of everything that can be judged from the row itself
/// (names, sessions, owners), so a hand-built or modified [`AppUsage`]
/// cannot smuggle a system process past the guard.
pub fn protection_reason(app: &AppUsage) -> Option<ProtectionReason> {
    let own_pid = std::process::id();
    let mut verdict: Option<ProtectionReason> = None;
    let mut consider = |reason: ProtectionReason| {
        // A permanent reason always wins over the transient foreground one.
        if verdict.is_none_or(|current| !current.is_permanent() && reason.is_permanent()) {
            verdict = Some(reason);
        }
    };
    if let Some(reason) = system_process_reason(&app.exe_name) {
        consider(reason);
    }
    for process in &app.processes {
        if process.pid == 0 || process.pid == 4 {
            consider(ProtectionReason::SystemProcess);
        }
        if process.pid == own_pid {
            consider(ProtectionReason::ThisProgram);
        }
        if let Some(reason) = system_process_reason(&process.name) {
            consider(reason);
        }
        if process.exe_path.as_ref().is_some_and(|p| {
            p.to_string_lossy()
                .to_lowercase()
                .starts_with(system_apps_dir())
        }) {
            consider(ProtectionReason::SystemProcess);
        }
        if process.session_id == 0 {
            consider(ProtectionReason::SystemSession);
        }
        match process.owner {
            ProcessOwner::System | ProcessOwner::Service => {
                consider(ProtectionReason::SystemAccount)
            }
            ProcessOwner::OtherUser => consider(ProtectionReason::OtherUser),
            ProcessOwner::Unknown => consider(ProtectionReason::Inaccessible),
            ProcessOwner::CurrentUser => {}
        }
        if process.window == WindowState::Foreground {
            consider(ProtectionReason::Foreground);
        }
    }
    if let Some(recorded) = app.protection {
        consider(recorded);
    }
    verdict
}

/// True when `app` must not be throttled, suspended, trimmed or ended.
pub fn is_protected(app: &AppUsage) -> bool {
    protection_reason(app).is_some()
}

const MIB: u64 = 1024 * 1024;

/// Estimates the background cost of a program.
///
/// * `avg_cpu_percent` – smoothed processor use as a share of the whole PC.
/// * `memory` – private working set in bytes.
/// * `io_bytes_per_sec` – smoothed read + write throughput.
/// * `total_memory` – installed RAM, used to scale the memory thresholds.
pub fn estimate_impact(
    avg_cpu_percent: f32,
    memory: u64,
    io_bytes_per_sec: u64,
    total_memory: u64,
) -> BackgroundImpact {
    let memory_high = (total_memory / 16).max(768 * MIB);
    let memory_medium = (total_memory / 64).max(200 * MIB);
    if avg_cpu_percent >= 5.0 || memory >= memory_high || io_bytes_per_sec >= 5 * MIB {
        BackgroundImpact::High
    } else if avg_cpu_percent >= 1.0 || memory >= memory_medium || io_bytes_per_sec >= MIB / 2 {
        BackgroundImpact::Medium
    } else {
        BackgroundImpact::Low
    }
}

/// Facts about one process that need a handle to find out. Cached by the
/// sampler for the lifetime of the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcStatic {
    pub exe_path: Option<PathBuf>,
    pub owner: ProcessOwner,
    /// Protection level reported by Windows, `None` for ordinary processes.
    pub protection_level: Option<u32>,
    pub critical: bool,
    pub elevated: bool,
}

impl ProcStatic {
    /// What is assumed about a process that cannot be opened.
    pub fn inaccessible() -> Self {
        ProcStatic {
            exe_path: None,
            owner: ProcessOwner::Unknown,
            protection_level: None,
            critical: false,
            elevated: false,
        }
    }
}

const PROTECTION_LEVEL_NONE: u32 = 0xFFFF_FFFE;
const PROTECTION_LEVEL_ANTIMALWARE_LIGHT: u32 = 3;

fn current_user_sid() -> &'static [u8] {
    use std::sync::OnceLock;
    static SID: OnceLock<Vec<u8>> = OnceLock::new();
    SID.get_or_init(|| {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Security::TOKEN_QUERY;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token = HANDLE::default();
        // SAFETY: the pseudo handle is always valid; the token handle is
        // owned by `OwnedHandle` and closed on drop.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return Vec::new();
        }
        let token = OwnedHandle(token);
        token_user_sid(&token).unwrap_or_default()
    })
}

/// Copies the user SID out of a token.
fn token_user_sid(token: &OwnedHandle) -> Option<Vec<u8>> {
    use windows::Win32::Security::{GetLengthSid, GetTokenInformation, TOKEN_USER, TokenUser};
    // u64 elements keep the TOKEN_USER structure correctly aligned.
    let mut buf = vec![0u64; 64];
    let mut returned = 0u32;
    // SAFETY: the buffer is valid for the byte length passed and aligned for
    // TOKEN_USER; the SID pointer inside it points into the same buffer.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut returned,
        )
        .ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid = user.User.Sid;
        if sid.0.is_null() {
            return None;
        }
        let len = GetLengthSid(sid) as usize;
        if len == 0 || len > 256 {
            return None;
        }
        Some(std::slice::from_raw_parts(sid.0 as *const u8, len).to_vec())
    }
}

fn classify_owner(sid: &[u8]) -> ProcessOwner {
    use windows::Win32::Security::{
        IsWellKnownSid, PSID, WinLocalServiceSid, WinLocalSystemSid, WinNetworkServiceSid,
    };
    let own = current_user_sid();
    if !own.is_empty() && own == sid {
        return ProcessOwner::CurrentUser;
    }
    let psid = PSID(sid.as_ptr() as *mut core::ffi::c_void);
    // SAFETY: `sid` is a complete, valid SID copied from a token.
    unsafe {
        if IsWellKnownSid(psid, WinLocalSystemSid).as_bool() {
            ProcessOwner::System
        } else if IsWellKnownSid(psid, WinLocalServiceSid).as_bool()
            || IsWellKnownSid(psid, WinNetworkServiceSid).as_bool()
        {
            ProcessOwner::Service
        } else {
            ProcessOwner::OtherUser
        }
    }
}

/// Opens the process once (limited query rights only) and collects the facts
/// that cannot be read from the process list.
pub(crate) fn inspect_process(pid: u32, create_time: u64) -> ProcStatic {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{
        GetProcessInformation, IsProcessCritical, OpenProcessToken,
        PROCESS_PROTECTION_LEVEL_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
        ProcessProtectionLevelInfo,
    };

    let Ok(handle) = sys::open_verified(pid, create_time, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return ProcStatic::inaccessible();
    };
    let mut info = ProcStatic {
        exe_path: sys::image_path(&handle),
        ..ProcStatic::inaccessible()
    };

    // SAFETY: valid handle; every out pointer refers to a correctly sized
    // local; the token handle is owned and closed on drop.
    unsafe {
        let mut critical = windows::core::BOOL(0);
        if IsProcessCritical(handle.0, &mut critical).is_ok() {
            info.critical = critical.as_bool();
        }

        let mut level = PROCESS_PROTECTION_LEVEL_INFORMATION::default();
        if GetProcessInformation(
            handle.0,
            ProcessProtectionLevelInfo,
            (&mut level as *mut PROCESS_PROTECTION_LEVEL_INFORMATION).cast(),
            size_of::<PROCESS_PROTECTION_LEVEL_INFORMATION>() as u32,
        )
        .is_ok()
        {
            let value = level.ProtectionLevel.0;
            if value != PROTECTION_LEVEL_NONE {
                info.protection_level = Some(value);
            }
        }

        let mut token = HANDLE::default();
        if OpenProcessToken(handle.0, TOKEN_QUERY, &mut token).is_ok() {
            let token = OwnedHandle(token);
            if let Some(sid) = token_user_sid(&token) {
                info.owner = classify_owner(&sid);
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut returned = 0u32;
            if GetTokenInformation(
                token.0,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
            .is_ok()
            {
                info.elevated = elevation.TokenIsElevated != 0;
            }
        }
    }
    info
}

/// Everything about "here and now" that protection depends on.
#[derive(Clone, Debug, Default)]
pub(crate) struct Context {
    pub own_pid: u32,
    /// The chain of processes that started this one (shell, terminal, ...).
    pub ancestors: HashSet<u32>,
    pub own_session: u32,
    pub foreground: Vec<u32>,
    pub security_dirs: &'static [String],
}

impl Context {
    /// Builds the context from a process listing and the foreground pids.
    ///
    /// `wait_for_security` makes the (one-off) security product lookup
    /// blocking; actions use it, the sampler does not.
    pub fn capture(processes: &[RawProcess], foreground: &[u32], wait_for_security: bool) -> Self {
        let own_pid = std::process::id();
        let by_pid: HashMap<u32, &RawProcess> = processes.iter().map(|p| (p.pid, p)).collect();
        let mut ancestors = HashSet::new();
        let mut current = by_pid.get(&own_pid).copied();
        for _ in 0..32 {
            let Some(child) = current else {
                break;
            };
            let Some(parent) = by_pid.get(&child.parent_pid).copied() else {
                break;
            };
            // A "parent" created after its child is a recycled pid, not a parent.
            if parent.pid == 0
                || parent.pid == child.pid
                || parent.create_time > child.create_time
                || !ancestors.insert(parent.pid)
            {
                break;
            }
            current = Some(parent);
        }
        Context {
            own_pid,
            ancestors,
            own_session: by_pid.get(&own_pid).map_or(u32::MAX, |p| p.session_id),
            foreground: foreground.to_vec(),
            security_dirs: security::product_dirs(wait_for_security),
        }
    }
}

/// The single source of truth for "may this process be touched?".
pub(crate) fn evaluate(
    process: &RawProcess,
    info: &ProcStatic,
    ctx: &Context,
) -> Option<ProtectionReason> {
    let name = process.name.to_lowercase();
    if process.pid == 0 || process.pid == 4 {
        return Some(ProtectionReason::SystemProcess);
    }
    if process.pid == ctx.own_pid || ctx.ancestors.contains(&process.pid) {
        return Some(ProtectionReason::ThisProgram);
    }
    if let Some(reason) = system_process_reason(&name) {
        return Some(reason);
    }
    if process.session_id == 0 {
        return Some(ProtectionReason::SystemSession);
    }
    if info.critical {
        return Some(ProtectionReason::SystemProcess);
    }
    if let Some(level) = info.protection_level {
        return Some(if level == PROTECTION_LEVEL_ANTIMALWARE_LIGHT {
            ProtectionReason::SecuritySoftware
        } else {
            ProtectionReason::ProtectedProcess
        });
    }
    if let Some(path) = &info.exe_path {
        let path = path.to_string_lossy().to_lowercase();
        if path.starts_with(system_apps_dir()) {
            return Some(ProtectionReason::SystemProcess);
        }
        if security::is_security_path(&path, ctx.security_dirs) {
            return Some(ProtectionReason::SecuritySoftware);
        }
    }
    match info.owner {
        ProcessOwner::System | ProcessOwner::Service => {
            return Some(ProtectionReason::SystemAccount);
        }
        ProcessOwner::OtherUser => return Some(ProtectionReason::OtherUser),
        ProcessOwner::Unknown => return Some(ProtectionReason::Inaccessible),
        ProcessOwner::CurrentUser => {}
    }
    if ctx.own_session != u32::MAX && process.session_id != ctx.own_session {
        return Some(ProtectionReason::OtherUser);
    }
    if ctx.foreground.contains(&process.pid) {
        return Some(ProtectionReason::Foreground);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pid: u32, parent: u32, name: &str, session: u32, created: u64) -> RawProcess {
        RawProcess {
            pid,
            parent_pid: parent,
            name: name.to_owned(),
            session_id: session,
            create_time: created,
            ..RawProcess::default()
        }
    }

    fn user_info() -> ProcStatic {
        ProcStatic {
            exe_path: Some(PathBuf::from(r"C:\Program Files\Vendor\App\app.exe")),
            owner: ProcessOwner::CurrentUser,
            protection_level: None,
            critical: false,
            elevated: false,
        }
    }

    fn ctx() -> Context {
        Context {
            own_pid: 1000,
            ancestors: HashSet::from([900, 800]),
            own_session: 1,
            foreground: vec![5000],
            security_dirs: &[],
        }
    }

    #[test]
    fn name_deny_list() {
        for name in [
            "System",
            "Registry",
            "smss.exe",
            "csrss.exe",
            "wininit.exe",
            "winlogon.exe",
            "services.exe",
            "lsass.exe",
            "svchost.exe",
            "dwm.exe",
            "fontdrvhost.exe",
            "explorer.exe",
            "sihost.exe",
            "ctfmon.exe",
            "audiodg.exe",
            "SVCHOST.EXE",
            "Memory Compression",
            "conhost.exe",
            "RuntimeBroker.exe",
        ] {
            assert_eq!(
                system_process_reason(name),
                Some(ProtectionReason::SystemProcess),
                "{name}"
            );
        }
        for name in ["MsMpEng.exe", "NisSrv.exe", "SecurityHealthService.exe"] {
            assert_eq!(
                system_process_reason(name),
                Some(ProtectionReason::SecuritySoftware)
            );
        }
        for name in ["chrome.exe", "spotify.exe", "notepad.exe", "ping.exe", ""] {
            assert_eq!(system_process_reason(name), None, "{name}");
            assert!(!is_system_process_name(name));
        }
    }

    #[test]
    fn evaluation_order_and_reasons() {
        let c = ctx();
        let ok = user_info();
        // An ordinary user program is not protected.
        assert_eq!(evaluate(&raw(2000, 1, "app.exe", 1, 10), &ok, &c), None);
        // Kernel pseudo processes.
        assert_eq!(
            evaluate(&raw(4, 0, "anything.exe", 1, 10), &ok, &c),
            Some(ProtectionReason::SystemProcess)
        );
        // This program and its ancestors.
        for pid in [1000, 900, 800] {
            assert_eq!(
                evaluate(&raw(pid, 1, "app.exe", 1, 10), &ok, &c),
                Some(ProtectionReason::ThisProgram)
            );
        }
        // Names.
        assert_eq!(
            evaluate(&raw(2000, 1, "Explorer.EXE", 1, 10), &ok, &c),
            Some(ProtectionReason::SystemProcess)
        );
        // Services session.
        assert_eq!(
            evaluate(&raw(2000, 1, "app.exe", 0, 10), &ok, &c),
            Some(ProtectionReason::SystemSession)
        );
        // Kernel-critical flag.
        let critical = ProcStatic {
            critical: true,
            ..user_info()
        };
        assert_eq!(
            evaluate(&raw(2000, 1, "app.exe", 1, 10), &critical, &c),
            Some(ProtectionReason::SystemProcess)
        );
        // Protected processes.
        let ppl = ProcStatic {
            protection_level: Some(1),
            ..user_info()
        };
        assert_eq!(
            evaluate(&raw(2000, 1, "app.exe", 1, 10), &ppl, &c),
            Some(ProtectionReason::ProtectedProcess)
        );
        let antimalware = ProcStatic {
            protection_level: Some(PROTECTION_LEVEL_ANTIMALWARE_LIGHT),
            ..user_info()
        };
        assert_eq!(
            evaluate(&raw(2000, 1, "app.exe", 1, 10), &antimalware, &c),
            Some(ProtectionReason::SecuritySoftware)
        );
        // Owners.
        for (owner, reason) in [
            (ProcessOwner::System, ProtectionReason::SystemAccount),
            (ProcessOwner::Service, ProtectionReason::SystemAccount),
            (ProcessOwner::OtherUser, ProtectionReason::OtherUser),
            (ProcessOwner::Unknown, ProtectionReason::Inaccessible),
        ] {
            let info = ProcStatic {
                owner,
                ..user_info()
            };
            assert_eq!(
                evaluate(&raw(2000, 1, "app.exe", 1, 10), &info, &c),
                Some(reason)
            );
        }
        // Another interactive session.
        assert_eq!(
            evaluate(&raw(2000, 1, "app.exe", 2, 10), &ok, &c),
            Some(ProtectionReason::OtherUser)
        );
        // Foreground.
        assert_eq!(
            evaluate(&raw(5000, 1, "app.exe", 1, 10), &ok, &c),
            Some(ProtectionReason::Foreground)
        );
    }

    #[test]
    fn terminals_shell_apps_and_security_folders() {
        let mut c = ctx();
        let ok = user_info();
        // Terminals and accessibility tools are protected wherever they run.
        for name in [
            "WindowsTerminal.exe",
            "OpenConsole.exe",
            "osk.exe",
            "Narrator.exe",
            "ShellHost.exe",
        ] {
            assert_eq!(
                evaluate(&raw(2000, 1, name, 1, 10), &ok, &c),
                Some(ProtectionReason::SystemProcess),
                "{name}"
            );
        }

        // Packaged shell components are recognised by where they live.
        let shell_app = ProcStatic {
            exe_path: Some(
                zest_core::paths::windows_dir()
                    .join(r"SystemApps\Some.Shell.Component_abc\Component.exe"),
            ),
            ..user_info()
        };
        assert_eq!(
            evaluate(&raw(2000, 1, "Component.exe", 1, 10), &shell_app, &c),
            Some(ProtectionReason::SystemProcess)
        );
        // ... but an ordinary tool elsewhere in the Windows folder is not.
        let tool = ProcStatic {
            exe_path: Some(zest_core::paths::system32_dir().join("ping.exe")),
            ..user_info()
        };
        assert_eq!(evaluate(&raw(2000, 1, "ping.exe", 1, 10), &tool, &c), None);

        // Anything running from a registered security product's folder.
        let dirs: &'static [String] =
            Box::leak(vec![r"c:\program files\vendor\app\".to_owned()].into_boxed_slice());
        c.security_dirs = dirs;
        assert_eq!(
            evaluate(&raw(2000, 1, "app.exe", 1, 10), &ok, &c),
            Some(ProtectionReason::SecuritySoftware)
        );
    }

    #[test]
    fn row_level_check_catches_doctored_apps() {
        use crate::monitor::ProcessUsage;
        let process = |name: &str, session: u32, owner: ProcessOwner| ProcessUsage {
            pid: 4242,
            name: name.to_owned(),
            session_id: session,
            owner,
            ..ProcessUsage::default()
        };
        let app = |exe: &str, p: ProcessUsage| AppUsage {
            exe_name: exe.to_owned(),
            processes: vec![p],
            ..AppUsage::default()
        };
        let user = ProcessOwner::CurrentUser;
        assert_eq!(
            protection_reason(&app("app.exe", process("app.exe", 1, user))),
            None
        );
        assert!(!is_protected(&app("app.exe", process("app.exe", 1, user))));
        // A system name on the app or on any of its processes.
        assert!(is_protected(&app("lsass.exe", process("app.exe", 1, user))));
        assert!(is_protected(&app("app.exe", process("csrss.exe", 1, user))));
        // Session 0, foreign owners, uninspectable processes.
        assert_eq!(
            protection_reason(&app("app.exe", process("app.exe", 0, user))),
            Some(ProtectionReason::SystemSession)
        );
        assert_eq!(
            protection_reason(&app("app.exe", process("app.exe", 1, ProcessOwner::System))),
            Some(ProtectionReason::SystemAccount)
        );
        assert_eq!(
            protection_reason(&app(
                "app.exe",
                process("app.exe", 1, ProcessOwner::Unknown)
            )),
            Some(ProtectionReason::Inaccessible)
        );
        // This very process.
        let mut me = process("app.exe", 1, user);
        me.pid = std::process::id();
        assert_eq!(
            protection_reason(&app("app.exe", me)),
            Some(ProtectionReason::ThisProgram)
        );
        // A permanent reason outranks the transient foreground one.
        let mut front = process("app.exe", 1, ProcessOwner::System);
        front.window = WindowState::Foreground;
        assert_eq!(
            protection_reason(&app("app.exe", front)),
            Some(ProtectionReason::SystemAccount)
        );
        let mut front = process("app.exe", 1, user);
        front.window = WindowState::Foreground;
        assert_eq!(
            protection_reason(&app("app.exe", front)),
            Some(ProtectionReason::Foreground)
        );
        assert!(!ProtectionReason::Foreground.is_permanent());
    }

    #[test]
    fn ancestors_stop_at_recycled_pids() {
        // 30 -> 20 -> 10, but "10" was created after 20: a recycled pid.
        let me = std::process::id();
        let procs = vec![
            raw(me, 20, "self.exe", 1, 300),
            raw(20, 10, "shell.exe", 1, 200),
            raw(10, 0, "unrelated.exe", 1, 250),
        ];
        let c = Context::capture(&procs, &[], false);
        assert!(c.ancestors.contains(&20));
        assert!(!c.ancestors.contains(&10));
        assert_eq!(c.own_session, 1);
    }

    #[test]
    fn live_context_contains_real_parent() {
        let procs = sys::ProcessLister::new().list().unwrap();
        let me = procs.iter().find(|p| p.pid == std::process::id()).unwrap();
        let c = Context::capture(&procs, &[], false);
        assert!(
            c.ancestors.contains(&me.parent_pid),
            "parent must be an ancestor"
        );
        assert!(!c.ancestors.contains(&me.pid));
    }

    #[test]
    fn inspecting_this_process() {
        let procs = sys::ProcessLister::new().list().unwrap();
        let me = procs.iter().find(|p| p.pid == std::process::id()).unwrap();
        let info = inspect_process(me.pid, me.create_time);
        assert_eq!(info.owner, ProcessOwner::CurrentUser);
        assert!(info.exe_path.as_ref().is_some_and(|p| p.is_file()));
        assert!(!info.critical);
        assert_eq!(info.protection_level, None);
        assert_eq!(info.elevated, zest_core::elevation::is_elevated());
        // A wrong start time means "not that process": nothing is assumed.
        assert_eq!(
            inspect_process(me.pid, me.create_time + 1),
            ProcStatic::inaccessible()
        );
        // The kernel's own process cannot be opened.
        assert_eq!(inspect_process(4, 0).owner, ProcessOwner::Unknown);
    }

    #[test]
    fn impact_thresholds() {
        let ram = 16 * 1024 * MIB;
        assert_eq!(
            estimate_impact(0.0, 50 * MIB, 0, ram),
            BackgroundImpact::Low
        );
        assert_eq!(
            estimate_impact(1.5, 50 * MIB, 0, ram),
            BackgroundImpact::Medium
        );
        assert_eq!(
            estimate_impact(0.0, 300 * MIB, 0, ram),
            BackgroundImpact::Medium
        );
        assert_eq!(
            estimate_impact(0.0, 50 * MIB, MIB, ram),
            BackgroundImpact::Medium
        );
        assert_eq!(
            estimate_impact(6.0, 50 * MIB, 0, ram),
            BackgroundImpact::High
        );
        assert_eq!(
            estimate_impact(0.0, 1100 * MIB, 0, ram),
            BackgroundImpact::High
        );
        assert_eq!(
            estimate_impact(0.0, 50 * MIB, 6 * MIB, ram),
            BackgroundImpact::High
        );
        // Small machines never go below the absolute floors.
        assert_eq!(
            estimate_impact(0.0, 150 * MIB, 0, 2 * 1024 * MIB),
            BackgroundImpact::Low
        );
        assert!(BackgroundImpact::High > BackgroundImpact::Medium);
    }
}
