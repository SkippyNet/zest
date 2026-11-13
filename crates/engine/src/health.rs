//! Health Check: one pass over the four things that matter most.
//!
//! * **Privacy** – tracking cookies (and, optionally, browsing history).
//! * **Space** – junk files from the user's selected cleaning rules.
//! * **Speed** – programs that start with Windows.
//! * **Security** – installed software with updates available.
//!
//! Checking is read-only. [`fix`] cleans the privacy and space findings;
//! startup programs and software updates are changed from their own pages,
//! where every item can be reviewed.

use crate::history::RunKind;
use crate::ops::{self, Analysis};
use crate::settings::Settings;
use std::collections::HashSet;
use zest_cleaner::{CleanReport, RuleKind, Section, scan};
use zest_core::Progress;
use zest_system::startup::{self, StartupItem};
use zest_updates::software::{self, CheckOptions, SoftwareUpdate};

/// What a health check found.
#[derive(Clone, Debug)]
pub struct HealthReport {
    /// The cleaner analysis behind the privacy and space figures.
    pub analysis: Analysis,
    /// Trackers (cookies and similar) that would be removed.
    pub trackers: u64,
    /// Other privacy items (history entries) that would be removed.
    pub privacy_items: u64,
    /// Bytes of junk.
    pub junk_bytes: u64,
    /// Number of junk items.
    pub junk_items: u64,
    /// Browsers that were running and could not be checked completely.
    pub blocked: Vec<String>,
    /// Enabled, non-Microsoft programs that start with Windows.
    pub startup: Vec<StartupItem>,
    /// Software with updates available; `None` when the Windows Package
    /// Manager is missing or could not be queried.
    pub updates: Option<Vec<SoftwareUpdate>>,
    /// Size of the system drive.
    pub drive_total: u64,
    /// Free space on the system drive.
    pub drive_free: u64,
}

impl HealthReport {
    /// Number of the four areas that need attention.
    pub fn areas_needing_attention(&self) -> usize {
        [
            self.trackers + self.privacy_items > 0,
            self.junk_bytes > 0,
            !self.startup.is_empty(),
            self.updates.as_ref().is_some_and(|u| !u.is_empty()),
        ]
        .into_iter()
        .filter(|needs| *needs)
        .count()
    }

    /// True when cleaning would change something.
    pub fn can_fix(&self) -> bool {
        self.analysis.scan.items > 0
    }
}

/// The rule ids a health check analyses: the user's junk selection plus the
/// privacy rules switched on in the Health Check settings.
pub fn health_rules(
    settings: &Settings,
    catalog: &zest_cleaner::Catalog,
) -> (HashSet<String>, HashSet<String>) {
    let junk_kind = catalog.rules_of_kind(RuleKind::Junk);
    let mut junk = ops::clean_options(settings, catalog).enabled;
    junk.retain(|id| junk_kind.contains(id));

    let mut privacy = HashSet::new();
    for group in catalog.section(Section::Browsers) {
        for rule in &group.rules {
            let wanted = (settings.health.clean_cookies && rule.id.ends_with(".cookies"))
                || (settings.health.clean_history
                    && (rule.id.ends_with(".history") || rule.id.ends_with(".downloads")));
            if wanted {
                privacy.insert(rule.id.clone());
            }
        }
    }
    (junk, privacy)
}

/// Runs the health check. Nothing is modified.
pub fn check(settings: &Settings, progress: &Progress) -> HealthReport {
    progress.set_message("Checking privacy and space");
    let catalog = ops::catalog(settings);
    let (junk_rules, privacy_rules) = health_rules(settings, &catalog);
    let mut options = ops::clean_options(settings, &catalog);
    options.enabled = junk_rules.union(&privacy_rules).cloned().collect();

    // The software check talks to the package manager and is the slow part,
    // so it runs alongside the local analysis.
    let ignored = settings.general.ignored_updates.clone();
    let (scan_report, startup_items, updates) = std::thread::scope(|scope| {
        let updates = scope.spawn(|| {
            if !software::is_available() {
                return None;
            }
            let check = CheckOptions {
                ignore_ids: ignored,
                ..Default::default()
            };
            software::check_updates(&check, &Progress::new())
                .map_err(|e| log::warn!("software update check failed: {e}"))
                .ok()
        });
        let startup_items = scope.spawn(|| {
            startup::list_startup_programs()
                .into_iter()
                .filter(|item| item.enabled && !item.is_microsoft)
                .collect::<Vec<_>>()
        });
        let scan_report = scan(&catalog, &options, progress);
        progress.set_message("Checking startup programs and software updates");
        (
            scan_report,
            startup_items.join().unwrap_or_default(),
            updates.join().unwrap_or(None),
        )
    });

    let mut report = HealthReport {
        trackers: 0,
        privacy_items: 0,
        junk_bytes: 0,
        junk_items: 0,
        blocked: scan_report
            .blockers()
            .into_iter()
            .map(|b| b.trim_end_matches(" is running").to_owned())
            .collect(),
        startup: startup_items,
        updates,
        drive_total: 0,
        drive_free: 0,
        analysis: Analysis {
            catalog,
            options,
            scan: scan_report,
        },
    };
    for result in &report.analysis.scan.results {
        if privacy_rules.contains(&result.rule_id) {
            if result.rule_id.ends_with(".cookies") {
                report.trackers += result.items;
            } else {
                report.privacy_items += result.items;
            }
        } else {
            report.junk_bytes += result.bytes;
            report.junk_items += result.items;
        }
    }
    let summary = zest_performance::power::system_summary();
    report.drive_total = summary.system_drive_total;
    report.drive_free = summary.system_drive_free;
    report
}

/// Cleans the privacy and space findings of a health check.
pub fn fix(report: &HealthReport, progress: &Progress) -> CleanReport {
    ops::run_clean(&report.analysis, RunKind::HealthCheck, false, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_rules_follow_the_settings() {
        let mut settings = Settings::default();
        let catalog = ops::catalog(&settings);
        let (junk, privacy) = health_rules(&settings, &catalog);
        assert!(junk.contains("win.temp"));
        // Junk never contains privacy rules, whatever the user selected.
        settings.cleaner.rules.insert("win.dns_cache".into(), true);
        let (junk_again, _) = health_rules(&settings, &catalog);
        assert!(!junk_again.contains("win.dns_cache"));
        assert!(
            junk.iter()
                .all(|id| !id.ends_with(".cookies") && !id.ends_with(".history"))
        );

        // Cookies are part of the privacy check by default, history is not.
        assert!(privacy.iter().all(|id| id.ends_with(".cookies")));
        settings.health.clean_cookies = false;
        settings.health.clean_history = true;
        let (_, privacy) = health_rules(&settings, &catalog);
        assert!(
            privacy
                .iter()
                .all(|id| id.ends_with(".history") || id.ends_with(".downloads"))
        );
        settings.health.clean_history = false;
        assert!(health_rules(&settings, &catalog).1.is_empty());
    }
}
