//! Boost: a temporary push for performance that puts everything back.
//!
//! Switching a boost on does two things:
//!
//! * the background programs the monitor recommends are put into efficiency
//!   mode, and
//! * the fastest power plan installed on the PC is made the active one.
//!
//! Switching it off wakes exactly those programs and re-activates the plan
//! that was active before, unless the user has picked another plan in the
//! meantime, in which case their choice stands.
//!
//! What a boost is about to change is written to a small journal *before*
//! anything changes. A boost cut short by a crash or a power cut is
//! therefore found again on the next start and undone by
//! [`Boost::recover`]. Dropping a [`Boost`] restores the power plan; the
//! programs are woken by the [`ControlState`] that put them to sleep, which
//! does the same when it is dropped.

use crate::control::{ControlState, SleepMode};
use crate::monitor::SystemSnapshot;
use crate::power::{self, PowerPlan};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zest_core::{IoContext, Result};

/// Windows' "Ultimate Performance" plan.
const ULTIMATE_PERFORMANCE: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";
/// Windows' "High performance" plan.
const HIGH_PERFORMANCE: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";

/// The power plans of a PC. A trait so that tests can stand in for Windows:
/// a boost must never switch the plan of the machine the tests run on.
pub trait PowerPlans {
    /// Lists the installed plans, marking the active one.
    fn list(&self) -> Result<Vec<PowerPlan>>;
    /// Makes the plan with this identifier the active one.
    fn activate(&self, guid: &str) -> Result<()>;
}

/// The power plans of this PC.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPowerPlans;

impl PowerPlans for SystemPowerPlans {
    fn list(&self) -> Result<Vec<PowerPlan>> {
        power::list_power_plans()
    }

    fn activate(&self, guid: &str) -> Result<()> {
        power::set_active_power_plan(guid)
    }
}

/// Picks the fastest plan among `plans`: Ultimate Performance where it
/// exists, otherwise High performance. Many laptops have neither (Windows
/// leaves them only "Balanced"); then there is nothing to switch to.
pub fn best_plan(plans: &[PowerPlan]) -> Option<&PowerPlan> {
    [ULTIMATE_PERFORMANCE, HIGH_PERFORMANCE]
        .iter()
        .find_map(|wanted| plans.iter().find(|p| p.guid.eq_ignore_ascii_case(wanted)))
}

/// What a running boost changed. This is the journal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// Plan that was active before the boost, when the boost switched plans.
    previous_plan: Option<String>,
    /// Plan the boost switched to.
    boost_plan: Option<String>,
    /// Identifiers of the programs the boost put to sleep.
    apps: Vec<String>,
}

/// What switching a boost on did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoostStart {
    /// Name of the power plan now in effect, when the boost switched plans.
    pub plan: Option<String>,
    /// Why the power plan could not be switched, when that failed.
    pub plan_error: Option<String>,
    /// Names of the programs put to sleep.
    pub apps: Vec<String>,
    /// Recommended programs that could not be put to sleep.
    pub apps_failed: usize,
}

/// What happened to the power plan when a boost ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanOutcome {
    /// The boost never switched plans.
    Untouched,
    /// The plan from before the boost is active again.
    Restored,
    /// Someone picked another plan during the boost; it was left alone.
    ChangedMeanwhile,
    /// The previous plan could not be re-activated.
    Failed(String),
}

/// What switching a boost off did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoostEnd {
    /// Programs woken.
    pub apps_woken: usize,
    /// What happened to the power plan.
    pub plan: PlanOutcome,
}

/// Where the boost journal is kept: `<data dir>\performance\boost.json`.
pub fn default_boost_journal() -> PathBuf {
    zest_core::paths::data_dir()
        .join("performance")
        .join("boost.json")
}

/// A boost that can be switched on and off. See the [module](self) text.
pub struct Boost<P: PowerPlans = SystemPowerPlans> {
    plans: P,
    journal: Option<PathBuf>,
    record: Option<Record>,
    /// The record was found on disk: an earlier run ended mid-boost.
    interrupted: bool,
}

impl Default for Boost<SystemPowerPlans> {
    fn default() -> Self {
        Self::new()
    }
}

