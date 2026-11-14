//! Windows privacy settings: what Windows collects, suggests and advertises.
//!
//! Each [`Tweak`] is a small set of registry values with two known states:
//! *private* (the setting is applied) and the *Windows default*. [`Privacy`]
//! reads the current state, applies settings and puts the defaults back.
//! Before anything is written, the values about to change are exported to a
//! `.reg` file, so one double-click undoes a run.
//!
//! Two kinds of values are used, and nothing else:
//!
//! * [`Mechanism::Setting`] – the value the Settings app itself writes for
//!   the signed-in user. Works on every edition without administrator rights.
//! * [`Mechanism::Policy`] – a documented administrative policy. Windows
//!   protects policy keys, so these need administrator rights, and Microsoft
//!   documents them for the Pro, Enterprise and Education editions only.

mod catalog;

pub use catalog::CATALOG;

use serde::Serialize;
use std::path::PathBuf;
use winreg::enums::RegType;
use zest_core::reg::{RegBackup, RegPath, delete_value};
use zest_core::{Error, Result, elevation};

/// Group a setting is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Category {
    /// Diagnostic data and feedback sent to Microsoft.
    Diagnostics,
    /// Advertising, suggestions and promotions.
    Suggestions,
    /// History of what was opened and searched for.
    Activity,
    /// Typing, handwriting and speech.
    Input,
    /// Copilot, Recall and similar features.
    Ai,
    /// Location and syncing between devices.
    Devices,
}

impl Category {
    /// All categories in display order.
    pub const ALL: [Category; 6] = [
        Category::Diagnostics,
        Category::Suggestions,
        Category::Activity,
        Category::Input,
        Category::Ai,
        Category::Devices,
    ];

    /// Heading shown above the category.
    pub fn label(self) -> &'static str {
        match self {
            Category::Diagnostics => "Diagnostics and feedback",
            Category::Suggestions => "Ads and suggestions",
            Category::Activity => "Activity and search",
            Category::Input => "Typing and speech",
            Category::Ai => "AI features",
            Category::Devices => "Location and other devices",
        }
    }
}

/// Who a setting applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Scope {
    /// The signed-in user (`HKEY_CURRENT_USER`).
    User,
    /// Everyone on this PC (`HKEY_LOCAL_MACHINE`).
    Machine,
}

/// How a setting is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Mechanism {
    /// The value the Settings app writes; the user can flip it there too.
    Setting,
    /// An administrative policy. Windows shows "managed by your
    /// organization" next to settings a policy controls.
    Policy,
}

/// One registry value of a [`Tweak`]. All catalog values are numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RegWrite {
    /// Key below the hive of the tweak's [`Scope`].
    pub key: &'static str,
    /// Value name.
    pub name: &'static str,
    /// The number that makes the setting private.
    pub private: u32,
    /// The Windows default: a number, or `None` when the value does not
    /// exist by default. A missing value always counts as the default.
    pub default: Option<u32>,
}

/// A privacy setting Zest can switch.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Tweak {
    /// Stable identifier, also used on the command line.
    pub id: &'static str,
    /// What switching the setting on does, as an instruction.
    pub title: &'static str,
    /// What changes for the user once it is on.
    pub description: &'static str,
    /// Extra context, or empty.
    pub note: &'static str,
    /// What cannot be undone once the setting is on, or empty. Front ends
    /// ask before switching such a setting on.
    pub caution: &'static str,
    pub category: Category,
    pub scope: Scope,
    pub mechanism: Mechanism,
    /// Part of the one-click set: private without taking a feature away.
    pub recommended: bool,
    /// First Windows build the setting applies to.
    pub min_build: Option<u32>,
    /// Last Windows build the setting applies to.
    pub max_build: Option<u32>,
    /// The Microsoft documentation the registry values were checked against.
    pub source: &'static str,
    /// The registry values that make up the setting.
    pub writes: &'static [RegWrite],
}

