//! Public data types of the registry cleaner.

use crate::check;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zest_core::reg::{RegPath, RegRoot};

/// A family of registry problems. Each category has its own scanner and can
/// be switched on or off independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    /// Shared DLL reference counts for files that no longer exist.
    SharedDlls,
    /// File extension keys that are empty or point at a missing file type.
    FileExtensions,
    /// COM / ActiveX classes whose server file is gone.
    ComClasses,
    /// Type libraries whose file is gone.
    TypeLibraries,
    /// "Open with" application registrations for missing programs.
    Applications,
    /// Font registrations for missing font files.
    Fonts,
    /// Application path entries for missing programs.
    AppPaths,
    /// Help file registrations for missing files.
    HelpFiles,
    /// Installer folder references for folders that no longer exist.
    Installer,
    /// Leftovers of software that has been removed.
    ObsoleteSoftware,
    /// Startup entries whose program is gone.
    Startup,
    /// Cached program names for missing programs.
    MuiCache,
    /// Sound events that reference missing sound files.
    SoundEvents,
    /// Windows services whose program file is gone (review carefully).
    Services,
    /// Firewall rules for programs that no longer exist.
    FirewallRules,
}

impl Category {
    /// Every category, in display order.
    pub const ALL: [Category; 15] = [
        Category::SharedDlls,
        Category::FileExtensions,
        Category::ComClasses,
        Category::TypeLibraries,
        Category::Applications,
        Category::Fonts,
        Category::AppPaths,
        Category::HelpFiles,
        Category::Installer,
        Category::ObsoleteSoftware,
        Category::Startup,
        Category::MuiCache,
        Category::SoundEvents,
        Category::Services,
        Category::FirewallRules,
    ];

    /// Short name for lists and check boxes.
    pub fn label(self) -> &'static str {
        match self {
            Category::SharedDlls => "Missing shared DLLs",
            Category::FileExtensions => "Unused file extensions",
            Category::ComClasses => "ActiveX and class issues",
            Category::TypeLibraries => "Type libraries",
            Category::Applications => "Applications",
            Category::Fonts => "Fonts",
            Category::AppPaths => "Application paths",
            Category::HelpFiles => "Help files",
            Category::Installer => "Installer references",
            Category::ObsoleteSoftware => "Obsolete software",
            Category::Startup => "Run at startup",
            Category::MuiCache => "MUI cache",
            Category::SoundEvents => "Sound events",
            Category::Services => "Windows services",
            Category::FirewallRules => "Firewall rules",
        }
    }

    /// One sentence explaining what the category looks for.
    pub fn description(self) -> &'static str {
        match self {
            Category::SharedDlls => {
                "Shared file counters that refer to DLLs which are no longer on this PC."
            }
            Category::FileExtensions => {
                "File extension entries that are empty or refer to a file type that does not exist."
            }
            Category::ComClasses => "Component registrations whose program file has been removed.",
            Category::TypeLibraries => {
                "Type library registrations whose library file has been removed."
            }
            Category::Applications => {
                "\"Open with\" entries for programs that are no longer installed."
            }
            Category::Fonts => "Font entries whose font file has been deleted.",
            Category::AppPaths => {
                "Application path shortcuts that point at programs which no longer exist."
            }
            Category::HelpFiles => "Help file entries whose help file has been removed.",
            Category::Installer => {
                "Installer bookkeeping for folders that have already been removed."
            }
            Category::ObsoleteSoftware => {
                "Uninstall entries and empty settings keys left behind by removed software."
            }
            Category::Startup => {
                "Startup entries whose program is missing, and leftover startup switches."
            }
            Category::MuiCache => "Cached display names of programs that no longer exist.",
            Category::SoundEvents => "Sound scheme entries that refer to missing sound files.",
            Category::Services => {
                "Services and drivers whose program file is missing. Review each one before removing."
            }
            Category::FirewallRules => "Firewall rules for programs that are no longer installed.",
        }
    }

    /// Whether the category is part of a default scan.
    ///
    /// Everything except [`Category::Services`] is enabled by default.
    pub fn default_enabled(self) -> bool {
        !matches!(self, Category::Services)
    }

    /// Stable machine readable identifier (used in issue ids, settings and
    /// on the command line).
    pub fn slug(self) -> &'static str {
        match self {
            Category::SharedDlls => "shared-dlls",
            Category::FileExtensions => "file-extensions",
            Category::ComClasses => "com-classes",
            Category::TypeLibraries => "type-libraries",
            Category::Applications => "applications",
            Category::Fonts => "fonts",
            Category::AppPaths => "app-paths",
            Category::HelpFiles => "help-files",
            Category::Installer => "installer",
            Category::ObsoleteSoftware => "obsolete-software",
            Category::Startup => "startup",
            Category::MuiCache => "mui-cache",
            Category::SoundEvents => "sound-events",
            Category::Services => "services",
            Category::FirewallRules => "firewall-rules",
        }
    }

    /// Parses a [`Category::slug`] (case-insensitive).
    pub fn from_slug(slug: &str) -> Option<Category> {
        let slug = slug.trim().to_ascii_lowercase();
        Category::ALL.into_iter().find(|c| c.slug() == slug)
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// How sure the scanner is that removing an entry is harmless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    /// The entry is demonstrably dead; selected for fixing by default.
    Safe,
    /// Probably dead, but the user should look before removing it; not
    /// selected by default.
    Review,
}

