//! Data model of the cleaner: what can be cleaned, what was found, what was removed.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_core::delete::WipeMethod;
use zest_core::reg::RegPath;

/// Top-level tab a group is shown under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Section {
    /// Windows itself: system junk, Explorer history, advanced items.
    Windows,
    /// Web browsers.
    Browsers,
    /// Third-party applications.
    Applications,
    /// Folders and files added by the user.
    Custom,
}

impl Section {
    /// All sections in display order.
    pub const ALL: [Section; 4] = [
        Section::Windows,
        Section::Browsers,
        Section::Applications,
        Section::Custom,
    ];

    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            Section::Windows => "Windows",
            Section::Browsers => "Browsers",
            Section::Applications => "Applications",
            Section::Custom => "Custom",
        }
    }
}

/// Hint for the icon a front end should show for a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupIcon {
    System,
    Explorer,
    Advanced,
    Chrome,
    Edge,
    Firefox,
    Brave,
    Opera,
    Vivaldi,
    Browser,
    Chat,
    Media,
    Game,
    Developer,
    Office,
    Graphics,
    Store,
    App,
    Custom,
}

/// Whether a rule frees space or removes traces of activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleKind {
    /// Temporary files, caches, logs, dumps: safe to remove, frees space.
    Junk,
    /// History, cookies, recently-used lists: removes traces of activity.
    Privacy,
}

/// Engine-implemented operations that are not plain file or registry deletion.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Special {
    /// Empty the Recycle Bin on all drives.
    RecycleBin,
    /// Flush the DNS resolver cache.
    DnsCache,
    /// Clear the clipboard.
    Clipboard,
    /// Clear all Windows event logs.
    EventLogs,
    /// Remove shortcuts whose target no longer exists.
    BrokenShortcuts { dir: PathBuf },
    /// Remove cookies from a Chromium cookie database, honouring the keep list.
    ChromiumCookies { db: PathBuf },
    /// Remove per-site storage of a Chromium profile, honouring the keep list.
    ChromiumSiteData { profile: PathBuf },
    /// Remove download records from a Chromium `History` database.
    ChromiumDownloads { db: PathBuf },
    /// Remove saved form entries from a Chromium `Web Data` database.
    ChromiumAutofill { db: PathBuf },
    /// Remove cookies from a Firefox `cookies.sqlite`, honouring the keep list.
    FirefoxCookies { db: PathBuf },
    /// Remove per-site storage of a Firefox profile, honouring the keep list.
    FirefoxSiteData { profile: PathBuf },
    /// Remove browsing history from a Firefox `places.sqlite`, keeping bookmarks.
    FirefoxHistory { db: PathBuf },
    /// Compact SQLite databases in place.
    SqliteVacuum { dbs: Vec<PathBuf> },
    /// Remove top-level dotted keys from a JSON file.
    JsonRemoveKeys { file: PathBuf, keys: Vec<String> },
}

/// One primitive operation of a rule, with every path already resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Delete files below `dir` whose name matches `pattern`
    /// (`;`-separated wildcards). The folder itself is kept.
    Files {
        dir: PathBuf,
        pattern: String,
        recursive: bool,
        /// Only files not modified for at least this many days (0 = any age).
        min_age_days: u32,
    },
    /// Delete one specific file.
    File(PathBuf),
    /// Delete a folder including everything inside it.
    Folder(PathBuf),
    /// Delete every value of a key (and its sub-keys when `include_subkeys`).
    /// The key itself is kept.
    RegValues { key: RegPath, include_subkeys: bool },
    /// Delete one value.
    RegValue { key: RegPath, name: String },
    /// Delete a key and everything below it.
    RegKey(RegPath),
    /// Engine-implemented operation.
    Special(Special),
}

/// One checkbox in the cleaner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable identifier, e.g. `chrome.cache`. Used in settings and on the CLI.
    pub id: String,
    /// Short display name.
    pub name: String,
    /// One-sentence explanation of what is removed and the consequence.
    pub description: String,
    /// Junk or privacy.
    pub kind: RuleKind,
    /// Whether the rule is selected in a fresh installation.
    pub default_enabled: bool,
    /// Shown before the rule is enabled when it removes something users may
    /// want to keep (saved passwords, open tabs, ...).
    pub warning: Option<String>,
    /// The rule only works fully with administrator rights.
    pub requires_admin: bool,
    /// Only files older than the configured minimum age are removed.
    pub age_filtered: bool,
    /// The operations to perform.
    pub actions: Vec<Action>,
}

