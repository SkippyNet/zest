//! Persistent user settings.
//!
//! Stored as pretty-printed JSON in `<data dir>\settings.json`. Every struct
//! uses `#[serde(default)]`, so files written by older or newer versions load
//! without errors: unknown fields are ignored and missing ones take their
//! default.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zest_cleaner::{Exclusion, Include};
use zest_core::delete::WipeMethod;
use zest_core::{IoContext, Result, paths};

/// Colour theme of the interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    /// Follow the Windows setting.
    #[default]
    System,
    Dark,
    Light,
}

/// Appearance options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Colour theme.
    pub theme: Theme,
    /// Interface zoom factor (1.0 = 100 %).
    pub zoom: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: Theme::System,
            zoom: 1.0,
        }
    }
}

/// Options of the junk and privacy cleaner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CleanerSettings {
    /// Explicit on/off choices per rule id. Rules without an entry use their default.
    pub rules: BTreeMap<String, bool>,
    /// Temporary files newer than this many hours are left alone.
    pub min_age_hours: u32,
    /// How file contents are destroyed.
    pub wipe: WipeMethod,
    /// Cookie domains that survive cleaning.
    pub cookie_keep: Vec<String>,
    /// Whether the keep list has been offered its initial suggestions.
    pub cookie_keep_seeded: bool,
    /// Extra locations to clean.
    pub includes: Vec<Include>,
    /// Locations that are never cleaned.
    pub exclusions: Vec<Exclusion>,
    /// Clean every Windows account on this PC (needs administrator rights).
    pub all_users: bool,
    /// Ask before cleaning starts.
    pub confirm_before_clean: bool,
}

impl Default for CleanerSettings {
    fn default() -> Self {
        CleanerSettings {
            rules: BTreeMap::new(),
            min_age_hours: 24,
            wipe: WipeMethod::None,
            cookie_keep: Vec::new(),
            cookie_keep_seeded: false,
            includes: Vec::new(),
            exclusions: Vec::new(),
            all_users: false,
            confirm_before_clean: true,
        }
    }
}

/// Options of the registry cleaner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RegistrySettings {
    /// Explicit on/off choices per category id.
    pub categories: BTreeMap<String, bool>,
    /// Key prefixes that are never reported.
    pub excluded: Vec<String>,
    /// Save a `.reg` backup before every fix.
    pub backup_before_fix: bool,
}

impl Default for RegistrySettings {
    fn default() -> Self {
        RegistrySettings {
            categories: BTreeMap::new(),
            excluded: Vec::new(),
            backup_before_fix: true,
        }
    }
}

/// How often a scheduled clean runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frequency {
    Daily,
    #[default]
    Weekly,
    Monthly,
    /// Each time the user signs in.
    AtLogon,
}

/// Scheduled cleaning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleSettings {
    /// Whether a scheduled task is registered.
    pub enabled: bool,
    /// How often.
    pub frequency: Frequency,
    /// Hour of day (0-23).
    pub hour: u8,
    /// Minute (0-59).
    pub minute: u8,
    /// Day of week for weekly runs (0 = Monday ... 6 = Sunday).
    pub weekday: u8,
    /// Day of month for monthly runs (1-28).
    pub day_of_month: u8,
}

impl Default for ScheduleSettings {
    fn default() -> Self {
        ScheduleSettings {
            enabled: false,
            frequency: Frequency::Weekly,
            hour: 12,
            minute: 0,
            weekday: 0,
            day_of_month: 1,
        }
    }
}

/// What smart cleaning does when it finds something.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmartAction {
    /// Do nothing.
    Off,
    /// Show a notification and let the user decide.
    #[default]
    Notify,
    /// Clean silently and report afterwards.
    Clean,
}