/// A fact that made an entry an issue.
///
/// Conditions are captured during the scan and checked again immediately
/// before fixing. If the PC changed in the meantime (a program was
/// reinstalled, a drive reconnected, a value rewritten) the issue is skipped
/// instead of fixed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Condition {
    /// This file or folder does not exist – neither at this location nor at
    /// its 32/64-bit twin (`Program Files` / `Program Files (x86)`,
    /// `System32` / `SysWOW64`).
    PathMissing(PathBuf),
    /// This registry key does not exist.
    KeyMissing(RegPath),
    /// This registry value does not exist.
    ValueMissing {
        /// Key that would hold the value.
        key: RegPath,
        /// Value name (empty for the default value).
        name: String,
    },
    /// This string value still holds exactly the data that was judged invalid.
    ValueEquals {
        /// Key holding the value.
        key: RegPath,
        /// Value name (empty for the default value).
        name: String,
        /// The data seen during the scan.
        data: String,
    },
    /// This key still has exactly this many sub-keys and values.
    KeyShape {
        /// The key.
        key: RegPath,
        /// Number of direct sub-keys.
        sub_keys: u32,
        /// Number of values.
        values: u32,
    },
}

impl Condition {
    /// Checks the condition against the current state of the PC.
    pub fn holds(&self) -> bool {
        match self {
            Condition::PathMissing(path) => check::confirmed_absent(path),
            Condition::KeyMissing(key) => check::key_definitely_absent(key),
            Condition::ValueMissing { key, name } => check::value_definitely_absent(key, name),
            Condition::ValueEquals { key, name, data } => key
                .open()
                .ok()
                .and_then(|k| k.get_value::<String, _>(name).ok())
                .is_some_and(|current| current == *data),
            Condition::KeyShape {
                key,
                sub_keys,
                values,
            } => key
                .open()
                .ok()
                .and_then(|k| k.query_info().ok())
                .is_some_and(|info| info.sub_keys == *sub_keys && info.values == *values),
        }
    }
}

/// One invalid registry entry found by a scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// Stable identifier: the same entry gets the same id in every scan.
    pub id: String,
    /// The category that reported it.
    pub category: Category,
    /// Short statement of the problem, e.g. "Startup entry points to a missing program".
    pub title: String,
    /// The offending data, e.g. the missing path.
    pub detail: String,
    /// The key that holds the problem.
    pub key: RegPath,
    /// `None`: fixing deletes the whole key. `Some(name)`: fixing deletes only
    /// that value (`Some("")` is the key's default value).
    pub value: Option<String>,
    /// How safe the fix is considered.
    pub confidence: Confidence,
    /// True when fixing needs administrator rights.
    pub requires_admin: bool,
    /// The facts that must still be true for the fix to be applied.
    pub conditions: Vec<Condition>,
}

impl Issue {
    /// Builds an issue that deletes a whole key.
    pub(crate) fn for_key(
        category: Category,
        key: &RegPath,
        title: impl Into<String>,
        detail: impl Into<String>,
        confidence: Confidence,
        conditions: Vec<Condition>,
    ) -> Issue {
        Issue {
            id: format!("{}|{}", category.slug(), key),
            category,
            title: title.into(),
            detail: detail.into(),
            key: key.clone(),
            value: None,
            confidence,
            requires_admin: root_needs_admin(key.root),
            conditions,
        }
    }

    /// Builds an issue that deletes a single value.
    pub(crate) fn for_value(
        category: Category,
        key: &RegPath,
        value: &str,
        title: impl Into<String>,
        detail: impl Into<String>,
        confidence: Confidence,
        conditions: Vec<Condition>,
    ) -> Issue {
        Issue {
            id: format!("{}|{}|{}", category.slug(), key, value),
            category,
            title: title.into(),
            detail: detail.into(),
            key: key.clone(),
            value: Some(value.to_owned()),
            confidence,
            requires_admin: root_needs_admin(key.root),
            conditions,
        }
    }

    /// Whether the issue should be ticked for fixing when first shown.
    pub fn selected_by_default(&self) -> bool {
        self.confidence == Confidence::Safe
    }

    /// Human readable location, e.g. `HKCU\Software\Example -> ValueName`.
    pub fn location(&self) -> String {
        match self.value.as_deref() {
            None => self.key.to_string(),
            Some("") => format!("{} -> (Default)", self.key),
            Some(name) => format!("{} -> {}", self.key, name),
        }
    }

    /// True when the entry still exists and every [`Condition`] still holds.
    ///
    /// [`crate::fix`] calls this for every issue right before changing
    /// anything, so stale scan results can never remove a live entry.
    pub fn still_valid(&self) -> bool {
        let Ok(key) = self.key.open() else {
            return false;
        };
        if let Some(name) = &self.value
            && key.get_raw_value(name).is_err()
        {
            return false;
        }
        self.conditions.iter().all(Condition::holds)
    }
}