impl Boost<SystemPowerPlans> {
    /// A boost for this PC with the default journal.
    pub fn new() -> Self {
        Self::with(SystemPowerPlans, Some(default_boost_journal()))
    }
}

impl<P: PowerPlans> Boost<P> {
    /// A boost over the given plans. With a `journal`, a record left there
    /// by a run that ended mid-boost is picked up: [`Boost::was_interrupted`]
    /// then returns true until [`Boost::recover`] has undone it.
    pub fn with(plans: P, journal: Option<PathBuf>) -> Self {
        let mut boost = Boost {
            plans,
            journal,
            record: None,
            interrupted: false,
        };
        let Some(path) = &boost.journal else {
            return boost;
        };
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Record>(&text) {
                Ok(record) => {
                    boost.record = Some(record);
                    boost.interrupted = true;
                }
                Err(e) => {
                    // Nothing can be restored from a journal that cannot be
                    // read; keeping it would only repeat this on every start.
                    log::warn!("ignoring unreadable boost journal {}: {e}", path.display());
                    let _ = std::fs::remove_file(path);
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("could not read boost journal {}: {e}", path.display()),
        }
        boost
    }

    /// True while a boost started by this value is in effect.
    pub fn is_on(&self) -> bool {
        self.record.is_some() && !self.interrupted
    }

    /// True when an earlier run ended in the middle of a boost and its
    /// changes have not been undone yet.
    pub fn was_interrupted(&self) -> bool {
        self.interrupted
    }

    fn save(&self) {
        let Some(path) = &self.journal else {
            return;
        };
        let result: Result<()> = (|| match &self.record {
            None => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).ctx("removing the boost journal"),
            },
            Some(record) => {
                if let Some(dir) = path.parent() {
                    zest_core::paths::ensure_dir(dir)?;
                }
                let text = serde_json::to_string_pretty(record)?;
                let temp = path.with_extension("json.tmp");
                std::fs::write(&temp, text).ctx("writing the boost journal")?;
                std::fs::rename(&temp, path).ctx("replacing the boost journal")
            }
        })();
        if let Err(e) = result {
            log::warn!("could not update boost journal {}: {e}", path.display());
        }
    }

    /// Switches the boost on: selects the fastest power plan and puts the
    /// programs `snapshot` recommends to sleep (efficiency mode) through
    /// `control`. Programs that are already asleep are left as they are and
    /// will not be woken when the boost ends. Does nothing when a boost is
    /// already on or an interrupted one has not been recovered.
    pub fn start(&mut self, control: &mut ControlState, snapshot: &SystemSnapshot) -> BoostStart {
        let mut report = BoostStart::default();
        if self.record.is_some() {
            return report;
        }
        let mut record = Record::default();

        // The power plan. The journal is written before the switch so that
        // the previous plan is never forgotten.
        let plans = self.plans.list().unwrap_or_default();
        let previous = plans.iter().find(|p| p.active);
        if let (Some(previous), Some(best)) = (previous, best_plan(&plans))
            && best.guid != previous.guid
        {
            record.previous_plan = Some(previous.guid.clone());
            record.boost_plan = Some(best.guid.clone());
            self.record = Some(record.clone());
            self.save();
            match self.plans.activate(&best.guid) {
                Ok(()) => report.plan = Some(best.name.clone()),
                Err(e) => {
                    record.previous_plan = None;
                    record.boost_plan = None;
                    report.plan_error = Some(e.to_string());
                }
            }
        }

        // The background programs. Their names go into the journal first,
        // for the same reason.
        let candidates: Vec<_> = snapshot
            .recommended()
            .into_iter()
            .filter(|app| !control.is_sleeping(&app.id))
            .collect();
        record.apps = candidates.iter().map(|app| app.id.clone()).collect();
        self.record = Some(record.clone());
        self.save();
        record.apps.clear();
        for app in candidates {
            match control.sleep(app, SleepMode::Efficiency) {
                Ok(_) if control.is_sleeping(&app.id) => {
                    record.apps.push(app.id.clone());
                    report.apps.push(app.name.clone());
                }
                Ok(_) | Err(_) => report.apps_failed += 1,
            }
        }
        // Keep only what really changed.
        self.record = Some(record);
        self.save();
        report
    }

    /// Switches the boost off: wakes the programs it put to sleep and
    /// re-activates the previous power plan. Does nothing (and reports
    /// nothing changed) when no boost is on.
    pub fn stop(&mut self, control: &mut ControlState) -> BoostEnd {
        let Some(record) = self.record.take() else {
            return BoostEnd {
                apps_woken: 0,
                plan: PlanOutcome::Untouched,
            };
        };
        self.interrupted = false;
        let mut apps_woken = 0;
        for id in &record.apps {
            // Only programs still in the state the boost left them in: one
            // the user has since frozen or woken by hand is theirs to manage.
            if control.sleep_mode(id) == Some(SleepMode::Efficiency) {
                control.wake(id);
                apps_woken += 1;
            }
        }
        let plan = self.restore_plan(&record);
        self.save();
        BoostEnd { apps_woken, plan }
    }

    /// Undoes a boost that an earlier run left unfinished. Returns `None`
    /// when there is nothing to recover.
    ///
    /// Pass the [`ControlState`] that was opened with the same journal as in
    /// that earlier run: it has adopted the programs that are still asleep.
    pub fn recover(&mut self, control: &mut ControlState) -> Option<BoostEnd> {
        self.interrupted.then(|| self.stop(control))
    }

    fn restore_plan(&self, record: &Record) -> PlanOutcome {
        let (Some(previous), Some(boosted)) = (&record.previous_plan, &record.boost_plan) else {
            return PlanOutcome::Untouched;
        };
        let active = match self.plans.list() {
            Ok(plans) => plans.into_iter().find(|p| p.active),
            Err(e) => return PlanOutcome::Failed(e.to_string()),
        };
        match active {
            Some(active) if active.guid.eq_ignore_ascii_case(boosted) => {
                match self.plans.activate(previous) {
                    Ok(()) => PlanOutcome::Restored,
                    Err(e) => PlanOutcome::Failed(e.to_string()),
                }
            }
            Some(active) if active.guid.eq_ignore_ascii_case(previous) => PlanOutcome::Restored,
            _ => PlanOutcome::ChangedMeanwhile,
        }
    }
}

