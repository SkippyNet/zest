//! Reversible control over background programs.
//!
//! Two ways of putting a program to sleep are offered:
//!
//! * **Efficiency mode** – Windows power throttling plus the lowest
//!   priority class. The program keeps running and responding, but yields
//!   the processor to everything else.
//! * **Suspend** – every process of the program is frozen. It uses no
//!   processor time at all until it is resumed.
//!
//! Everything this module changes is tracked in a [`ControlState`] and put
//! back by [`ControlState::resume_all`], which also runs when the state is
//! dropped. With a journal file the tracking survives a crash, so a later
//! run can still wake whatever was left asleep.
//!
//! No function here ever touches a protected program (see
//! [`crate::classify`]); the check is repeated against live data immediately
//! before each action.

use crate::classify::{self, Context, ProtectionReason};
use crate::monitor::{AppUsage, SystemSnapshot, WindowState};
use crate::sys::{self, OpenError, OwnedHandle, ProcessLister, RawProcess};
use crate::windowing;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessInformation, IDLE_PRIORITY_CLASS, PROCESS_ACCESS_RIGHTS,
    PROCESS_CREATION_FLAGS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_SUSPEND_RESUME, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ProcessPowerThrottling,
    SetPriorityClass, SetProcessInformation, TerminateProcess, WaitForSingleObject,
};
use zest_core::{Error, IoContext, Result};

/// How a program is kept quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SleepMode {
    /// Power throttling plus lowest priority; the program stays responsive.
    Efficiency,
    /// All processes frozen until resumed. A frozen program cannot react to
    /// anything – its tray icon, notifications and background sync stop
    /// until it is woken or started again.
    Suspend,
}

impl SleepMode {
    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            SleepMode::Efficiency => "Efficiency mode",
            SleepMode::Suspend => "Suspended",
        }
    }
}

/// Who put a program to sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SleepOrigin {
    /// The user, through a direct action.
    Manual,
    /// A [`SleepRule`].
    Rule,
}

/// A standing instruction to keep one program asleep in the background.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepRule {
    /// Executable name (`spotify.exe`, the extension is optional) or a full
    /// executable path. Matching ignores case.
    pub target: String,
    /// How the program is kept quiet.
    pub mode: SleepMode,
    /// Wake the program while the user is working in it, and put it back to
    /// sleep once they have moved on.
    pub wake_on_foreground: bool,
    /// Disabled rules are kept but ignored.
    pub enabled: bool,
}

impl SleepRule {
    /// An enabled rule that wakes the program when it comes to the foreground.
    pub fn new(target: impl Into<String>, mode: SleepMode) -> Self {
        SleepRule {
            target: target.into(),
            mode,
            wake_on_foreground: true,
            enabled: true,
        }
    }

    /// A rule for the program shown in `app` (matches its full path when
    /// known, otherwise its executable name).
    pub fn for_app(app: &AppUsage, mode: SleepMode) -> Self {
        let target = app
            .exe_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| app.exe_name.clone());
        Self::new(target, mode)
    }

    fn matches_program(&self, exe_name: &str, exe_path: Option<&Path>) -> bool {
        let target = self
            .target
            .trim()
            .trim_matches('"')
            .replace('/', "\\")
            .to_lowercase();
        if target.is_empty() {
            return false;
        }
        if target.contains('\\') {
            exe_path.is_some_and(|p| p.to_string_lossy().to_lowercase() == target)
        } else {
            let name = exe_name.to_lowercase();
            name == target || name == format!("{target}.exe")
        }
    }

    /// True when this rule is enabled and applies to `app`.
    pub fn matches(&self, app: &AppUsage) -> bool {
        self.enabled && self.matches_program(&app.exe_name, app.exe_path.as_deref())
    }
}

/// The user's list of sleep rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepRules {
    /// Rules in priority order; the first enabled match wins.
    pub rules: Vec<SleepRule>,
}

impl SleepRules {
    /// The first enabled rule that applies to `app`.
    pub fn find(&self, app: &AppUsage) -> Option<&SleepRule> {
        self.rules.iter().find(|r| r.matches(app))
    }

    /// Adds `rule`, replacing an existing rule with the same target.
    pub fn upsert(&mut self, rule: SleepRule) {
        match self
            .rules
            .iter_mut()
            .find(|r| r.target.eq_ignore_ascii_case(&rule.target))
        {
            Some(existing) => *existing = rule,
            None => self.rules.push(rule),
        }
    }

    /// Removes the rule with this target. Returns true when one was removed.
    pub fn remove(&mut self, target: &str) -> bool {
        let before = self.rules.len();
        self.rules
            .retain(|r| !r.target.eq_ignore_ascii_case(target));
        self.rules.len() != before
    }

    /// Number of rules (enabled or not).
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True when there are no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// One process an action could not be applied to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionFailure {
    /// Process id.
    pub pid: u32,
    /// Executable name.
    pub name: String,
    /// What went wrong.
    pub error: String,
    /// True when running as administrator would make the action possible.
    pub needs_admin: bool,
}

/// Per-process outcome of an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReport {
    /// Processes the action was applied to.
    pub succeeded: Vec<u32>,
    /// Processes it could not be applied to.
    pub failed: Vec<ActionFailure>,
}

impl ActionReport {
    /// True when nothing failed.
    pub fn is_success(&self) -> bool {
        self.failed.is_empty()
    }

    /// True when at least one failure was caused by missing administrator rights.
    pub fn needs_admin(&self) -> bool {
        self.failed.iter().any(|f| f.needs_admin)
    }

    /// Adds another report to this one.
    pub fn merge(&mut self, other: ActionReport) {
        self.succeeded.extend(other.succeeded);
        self.failed.extend(other.failed);
    }

    fn fail(&mut self, pid: u32, name: &str, error: impl std::fmt::Display, needs_admin: bool) {
        self.failed.push(ActionFailure {
            pid,
            name: name.to_owned(),
            error: error.to_string(),
            needs_admin,
        });
    }

    fn fail_open(&mut self, pid: u32, name: &str, error: &OpenError) {
        match error {
            OpenError::Exited => {}
            OpenError::Denied => self.fail(
                pid,
                name,
                "access denied (administrator rights required)",
                true,
            ),
            OpenError::Other(message) => self.fail(pid, name, message, false),
        }
    }
}

/// A program this [`ControlState`] is currently keeping asleep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepingApp {
    /// [`AppUsage::id`] of the program.
    pub app_id: String,
    /// Friendly name.
    pub name: String,
    /// How it is kept quiet.
    pub mode: SleepMode,
    /// Who put it to sleep.
    pub origin: SleepOrigin,
    /// The processes affected.
    pub pids: Vec<u32>,
    /// When it was put to sleep.
    pub since: SystemTime,
}

/// Why a sleeping program was woken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WakeReason {
    /// The user asked for it.
    Manual,
    /// The user switched to the program.
    Foreground,
    /// The program was started again while frozen.
    Relaunched,
    /// The rule that kept it asleep was removed, disabled or changed.
    RuleRemoved,
    /// All of its processes have exited.
    Exited,
    /// Everything is being restored (shutdown).
    Shutdown,
}

/// What happened to a program during a tick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlEventKind {
    /// It was put to sleep.
    Slept(SleepMode),
    /// It was woken.
    Woken(WakeReason),
    /// A rule could not be applied.
    Failed(String),
}