fn root_needs_admin(root: RegRoot) -> bool {
    // Zest reports class registrations against the physical hives
    // (HKLM\SOFTWARE\Classes or HKCU\Software\Classes), never the merged
    // HKCR view, so the hive alone decides.
    !matches!(root, RegRoot::Hkcu)
}

/// What to scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Categories to scan. Duplicates are ignored.
    pub categories: Vec<Category>,
    /// Text fragments the user never wants reported. An issue is dropped
    /// when its location (`HKLM\...` or `HKEY_LOCAL_MACHINE\...`, optionally
    /// followed by `\ValueName`) contains one of them, ignoring case.
    pub excluded: Vec<String>,
}

impl Default for ScanOptions {
    /// Every category with [`Category::default_enabled`], no exclusions.
    fn default() -> Self {
        ScanOptions {
            categories: Category::ALL
                .into_iter()
                .filter(|c| c.default_enabled())
                .collect(),
            excluded: Vec::new(),
        }
    }
}

impl ScanOptions {
    /// Every category, including the review-only ones, no exclusions.
    pub fn all() -> Self {
        ScanOptions {
            categories: Category::ALL.to_vec(),
            excluded: Vec::new(),
        }
    }

    /// Only the given categories, no exclusions.
    pub fn only(categories: &[Category]) -> Self {
        ScanOptions {
            categories: categories.to_vec(),
            excluded: Vec::new(),
        }
    }

    /// True when `issue` matches one of the exclusion fragments.
    pub fn is_excluded(&self, issue: &Issue) -> bool {
        if self.excluded.is_empty() {
            return false;
        }
        let value = issue.value.as_deref().unwrap_or("");
        let short = format!("{}\\{}", issue.key, value).to_lowercase();
        let long = format!("{}\\{}", issue.key.full_name(), value).to_lowercase();
        self.excluded
            .iter()
            .map(|fragment| fragment.trim().to_lowercase())
            .filter(|fragment| !fragment.is_empty())
            .any(|fragment| short.contains(&fragment) || long.contains(&fragment))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for category in Category::ALL {
            assert!(
                seen.insert(category.slug()),
                "duplicate slug {}",
                category.slug()
            );
            assert_eq!(Category::from_slug(category.slug()), Some(category));
            assert_eq!(
                Category::from_slug(&category.slug().to_uppercase()),
                Some(category)
            );
            assert!(!category.label().is_empty());
            assert!(category.description().ends_with('.'));
        }
        assert_eq!(Category::from_slug("nope"), None);
    }

    #[test]
    fn default_scan_leaves_out_review_only_categories() {
        let defaults = ScanOptions::default();
        assert!(!defaults.categories.contains(&Category::Services));
        assert_eq!(defaults.categories.len(), Category::ALL.len() - 1);
        assert_eq!(ScanOptions::all().categories.len(), Category::ALL.len());
    }

    #[test]
    fn ids_locations_and_admin_flags() {
        let key = RegPath::hkcu(r"Software\Vendor\App");
        let whole = Issue::for_key(
            Category::ObsoleteSoftware,
            &key,
            "t",
            "d",
            Confidence::Safe,
            vec![],
        );
        assert_eq!(whole.id, r"obsolete-software|HKCU\Software\Vendor\App");
        assert_eq!(whole.location(), r"HKCU\Software\Vendor\App");
        assert!(!whole.requires_admin);
        assert!(whole.selected_by_default());

        let machine = RegPath::hklm(r"SOFTWARE\Vendor");
        let value = Issue::for_value(
            Category::Startup,
            &machine,
            "Updater",
            "t",
            "d",
            Confidence::Review,
            vec![],
        );
        assert_eq!(value.id, r"startup|HKLM\SOFTWARE\Vendor|Updater");
        assert_eq!(value.location(), r"HKLM\SOFTWARE\Vendor -> Updater");
        assert!(value.requires_admin);
        assert!(!value.selected_by_default());

        let default_value = Issue::for_value(
            Category::SoundEvents,
            &key,
            "",
            "t",
            "d",
            Confidence::Safe,
            vec![],
        );
        assert_eq!(
            default_value.location(),
            r"HKCU\Software\Vendor\App -> (Default)"
        );
    }

    #[test]
    fn exclusions_match_short_and_long_names() {
        let issue = Issue::for_value(
            Category::Startup,
            &RegPath::hklm(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"),
            "MyUpdater",
            "t",
            "d",
            Confidence::Safe,
            vec![],
        );
        let with = |fragment: &str| ScanOptions {
            categories: vec![],
            excluded: vec![fragment.to_owned()],
        };
        assert!(with("myupdater").is_excluded(&issue));
        assert!(with(r"HKLM\SOFTWARE\Microsoft").is_excluded(&issue));
        assert!(with(r"hkey_local_machine\software").is_excluded(&issue));
        assert!(with(r"CurrentVersion\Run\MyUpdater").is_excluded(&issue));
        assert!(!with("SomethingElse").is_excluded(&issue));
        assert!(!with("   ").is_excluded(&issue));
        assert!(!ScanOptions::default().is_excluded(&issue));
    }
}
