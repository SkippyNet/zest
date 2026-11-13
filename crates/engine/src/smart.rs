//! Smart cleaning: the decisions made by the background agent.
//!
//! The agent calls [`SmartMonitor::tick`] every few seconds. The monitor
//! watches two things and reports them as [`SmartEvent`]s; what happens next
//! (notify or clean) is decided by the settings and carried out by the agent.
//!
//! * **Junk threshold** – every `check_interval_minutes` the junk rules are
//!   analysed (read-only). Crossing the configured size raises an event.
//! * **Browser closed** – when the last process of a browser exits.

use crate::ops;
use crate::settings::{Settings, SmartAction};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use zest_cleaner::{RuleKind, Section, scan};
use zest_core::{Progress, process};

/// After reporting a full junk folder, stay quiet for this long unless the
/// user cleans in between.
const JUNK_QUIET_PERIOD: Duration = Duration::from_secs(6 * 3600);

/// Something the agent should react to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmartEvent {
    /// Junk has grown past the configured threshold.
    JunkOverThreshold {
        /// Bytes that a clean would free.
        bytes: u64,
        /// Number of items.
        items: u64,
    },
    /// A browser was closed.
    BrowserClosed {
        /// Catalog group id, e.g. `chrome`.
        group_id: String,
        /// Display name, e.g. `Google Chrome`.
        name: String,
    },
}

/// Watches the system on behalf of the background agent.
#[derive(Debug, Default)]
pub struct SmartMonitor {
    last_junk_check: Option<Instant>,
    last_junk_event: Option<Instant>,
    /// Running state per browser group, from the previous tick.
    browsers: HashMap<String, bool>,
    /// Browser groups: `(group id, name, process names)`.
    known_browsers: Vec<(String, String, Vec<String>)>,
    last_browser_refresh: Option<Instant>,
}

impl SmartMonitor {
    /// Creates a monitor. The first junk check happens one interval after start.
    pub fn new() -> Self {
        SmartMonitor {
            last_junk_check: Some(Instant::now()),
            ..Default::default()
        }
    }

    /// Call after a clean so the junk notification can fire again later.
    pub fn note_cleaned(&mut self) {
        self.last_junk_event = None;
        self.last_junk_check = Some(Instant::now());
    }

    /// Checks what changed since the last call.
    pub fn tick(&mut self, settings: &Settings) -> Vec<SmartEvent> {
        let mut events = Vec::new();
        if !settings.smart.enabled {
            return events;
        }
        if settings.smart.browser_action != SmartAction::Off {
            self.check_browsers(settings, &mut events);
        } else {
            self.browsers.clear();
        }
        if settings.smart.junk_action != SmartAction::Off {
            self.check_junk(settings, &mut events);
        }
        events
    }

    fn check_browsers(&mut self, settings: &Settings, events: &mut Vec<SmartEvent>) {
        // Re-discover installed browsers now and then; it is cheap but not free.
        let stale = self
            .last_browser_refresh
            .is_none_or(|t| t.elapsed() > Duration::from_secs(600));
        if stale {
            self.known_browsers = ops::catalog(settings)
                .section(Section::Browsers)
                .map(|g| {
                    (
                        g.id.clone(),
                        g.name.clone(),
                        g.processes.iter().map(|p| p.to_lowercase()).collect(),
                    )
                })
                .collect();
            self.last_browser_refresh = Some(Instant::now());
        }
        let running = process::running_process_names();
        for (id, name, processes) in &self.known_browsers {
            let now_running = processes.iter().any(|p| running.contains(p));
            let was_running = self
                .browsers
                .insert(id.clone(), now_running)
                .unwrap_or(false);
            if was_running && !now_running {
                events.push(SmartEvent::BrowserClosed {
                    group_id: id.clone(),
                    name: name.clone(),
                });
            }
        }
    }