impl<P: PowerPlans> Drop for Boost<P> {
    fn drop(&mut self) {
        // A boost that is on ends with its owner. An interrupted one that
        // was never recovered stays in the journal for the next start.
        if !self.is_on() {
            return;
        }
        if let Some(record) = self.record.take() {
            if let PlanOutcome::Failed(e) = self.restore_plan(&record) {
                log::warn!("the power plan could not be restored after a boost: {e}");
            }
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::is_efficiency_mode;
    use crate::monitor::{AppUsage, ProcessOwner, ProcessUsage};
    use crate::sys::ProcessLister;
    use std::cell::RefCell;
    use std::process::{Child, Stdio};
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};
    use zest_core::Error;

    const BALANCED: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";

    /// Stand-in for Windows' power plans. No test in this module ever
    /// touches the real ones.
    #[derive(Clone, Default)]
    struct FakePlans {
        plans: Rc<RefCell<Vec<PowerPlan>>>,
        activations: Rc<RefCell<Vec<String>>>,
        refuse: Rc<RefCell<bool>>,
    }

    impl FakePlans {
        fn new(guids: &[(&str, &str)], active: &str) -> Self {
            let fake = FakePlans::default();
            *fake.plans.borrow_mut() = guids
                .iter()
                .map(|(guid, name)| PowerPlan {
                    guid: (*guid).to_owned(),
                    name: (*name).to_owned(),
                    active: *guid == active,
                })
                .collect();
            fake
        }

        fn desktop() -> Self {
            Self::new(
                &[
                    (BALANCED, "Balanced"),
                    (HIGH_PERFORMANCE, "High performance"),
                    ("a1841308-3541-4fab-bc81-f71556f20b4a", "Power saver"),
                ],
                BALANCED,
            )
        }

        fn active(&self) -> String {
            self.plans
                .borrow()
                .iter()
                .find(|p| p.active)
                .map(|p| p.guid.clone())
                .unwrap_or_default()
        }

        fn set_active(&self, guid: &str) {
            for plan in self.plans.borrow_mut().iter_mut() {
                plan.active = plan.guid == guid;
            }
        }
    }

    impl PowerPlans for FakePlans {
        fn list(&self) -> Result<Vec<PowerPlan>> {
            Ok(self.plans.borrow().clone())
        }

        fn activate(&self, guid: &str) -> Result<()> {
            if *self.refuse.borrow() {
                return Err(Error::NeedsElevation("change the power plan".to_owned()));
            }
            self.activations.borrow_mut().push(guid.to_owned());
            self.set_active(guid);
            Ok(())
        }
    }

    /// A harmless helper process started by the test itself: the only kind
    /// of process these tests ever put to sleep. Always killed and reaped.
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

        /// The helper as a one-process app the monitor would recommend.
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
                recommended: true,
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

    fn snapshot_of(apps: Vec<AppUsage>) -> SystemSnapshot {
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
            foreground_pids: Vec::new(),
        }
    }