/// A program or area the rules belong to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Stable identifier, e.g. `chrome`.
    pub id: String,
    /// Tab the group is shown under.
    pub section: Section,
    /// Display name, e.g. `Google Chrome`.
    pub name: String,
    /// Icon hint.
    pub icon: GroupIcon,
    /// Executable names that must not be running while the group is cleaned.
    pub processes: Vec<String>,
    /// The group's rules.
    pub rules: Vec<Rule>,
}

/// Everything that can be cleaned on this PC right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// Groups in display order.
    pub groups: Vec<Group>,
}

impl Catalog {
    /// Iterates over all rules together with their group.
    pub fn rules(&self) -> impl Iterator<Item = (&Group, &Rule)> {
        self.groups
            .iter()
            .flat_map(|g| g.rules.iter().map(move |r| (g, r)))
    }

    /// Finds a rule by identifier.
    pub fn rule(&self, id: &str) -> Option<(&Group, &Rule)> {
        self.rules().find(|(_, r)| r.id == id)
    }

    /// Groups of one section.
    pub fn section(&self, section: Section) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(move |g| g.section == section)
    }

    /// Identifiers of all rules that are on by default.
    pub fn default_selection(&self) -> HashSet<String> {
        self.rules()
            .filter(|(_, r)| r.default_enabled)
            .map(|(_, r)| r.id.clone())
            .collect()
    }

    /// Applies the user's explicit choices on top of the defaults.
    ///
    /// `overrides` maps rule id to "enabled". Rules without an entry use
    /// their default. Unknown ids are ignored.
    pub fn selection(&self, overrides: &BTreeMap<String, bool>) -> HashSet<String> {
        self.rules()
            .filter(|(_, r)| overrides.get(&r.id).copied().unwrap_or(r.default_enabled))
            .map(|(_, r)| r.id.clone())
            .collect()
    }

    /// Identifiers of every rule of the given kind.
    pub fn rules_of_kind(&self, kind: RuleKind) -> HashSet<String> {
        self.rules()
            .filter(|(_, r)| r.kind == kind)
            .map(|(_, r)| r.id.clone())
            .collect()
    }
}

/// How a custom include treats its folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IncludeMode {
    /// Matching files directly inside the folder.
    #[default]
    FilesOnly,
    /// Matching files in the folder and all sub-folders (emptied sub-folders are removed).
    FilesAndSubfolders,
    /// The folder itself and everything in it.
    WholeFolder,
}

/// A user-defined location to clean.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Include {
    /// A file, or a folder.
    pub path: PathBuf,
    /// File name wildcards (`;`-separated). Empty means all files.
    #[serde(default)]
    pub pattern: String,
    /// How a folder is treated. Ignored for single files.
    #[serde(default)]
    pub mode: IncludeMode,
}

/// Something the user never wants cleaned.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exclusion {
    /// A file, or a folder and everything below it.
    Path(PathBuf),
    /// Files matching a wildcard inside a folder (and its sub-folders).
    Pattern { dir: PathBuf, pattern: String },
    /// A registry key (and everything below it), written like `HKCU\Software\X`.
    Registry(String),
}

/// Options for one scan / clean run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanOptions {
    /// Identifiers of the rules to run.
    pub enabled: HashSet<String>,
    /// Age-filtered rules skip files modified within this many hours.
    pub min_age_hours: u32,
    /// Locations and patterns that are never touched.
    pub exclusions: Vec<Exclusion>,
    /// Cookie domains to keep, e.g. `example.com` (sub-domains included).
    pub cookie_keep: Vec<String>,
    /// How file contents are destroyed.
    pub wipe: WipeMethod,
    /// Clean groups even though one of their programs is running.
    pub ignore_running: bool,
}

/// What a finding refers to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// A file (or a link) to delete.
    File(PathBuf),
    /// A folder to delete with all contents.
    Dir(PathBuf),
    /// A registry value to delete.
    RegValue { key: RegPath, name: String },
    /// A registry key to delete.
    RegKey(RegPath),
    /// A special operation to run.
    Special(Special),
}

impl Target {
    /// Text for the detailed results list.
    pub fn display(&self) -> String {
        match self {
            Target::File(p) | Target::Dir(p) => p.display().to_string(),
            Target::RegValue { key, name } if name.is_empty() => format!("{key}\\(Default)"),
            Target::RegValue { key, name } => format!("{key}\\{name}"),
            Target::RegKey(key) => key.to_string(),
            Target::Special(special) => special.display(),
        }
    }

    /// The file system path, for file and folder targets.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Target::File(p) | Target::Dir(p) => Some(p),
            _ => None,
        }
    }
}

