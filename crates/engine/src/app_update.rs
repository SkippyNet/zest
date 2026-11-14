//! Zest's own updates: the glue between the saved settings and the release
//! check in [`zest_updates::selfupdate`].
//!
//! The check itself is one anonymous request to the public repository's
//! release feed. This module decides *when* it runs (at most once a day, and
//! only when the user has not switched it off) and remembers what it found,
//! so that the next start can show "an update is available" without asking
//! the network again.

use crate::Settings;
use zest_updates::selfupdate::{self, CheckOutcome, UpdateError};

/// The current time in seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// True when the automatic check should run now: it is switched on and the
/// last check is at least a day old.
pub fn automatic_check_due(settings: &Settings, now: i64) -> bool {
    settings.app_updates.check_automatically
        && selfupdate::check_is_due(settings.app_updates.last_checked, now)
}

/// Switches the daily check on or off; checking by hand keeps working either
/// way. Returns true when the settings changed.
pub fn set_automatic(settings: &mut Settings, on: bool) -> bool {
    let changed = settings.app_updates.check_automatically != on;
    settings.app_updates.check_automatically = on;
    changed
}

/// Stores the result of a check. Failed checks are remembered as attempts
/// too, so that an unreachable server is not asked again on every start.
/// Returns true when the settings changed.
pub fn record_check(
    settings: &mut Settings,
    outcome: &Result<CheckOutcome, UpdateError>,
    now: i64,
) -> bool {
    let before = settings.app_updates.clone();
    settings.app_updates.last_checked = Some(now);
    if let Ok(outcome) = outcome {
        settings.app_updates.latest_seen = outcome.latest().map(ToString::to_string);
    }
    settings.app_updates != before
}

/// The version to offer, going by what the last check found: newer than the
/// running one and not skipped by the user.
pub fn version_on_offer(settings: &Settings) -> Option<String> {
    let latest = selfupdate::parse_version(settings.app_updates.latest_seen.as_deref()?)?;
    selfupdate::should_offer(
        &latest,
        &selfupdate::current_version(),
        settings.app_updates.skipped_version.as_deref(),
    )
    .then(|| latest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_updates::selfupdate::CHECK_INTERVAL_SECS;

    #[test]
    fn the_automatic_check_respects_the_switch_and_the_interval() {
        let mut settings = Settings::default();
        let now = 1_800_000_000;
        assert!(settings.app_updates.check_automatically, "on by default");
        assert!(automatic_check_due(&settings, now));
        settings.app_updates.last_checked = Some(now - 3600);
        assert!(!automatic_check_due(&settings, now));
        settings.app_updates.last_checked = Some(now - CHECK_INTERVAL_SECS - 1);
        assert!(automatic_check_due(&settings, now));
        settings.app_updates.check_automatically = false;
        assert!(!automatic_check_due(&settings, now));
        assert!(super::now() > 1_700_000_000);
    }

    #[test]
    fn the_switch_reports_whether_it_changed_anything() {
        let mut settings = Settings::default();
        settings.app_updates.skipped_version = Some("1.2.3".to_owned());
        assert!(!set_automatic(&mut settings, true), "already on");
        assert!(set_automatic(&mut settings, false));
        assert!(!settings.app_updates.check_automatically);
        assert!(!automatic_check_due(&settings, 1_800_000_000));
        assert!(!set_automatic(&mut settings, false), "already off");
        assert!(set_automatic(&mut settings, true));
        assert!(settings.app_updates.check_automatically);
        // Nothing else about the updater is touched.
        assert_eq!(
            settings.app_updates.skipped_version.as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn checks_are_remembered_whether_they_worked_or_not() {
        let mut settings = Settings::default();
        let found = Ok(CheckOutcome::UpToDate {
            latest: selfupdate::parse_version("0.0.1"),
        });
        assert!(record_check(&mut settings, &found, 100));
        assert_eq!(settings.app_updates.last_checked, Some(100));
        assert_eq!(settings.app_updates.latest_seen.as_deref(), Some("0.0.1"));
        // An older release than the running version is nothing to offer.
        assert_eq!(version_on_offer(&settings), None);

        // A failed check keeps what was known and still counts as an attempt.
        let failed = Err(UpdateError::Offline("no route".to_owned()));
        assert!(record_check(&mut settings, &failed, 200));
        assert_eq!(settings.app_updates.last_checked, Some(200));
        assert_eq!(settings.app_updates.latest_seen.as_deref(), Some("0.0.1"));
        assert!(
            !record_check(&mut settings, &failed, 200),
            "nothing changed"
        );

        // Nothing published yet.
        let none = Ok(CheckOutcome::UpToDate { latest: None });
        record_check(&mut settings, &none, 300);
        assert_eq!(settings.app_updates.latest_seen, None);
    }

    #[test]
    fn a_newer_version_is_offered_until_it_is_skipped() {
        let mut settings = Settings::default();
        assert_eq!(version_on_offer(&settings), None);
        settings.app_updates.latest_seen = Some("999.0.0".to_owned());
        assert_eq!(version_on_offer(&settings).as_deref(), Some("999.0.0"));
        settings.app_updates.skipped_version = Some("999.0.0".to_owned());
        assert_eq!(version_on_offer(&settings), None);
        // The next release is offered again.
        settings.app_updates.latest_seen = Some("999.1.0".to_owned());
        assert_eq!(version_on_offer(&settings).as_deref(), Some("999.1.0"));
        settings.app_updates.latest_seen = Some("not a version".to_owned());
        assert_eq!(version_on_offer(&settings), None);
    }

    #[test]
    fn update_settings_survive_a_round_trip_and_old_files() {
        let mut settings = Settings::default();
        settings.app_updates.skipped_version = Some("1.2.3".to_owned());
        settings.app_updates.last_checked = Some(42);
        settings.general.catalog_fallback = false;
        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.app_updates, settings.app_updates);
        assert!(!back.general.catalog_fallback);
        // A settings file written before these options existed.
        let old: Settings = serde_json::from_str(r#"{"general":{"close_to_tray":true}}"#).unwrap();
        assert!(old.app_updates.check_automatically);
        assert!(old.general.catalog_fallback);
        assert_eq!(old.app_updates.last_checked, None);
    }
}