    #[test]
    fn the_fastest_installed_plan_is_chosen() {
        let plan = |guid: &str| PowerPlan {
            guid: guid.to_owned(),
            name: guid.to_owned(),
            active: false,
        };
        let all = [
            plan(BALANCED),
            plan(HIGH_PERFORMANCE),
            plan(ULTIMATE_PERFORMANCE),
        ];
        assert_eq!(best_plan(&all).unwrap().guid, ULTIMATE_PERFORMANCE);
        assert_eq!(best_plan(&all[..2]).unwrap().guid, HIGH_PERFORMANCE);
        assert!(
            best_plan(&all[..1]).is_none(),
            "Balanced alone offers nothing faster"
        );
        assert!(best_plan(&[]).is_none());
        let upper = [plan(&HIGH_PERFORMANCE.to_uppercase())];
        assert!(
            best_plan(&upper).is_some(),
            "identifiers compare without case"
        );
    }

    #[test]
    fn a_boost_switches_the_plan_and_puts_it_back() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("boost.json");
        let plans = FakePlans::desktop();
        let mut control = ControlState::new();
        let mut boost = Boost::with(plans.clone(), Some(journal.clone()));
        assert!(!boost.is_on() && !boost.was_interrupted());

        let started = boost.start(&mut control, &snapshot_of(Vec::new()));
        assert_eq!(started.plan.as_deref(), Some("High performance"));
        assert_eq!(started.plan_error, None);
        assert!(started.apps.is_empty());
        assert!(boost.is_on());
        assert_eq!(plans.active(), HIGH_PERFORMANCE);
        assert!(journal.exists(), "the journal exists while the boost is on");

        // Starting again changes nothing.
        let again = boost.start(&mut control, &snapshot_of(Vec::new()));
        assert_eq!(again, BoostStart::default());
        assert_eq!(plans.activations.borrow().len(), 1);

        let ended = boost.stop(&mut control);
        assert_eq!(ended.plan, PlanOutcome::Restored);
        assert_eq!(ended.apps_woken, 0);
        assert_eq!(plans.active(), BALANCED);
        assert!(!boost.is_on());
        assert!(!journal.exists(), "nothing is left behind");