impl Special {
    /// Text for the detailed results list.
    pub fn display(&self) -> String {
        match self {
            Special::RecycleBin => "Recycle Bin".to_owned(),
            Special::DnsCache => "DNS resolver cache".to_owned(),
            Special::Clipboard => "Clipboard contents".to_owned(),
            Special::EventLogs => "Windows event logs".to_owned(),
            Special::BrokenShortcuts { dir } => dir.display().to_string(),
            Special::ChromiumCookies { db }
            | Special::ChromiumDownloads { db }
            | Special::ChromiumAutofill { db }
            | Special::FirefoxCookies { db }
            | Special::FirefoxHistory { db } => db.display().to_string(),
            Special::ChromiumSiteData { profile } | Special::FirefoxSiteData { profile } => {
                profile.display().to_string()
            }
            Special::SqliteVacuum { dbs } => dbs
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            Special::JsonRemoveKeys { file, .. } => file.display().to_string(),
        }
    }
}

/// One thing a scan found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// What would be removed.
    pub target: Target,
    /// Bytes that removing it frees.
    pub size: u64,
    /// Number of user-visible items this stands for (files, cookies, entries).
    pub items: u64,
    /// Extra description, e.g. `1,204 cookies`.
    pub note: Option<String>,
}

impl Finding {
    /// A single file of `size` bytes.
    pub fn file(path: PathBuf, size: u64) -> Self {
        Finding {
            target: Target::File(path),
            size,
            items: 1,
            note: None,
        }
    }
}

/// Scan outcome for one rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleResult {
    /// Rule identifier.
    pub rule_id: String,
    /// Group identifier.
    pub group_id: String,
    /// Everything found.
    pub findings: Vec<Finding>,
    /// Total bytes.
    pub bytes: u64,
    /// Total items.
    pub items: u64,
    /// Set when the group's program is running, e.g. `Google Chrome is running`.
    pub blocked_by: Option<String>,
    /// The rule needs administrator rights that are not available.
    pub needs_admin: bool,
    /// Folders whose emptied sub-folders are removed after cleaning.
    pub prune_roots: Vec<PathBuf>,
}

impl RuleResult {
    /// Adds a finding and updates the totals.
    pub fn push(&mut self, finding: Finding) {
        self.bytes += finding.size;
        self.items += finding.items;
        self.findings.push(finding);
    }

    /// True when there is nothing to clean.
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Outcome of a scan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// One entry per enabled rule that found something, in catalog order.
    pub results: Vec<RuleResult>,
    /// Total bytes that cleaning would free.
    pub bytes: u64,
    /// Total number of items.
    pub items: u64,
    /// How long the scan took.
    pub elapsed: Duration,
}

impl ScanReport {
    /// Result for one rule, if it found anything.
    pub fn rule(&self, rule_id: &str) -> Option<&RuleResult> {
        self.results.iter().find(|r| r.rule_id == rule_id)
    }

    /// Bytes found per group, in catalog order.
    pub fn bytes_by_group(&self) -> Vec<(String, u64, u64)> {
        let mut out: Vec<(String, u64, u64)> = Vec::new();
        for result in &self.results {
            match out.iter_mut().find(|(id, _, _)| *id == result.group_id) {
                Some(entry) => {
                    entry.1 += result.bytes;
                    entry.2 += result.items;
                }
                None => out.push((result.group_id.clone(), result.bytes, result.items)),
            }
        }
        out
    }

    /// Display names of running programs that block some results.
    pub fn blockers(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for result in &self.results {
            if let Some(blocker) = &result.blocked_by
                && !names.contains(blocker)
            {
                names.push(blocker.clone());
            }
        }
        names
    }
}

/// Clean outcome for one rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCleanResult {
    /// Rule identifier.
    pub rule_id: String,
    /// Group identifier.
    pub group_id: String,
    /// Bytes actually freed.
    pub bytes: u64,
    /// Items actually removed.
    pub items: u64,
    /// Items skipped because they were in use by a running program.
    pub in_use: u64,
    /// Items that failed for another reason.
    pub failed: u64,
    /// Why the whole rule was skipped, if it was.
    pub skipped: Option<String>,
}

/// Outcome of a clean run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanReport {
    /// One entry per rule that was processed.
    pub rules: Vec<RuleCleanResult>,
    /// Total bytes freed.
    pub bytes: u64,
    /// Total items removed.
    pub items: u64,
    /// Items skipped because they were in use.
    pub in_use: u64,
    /// Items that could not be removed for another reason.
    pub failed: u64,
    /// The first failures, for display: (what, why).
    pub failures: Vec<(String, String)>,
    /// How long cleaning took.
    pub elapsed: Duration,
    /// True for a dry run: nothing was actually removed.
    pub dry_run: bool,
}

/// Maximum number of failures kept in a [`CleanReport`].
pub const MAX_REPORTED_FAILURES: usize = 200;
