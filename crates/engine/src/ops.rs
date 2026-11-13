//! One-call operations that turn [`Settings`] into cleaner runs.

use crate::history::{History, HistoryEntry, RunKind};
use crate::settings::Settings;
use std::collections::HashSet;
use zest_cleaner::{
    Catalog, CleanOptions, CleanReport, RuleKind, ScanReport, clean, cookies, scan,
};
use zest_core::paths::UserDirs;
use zest_core::{Progress, elevation};

/// The Windows accounts a clean applies to: everyone when the user asked for
/// it and the process is elevated, otherwise just the current user.
pub fn users(settings: &Settings) -> Vec<UserDirs> {
    if settings.cleaner.all_users && elevation::is_elevated() {
        UserDirs::all()
    } else {
        vec![UserDirs::current()]
    }
}

/// Builds the catalog for the current settings.
pub fn catalog(settings: &Settings) -> Catalog {
    Catalog::build(&users(settings), &settings.cleaner.includes)
}

/// Translates settings into engine options, using the user's rule choices.
pub fn clean_options(settings: &Settings, catalog: &Catalog) -> CleanOptions {
    let mut enabled = catalog.selection(&settings.cleaner.rules);
    // Custom includes exist because the user added them: on unless switched off.
    for (group, rule) in catalog.rules() {
        if group.section == zest_cleaner::Section::Custom
            && settings
                .cleaner
                .rules
                .get(&rule.id)
                .copied()
                .unwrap_or(true)
        {
            enabled.insert(rule.id.clone());
        }
    }
    CleanOptions {
        enabled,
        min_age_hours: settings.cleaner.min_age_hours,
        exclusions: settings.cleaner.exclusions.clone(),
        cookie_keep: settings.cleaner.cookie_keep.clone(),
        wipe: settings.cleaner.wipe,
        ignore_running: false,
    }
}

/// On first use, pre-fills the cookie keep list with common sign-in sites
/// that actually have cookies on this PC. Returns how many were added.
pub fn seed_cookie_keep(settings: &mut Settings) -> usize {
    if settings.cleaner.cookie_keep_seeded {
        return 0;
    }
    settings.cleaner.cookie_keep_seeded = true;
    let inventory = cookies::inventory(&users(settings));
    let mut added = 0;
    for domain in cookies::suggest_keep(&inventory.hosts) {
        if !settings.cleaner.cookie_keep.contains(&domain) {
            settings.cleaner.cookie_keep.push(domain);
            added += 1;
        }
    }
    settings.cleaner.cookie_keep.sort();
    added
}

/// Everything needed to review and then run a clean.
#[derive(Clone, Debug)]
pub struct Analysis {
    /// The catalog the scan was made with.
    pub catalog: Catalog,
    /// The options the scan was made with.
    pub options: CleanOptions,
    /// What was found.
    pub scan: ScanReport,
}

/// Analyses the PC with the user's settings. `only` restricts the run to a
/// subset of rule ids (intersected with nothing: exactly those rules run).
pub fn analyze(
    settings: &Settings,
    only: Option<&HashSet<String>>,
    progress: &Progress,
) -> Analysis {
    let catalog = catalog(settings);
    let mut options = clean_options(settings, &catalog);
    if let Some(only) = only {
        options.enabled = only.clone();
    }
    let scan = scan(&catalog, &options, progress);
    Analysis {
        catalog,
        options,
        scan,
    }
}

/// Cleans what `analysis` found and records the run in the history.
pub fn run_clean(
    analysis: &Analysis,
    kind: RunKind,
    dry_run: bool,
    progress: &Progress,
) -> CleanReport {
    let report = clean(
        &analysis.catalog,
        &analysis.scan,
        &analysis.options,
        dry_run,
        progress,
    );
    if !dry_run && (report.bytes > 0 || report.items > 0) {
        History::append(history_entry(&analysis.catalog, &report, kind));
    }
    report
}

/// Builds the history entry for a finished clean.
pub fn history_entry(catalog: &Catalog, report: &CleanReport, kind: RunKind) -> HistoryEntry {
    let mut entry = HistoryEntry::now(
        kind,
        report.bytes,
        report.items,
        report.elapsed.as_millis() as u64,
    );
    let mut areas: Vec<(String, u64)> = Vec::new();
    for rule in &report.rules {
        if rule.bytes == 0 {
            continue;
        }
        let name = catalog
            .groups
            .iter()
            .find(|g| g.id == rule.group_id)
            .map_or_else(|| rule.group_id.clone(), |g| g.name.clone());
        match areas.iter_mut().find(|(n, _)| *n == name) {
            Some(area) => area.1 += rule.bytes,
            None => areas.push((name, rule.bytes)),
        }
    }
    areas.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    areas.truncate(8);
    entry.areas = areas;
    entry
}