        // Stopping a boost that is not on does nothing.
        let idle = boost.stop(&mut control);
        assert_eq!(idle.plan, PlanOutcome::Untouched);
        assert_eq!(plans.activations.borrow().len(), 2);
    }

    #[test]
    fn nothing_faster_installed_means_no_plan_change() {
        let tmp = tempfile::tempdir().unwrap();
        let laptop = FakePlans::new(&[(BALANCED, "Balanced")], BALANCED);
        let mut control = ControlState::new();
        let mut boost = Boost::with(laptop.clone(), Some(tmp.path().join("boost.json")));
        let started = boost.start(&mut control, &snapshot_of(Vec::new()));
        assert_eq!((started.plan, started.plan_error), (None, None));
        assert!(
            boost.is_on(),
            "the boost is on even without a plan to switch to"
        );
        assert_eq!(boost.stop(&mut control).plan, PlanOutcome::Untouched);
        assert!(laptop.activations.borrow().is_empty());

        // Already on the fastest plan: also nothing to do.
        let fast = FakePlans::desktop();
        fast.set_active(HIGH_PERFORMANCE);
        let mut boost = Boost::with(fast.clone(), None);
        assert_eq!(
            boost.start(&mut control, &snapshot_of(Vec::new())).plan,
            None
        );
        assert_eq!(boost.stop(&mut control).plan, PlanOutcome::Untouched);
        assert!(fast.activations.borrow().is_empty());
        assert_eq!(fast.active(), HIGH_PERFORMANCE);
    }

    #[test]
    fn a_plan_the_user_picked_during_the_boost_is_kept() {
        let plans = FakePlans::desktop();
        let mut control = ControlState::new();
        let mut boost = Boost::with(plans.clone(), None);
        boost.start(&mut control, &snapshot_of(Vec::new()));
        let saver = "a1841308-3541-4fab-bc81-f71556f20b4a";
        plans.set_active(saver);
        assert_eq!(boost.stop(&mut control).plan, PlanOutcome::ChangedMeanwhile);
        assert_eq!(plans.active(), saver);

        // Going back to the old plan by hand counts as restored.
        plans.set_active(BALANCED);
        boost.start(&mut control, &snapshot_of(Vec::new()));
        assert_eq!(plans.active(), HIGH_PERFORMANCE);
        plans.set_active(BALANCED);
        assert_eq!(boost.stop(&mut control).plan, PlanOutcome::Restored);
        assert_eq!(plans.active(), BALANCED);
    }

    #[test]
    fn a_refused_plan_change_is_reported_and_not_undone() {
        let plans = FakePlans::desktop();
        *plans.refuse.borrow_mut() = true;
        let mut control = ControlState::new();
        let mut boost = Boost::with(plans.clone(), None);
        let started = boost.start(&mut control, &snapshot_of(Vec::new()));
        assert_eq!(started.plan, None);
        assert!(started.plan_error.unwrap().contains("administrator"));
        assert_eq!(plans.active(), BALANCED);
        *plans.refuse.borrow_mut() = false;
        assert_eq!(
            boost.stop(&mut control).plan,
            PlanOutcome::Untouched,
            "a switch that never happened is not reversed"
        );
        assert!(plans.activations.borrow().is_empty());

        // The other way round: the switch works, switching back is refused.
        boost.start(&mut control, &snapshot_of(Vec::new()));
        *plans.refuse.borrow_mut() = true;
        assert!(matches!(
            boost.stop(&mut control).plan,
            PlanOutcome::Failed(_)
        ));
    }

    #[test]
    fn dropping_a_boost_restores_the_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("boost.json");
        let plans = FakePlans::desktop();
        let mut control = ControlState::new();
        {
            let mut boost = Boost::with(plans.clone(), Some(journal.clone()));
            boost.start(&mut control, &snapshot_of(Vec::new()));
            assert_eq!(plans.active(), HIGH_PERFORMANCE);
        }
        assert_eq!(plans.active(), BALANCED);
        assert!(!journal.exists());
    }

    #[test]
    fn recommended_apps_sleep_during_the_boost_and_wake_after() {
        let helper = Helper::spawn();
        let other = Helper::spawn();
        let app = helper.app();
        // A program the user already put to sleep by hand.
        let users_own = other.app();
        // And one the monitor does not recommend.
        let third = Helper::spawn();
        let mut ignored = third.app();
        ignored.recommended = false;

        let mut control = ControlState::new();
        control.sleep(&users_own, SleepMode::Efficiency).unwrap();
        let mut boost = Boost::with(FakePlans::desktop(), None);
        let snapshot = snapshot_of(vec![app.clone(), users_own.clone(), ignored.clone()]);

        let started = boost.start(&mut control, &snapshot);
        assert_eq!(started.apps, vec!["Test helper".to_owned()]);
        assert_eq!(started.apps_failed, 0);
        assert!(control.is_sleeping(&app.id));
        assert_eq!(is_efficiency_mode(helper.pid()), Some(true));
        assert!(
            !control.is_sleeping(&ignored.id),
            "only recommended apps are touched"
        );

        let ended = boost.stop(&mut control);
        assert_eq!(ended.apps_woken, 1);
        assert!(!control.is_sleeping(&app.id));
        assert_eq!(is_efficiency_mode(helper.pid()), Some(false));
        assert!(
            control.is_sleeping(&users_own.id),
            "what the user put to sleep stays asleep"
        );
        control.resume_all();
    }

    #[test]
    fn an_app_the_user_woke_or_froze_meanwhile_is_left_alone() {
        let helper = Helper::spawn();
        let app = helper.app();
        let mut control = ControlState::new();
        let mut boost = Boost::with(FakePlans::desktop(), None);
        boost.start(&mut control, &snapshot_of(vec![app.clone()]));
        // The user decides to freeze it completely instead.
        control.wake(&app.id);
        control.sleep(&app, SleepMode::Suspend).unwrap();
        let ended = boost.stop(&mut control);
        assert_eq!(ended.apps_woken, 0);
        assert_eq!(control.sleep_mode(&app.id), Some(SleepMode::Suspend));
        control.resume_all();
    }

    #[test]
    fn a_boost_cut_short_is_undone_on_the_next_start() {
        let tmp = tempfile::tempdir().unwrap();
        let boost_journal = tmp.path().join("boost.json");
        let sleep_journal = tmp.path().join("sleeping.json");
        let plans = FakePlans::desktop();
        let helper = Helper::spawn();
        let app = helper.app();

        // First run: the boost starts, then the process "crashes": neither
        // value gets to run its clean-up.
        {
            let mut control = ControlState::with_journal(sleep_journal.clone());
            let mut boost = Boost::with(plans.clone(), Some(boost_journal.clone()));
            let started = boost.start(&mut control, &snapshot_of(vec![app.clone()]));
            assert_eq!(started.apps.len(), 1);
            std::mem::forget(boost);
            std::mem::forget(control);
        }
        assert_eq!(
            plans.active(),
            HIGH_PERFORMANCE,
            "still boosted after the crash"
        );
        assert_eq!(is_efficiency_mode(helper.pid()), Some(true));
        assert!(boost_journal.exists());

        // Second run.
        let mut control = ControlState::with_journal(sleep_journal);
        let mut boost = Boost::with(plans.clone(), Some(boost_journal.clone()));
        assert!(boost.was_interrupted());
        assert!(!boost.is_on(), "an interrupted boost is not a running one");
        assert_eq!(
            boost.start(&mut control, &snapshot_of(Vec::new())),
            BoostStart::default(),
            "no new boost before the old one is undone"
        );
        let recovered = boost.recover(&mut control).unwrap();
        assert_eq!(recovered.plan, PlanOutcome::Restored);
        assert_eq!(recovered.apps_woken, 1);
        assert_eq!(plans.active(), BALANCED);
        assert_eq!(is_efficiency_mode(helper.pid()), Some(false));
        assert!(!boost.was_interrupted());
        assert!(!boost_journal.exists());
        assert!(
            boost.recover(&mut control).is_none(),
            "nothing left to recover"
        );
    }

    #[test]
    fn an_interrupted_boost_that_is_never_recovered_stays_on_record() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("boost.json");
        let plans = FakePlans::desktop();
        {
            let mut control = ControlState::new();
            let mut boost = Boost::with(plans.clone(), Some(journal.clone()));
            boost.start(&mut control, &snapshot_of(Vec::new()));
            std::mem::forget(boost);
        }
        // A run that never gets round to recovering must not lose the record.
        drop(Boost::with(plans.clone(), Some(journal.clone())));
        assert!(journal.exists());
        assert_eq!(plans.active(), HIGH_PERFORMANCE);
        let mut boost = Boost::with(plans.clone(), Some(journal.clone()));
        assert!(boost.recover(&mut ControlState::new()).is_some());
        assert_eq!(plans.active(), BALANCED);
    }

    #[test]
    fn an_unreadable_journal_is_discarded() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("boost.json");
        std::fs::write(&journal, "not json at all").unwrap();
        let boost = Boost::with(FakePlans::desktop(), Some(journal.clone()));
        assert!(!boost.was_interrupted() && !boost.is_on());
        assert!(!journal.exists());
    }

    #[test]
    fn the_plans_of_this_pc_can_be_listed() {
        // Read-only: the real plans are listed, never switched.
        let plans = SystemPowerPlans.list().unwrap();
        assert!(plans.iter().filter(|p| p.active).count() <= 1);
        if let Some(best) = best_plan(&plans) {
            assert!(!best.name.is_empty());
        }
        assert!(default_boost_journal().ends_with("performance/boost.json"));
    }
}