    fn check_junk(&mut self, settings: &Settings, events: &mut Vec<SmartEvent>) {
        let interval = Duration::from_secs(u64::from(settings.smart.check_interval_minutes) * 60);
        if self.last_junk_check.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        self.last_junk_check = Some(Instant::now());
        if self
            .last_junk_event
            .is_some_and(|t| t.elapsed() < JUNK_QUIET_PERIOD)
        {
            return;
        }
        let (bytes, items) = junk_size(settings);
        if bytes >= settings.smart.junk_threshold_mb.saturating_mul(1024 * 1024) {
            self.last_junk_event = Some(Instant::now());
            events.push(SmartEvent::JunkOverThreshold { bytes, items });
        }
    }
}

/// Size of what the user's selected junk rules would remove right now
/// (read-only analysis): `(bytes, items)`.
pub fn junk_size(settings: &Settings) -> (u64, u64) {
    let catalog = ops::catalog(settings);
    let mut options = ops::clean_options(settings, &catalog);
    let junk = catalog.rules_of_kind(RuleKind::Junk);
    options.enabled.retain(|id| junk.contains(id));
    let report = scan(&catalog, &options, &Progress::new());
    (report.bytes, report.items)
}

/// The rule ids that a "browser closed" clean runs for one browser group:
/// the user's selected rules of that group.
pub fn browser_rules(settings: &Settings, group_id: &str) -> std::collections::HashSet<String> {
    let catalog = ops::catalog(settings);
    let selected = ops::clean_options(settings, &catalog).enabled;
    catalog
        .groups
        .iter()
        .filter(|g| g.id == group_id)
        .flat_map(|g| g.rules.iter())
        .filter(|r| selected.contains(&r.id))
        .map(|r| r.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_monitor_is_silent() {
        let mut monitor = SmartMonitor::new();
        let settings = Settings::default();
        assert!(!settings.smart.enabled);
        assert!(monitor.tick(&settings).is_empty());
    }

    #[test]
    fn junk_check_waits_for_its_interval() {
        let mut settings = Settings::default();
        settings.smart.enabled = true;
        settings.smart.junk_threshold_mb = 50;
        let mut monitor = SmartMonitor::new();
        // Freshly created: the interval has not elapsed, so no scan and no event.
        assert!(monitor.tick(&settings).is_empty());
        // Pretend the interval has long passed and a notification was just shown:
        // the quiet period suppresses another one.
        monitor.last_junk_check = None;
        monitor.last_junk_event = Some(Instant::now());
        assert!(monitor.tick(&settings).is_empty());
        assert!(monitor.last_junk_check.is_some());
        monitor.note_cleaned();
        assert!(monitor.last_junk_event.is_none());
    }

    #[test]
    fn browser_close_is_detected_on_transition_only() {
        let mut settings = Settings::default();
        settings.smart.enabled = true;
        settings.smart.junk_action = SmartAction::Off;
        settings.smart.browser_action = SmartAction::Notify;
        let mut monitor = SmartMonitor::new();
        // Use a fake browser whose "process" is definitely not running.
        monitor.known_browsers = vec![(
            "fake".into(),
            "Fake Browser".into(),
            vec!["zest-no-such-browser.exe".into()],
        )];
        monitor.last_browser_refresh = Some(Instant::now());

        // Not running before, not running now: nothing.
        assert!(monitor.tick(&settings).is_empty());
        // Was running on the previous tick, gone now: one event.
        monitor.browsers.insert("fake".into(), true);
        assert_eq!(
            monitor.tick(&settings),
            vec![SmartEvent::BrowserClosed {
                group_id: "fake".into(),
                name: "Fake Browser".into()
            }]
        );
        // And only once.
        assert!(monitor.tick(&settings).is_empty());
    }

    #[test]
    fn junk_size_is_read_only_and_consistent() {
        let settings = Settings::default();
        let (bytes, items) = junk_size(&settings);
        // Whatever the machine holds, bytes without items would be a bug.
        assert!(items > 0 || bytes == 0);
    }
}