/// Unattended clean used by the scheduler and by smart cleaning.
///
/// Uses the user's selection; with `junk_only` privacy rules are left out
/// even when selected, so a background run never signs anyone out.
pub fn auto_clean(
    settings: &Settings,
    kind: RunKind,
    junk_only: bool,
    progress: &Progress,
) -> CleanReport {
    let catalog = catalog(settings);
    let mut options = clean_options(settings, &catalog);
    if junk_only {
        let junk = catalog.rules_of_kind(RuleKind::Junk);
        options.enabled.retain(|id| junk.contains(id));
    }
    let scan = scan(&catalog, &options, progress);
    let analysis = Analysis {
        catalog,
        options,
        scan,
    };
    run_clean(&analysis, kind, false, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_cleaner::{Include, IncludeMode, RuleCleanResult, Section};

    #[test]
    fn options_follow_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.cleaner.min_age_hours = 6;
        settings.cleaner.cookie_keep = vec!["example.com".into()];
        settings.cleaner.includes = vec![Include {
            path: tmp.path().to_path_buf(),
            pattern: "*.tmp".into(),
            mode: IncludeMode::FilesOnly,
        }];
        let catalog = catalog(&settings);
        let custom_id = catalog
            .section(Section::Custom)
            .next()
            .expect("custom group")
            .rules[0]
            .id
            .clone();

        let opts = clean_options(&settings, &catalog);
        assert_eq!(opts.min_age_hours, 6);
        assert_eq!(opts.cookie_keep, vec!["example.com"]);
        assert!(opts.enabled.contains("win.temp"));
        assert!(
            opts.enabled.contains(&custom_id),
            "custom includes default to on"
        );
        assert!(!opts.ignore_running);

        // Explicit choices override defaults in both directions.
        settings.cleaner.rules.insert("win.temp".into(), false);
        settings.cleaner.rules.insert("win.dns_cache".into(), true);
        settings.cleaner.rules.insert(custom_id.clone(), false);
        let opts = clean_options(&settings, &catalog);
        assert!(!opts.enabled.contains("win.temp"));
        assert!(opts.enabled.contains("win.dns_cache"));
        assert!(!opts.enabled.contains(&custom_id));
    }

    #[test]
    fn sandboxed_analyze_and_dry_run() {
        // Restrict the run to one custom rule pointing at a temp folder so
        // nothing on the real PC is involved.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.tmp"), vec![0u8; 2048]).unwrap();
        std::fs::write(tmp.path().join("keep.txt"), b"keep").unwrap();
        let mut settings = Settings::default();
        settings.cleaner.includes = vec![Include {
            path: tmp.path().to_path_buf(),
            pattern: "*.tmp".into(),
            mode: IncludeMode::FilesOnly,
        }];
        let only: HashSet<String> = catalog(&settings)
            .section(Section::Custom)
            .flat_map(|g| g.rules.iter().map(|r| r.id.clone()))
            .collect();
        let analysis = analyze(&settings, Some(&only), &Progress::new());
        assert_eq!((analysis.scan.bytes, analysis.scan.items), (2048, 1));

        let report = run_clean(&analysis, RunKind::Manual, true, &Progress::new());
        assert!(report.dry_run);
        assert_eq!(report.bytes, 2048);
        assert!(tmp.path().join("a.tmp").exists());
    }

    #[test]
    fn history_entry_groups_by_area() {
        let settings = Settings::default();
        let catalog = catalog(&settings);
        let report = CleanReport {
            bytes: 350,
            items: 7,
            rules: vec![
                RuleCleanResult {
                    rule_id: "win.temp".into(),
                    group_id: "win.system".into(),
                    bytes: 100,
                    ..Default::default()
                },
                RuleCleanResult {
                    rule_id: "win.thumbnails".into(),
                    group_id: "win.system".into(),
                    bytes: 50,
                    ..Default::default()
                },
                RuleCleanResult {
                    rule_id: "x.cache".into(),
                    group_id: "unknown-group".into(),
                    bytes: 200,
                    ..Default::default()
                },
                RuleCleanResult {
                    rule_id: "y".into(),
                    group_id: "empty".into(),
                    bytes: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let entry = history_entry(&catalog, &report, RunKind::Scheduled);
        assert_eq!(entry.kind, RunKind::Scheduled);
        assert_eq!((entry.bytes, entry.items), (350, 7));
        assert_eq!(
            entry.areas,
            vec![
                ("unknown-group".to_owned(), 200),
                ("System".to_owned(), 150)
            ]
        );
    }
}