impl Tweak {
    /// Looks a setting up by id.
    pub fn find(id: &str) -> Option<&'static Tweak> {
        CATALOG.iter().find(|t| t.id.eq_ignore_ascii_case(id))
    }

    /// True when switching it needs administrator rights. Policies do even
    /// for the current user: Windows lets only administrators write to
    /// `HKCU\Software\Policies`.
    pub fn requires_admin(&self) -> bool {
        self.scope == Scope::Machine || self.mechanism == Mechanism::Policy
    }

    /// True when the setting exists on the given Windows build.
    pub fn applies_to(&self, build: u32) -> bool {
        self.min_build.is_none_or(|min| build >= min)
            && self.max_build.is_none_or(|max| build <= max)
    }
}

/// Where a setting currently stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum State {
    /// Every value is at its private setting.
    Private,
    /// Every value is at the Windows default.
    Default,
    /// Something else: partly applied, or set to another value.
    Custom,
    /// The setting does not exist on this version of Windows.
    NotApplicable,
}

/// A setting together with its current state on this PC.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TweakStatus {
    pub tweak: &'static Tweak,
    pub state: State,
    /// Changing it needs administrator rights the process does not have.
    pub needs_admin: bool,
    /// A policy on Windows Home: Microsoft documents it for Pro and above,
    /// so Home may not follow it.
    pub edition_caveat: bool,
}

/// The parts of the Windows version that decide which settings apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Windows {
    /// Build number, e.g. 26100 for Windows 11 24H2.
    pub build: u32,
    /// A Home edition.
    pub home: bool,
}

impl Windows {
    /// Reads the running Windows version.
    pub fn current() -> Windows {
        let key = RegPath::hklm(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
        let build = key
            .read_string("CurrentBuildNumber")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let home = key
            .read_string("EditionID")
            .is_some_and(|edition| is_home_edition(&edition));
        Windows { build, home }
    }
}

/// Home editions are called `Core`, `CoreN`, `CoreSingleLanguage` and
/// `CoreCountrySpecific` in the registry.
fn is_home_edition(edition_id: &str) -> bool {
    edition_id.to_ascii_lowercase().starts_with("core")
}

/// Maps the two hives the catalog writes to onto real registry locations.
///
/// Tests redirect both below a scratch key so nothing real is touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hives {
    user: RegPath,
    machine: RegPath,
}

impl Hives {
    /// The real `HKEY_CURRENT_USER` and `HKEY_LOCAL_MACHINE`.
    pub fn system() -> Hives {
        Hives {
            user: RegPath::hkcu(""),
            machine: RegPath::hklm(""),
        }
    }

    /// Both hives redirected to `<base>\HKCU` and `<base>\HKLM`.
    pub fn redirected(base: &RegPath) -> Hives {
        Hives {
            user: base.child("HKCU"),
            machine: base.child("HKLM"),
        }
    }

    /// The full location of a catalog key.
    pub fn locate(&self, scope: Scope, key: &str) -> RegPath {
        match scope {
            Scope::User => self.user.child(key),
            Scope::Machine => self.machine.child(key),
        }
    }
}

/// What a registry value currently holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Current {
    Absent,
    Number(u64),
    /// Present, but not a number (or not readable).
    Other,
}

fn read_current(path: &RegPath, name: &str) -> Current {
    let key = match path.open() {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Current::Absent,
        Err(_) => return Current::Other,
    };
    match key.get_raw_value(name) {
        Ok(value) => match (&value.vtype, value.bytes.len()) {
            (RegType::REG_DWORD, 4) => Current::Number(u64::from(u32::from_le_bytes([
                value.bytes[0],
                value.bytes[1],
                value.bytes[2],
                value.bytes[3],
            ]))),
            (RegType::REG_QWORD, 8) => {
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&value.bytes);
                Current::Number(u64::from_le_bytes(raw))
            }
            _ => Current::Other,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Current::Absent,
        Err(_) => Current::Other,
    }
}

impl RegWrite {
    fn is_private(&self, current: Current) -> bool {
        current == Current::Number(u64::from(self.private))
    }

    fn is_default(&self, current: Current) -> bool {
        match current {
            Current::Absent => true,
            Current::Number(n) => self.default.is_some_and(|d| n == u64::from(d)),
            Current::Other => false,
        }
    }
}