/// Background monitoring ("smart cleaning").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartSettings {
    /// Run the background agent in the notification area.
    pub enabled: bool,
    /// Junk size that triggers `junk_action`, in megabytes.
    pub junk_threshold_mb: u64,
    /// What to do when the threshold is exceeded.
    pub junk_action: SmartAction,
    /// What to do when a browser is closed.
    pub browser_action: SmartAction,
    /// Minutes between junk checks.
    pub check_interval_minutes: u32,
}

impl Default for SmartSettings {
    fn default() -> Self {
        SmartSettings {
            enabled: false,
            junk_threshold_mb: 1024,
            junk_action: SmartAction::Notify,
            browser_action: SmartAction::Off,
            check_interval_minutes: 60,
        }
    }
}

/// Health Check options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthSettings {
    /// Remove tracking cookies (keep list respected) as part of "Privacy".
    pub clean_cookies: bool,
    /// Remove browsing history as part of "Privacy".
    pub clean_history: bool,
}

impl Default for HealthSettings {
    fn default() -> Self {
        HealthSettings {
            clean_cookies: true,
            clean_history: false,
        }
    }
}

/// General behaviour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// Files removed by tools (duplicates, large files, leftovers) go to the
    /// Recycle Bin instead of being deleted permanently.
    pub tools_use_recycle_bin: bool,
    /// Closing the window keeps the app running in the notification area.
    pub close_to_tray: bool,
    /// Show Microsoft/Windows entries in startup, service and task lists.
    pub show_system_entries: bool,
    /// Package ids the software updater never offers.
    pub ignored_updates: Vec<String>,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        GeneralSettings {
            tools_use_recycle_bin: true,
            close_to_tray: false,
            show_system_entries: false,
            ignored_updates: Vec::new(),
        }
    }
}

/// Remembered window placement.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState {
            width: 1180.0,
            height: 760.0,
            maximized: false,
        }
    }
}

/// Current settings schema version.
pub const SETTINGS_VERSION: u32 = 1;