/// One state change reported by [`ControlState::tick`] or [`apply_rules`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEvent {
    /// [`AppUsage::id`] of the program.
    pub app_id: String,
    /// Friendly name.
    pub app_name: String,
    /// What happened.
    pub kind: ControlEventKind,
}

/// Result of [`trim_memory`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimReport {
    /// Processes whose working set was trimmed.
    pub processes: u32,
    /// Processes that could not be trimmed.
    pub failed: u32,
    /// Combined working set before, in bytes.
    pub before: u64,
    /// Combined working set right after, in bytes. Windows pages memory
    /// back in as the program touches it, so this number grows again; the
    /// difference is not "freed" memory in any lasting sense.
    pub after: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TrackedProcess {
    pid: u32,
    start: u64,
    name: String,
    /// Priority class before efficiency mode was applied.
    prev_priority: Option<u32>,
    /// Power throttling `(control mask, state mask)` before it was applied.
    prev_throttle: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SleepEntry {
    app_id: String,
    name: String,
    exe_name: String,
    exe_path: Option<PathBuf>,
    mode: SleepMode,
    origin: SleepOrigin,
    wake_on_foreground: bool,
    since: SystemTime,
    processes: Vec<TrackedProcess>,
    /// Processes of the program that could not be put to sleep
    /// (`(pid, start)`); remembered so they are not retried every tick.
    #[serde(default)]
    skipped: Vec<(u32, u64)>,
}

impl SleepEntry {
    fn tracks(&self, pid: u32, start: u64) -> bool {
        self.processes
            .iter()
            .any(|t| t.pid == pid && t.start == start)
    }
}

/// Where the journal of a component is kept by default:
/// `<data dir>\performance\sleeping-<component>.json`.
///
/// Each running program (the window, the background agent, the command line)
/// must use its own component name so they do not adopt each other's work.
pub fn default_journal_path(component: &str) -> PathBuf {
    let safe: String = component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    zest_core::paths::data_dir()
        .join("performance")
        .join(format!("sleeping-{safe}.json"))
}

/// Tracks every program this process has put to sleep so that each change
/// can be undone.
///
/// Dropping the state wakes everything (unless [`ControlState::restore_on_drop`]
/// is cleared), so nothing stays frozen when the owner exits normally. For
/// abnormal exits, create the state with [`ControlState::with_journal`].
pub struct ControlState {
    entries: BTreeMap<String, SleepEntry>,
    journal: Option<PathBuf>,
    snoozed: HashMap<String, Instant>,
    last_foreground: HashMap<String, Instant>,
    /// How long after leaving the foreground a program is still considered
    /// in use by [`apply_rules`]. Default: 30 seconds.
    pub foreground_grace: Duration,
    /// How long rules leave a program alone after the user woke it by hand.
    /// Default: 10 minutes.
    pub manual_wake_hold: Duration,
    /// Wake everything when this state is dropped. Default: true.
    pub restore_on_drop: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ControlState {
    fn drop(&mut self) {
        if self.restore_on_drop && !self.entries.is_empty() {
            let report = self.resume_all();
            if !report.is_success() {
                log::warn!(
                    "{} sleeping process(es) could not be woken on shutdown",
                    report.failed.len()
                );
            }
        }
    }
}

impl ControlState {
    /// A state without a journal.
    pub fn new() -> Self {
        ControlState {
            entries: BTreeMap::new(),
            journal: None,
            snoozed: HashMap::new(),
            last_foreground: HashMap::new(),
            foreground_grace: Duration::from_secs(30),
            manual_wake_hold: Duration::from_secs(600),
            restore_on_drop: true,
        }
    }

    /// A state that records its work in `journal`.
    ///
    /// Programs listed in an existing journal (left behind by a run that
    /// ended abnormally) are adopted if they are still running, so they show
    /// up in [`ControlState::sleeping`] and can be woken.
    pub fn with_journal(journal: PathBuf) -> Self {
        let mut state = Self::new();
        if let Ok(text) = std::fs::read_to_string(&journal) {
            match serde_json::from_str::<Vec<SleepEntry>>(&text) {
                Ok(entries) => {
                    for mut entry in entries {
                        entry.processes.retain(|t| {
                            sys::open_verified(t.pid, t.start, PROCESS_QUERY_LIMITED_INFORMATION)
                                .is_ok()
                        });
                        if !entry.processes.is_empty() {
                            state.entries.insert(entry.app_id.clone(), entry);
                        }
                    }
                }
                Err(e) => log::warn!(
                    "ignoring unreadable sleep journal {}: {e}",
                    journal.display()
                ),
            }
        }
        state.journal = Some(journal);
        state.save_journal();
        state
    }

    fn save_journal(&self) {
        let Some(path) = &self.journal else {
            return;
        };
        let result: Result<()> = (|| {
            if self.entries.is_empty() {
                return match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e).ctx("removing the sleep journal"),
                };
            }
            if let Some(dir) = path.parent() {
                zest_core::paths::ensure_dir(dir)?;
            }
            let entries: Vec<&SleepEntry> = self.entries.values().collect();
            let text = serde_json::to_string_pretty(&entries)?;
            let temp = path.with_extension("json.tmp");
            std::fs::write(&temp, text).ctx("writing the sleep journal")?;
            std::fs::rename(&temp, path).ctx("replacing the sleep journal")
        })();
        if let Err(e) = result {
            log::warn!("could not update sleep journal {}: {e}", path.display());
        }
    }

    /// Programs currently kept asleep by this state.
    pub fn sleeping(&self) -> Vec<SleepingApp> {
        self.entries
            .values()
            .map(|e| SleepingApp {
                app_id: e.app_id.clone(),
                name: e.name.clone(),
                mode: e.mode,
                origin: e.origin,
                pids: e.processes.iter().map(|t| t.pid).collect(),
                since: e.since,
            })
            .collect()
    }

    /// How `app_id` is being kept asleep by this state, if it is.
    pub fn sleep_mode(&self, app_id: &str) -> Option<SleepMode> {
        self.entries.get(app_id).map(|e| e.mode)
    }

    /// True when this state is keeping `app_id` asleep.
    pub fn is_sleeping(&self, app_id: &str) -> bool {
        self.entries.contains_key(app_id)
    }

    /// Puts `app` to sleep on the user's behalf.
    ///
    /// A suspended program wakes automatically when the user switches to
    /// it; efficiency mode stays on until it is turned off.
    ///
    /// Suspension is all-or-nothing: if any process cannot be frozen, the
    /// ones already frozen are resumed and the report lists the failure.
    /// Fails outright for protected programs.
    pub fn sleep(&mut self, app: &AppUsage, mode: SleepMode) -> Result<ActionReport> {
        let wake_on_foreground = mode == SleepMode::Suspend;
        self.sleep_as(app, mode, SleepOrigin::Manual, wake_on_foreground)
    }

    /// Freezes every process of `app`. See [`ControlState::sleep`].
    pub fn suspend(&mut self, app: &AppUsage) -> Result<ActionReport> {
        self.sleep(app, SleepMode::Suspend)
    }

    /// Turns efficiency mode on or off for every process of `app`.
    ///
    /// Turning it off restores the priority class and throttling state each
    /// process had before.
    pub fn set_efficiency_mode(&mut self, app: &AppUsage, enabled: bool) -> Result<ActionReport> {
        if enabled {
            self.sleep(app, SleepMode::Efficiency)
        } else if self.sleep_mode(&app.id) == Some(SleepMode::Efficiency) {
            Ok(self.wake(&app.id))
        } else {
            Ok(ActionReport::default())
        }
    }

    /// Wakes a program the user asked for (either mode). Rules leave it
    /// alone for [`ControlState::manual_wake_hold`] afterwards.
    pub fn wake(&mut self, app_id: &str) -> ActionReport {
        let report = self.wake_entry(app_id);
        self.snoozed
            .insert(app_id.to_owned(), Instant::now() + self.manual_wake_hold);
        report
    }

    /// Resumes a suspended program. Same as [`ControlState::wake`].
    pub fn resume(&mut self, app_id: &str) -> ActionReport {
        self.wake(app_id)
    }

    /// Wakes everything this state is keeping asleep.
    pub fn resume_all(&mut self) -> ActionReport {
        let mut report = ActionReport::default();
        let ids: Vec<String> = self.entries.keys().cloned().collect();
        for id in ids {
            report.merge(self.wake_entry(&id));
        }
        report
    }

    fn sleep_as(
        &mut self,
        app: &AppUsage,
        mode: SleepMode,
        origin: SleepOrigin,
        wake_on_foreground: bool,
    ) -> Result<ActionReport> {
        // Switching modes: undo the old one first.
        if let Some(existing) = self.entries.get(&app.id)
            && existing.mode != mode
        {
            self.wake_entry(&app.id);
        }

        let targets = Live::capture()?.targets(app)?;
        if targets.is_empty() {
            return Err(Error::NotFound(format!(
                "{} is no longer running",
                app.name
            )));
        }
        let already: Vec<TrackedProcess> = self
            .entries
            .get(&app.id)
            .map(|e| e.processes.clone())
            .unwrap_or_default();
        let is_tracked = |t: &Target| already.iter().any(|a| a.pid == t.pid && a.start == t.start);

        let mut report = ActionReport::default();
        let mut newly: Vec<TrackedProcess> = Vec::new();
        match mode {
            SleepMode::Suspend => {
                let mut frozen: Vec<OwnedHandle> = Vec::new();
                for target in targets.iter().filter(|t| !is_tracked(t)) {
                    let access = PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION;
                    match sys::open_verified(target.pid, target.start, access) {
                        Ok(handle) => match sys::suspend_process(&handle) {
                            Ok(()) => {
                                frozen.push(handle);
                                newly.push(target.tracked(None, None));
                            }
                            Err(e) => {
                                report.fail(target.pid, &target.name, &e, e.is_access_denied())
                            }
                        },
                        Err(e) => report.fail_open(target.pid, &target.name, &e),
                    }
                    if !report.failed.is_empty() {
                        break;
                    }
                }
                if !report.failed.is_empty() {
                    // Roll back: a half-frozen program is worse than an awake one.
                    for handle in &frozen {
                        let _ = sys::resume_process(handle);
                    }
                    return Ok(report);
                }
            }
            SleepMode::Efficiency => {
                for target in targets.iter().filter(|t| !is_tracked(t)) {
                    let access = PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION;
                    match sys::open_verified(target.pid, target.start, access) {
                        Ok(handle) => match apply_efficiency(&handle) {
                            Ok((priority, throttle)) => {
                                newly.push(target.tracked(Some(priority), throttle));
                            }
                            Err(e) => {
                                report.fail(target.pid, &target.name, &e, e.is_access_denied())
                            }
                        },
                        Err(e) => report.fail_open(target.pid, &target.name, &e),
                    }
                }
            }
        }

        report.succeeded.extend(already.iter().map(|t| t.pid));
        report.succeeded.extend(newly.iter().map(|t| t.pid));
        if !newly.is_empty() {
            let entry = self
                .entries
                .entry(app.id.clone())
                .or_insert_with(|| SleepEntry {
                    app_id: app.id.clone(),
                    name: app.name.clone(),
                    exe_name: app.exe_name.clone(),
                    exe_path: app.exe_path.clone(),
                    mode,
                    origin,
                    wake_on_foreground,
                    since: SystemTime::now(),
                    processes: Vec::new(),
                    skipped: Vec::new(),
                });
            entry.processes.extend(newly);
            self.save_journal();
        }
        Ok(report)
    }

    fn wake_entry(&mut self, app_id: &str) -> ActionReport {
        let mut report = ActionReport::default();
        let Some(mut entry) = self.entries.remove(app_id) else {
            return report;
        };
        let mut still_asleep = Vec::new();
        for tracked in std::mem::take(&mut entry.processes) {
            let access = match entry.mode {
                SleepMode::Suspend => PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION,
                SleepMode::Efficiency => {
                    PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION
                }
            };
            match sys::open_verified(tracked.pid, tracked.start, access) {
                Ok(handle) => {
                    let outcome = match entry.mode {
                        SleepMode::Suspend => sys::resume_process(&handle),
                        SleepMode::Efficiency => restore_efficiency(&handle, &tracked),
                    };
                    match outcome {
                        Ok(()) => report.succeeded.push(tracked.pid),
                        Err(e) => {
                            report.fail(tracked.pid, &tracked.name, &e, e.is_access_denied());
                            still_asleep.push(tracked);
                        }
                    }
                }
                // Already gone: nothing left to undo.
                Err(OpenError::Exited) => {}
                Err(e) => {
                    report.fail_open(tracked.pid, &tracked.name, &e);
                    still_asleep.push(tracked);
                }
            }
        }
        if !still_asleep.is_empty() {
            // Keep what could not be woken so the user can try again.
            entry.processes = still_asleep;
            self.entries.insert(app_id.to_owned(), entry);
        }
        self.save_journal();
        report
    }

    /// Keeps the state in step with reality. Call once per snapshot.
    ///
    /// * Forgets processes that have exited.
    /// * Wakes programs the user has switched to (where configured).
    /// * Wakes a frozen program when another copy of it is started, since a
    ///   frozen single-instance program could never answer.
    /// * Extends efficiency mode to helper processes started since.
    pub fn tick(&mut self, snapshot: &SystemSnapshot) -> Vec<ControlEvent> {
        let now = Instant::now();
        let mut events = Vec::new();
        for app in &snapshot.apps {
            if app.window == WindowState::Foreground {
                self.last_foreground.insert(app.id.clone(), now);
            }
        }
        self.snoozed.retain(|_, until| *until > now);
        self.last_foreground
            .retain(|_, at| now.duration_since(*at) < Duration::from_secs(3600));

        let alive: HashSet<(u32, u64)> = snapshot
            .apps
            .iter()
            .flat_map(|a| a.processes.iter().map(|p| (p.pid, p.start_ticks)))
            .collect();
        let ids: Vec<String> = self.entries.keys().cloned().collect();
        let mut dirty = false;
        for id in ids {
            let Some(entry) = self.entries.get_mut(&id) else {
                continue;
            };
            let before = entry.processes.len();
            entry
                .processes
                .retain(|t| alive.contains(&(t.pid, t.start)));
            dirty |= entry.processes.len() != before;
            let name = entry.name.clone();
            if entry.processes.is_empty() {
                self.entries.remove(&id);
                dirty = true;
                events.push(event(
                    &id,
                    &name,
                    ControlEventKind::Woken(WakeReason::Exited),
                ));
                continue;
            }

            // The program may have been regrouped; fall back to a tracked pid.
            let first_pid = entry.processes[0].pid;
            let app = snapshot
                .app(&id)
                .or_else(|| snapshot.app_for_pid(first_pid));
            let foreground = entry
                .processes
                .iter()
                .any(|t| snapshot.foreground_pids.contains(&t.pid))
                || app.is_some_and(|a| a.window == WindowState::Foreground);
            if entry.wake_on_foreground && foreground {
                self.wake_entry(&id);
                events.push(event(
                    &id,
                    &name,
                    ControlEventKind::Woken(WakeReason::Foreground),
                ));
                continue;
            }

            let Some(app) = app else {
                continue;
            };
            entry.skipped.retain(|key| alive.contains(key));
            let untracked: Vec<(u32, u64, bool)> = app
                .processes
                .iter()
                .filter(|p| !entry.tracks(p.pid, p.start_ticks))
                .filter(|p| !entry.skipped.contains(&(p.pid, p.start_ticks)))
                .map(|p| (p.pid, p.start_ticks, p.suspended))
                .collect();
            match entry.mode {
                SleepMode::Suspend => {
                    if untracked.iter().any(|(_, _, suspended)| !suspended) {
                        self.wake_entry(&id);
                        // Give the freshly started copy time to settle.
                        self.snoozed.insert(id.clone(), now + self.foreground_grace);
                        events.push(event(
                            &id,
                            &name,
                            ControlEventKind::Woken(WakeReason::Relaunched),
                        ));
                    }
                }
                SleepMode::Efficiency => {
                    // Helper processes started since are not reliably
                    // throttled by inheritance; extend the mode to them.
                    // While the program is in the foreground the live guard
                    // would refuse anyway.
                    if !untracked.is_empty()
                        && app.id == id
                        && app.window != WindowState::Foreground
                    {
                        let origin = entry.origin;
                        let wake_on_foreground = entry.wake_on_foreground;
                        let outcome =
                            self.sleep_as(app, SleepMode::Efficiency, origin, wake_on_foreground);
                        // Whatever could not be throttled is remembered so
                        // it is not retried on every tick.
                        if let (Ok(report), Some(entry)) = (outcome, self.entries.get_mut(&id)) {
                            for failure in &report.failed {
                                if let Some((pid, start, _)) =
                                    untracked.iter().find(|(pid, _, _)| *pid == failure.pid)
                                {
                                    entry.skipped.push((*pid, *start));
                                    dirty = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        if dirty {
            self.save_journal();
        }
        events
    }

    fn recently_foreground(&self, app_id: &str, now: Instant) -> bool {
        self.last_foreground
            .get(app_id)
            .is_some_and(|at| now.duration_since(*at) < self.foreground_grace)
    }

    fn is_snoozed(&self, app_id: &str, now: Instant) -> bool {
        self.snoozed.get(app_id).is_some_and(|until| *until > now)
    }
}

fn event(app_id: &str, app_name: &str, kind: ControlEventKind) -> ControlEvent {
    ControlEvent {
        app_id: app_id.to_owned(),
        app_name: app_name.to_owned(),
        kind,
    }
}

/// Applies the user's sleep rules to one snapshot. Call once per tick from
/// the background agent; it runs [`ControlState::tick`] itself.
///
/// * A matching program is put to sleep once it is in the background. For
///   [`SleepMode::Efficiency`] that simply means not being the active
///   program. For [`SleepMode::Suspend`] it means having no application
///   window at all: a frozen program cannot repaint or restore a window, so
///   programs with a window – even a minimized one – are never frozen by a
///   rule.
/// * A program the user just left is given [`ControlState::foreground_grace`]
///   before it is put to sleep, and one the user woke by hand is left alone
///   for [`ControlState::manual_wake_hold`].
/// * Programs put to sleep by a rule that has since been removed, disabled
///   or changed are woken.
/// * Protected programs are skipped whatever the rules say.
pub fn apply_rules(
    rules: &SleepRules,
    snapshot: &SystemSnapshot,
    state: &mut ControlState,
) -> Vec<ControlEvent> {
    let mut events = state.tick(snapshot);
    let now = Instant::now();

    // Rules that no longer hold.
    let stale: Vec<(String, String)> = state
        .entries
        .values()
        .filter(|e| e.origin == SleepOrigin::Rule)
        .filter(|e| {
            !rules.rules.iter().any(|r| {
                r.enabled
                    && r.mode == e.mode
                    && r.matches_program(&e.exe_name, e.exe_path.as_deref())
            })
        })
        .map(|e| (e.app_id.clone(), e.name.clone()))
        .collect();
    for (id, name) in stale {
        state.wake_entry(&id);
        events.push(event(
            &id,
            &name,
            ControlEventKind::Woken(WakeReason::RuleRemoved),
        ));
    }

    for app in &snapshot.apps {
        let Some(rule) = rules.find(app) else {
            continue;
        };
        if state.is_sleeping(&app.id) || app.suspended || classify::is_protected(app) {
            continue;
        }
        let in_use = match rule.mode {
            SleepMode::Suspend => app.window != WindowState::None,
            SleepMode::Efficiency => app.window == WindowState::Foreground,
        };
        if in_use || state.recently_foreground(&app.id, now) || state.is_snoozed(&app.id, now) {
            continue;
        }
        match state.sleep_as(app, rule.mode, SleepOrigin::Rule, rule.wake_on_foreground) {
            Ok(report) if report.is_success() && !report.succeeded.is_empty() => {
                events.push(event(
                    &app.id,
                    &app.name,
                    ControlEventKind::Slept(rule.mode),
                ));
            }
            Ok(report) => {
                if let Some(failure) = report.failed.first() {
                    // Do not retry every tick.
                    state
                        .snoozed
                        .insert(app.id.clone(), now + state.manual_wake_hold);
                    events.push(event(
                        &app.id,
                        &app.name,
                        ControlEventKind::Failed(failure.error.clone()),
                    ));
                }
            }
            // The live check found it protected or gone: silently skip.
            Err(_) => {}
        }
    }
    events
}

/// A process an action is about to be applied to.
struct Target {
    pid: u32,
    start: u64,
    name: String,
}

impl Target {
    fn tracked(
        &self,
        prev_priority: Option<u32>,
        prev_throttle: Option<(u32, u32)>,
    ) -> TrackedProcess {
        TrackedProcess {
            pid: self.pid,
            start: self.start,
            name: self.name.clone(),
            prev_priority,
            prev_throttle,
        }
    }
}

fn protected_error(app: &AppUsage, reason: ProtectionReason) -> Error {
    Error::Other(format!(
        "{} is protected and was left untouched ({})",
        app.name,
        reason.label().to_lowercase()
    ))
}

/// The system as it is right now, captured immediately before an action so
/// that no decision rests on a stale snapshot.
struct Live {
    processes: Vec<RawProcess>,
    ctx: Context,
}

impl Live {
    fn capture() -> Result<Self> {
        let processes = ProcessLister::new().list()?;
        let windows = windowing::scan();
        let ctx = Context::capture(&processes, &windows.foreground_pids, true);
        Ok(Live { processes, ctx })
    }

    /// Re-validates `app` and returns its processes that are still running.
    /// Fails when the program is protected.
    fn targets(&self, app: &AppUsage) -> Result<Vec<Target>> {
        // Whatever the snapshot recorded as a permanent reason stands. A
        // recorded "foreground" is transient and re-evaluated below instead.
        if let Some(reason) = classify::protection_reason(app)
            && reason.is_permanent()
        {
            return Err(protected_error(app, reason));
        }
        let mut targets = Vec::with_capacity(app.processes.len());
        for process in &app.processes {
            let Some(live) = self
                .processes
                .iter()
                .find(|r| r.pid == process.pid && r.create_time == process.start_ticks)
            else {
                continue;
            };
            let info = classify::inspect_process(live.pid, live.create_time);
            if let Some(reason) = classify::evaluate(live, &info, &self.ctx) {
                return Err(protected_error(app, reason));
            }
            targets.push(Target {
                pid: live.pid,
                start: live.create_time,
                name: live.name.clone(),
            });
        }
        Ok(targets)
    }
}

fn throttle_state(handle: &OwnedHandle) -> Option<(u32, u32)> {
    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ..Default::default()
    };
    // SAFETY: valid handle; the buffer is a correctly sized structure.
    unsafe {
        GetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            (&mut state as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .ok()
    .map(|()| (state.ControlMask, state.StateMask))
}

fn set_throttle(handle: &OwnedHandle, control: u32, state: u32) -> windows::core::Result<()> {
    let value = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: control,
        StateMask: state,
    };
    // SAFETY: valid handle; the buffer is a correctly sized structure.
    unsafe {
        SetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            (&value as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
}

/// Turns efficiency mode on and returns what must be restored later.
fn apply_efficiency(handle: &OwnedHandle) -> Result<(u32, Option<(u32, u32)>)> {
    // SAFETY: valid handle.
    let priority = unsafe { GetPriorityClass(handle.0) };
    if priority == 0 {
        return Err(windows::core::Error::from_thread().into());
    }
    let previous = throttle_state(handle);
    let (control, state) = previous.unwrap_or((0, 0));
    // Older Windows versions have no power throttling; the priority change
    // alone still does most of the work there.
    if let Err(e) = set_throttle(
        handle,
        control | PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        state | PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    ) {
        log::debug!("power throttling unavailable: {e}");
    }
    // SAFETY: valid handle.
    unsafe { SetPriorityClass(handle.0, IDLE_PRIORITY_CLASS) }?;
    Ok((priority, previous))
}

/// Puts back the priority class and throttling state saved by [`apply_efficiency`].
fn restore_efficiency(handle: &OwnedHandle, tracked: &TrackedProcess) -> Result<()> {
    let (control, state) = tracked.prev_throttle.unwrap_or((0, 0));
    if let Err(e) = set_throttle(handle, control, state) {
        log::debug!("power throttling unavailable: {e}");
    }
    if let Some(priority) = tracked.prev_priority.filter(|p| *p != 0) {
        // SAFETY: valid handle; `priority` is a class previously read from it.
        unsafe { SetPriorityClass(handle.0, PROCESS_CREATION_FLAGS(priority)) }?;
    }
    Ok(())
}

/// Reads whether Windows power throttling ("efficiency mode") is on for a
/// process. `None` when the process cannot be queried or the Windows version
/// does not report it.
pub fn is_efficiency_mode(pid: u32) -> Option<bool> {
    let handle = sys::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let (_, state) = throttle_state(&handle)?;
    Some(state & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0)
}

fn working_set(handle: &OwnedHandle) -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: valid handle; the buffer is a correctly sized structure.
    if unsafe { GetProcessMemoryInfo(handle.0, &mut counters, counters.cb) }.is_ok() {
        counters.WorkingSetSize as u64
    } else {
        0
    }
}

/// Asks Windows to move the working set of every process of `app` out of
/// physical memory.
///
/// This makes RAM available to other programs immediately, but the program
/// pages its memory back in as it runs, so the effect is temporary by
/// design. Fails outright for protected programs.
pub fn trim_memory(app: &AppUsage) -> Result<TrimReport> {
    let targets = Live::capture()?.targets(app)?;
    Ok(trim_targets(targets))
}

/// Trims the working set of every unprotected program in `snapshot`
/// (protected programs and the one in the foreground are skipped silently).
///
/// The same honesty note as for [`trim_memory`] applies: memory comes back
/// as programs touch it again, and programs that are in active use become
/// briefly slower while it does.
pub fn trim_all(snapshot: &SystemSnapshot) -> Result<TrimReport> {
    let live = Live::capture()?;
    let mut total = TrimReport::default();
    for app in &snapshot.apps {
        if classify::is_protected(app) {
            continue;
        }
        // Protected or gone by now: nothing to do for this one.
        if let Ok(targets) = live.targets(app) {
            let report = trim_targets(targets);
            total.processes += report.processes;
            total.failed += report.failed;
            total.before += report.before;
            total.after += report.after;
        }
    }
    Ok(total)
}

fn trim_targets(targets: Vec<Target>) -> TrimReport {
    use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
    let mut report = TrimReport::default();
    for target in targets {
        let access = PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION;
        match sys::open_verified(target.pid, target.start, access) {
            Ok(handle) => {
                let before = working_set(&handle);
                // SAFETY: valid handle with the access rights the call needs.
                if unsafe { EmptyWorkingSet(handle.0) }.is_ok() {
                    report.processes += 1;
                    report.before += before;
                    report.after += working_set(&handle);
                } else {
                    report.failed += 1;
                }
            }
            Err(OpenError::Exited) => {}
            Err(_) => report.failed += 1,
        }
    }
    report
}

/// Ends a program ("end task").
///
/// Every window of the program is first asked to close, exactly as if the
/// user had clicked its close button, and the program gets `grace` to exit
/// on its own. Processes still running after that are terminated only when
/// `force` is set; otherwise they are reported as failures.
///
/// A frozen program cannot react to the close request – wake it first, or
/// use `force`. Fails outright for protected programs.
pub fn terminate(app: &AppUsage, force: bool, grace: Duration) -> Result<ActionReport> {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    let targets = Live::capture()?.targets(app)?;
    let mut report = ActionReport::default();
    let mut open: Vec<(Target, OwnedHandle)> = Vec::new();
    for target in targets {
        let access: PROCESS_ACCESS_RIGHTS =
            PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
        match sys::open_verified(target.pid, target.start, access) {
            Ok(handle) => open.push((target, handle)),
            Err(e) => report.fail_open(target.pid, &target.name, &e),
        }
    }

    let pids: HashSet<u32> = open.iter().map(|(t, _)| t.pid).collect();
    windowing::request_close(&pids);

    let deadline = Instant::now() + grace;
    let exited = |handle: &OwnedHandle| {
        // SAFETY: the handle is valid and was opened with SYNCHRONIZE.
        let state = unsafe { WaitForSingleObject(handle.0, 0) };
        state == WAIT_OBJECT_0
    };
    loop {
        if open.iter().all(|(_, handle)| exited(handle)) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    for (target, handle) in open {
        if exited(&handle) {
            report.succeeded.push(target.pid);
        } else if force {
            // SAFETY: valid handle with PROCESS_TERMINATE.
            match unsafe { TerminateProcess(handle.0, 1) } {
                Ok(()) => {
                    // SAFETY: as above; bounded wait for the exit to complete.
                    unsafe { WaitForSingleObject(handle.0, 2000) };
                    report.succeeded.push(target.pid);
                }
                Err(e) => {
                    let denied = e.code().0 as u32 == 0x8007_0005;
                    report.fail(target.pid, &target.name, e.message(), denied);
                }
            }
        } else {
            report.fail(target.pid, &target.name, "did not close when asked", false);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{ProcessOwner, ProcessUsage, Sampler};
    use std::process::{Child, Stdio};

    /// A harmless helper process started by the test itself. It is the only
    /// kind of process these tests ever act on, and it is always killed and
    /// reaped again, even when an assertion fails.
    struct Helper(Child);

    impl Helper {
        fn spawn() -> Self {
            let child =
                zest_core::process::hidden_command(zest_core::process::system_tool("PING.EXE"))
                    .args(["-n", "120", "127.0.0.1"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("could not start the helper process");
            Helper(child)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        fn is_running(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }

        /// The helper as a one-process app. Built by hand so that nothing
        /// but this test's own child can ever be part of it.
        fn app(&self) -> AppUsage {
            let listing = ProcessLister::new().list().unwrap();
            let raw = listing
                .iter()
                .find(|p| p.pid == self.pid())
                .expect("helper must be running");
            assert_eq!(
                raw.parent_pid,
                std::process::id(),
                "helper must be our own child"
            );
            let exe = zest_core::process::system_tool("PING.EXE");
            AppUsage {
                id: format!("test-helper-{}", self.pid()),
                name: "Test helper".to_owned(),
                exe_name: raw.name.clone(),
                exe_path: Some(exe.clone()),
                processes: vec![ProcessUsage {
                    pid: raw.pid,
                    parent_pid: raw.parent_pid,
                    name: raw.name.clone(),
                    exe_path: Some(exe),
                    session_id: raw.session_id,
                    start_ticks: raw.create_time,
                    owner: ProcessOwner::CurrentUser,
                    ..ProcessUsage::default()
                }],
                ..AppUsage::default()
            }
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn snapshot_of(apps: Vec<AppUsage>, foreground: Vec<u32>) -> SystemSnapshot {
        SystemSnapshot {
            taken_at: SystemTime::now(),
            interval: Duration::from_secs(1),
            cpu_percent: 0.0,
            per_core: Vec::new(),
            memory: Default::default(),
            uptime: Duration::from_secs(60),
            boot_time: SystemTime::now(),
            process_count: apps.len() as u32,
            thread_count: 0,
            handle_count: 0,
            apps,
            foreground_pids: foreground,
        }
    }

    fn is_frozen(pid: u32) -> bool {
        ProcessLister::new()
            .list()
            .unwrap()
            .iter()
            .any(|p| p.pid == pid && p.suspended)
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        condition()
    }

    fn priority_of(pid: u32) -> u32 {
        let handle = sys::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        // SAFETY: valid handle.
        unsafe { GetPriorityClass(handle.0) }
    }

    #[test]
    fn suspend_and_resume_a_child() {
        let helper = Helper::spawn();
        let app = helper.app();
        let mut state = ControlState::new();

        let report = state.suspend(&app).unwrap();
        assert!(report.is_success(), "{report:?}");
        assert_eq!(report.succeeded, vec![helper.pid()]);
        assert!(
            wait_until(|| is_frozen(helper.pid())),
            "helper should be frozen"
        );
        assert_eq!(state.sleep_mode(&app.id), Some(SleepMode::Suspend));
        let sleeping = state.sleeping();
        assert_eq!(sleeping.len(), 1);
        assert_eq!(sleeping[0].pids, vec![helper.pid()]);
        assert_eq!(sleeping[0].origin, SleepOrigin::Manual);

        // Suspending again must not stack a second suspension.
        let again = state.suspend(&app).unwrap();
        assert_eq!(again.succeeded, vec![helper.pid()]);

        let report = state.resume(&app.id);
        assert!(report.is_success(), "{report:?}");
        assert!(
            wait_until(|| !is_frozen(helper.pid())),
            "one resume must be enough"
        );
        assert!(!state.is_sleeping(&app.id));
        assert!(state.sleeping().is_empty());
    }

    #[test]
    fn dropping_the_state_wakes_everything() {
        let helper = Helper::spawn();
        let app = helper.app();
        {
            let mut state = ControlState::new();
            state.suspend(&app).unwrap();
            assert!(wait_until(|| is_frozen(helper.pid())));
        }
        assert!(
            wait_until(|| !is_frozen(helper.pid())),
            "drop must resume the helper"
        );
    }

    #[test]
    fn efficiency_mode_round_trip() {
        let helper = Helper::spawn();
        let app = helper.app();
        let original_priority = priority_of(helper.pid());
        let original_throttle = is_efficiency_mode(helper.pid());
        assert_ne!(original_priority, 0);
        assert_ne!(
            original_priority, IDLE_PRIORITY_CLASS.0,
            "test needs a non-idle helper"
        );

        let mut state = ControlState::new();
        let report = state.set_efficiency_mode(&app, true).unwrap();
        assert!(report.is_success(), "{report:?}");
        assert_eq!(priority_of(helper.pid()), IDLE_PRIORITY_CLASS.0);
        if original_throttle.is_some() {
            assert_eq!(is_efficiency_mode(helper.pid()), Some(true));
        }
        assert_eq!(state.sleep_mode(&app.id), Some(SleepMode::Efficiency));
        assert!(
            !is_frozen(helper.pid()),
            "efficiency mode must not freeze anything"
        );

        let report = state.set_efficiency_mode(&app, false).unwrap();
        assert!(report.is_success(), "{report:?}");
        assert_eq!(priority_of(helper.pid()), original_priority);
        assert_eq!(is_efficiency_mode(helper.pid()), original_throttle);
        assert!(!state.is_sleeping(&app.id));

        // Turning it off when it is not on is a no-op.
        assert_eq!(
            state.set_efficiency_mode(&app, false).unwrap(),
            ActionReport::default()
        );
    }

    #[test]
    fn switching_modes_undoes_the_previous_one() {
        let helper = Helper::spawn();
        let app = helper.app();
        let original_priority = priority_of(helper.pid());
        let mut state = ControlState::new();

        state.set_efficiency_mode(&app, true).unwrap();
        state.suspend(&app).unwrap();
        assert_eq!(state.sleep_mode(&app.id), Some(SleepMode::Suspend));
        assert_eq!(priority_of(helper.pid()), original_priority);
        assert!(wait_until(|| is_frozen(helper.pid())));

        assert!(state.resume_all().is_success());
        assert!(wait_until(|| !is_frozen(helper.pid())));
    }

    #[test]
    fn journal_lets_a_later_run_wake_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("nested").join("sleeping-test.json");
        let helper = Helper::spawn();
        let app = helper.app();

        let mut crashed = ControlState::with_journal(journal.clone());
        crashed.suspend(&app).unwrap();
        assert!(wait_until(|| is_frozen(helper.pid())));
        assert!(journal.is_file(), "journal must be written");
        // Simulate an abnormal exit: no destructor, nothing resumed.
        std::mem::forget(crashed);
        assert!(is_frozen(helper.pid()));

        let mut next_run = ControlState::with_journal(journal.clone());
        let adopted = next_run.sleeping();
        assert_eq!(adopted.len(), 1);
        assert_eq!(adopted[0].pids, vec![helper.pid()]);
        assert_eq!(adopted[0].mode, SleepMode::Suspend);
        assert!(next_run.resume_all().is_success());
        assert!(wait_until(|| !is_frozen(helper.pid())));
        assert!(!journal.exists(), "an empty journal is removed");
    }

    #[test]
    fn journal_drops_processes_that_are_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("sleeping.json");
        let entry = SleepEntry {
            app_id: "gone".to_owned(),
            name: "Gone".to_owned(),
            exe_name: "gone.exe".to_owned(),
            exe_path: None,
            mode: SleepMode::Suspend,
            origin: SleepOrigin::Rule,
            wake_on_foreground: true,
            since: SystemTime::now(),
            processes: vec![TrackedProcess {
                pid: 0x7FFF_FFF0,
                start: 1,
                name: "gone.exe".to_owned(),
                prev_priority: None,
                prev_throttle: None,
            }],
            skipped: Vec::new(),
        };
        std::fs::write(&journal, serde_json::to_string(&vec![entry]).unwrap()).unwrap();
        let state = ControlState::with_journal(journal.clone());
        assert!(state.sleeping().is_empty());
        assert!(!journal.exists());

        // Garbage in the journal is ignored rather than fatal.
        std::fs::write(&journal, "not json").unwrap();
        assert!(ControlState::with_journal(journal).sleeping().is_empty());
    }

    #[test]
    fn protected_programs_are_refused_before_anything_happens() {
        let helper = Helper::spawn();
        let mut state = ControlState::new();

        // Recorded as protected in the snapshot.
        let mut recorded = helper.app();
        recorded.protection = Some(ProtectionReason::SecuritySoftware);
        // Looks like a system component by name.
        let mut named = helper.app();
        named.exe_name = "svchost.exe".to_owned();
        // Claims to run in the services session.
        let mut session = helper.app();
        session.processes[0].session_id = 0;
        // Owned by the system account.
        let mut owner = helper.app();
        owner.processes[0].owner = ProcessOwner::System;

        for app in [&recorded, &named, &session, &owner] {
            assert!(state.suspend(app).is_err());
            assert!(state.set_efficiency_mode(app, true).is_err());
            assert!(trim_memory(app).is_err());
            assert!(terminate(app, true, Duration::ZERO).is_err());
        }
        assert!(state.sleeping().is_empty());
        assert!(!is_frozen(helper.pid()));
    }

    #[test]
    fn stale_rows_never_reach_another_process() {
        // Same pid, different start time: the row describes a process that
        // has exited. Nothing may be done to the pid's current owner.
        let helper = Helper::spawn();
        let mut stale = helper.app();
        stale.processes[0].start_ticks += 1;
        let mut state = ControlState::new();
        assert!(matches!(state.suspend(&stale), Err(Error::NotFound(_))));
        assert!(!is_frozen(helper.pid()));
        assert_eq!(trim_memory(&stale).unwrap(), TrimReport::default());
        assert_eq!(
            terminate(&stale, true, Duration::ZERO).unwrap(),
            ActionReport::default()
        );
    }

    #[test]
    fn trimming_reports_honest_numbers() {
        let helper = Helper::spawn();
        // Let the helper finish loading so it has a working set to trim.
        std::thread::sleep(Duration::from_millis(300));
        let report = trim_memory(&helper.app()).unwrap();
        assert_eq!((report.processes, report.failed), (1, 0));
        assert!(report.before > 0);
        assert!(report.after <= report.before, "{report:?}");

        // The bulk variant works on exactly the apps in the snapshot it is
        // given (here: only helpers) and skips protected ones.
        let second = Helper::spawn();
        std::thread::sleep(Duration::from_millis(300));
        let mut protected = second.app();
        protected.protection = Some(ProtectionReason::SecuritySoftware);
        let all = trim_all(&snapshot_of(vec![helper.app(), protected], Vec::new())).unwrap();
        assert_eq!((all.processes, all.failed), (1, 0));
        assert_eq!(
            trim_all(&snapshot_of(Vec::new(), Vec::new())).unwrap(),
            TrimReport::default()
        );
    }

    #[test]
    fn state_types_can_move_between_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<ControlState>();
        assert_send::<Sampler>();
        assert_send::<SystemSnapshot>();
        assert_send::<SleepRules>();
    }

    #[test]
    fn efficiency_mode_follows_new_helper_processes() {
        let first = Helper::spawn();
        let second = Helper::spawn();
        let app = first.app();
        let original_priority = priority_of(second.pid());
        let mut state = ControlState::new();
        state.set_efficiency_mode(&app, true).unwrap();
        assert_eq!(priority_of(first.pid()), IDLE_PRIORITY_CLASS.0);
        assert_eq!(priority_of(second.pid()), original_priority);

        // The program has gained a process since it was put to sleep.
        let mut grown = app.clone();
        grown.processes.push(second.app().processes.remove(0));
        assert!(
            state
                .tick(&snapshot_of(vec![grown.clone()], Vec::new()))
                .is_empty()
        );
        assert_eq!(priority_of(second.pid()), IDLE_PRIORITY_CLASS.0);
        let mut pids = state.sleeping()[0].pids.clone();
        pids.sort_unstable();
        let mut expected = vec![first.pid(), second.pid()];
        expected.sort_unstable();
        assert_eq!(pids, expected);

        // Turning it off restores both.
        assert!(
            state
                .set_efficiency_mode(&grown, false)
                .unwrap()
                .is_success()
        );
        assert_eq!(priority_of(first.pid()), original_priority);
        assert_eq!(priority_of(second.pid()), original_priority);
    }

    #[test]
    fn terminate_asks_first_and_forces_only_when_told() {
        let mut helper = Helper::spawn();
        let app = helper.app();
        // The helper has no window, so a polite request cannot end it.
        let polite = terminate(&app, false, Duration::from_millis(200)).unwrap();
        assert!(polite.succeeded.is_empty());
        assert_eq!(polite.failed.len(), 1);
        assert!(!polite.failed[0].needs_admin);
        assert!(helper.is_running());

        let forced = terminate(&app, true, Duration::ZERO).unwrap();
        assert!(forced.is_success(), "{forced:?}");
        assert_eq!(forced.succeeded, vec![helper.pid()]);
        assert!(wait_until(|| !helper.is_running()));
    }

    #[test]
    fn tick_forgets_exited_programs() {
        let helper = Helper::spawn();
        let app = helper.app();
        let mut state = ControlState::new();
        state.suspend(&app).unwrap();
        // A snapshot in which the helper no longer exists.
        let events = state.tick(&snapshot_of(Vec::new(), Vec::new()));
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Woken(WakeReason::Exited)
            )]
        );
        assert!(state.sleeping().is_empty());
        // The state no longer tracks it, so wake it by hand for a clean exit.
        let handle = sys::open_process(helper.pid(), PROCESS_SUSPEND_RESUME).unwrap();
        sys::resume_process(&handle).unwrap();
    }

    #[test]
    fn tick_wakes_a_frozen_program_when_it_comes_to_the_foreground() {
        let helper = Helper::spawn();
        let app = helper.app();
        let mut state = ControlState::new();
        state.suspend(&app).unwrap();
        assert!(wait_until(|| is_frozen(helper.pid())));

        // Still in the background: nothing changes.
        assert!(
            state
                .tick(&snapshot_of(vec![app.clone()], Vec::new()))
                .is_empty()
        );
        assert!(state.is_sleeping(&app.id));

        let events = state.tick(&snapshot_of(vec![app.clone()], vec![helper.pid()]));
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Woken(WakeReason::Foreground)
            )]
        );
        assert!(wait_until(|| !is_frozen(helper.pid())));
    }

    #[test]
    fn tick_wakes_a_frozen_program_when_it_is_started_again() {
        let first = Helper::spawn();
        let second = Helper::spawn();
        let app = first.app();
        let mut state = ControlState::new();
        state.suspend(&app).unwrap();
        assert!(wait_until(|| is_frozen(first.pid())));

        // The same program now also has a second, running process.
        let mut relaunched = app.clone();
        relaunched.processes.push(second.app().processes.remove(0));
        let events = state.tick(&snapshot_of(vec![relaunched], Vec::new()));
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Woken(WakeReason::Relaunched)
            )]
        );
        assert!(wait_until(|| !is_frozen(first.pid())));
        assert!(!is_frozen(second.pid()));
    }

    #[test]
    fn rules_sleep_background_programs_and_wake_them_in_the_foreground() {
        let helper = Helper::spawn();
        let mut app = helper.app();
        let mut rules = SleepRules::default();
        rules.upsert(SleepRule::new("ping", SleepMode::Suspend));
        let mut state = ControlState::new();
        state.foreground_grace = Duration::from_millis(150);

        // Any window, even a minimized one: never frozen by a rule.
        for window in [WindowState::Visible, WindowState::Background] {
            app.window = window;
            assert!(
                apply_rules(
                    &rules,
                    &snapshot_of(vec![app.clone()], Vec::new()),
                    &mut state
                )
                .is_empty()
            );
            assert!(!state.is_sleeping(&app.id));
        }

        // In the background: put to sleep.
        app.window = WindowState::None;
        let events = apply_rules(
            &rules,
            &snapshot_of(vec![app.clone()], Vec::new()),
            &mut state,
        );
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Slept(SleepMode::Suspend)
            )]
        );
        assert!(wait_until(|| is_frozen(helper.pid())));
        assert_eq!(state.sleeping()[0].origin, SleepOrigin::Rule);
        // Stable while nothing changes.
        assert!(
            apply_rules(
                &rules,
                &snapshot_of(vec![app.clone()], Vec::new()),
                &mut state
            )
            .is_empty()
        );

        // The user switches to it: woken, and not re-frozen while in use.
        app.window = WindowState::Foreground;
        let events = apply_rules(
            &rules,
            &snapshot_of(vec![app.clone()], vec![helper.pid()]),
            &mut state,
        );
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Woken(WakeReason::Foreground)
            )]
        );
        assert!(wait_until(|| !is_frozen(helper.pid())));

        // Just left the foreground: the grace period still protects it.
        app.window = WindowState::None;
        assert!(
            apply_rules(
                &rules,
                &snapshot_of(vec![app.clone()], Vec::new()),
                &mut state
            )
            .is_empty()
        );
        std::thread::sleep(Duration::from_millis(200));
        let events = apply_rules(
            &rules,
            &snapshot_of(vec![app.clone()], Vec::new()),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(wait_until(|| is_frozen(helper.pid())));

        // Removing the rule wakes it.
        rules.remove("ping");
        let events = apply_rules(
            &rules,
            &snapshot_of(vec![app.clone()], Vec::new()),
            &mut state,
        );
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Woken(WakeReason::RuleRemoved)
            )]
        );
        assert!(wait_until(|| !is_frozen(helper.pid())));
    }

    #[test]
    fn rules_respect_manual_wake_and_protection() {
        let helper = Helper::spawn();
        let app = helper.app();
        let mut rules = SleepRules::default();
        rules.upsert(SleepRule::for_app(&app, SleepMode::Efficiency));
        let original_priority = priority_of(helper.pid());
        let mut state = ControlState::new();

        // Protected: skipped whatever the rule says.
        let mut protected = app.clone();
        protected.protection = Some(ProtectionReason::SecuritySoftware);
        assert!(
            apply_rules(
                &rules,
                &snapshot_of(vec![protected], Vec::new()),
                &mut state
            )
            .is_empty()
        );
        assert_eq!(priority_of(helper.pid()), original_priority);

        // Disabled rule: ignored.
        rules.rules[0].enabled = false;
        assert!(
            apply_rules(
                &rules,
                &snapshot_of(vec![app.clone()], Vec::new()),
                &mut state
            )
            .is_empty()
        );
        rules.rules[0].enabled = true;

        let events = apply_rules(
            &rules,
            &snapshot_of(vec![app.clone()], Vec::new()),
            &mut state,
        );
        assert_eq!(
            events,
            vec![event(
                &app.id,
                &app.name,
                ControlEventKind::Slept(SleepMode::Efficiency)
            )]
        );
        assert_eq!(priority_of(helper.pid()), IDLE_PRIORITY_CLASS.0);

        // The user wakes it by hand: the rule must not undo that right away.
        assert!(state.wake(&app.id).is_success());
        assert_eq!(priority_of(helper.pid()), original_priority);
        assert!(
            apply_rules(
                &rules,
                &snapshot_of(vec![app.clone()], Vec::new()),
                &mut state
            )
            .is_empty()
        );
        assert_eq!(priority_of(helper.pid()), original_priority);
    }

    #[test]
    fn rule_matching() {
        let app = AppUsage {
            exe_name: "Spotify.exe".to_owned(),
            exe_path: Some(PathBuf::from(
                r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe",
            )),
            ..AppUsage::default()
        };
        assert!(SleepRule::new("spotify.exe", SleepMode::Suspend).matches(&app));
        assert!(SleepRule::new("SPOTIFY", SleepMode::Suspend).matches(&app));
        assert!(SleepRule::new(" \"spotify.exe\" ", SleepMode::Suspend).matches(&app));
        assert!(
            SleepRule::new(
                "c:/users/me/appdata/roaming/spotify/spotify.exe",
                SleepMode::Suspend
            )
            .matches(&app)
        );
        assert!(!SleepRule::new(r"C:\Other\Spotify.exe", SleepMode::Suspend).matches(&app));
        assert!(!SleepRule::new("spot", SleepMode::Suspend).matches(&app));
        assert!(!SleepRule::new("", SleepMode::Suspend).matches(&app));
        let mut disabled = SleepRule::new("spotify", SleepMode::Suspend);
        disabled.enabled = false;
        assert!(!disabled.matches(&app));
        assert_eq!(
            SleepRule::for_app(&app, SleepMode::Efficiency).target,
            r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe"
        );

        let mut rules = SleepRules::default();
        assert!(rules.is_empty());
        rules.upsert(SleepRule::new("spotify.exe", SleepMode::Suspend));
        rules.upsert(SleepRule::new("Spotify.EXE", SleepMode::Efficiency));
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules.find(&app).map(|r| r.mode),
            Some(SleepMode::Efficiency)
        );
        assert!(rules.remove("SPOTIFY.exe"));
        assert!(!rules.remove("spotify.exe"));
        let json = serde_json::to_string(&rules).unwrap();
        assert_eq!(serde_json::from_str::<SleepRules>(&json).unwrap(), rules);
    }

    #[test]
    fn real_snapshot_never_offers_protected_programs_to_rules() {
        // Read-only: a rule that matches *everything* must still leave every
        // protected program alone. Nothing is applied here – the rule set is
        // only evaluated.
        let snapshot = Sampler::new().sample().unwrap();
        for app in &snapshot.apps {
            if classify::is_system_process_name(&app.exe_name)
                || app
                    .processes
                    .iter()
                    .any(|p| p.session_id == 0 || p.pid == std::process::id())
            {
                assert!(
                    classify::is_protected(app),
                    "{} must be protected",
                    app.exe_name
                );
            }
        }
        assert_eq!(
            default_journal_path("agent/1").file_name().unwrap(),
            "sleeping-agent-1.json"
        );
    }
}