/// What happened to one setting during [`Privacy::apply`] or [`Privacy::revert`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ItemOutcome {
    /// The registry was changed.
    Changed,
    /// It was already in the requested state.
    Unchanged,
    /// Skipped: administrator rights are needed.
    NeedsAdmin,
    /// Skipped: the setting does not exist on this version of Windows.
    NotApplicable,
    /// No setting has this id.
    UnknownId,
    /// A registry write failed; the message says which.
    Failed(String),
}

/// The result for one requested setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ItemResult {
    pub id: String,
    pub title: String,
    pub outcome: ItemOutcome,
}

/// The result of one [`Privacy::apply`] or [`Privacy::revert`] call.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Outcome {
    /// One entry per requested id, in request order.
    pub items: Vec<ItemResult>,
    /// The `.reg` file that undoes this run, when anything was changed.
    pub backup: Option<PathBuf>,
}

impl Outcome {
    /// Number of settings that were changed.
    pub fn changed(&self) -> usize {
        self.count(|o| *o == ItemOutcome::Changed)
    }

    /// Number of settings skipped for lack of administrator rights.
    pub fn needs_admin(&self) -> usize {
        self.count(|o| *o == ItemOutcome::NeedsAdmin)
    }

    /// Number of settings where a write failed.
    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, ItemOutcome::Failed(_)))
    }

    fn count(&self, pick: impl Fn(&ItemOutcome) -> bool) -> usize {
        self.items.iter().filter(|i| pick(&i.outcome)).count()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Private,
    Default,
}

/// Reads and switches the settings of the [`CATALOG`].
#[derive(Clone, Debug)]
pub struct Privacy {
    hives: Hives,
    windows: Windows,
    elevated: bool,
    backup_dir: Option<PathBuf>,
}

impl Privacy {
    /// Works on the real registry of this PC.
    pub fn system() -> Privacy {
        Privacy {
            hives: Hives::system(),
            windows: Windows::current(),
            elevated: elevation::is_elevated(),
            backup_dir: None,
        }
    }

    /// Works on the given hives as if running on `windows`. `elevated` says
    /// whether settings that need administrator rights may be written.
    pub fn new(hives: Hives, windows: Windows, elevated: bool) -> Privacy {
        Privacy {
            hives,
            windows,
            elevated,
            backup_dir: None,
        }
    }

    /// Saves backups into `dir` instead of the registry backup folder.
    pub fn with_backup_dir(mut self, dir: impl Into<PathBuf>) -> Privacy {
        self.backup_dir = Some(dir.into());
        self
    }

    /// The Windows version settings are judged against.
    pub fn windows(&self) -> Windows {
        self.windows
    }

    /// Every setting of the catalog with its current state. Read-only.
    pub fn list(&self) -> Vec<TweakStatus> {
        CATALOG.iter().map(|tweak| self.status_of(tweak)).collect()
    }

    /// The current state of one setting. Read-only.
    pub fn status(&self, id: &str) -> Option<TweakStatus> {
        Tweak::find(id).map(|tweak| self.status_of(tweak))
    }