/// All user settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version of the file.
    pub version: u32,
    pub appearance: Appearance,
    pub cleaner: CleanerSettings,
    pub registry: RegistrySettings,
    pub schedule: ScheduleSettings,
    pub smart: SmartSettings,
    pub health: HealthSettings,
    pub general: GeneralSettings,
    pub window: WindowState,
    /// Free-form per-feature state (sleep rules, duplicate finder options, ...)
    /// stored by the front end as JSON values.
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            appearance: Appearance::default(),
            cleaner: CleanerSettings::default(),
            registry: RegistrySettings::default(),
            schedule: ScheduleSettings::default(),
            smart: SmartSettings::default(),
            health: HealthSettings::default(),
            general: GeneralSettings::default(),
            window: WindowState::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Default location of the settings file.
    pub fn path() -> PathBuf {
        paths::data_dir().join("settings.json")
    }

    /// Loads settings from the default location. A missing file yields the
    /// defaults; a damaged file is preserved as `settings.json.bad` and the
    /// defaults are used.
    pub fn load() -> Settings {
        Self::load_from(&Self::path())
    }

    /// Loads settings from an explicit file (see [`Settings::load`]).
    pub fn load_from(file: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(file) else {
            return Settings::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                settings.sanitize();
                settings
            }
            Err(e) => {
                log::error!("settings file {} is damaged: {e}", file.display());
                let mut bad = file.as_os_str().to_os_string();
                bad.push(".bad");
                let _ = std::fs::copy(file, PathBuf::from(bad));
                Settings::default()
            }
        }
    }

    /// Saves to the default location.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    /// Saves to an explicit file, atomically (write to a temporary file,
    /// then rename), so a crash can never leave a half-written file.
    pub fn save_to(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            paths::ensure_dir(dir)?;
        }
        let mut tmp = file.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .ctx(format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, file).ctx(format!("replacing {}", file.display()))
    }

    /// Clamps values that came from a hand-edited or foreign file.
    pub fn sanitize(&mut self) {
        self.version = SETTINGS_VERSION;
        self.appearance.zoom = if self.appearance.zoom.is_finite() {
            self.appearance.zoom.clamp(0.75, 2.0)
        } else {
            1.0
        };
        self.cleaner.min_age_hours = self.cleaner.min_age_hours.min(24 * 365);
        self.schedule.hour = self.schedule.hour.min(23);
        self.schedule.minute = self.schedule.minute.min(59);
        self.schedule.weekday = self.schedule.weekday.min(6);
        self.schedule.day_of_month = self.schedule.day_of_month.clamp(1, 28);
        self.smart.junk_threshold_mb = self.smart.junk_threshold_mb.clamp(50, 1024 * 1024);
        self.smart.check_interval_minutes = self.smart.check_interval_minutes.clamp(5, 24 * 60);
        if !self.window.width.is_finite() || !self.window.height.is_finite() {
            self.window = WindowState::default();
        }
        self.window.width = self.window.width.clamp(900.0, 10_000.0);
        self.window.height = self.window.height.clamp(600.0, 10_000.0);
    }

    /// Reads a typed value from [`Settings::extra`].
    pub fn extra<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.extra
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Stores a typed value in [`Settings::extra`].
    pub fn set_extra<T: Serialize>(&mut self, key: &str, value: &T) {
        if let Ok(json) = serde_json::to_value(value) {
            self.extra.insert(key.to_owned(), json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("nested").join("settings.json");
        let mut settings = Settings::default();
        settings.cleaner.rules.insert("chrome.cookies".into(), true);
        settings.cleaner.cookie_keep = vec!["example.com".into()];
        settings.cleaner.wipe = WipeMethod::Dod3;
        settings.schedule.enabled = true;
        settings.schedule.frequency = Frequency::Monthly;
        settings.set_extra("answer", &42u32);
        settings.save_to(&file).unwrap();

        let loaded = Settings::load_from(&file);
        assert_eq!(loaded, settings);
        assert_eq!(loaded.extra::<u32>("answer"), 42);
        assert_eq!(loaded.extra::<u32>("missing"), 0);
        assert!(!tmp.path().join("nested").join("settings.json.tmp").exists());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = Settings::load_from(&tmp.path().join("nope.json"));
        assert_eq!(settings, Settings::default());
        assert!(settings.cleaner.confirm_before_clean);
        assert!(settings.registry.backup_before_fix);
        assert!(settings.general.tools_use_recycle_bin);
        assert_eq!(settings.cleaner.min_age_hours, 24);
    }

    #[test]
    fn partial_and_unknown_fields_are_tolerated() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"cleaner":{"min_age_hours":48,"future_option":true},"brand_new_section":{"x":1}}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&file);
        assert_eq!(settings.cleaner.min_age_hours, 48);
        assert!(settings.cleaner.confirm_before_clean);
        assert_eq!(settings.schedule, ScheduleSettings::default());
    }

    #[test]
    fn damaged_file_is_preserved_and_defaults_used() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        std::fs::write(&file, "{ this is not json").unwrap();
        assert_eq!(Settings::load_from(&file), Settings::default());
        assert!(tmp.path().join("settings.json.bad").exists());
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"appearance":{"zoom":99.0},"schedule":{"hour":77,"minute":200,"weekday":9,"day_of_month":31},
                "smart":{"junk_threshold_mb":0,"check_interval_minutes":0},"window":{"width":10.0,"height":5.0}}"#,
        )
        .unwrap();
        let s = Settings::load_from(&file);
        assert_eq!(s.appearance.zoom, 2.0);
        assert_eq!(
            (
                s.schedule.hour,
                s.schedule.minute,
                s.schedule.weekday,
                s.schedule.day_of_month
            ),
            (23, 59, 6, 28)
        );
        assert_eq!(
            (s.smart.junk_threshold_mb, s.smart.check_interval_minutes),
            (50, 5)
        );
        assert_eq!((s.window.width, s.window.height), (900.0, 600.0));
    }
}