    /// Ids of the recommended settings that apply here and are not private yet.
    pub fn recommended_pending(&self) -> Vec<&'static str> {
        self.list()
            .into_iter()
            .filter(|s| {
                s.tweak.recommended && !matches!(s.state, State::Private | State::NotApplicable)
            })
            .map(|s| s.tweak.id)
            .collect()
    }

    fn status_of(&self, tweak: &'static Tweak) -> TweakStatus {
        TweakStatus {
            tweak,
            state: self.state_of(tweak),
            needs_admin: tweak.requires_admin() && !self.elevated,
            edition_caveat: tweak.mechanism == Mechanism::Policy && self.windows.home,
        }
    }

    fn state_of(&self, tweak: &Tweak) -> State {
        if !tweak.applies_to(self.windows.build) {
            return State::NotApplicable;
        }
        let current: Vec<Current> = tweak
            .writes
            .iter()
            .map(|w| read_current(&self.hives.locate(tweak.scope, w.key), w.name))
            .collect();
        let all = |test: fn(&RegWrite, Current) -> bool| {
            tweak.writes.iter().zip(&current).all(|(w, c)| test(w, *c))
        };
        if all(RegWrite::is_private) {
            State::Private
        } else if all(RegWrite::is_default) {
            State::Default
        } else {
            State::Custom
        }
    }

    /// Makes the given settings private.
    ///
    /// The values about to change are saved to a `.reg` file first; when that
    /// file cannot be written nothing is changed and the error is returned.
    /// Problems with individual settings are reported per item instead.
    pub fn apply(&self, ids: &[&str]) -> Result<Outcome> {
        self.change(ids, Target::Private, "privacy-apply")
    }

    /// Puts the given settings back to the Windows default. Backed up and
    /// reported the same way as [`Privacy::apply`].
    pub fn revert(&self, ids: &[&str]) -> Result<Outcome> {
        self.change(ids, Target::Default, "privacy-defaults")
    }

    fn change(&self, ids: &[&str], target: Target, label: &str) -> Result<Outcome> {
        // Decide what to do for each id before touching anything.
        let mut items = Vec::with_capacity(ids.len());
        let mut work: Vec<(usize, &'static Tweak)> = Vec::new();
        for id in ids {
            let Some(tweak) = Tweak::find(id) else {
                items.push(ItemResult {
                    id: (*id).to_owned(),
                    title: String::new(),
                    outcome: ItemOutcome::UnknownId,
                });
                continue;
            };
            let state = self.state_of(tweak);
            let outcome = if state == State::NotApplicable {
                ItemOutcome::NotApplicable
            } else if (target == Target::Private && state == State::Private)
                || (target == Target::Default && state == State::Default)
            {
                ItemOutcome::Unchanged
            } else if tweak.requires_admin() && !self.elevated {
                ItemOutcome::NeedsAdmin
            } else {
                work.push((items.len(), tweak));
                ItemOutcome::Changed
            };
            items.push(ItemResult {
                id: tweak.id.to_owned(),
                title: tweak.title.to_owned(),
                outcome,
            });
        }
        if work.is_empty() {
            return Ok(Outcome {
                items,
                backup: None,
            });
        }

        // The undo file is on disk before the first write.
        let mut backup = match &self.backup_dir {
            Some(dir) => RegBackup::at(unique_file(dir, label)),
            None => RegBackup::new(label),
        };
        for (_, tweak) in &work {
            for write in tweak.writes {
                let path = self.hives.locate(tweak.scope, write.key);
                match read_current(&path, write.name) {
                    Current::Absent => {
                        // Only worth recording when this run creates the value.
                        if target == Target::Private || write.default.is_some() {
                            backup.add_absent_value(&path, write.name);
                        }
                    }
                    Current::Number(_) | Current::Other => backup.add_value(&path, write.name)?,
                }
            }
        }
        let saved = backup.save()?;

        for (index, tweak) in work {
            if let Err(e) = self.write_tweak(tweak, target) {
                log::warn!("privacy setting {} was not changed: {e}", tweak.id);
                items[index].outcome = if e.is_access_denied() {
                    ItemOutcome::NeedsAdmin
                } else {
                    ItemOutcome::Failed(e.to_string())
                };
            }
        }
        log::info!(
            "privacy: {} of {} settings changed ({label})",
            items
                .iter()
                .filter(|i| i.outcome == ItemOutcome::Changed)
                .count(),
            items.len()
        );
        Ok(Outcome {
            items,
            backup: saved,
        })
    }

    fn write_tweak(&self, tweak: &Tweak, target: Target) -> Result<()> {
        for write in tweak.writes {
            let path = self.hives.locate(tweak.scope, write.key);
            let wanted = match target {
                Target::Private => Some(write.private),
                Target::Default => write.default,
            };
            match wanted {
                Some(number) => set_number(&path, write.name, number)?,
                None => delete_value(&path, write.name, None)?,
            }
        }
        Ok(())
    }
}

fn unique_file(dir: &std::path::Path, label: &str) -> PathBuf {
    let mut file = dir.join(format!("{label}.reg"));
    let mut n = 1;
    while file.exists() {
        n += 1;
        file = dir.join(format!("{label}-{n}.reg"));
    }
    file
}

fn set_number(path: &RegPath, name: &str, number: u32) -> Result<()> {
    let what = format!("write registry value {path}\\{name}");
    let (key, _) = path
        .root
        .key()
        .create_subkey(&path.path)
        .map_err(|e| access_error(e, &what))?;
    key.set_value(name, &number)
        .map_err(|e| access_error(e, &what))
}

fn access_error(e: std::io::Error, what: &str) -> Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        Error::NeedsElevation(what.to_owned())
    } else {
        Error::Io {
            context: what.to_owned(),
            source: e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use zest_core::reg::{RegRoot, import_reg_file};

    const WIN11_24H2: Windows = Windows {
        build: 26100,
        home: false,
    };

    /// A scratch area under HKCU standing in for both hives; removed on drop.
    struct Sandbox {
        base: RegPath,
        tmp: tempfile::TempDir,
    }

    impl Sandbox {
        fn new() -> Sandbox {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let base = RegPath::hkcu(format!(
                "Software\\ZestTests\\tweaks-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            RegRoot::Hkcu.key().create_subkey(&base.path).unwrap();
            Sandbox {
                base,
                tmp: tempfile::tempdir().unwrap(),
            }
        }

        fn privacy(&self, windows: Windows, elevated: bool) -> Privacy {
            Privacy::new(Hives::redirected(&self.base), windows, elevated)
                .with_backup_dir(self.tmp.path())
        }

        fn path(&self, tweak: &Tweak, write: &RegWrite) -> RegPath {
            Hives::redirected(&self.base).locate(tweak.scope, write.key)
        }

        fn set(&self, tweak: &Tweak, write: &RegWrite, number: u32) {
            set_number(&self.path(tweak, write), write.name, number).unwrap();
        }

        fn get(&self, tweak: &Tweak, write: &RegWrite) -> Option<u32> {
            self.path(tweak, write).read_u32(write.name)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = RegRoot::Hkcu.key().delete_subkey_all(&self.base.path);
            // Remove the shared parent when no other test is using it.
            let _ = RegRoot::Hkcu.key().delete_subkey("Software\\ZestTests");
        }
    }

    fn tweak(id: &str) -> &'static Tweak {
        Tweak::find(id).unwrap_or_else(|| panic!("no tweak {id}"))
    }

    #[test]
    fn a_fresh_registry_is_all_defaults() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let list = privacy.list();
        assert_eq!(list.len(), CATALOG.len());
        for status in &list {
            let expected = if status.tweak.applies_to(WIN11_24H2.build) {
                State::Default
            } else {
                State::NotApplicable
            };
            assert_eq!(status.state, expected, "{}", status.tweak.id);
            assert!(!status.needs_admin);
            assert!(!status.edition_caveat);
        }
    }

    #[test]
    fn apply_then_revert_round_trips_every_setting() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let ids: Vec<&str> = CATALOG.iter().map(|t| t.id).collect();

        let applied = privacy.apply(&ids).unwrap();
        assert_eq!(applied.items.len(), ids.len());
        assert!(applied.backup.as_ref().is_some_and(|f| f.is_file()));
        for (item, tweak) in applied.items.iter().zip(CATALOG) {
            if tweak.applies_to(WIN11_24H2.build) {
                assert_eq!(item.outcome, ItemOutcome::Changed, "{}", tweak.id);
                assert_eq!(privacy.status(tweak.id).unwrap().state, State::Private);
                for write in tweak.writes {
                    assert_eq!(sandbox.get(tweak, write), Some(write.private));
                }
            } else {
                assert_eq!(item.outcome, ItemOutcome::NotApplicable, "{}", tweak.id);
                for write in tweak.writes {
                    assert_eq!(sandbox.get(tweak, write), None, "{}", tweak.id);
                }
            }
        }

        // A second run has nothing left to do and writes no backup.
        let again = privacy.apply(&ids).unwrap();
        assert_eq!(again.changed(), 0);
        assert!(again.backup.is_none());

        let reverted = privacy.revert(&ids).unwrap();
        assert_eq!(reverted.changed(), applied.changed());
        for tweak in CATALOG.iter().filter(|t| t.applies_to(WIN11_24H2.build)) {
            assert_eq!(
                privacy.status(tweak.id).unwrap().state,
                State::Default,
                "{}",
                tweak.id
            );
            for write in tweak.writes {
                assert_eq!(sandbox.get(tweak, write), write.default, "{}", tweak.id);
            }
        }
    }

    #[test]
    fn state_tells_private_default_and_custom_apart() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);

        // One value, default "absent".
        let telemetry = tweak("diagnostic-data");
        let write = &telemetry.writes[0];
        assert_eq!(privacy.status(telemetry.id).unwrap().state, State::Default);
        sandbox.set(telemetry, write, 3);
        assert_eq!(privacy.status(telemetry.id).unwrap().state, State::Custom);
        sandbox.set(telemetry, write, 1);
        assert_eq!(privacy.status(telemetry.id).unwrap().state, State::Private);

        // Several values with explicit defaults: partly applied is custom.
        let tips = tweak("suggestions-and-tips");
        for write in tips.writes {
            sandbox.set(tips, write, 1);
        }
        assert_eq!(privacy.status(tips.id).unwrap().state, State::Default);
        sandbox.set(tips, &tips.writes[0], 0);
        assert_eq!(privacy.status(tips.id).unwrap().state, State::Custom);
        for write in tips.writes {
            sandbox.set(tips, write, 0);
        }
        assert_eq!(privacy.status(tips.id).unwrap().state, State::Private);
    }

    #[test]
    fn numbers_stored_as_qwords_and_other_types_are_understood() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let feedback = tweak("feedback-requests");
        for write in feedback.writes {
            let path = sandbox.path(feedback, write);
            let (key, _) = RegRoot::Hkcu.key().create_subkey(&path.path).unwrap();
            key.set_value(write.name, &0u64).unwrap();
        }
        assert_eq!(privacy.status(feedback.id).unwrap().state, State::Private);

        let path = sandbox.path(feedback, &feedback.writes[0]);
        path.open_write()
            .unwrap()
            .set_value(feedback.writes[0].name, &"never")
            .unwrap();
        assert_eq!(privacy.status(feedback.id).unwrap().state, State::Custom);
    }

    #[test]
    fn settings_outside_the_build_range_are_left_alone() {
        let sandbox = Sandbox::new();
        let copilot = tweak("copilot-panel");
        let recall = tweak("recall-snapshots");

        let win11_23h2 = sandbox.privacy(
            Windows {
                build: 22631,
                home: false,
            },
            true,
        );
        assert_eq!(win11_23h2.status(copilot.id).unwrap().state, State::Default);
        assert_eq!(
            win11_23h2.status(recall.id).unwrap().state,
            State::NotApplicable
        );
        let outcome = win11_23h2.apply(&[recall.id]).unwrap();
        assert_eq!(outcome.items[0].outcome, ItemOutcome::NotApplicable);
        assert_eq!(sandbox.get(recall, &recall.writes[0]), None);
        assert!(outcome.backup.is_none());

        let win11_24h2 = sandbox.privacy(WIN11_24H2, true);
        assert_eq!(
            win11_24h2.status(copilot.id).unwrap().state,
            State::NotApplicable
        );
        assert_eq!(win11_24h2.status(recall.id).unwrap().state, State::Default);

        let win10 = sandbox.privacy(
            Windows {
                build: 19045,
                home: false,
            },
            true,
        );
        assert_eq!(win10.status("widgets").unwrap().state, State::NotApplicable);
        assert_eq!(
            win10.status("news-and-interests").unwrap().state,
            State::Default
        );
    }

    #[test]
    fn without_administrator_rights_only_plain_settings_change() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, false);
        let outcome = privacy
            .apply(&[
                "diagnostic-data",
                "tailored-experiences",
                "suggestions-and-tips",
            ])
            .unwrap();
        assert_eq!(outcome.items[0].outcome, ItemOutcome::NeedsAdmin);
        assert_eq!(
            outcome.items[1].outcome,
            ItemOutcome::NeedsAdmin,
            "per-user policies are protected too"
        );
        assert_eq!(outcome.items[2].outcome, ItemOutcome::Changed);
        assert_eq!(outcome.needs_admin(), 2);
        assert_eq!(outcome.changed(), 1);

        let telemetry = tweak("diagnostic-data");
        assert_eq!(sandbox.get(telemetry, &telemetry.writes[0]), None);
        assert!(privacy.status("diagnostic-data").unwrap().needs_admin);
        assert!(!privacy.status("suggestions-and-tips").unwrap().needs_admin);
    }

    #[test]
    fn unknown_ids_are_reported_and_nothing_else_is_disturbed() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let outcome = privacy
            .apply(&["no-such-setting", "Language-List"])
            .unwrap();
        assert_eq!(outcome.items[0].outcome, ItemOutcome::UnknownId);
        assert_eq!(outcome.items[1].id, "language-list", "ids ignore case");
        assert_eq!(outcome.items[1].outcome, ItemOutcome::Changed);
    }

    #[test]
    fn the_backup_file_undoes_a_run_exactly() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let telemetry = tweak("diagnostic-data");
        let tips = tweak("suggestions-and-tips");
        // The user had chosen their own values before.
        sandbox.set(telemetry, &telemetry.writes[0], 3);
        sandbox.set(tips, &tips.writes[0], 1);

        let outcome = privacy.apply(&[telemetry.id, tips.id]).unwrap();
        assert_eq!(outcome.changed(), 2);
        let backup = outcome.backup.expect("a backup is written");
        assert_eq!(sandbox.get(telemetry, &telemetry.writes[0]), Some(1));

        import_reg_file(&backup).unwrap();
        assert_eq!(
            sandbox.get(telemetry, &telemetry.writes[0]),
            Some(3),
            "the earlier value is back"
        );
        assert_eq!(sandbox.get(tips, &tips.writes[0]), Some(1));
        for write in &tips.writes[1..] {
            assert_eq!(
                sandbox.get(tips, write),
                None,
                "values the run created are gone again"
            );
        }
    }

    #[test]
    fn revert_backs_up_what_it_removes() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let history = tweak("activity-history");
        privacy.apply(&[history.id]).unwrap();

        let outcome = privacy.revert(&[history.id]).unwrap();
        assert_eq!(outcome.changed(), 1);
        for write in history.writes {
            assert_eq!(sandbox.get(history, write), None);
        }
        import_reg_file(&outcome.backup.unwrap()).unwrap();
        assert_eq!(privacy.status(history.id).unwrap().state, State::Private);
    }

    #[test]
    fn when_the_backup_cannot_be_saved_nothing_changes() {
        let sandbox = Sandbox::new();
        // A file where the backup folder should be makes saving impossible.
        let blocker = sandbox.tmp.path().join("not-a-folder");
        std::fs::write(&blocker, b"x").unwrap();
        let privacy = Privacy::new(Hives::redirected(&sandbox.base), WIN11_24H2, true)
            .with_backup_dir(blocker.join("backups"));
        let tips = tweak("suggestions-and-tips");

        assert!(privacy.apply(&[tips.id]).is_err());
        for write in tips.writes {
            assert_eq!(sandbox.get(tips, write), None);
        }
    }

    #[test]
    fn recommended_pending_lists_what_one_click_would_change() {
        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(WIN11_24H2, true);
        let pending = privacy.recommended_pending();
        assert!(pending.contains(&"diagnostic-data"));
        assert!(pending.contains(&"recall-snapshots"));
        assert!(
            !pending.contains(&"copilot-panel"),
            "not applicable on 24H2"
        );
        assert!(!pending.contains(&"location"), "not recommended");

        privacy.apply(&pending).unwrap();
        assert!(privacy.recommended_pending().is_empty());
    }

    #[test]
    fn home_editions_get_a_caveat_on_policies_only() {
        assert!(is_home_edition("Core"));
        assert!(is_home_edition("CoreSingleLanguage"));
        assert!(!is_home_edition("Professional"));
        assert!(!is_home_edition("Enterprise"));

        let sandbox = Sandbox::new();
        let privacy = sandbox.privacy(
            Windows {
                build: 26100,
                home: true,
            },
            true,
        );
        assert!(privacy.status("diagnostic-data").unwrap().edition_caveat);
        assert!(
            !privacy
                .status("suggestions-and-tips")
                .unwrap()
                .edition_caveat
        );
    }

    #[test]
    fn the_running_windows_version_is_readable() {
        let windows = Windows::current();
        assert!(windows.build >= 10240, "build {}", windows.build);
    }

    #[test]
    fn listing_the_real_registry_is_read_only_and_complete() {
        let list = Privacy::system().list();
        assert_eq!(list.len(), CATALOG.len());
    }
}
