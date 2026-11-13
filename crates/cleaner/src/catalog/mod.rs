//! Builds the [`Catalog`]: every cleaning rule that applies to this PC.
//!
//! The catalog is rebuilt before each scan (it is cheap), so newly installed
//! programs and new browser profiles are picked up automatically. Rule
//! identifiers are stable across rebuilds.

mod apps;
mod browser_rules;
mod windows;

use crate::browsers;
use crate::model::{
    Action, Catalog, Group, GroupIcon, Include, IncludeMode, Rule, RuleKind, Section, Special,
};
use crate::pattern::{expand_glob, has_wildcard};
use std::path::PathBuf;
use zest_core::paths::{self, UserDirs};
use zest_core::reg::{RegPath, RegRoot};

/// Machine-wide folders used by rule templates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineDirs {
    /// `C:\Windows`
    pub windows: PathBuf,
    /// `C:\ProgramData`
    pub program_data: PathBuf,
    /// `C:\`
    pub system_drive: PathBuf,
    /// `C:\Program Files`
    pub program_files: PathBuf,
    /// `C:\Program Files (x86)`
    pub program_files_x86: PathBuf,
}

impl MachineDirs {
    /// The real folders of this PC.
    pub fn real() -> Self {
        MachineDirs {
            windows: paths::windows_dir(),
            program_data: paths::program_data(),
            system_drive: paths::system_drive(),
            program_files: paths::program_files(),
            program_files_x86: paths::program_files_x86(),
        }
    }

    /// A self-contained fake layout below `root` (for tests).
    pub fn under(root: &std::path::Path) -> Self {
        MachineDirs {
            windows: root.join("Windows"),
            program_data: root.join("ProgramData"),
            system_drive: root.to_path_buf(),
            program_files: root.join("Program Files"),
            program_files_x86: root.join("Program Files (x86)"),
        }
    }
}

/// Everything rule definitions need to resolve their templates.
pub struct Ctx<'a> {
    /// The Windows accounts to clean.
    pub users: &'a [UserDirs],
    /// Machine-wide folders.
    pub machine: &'a MachineDirs,
    /// Whether registry rules may be generated (false in file-system-only tests).
    pub registry: bool,
}

impl Ctx<'_> {
    fn expand_machine(&self, template: &str) -> String {
        let trim = |p: &PathBuf| p.to_string_lossy().trim_end_matches('\\').to_owned();
        template
            .replace("{windows}", &trim(&self.machine.windows))
            .replace("{programdata}", &trim(&self.machine.program_data))
            .replace("{systemdrive}", &trim(&self.machine.system_drive))
            .replace("{programfiles}", &trim(&self.machine.program_files))
            .replace("{programfiles86}", &trim(&self.machine.program_files_x86))
    }

    fn resolve(text: String, out: &mut Vec<PathBuf>) {
        let path = PathBuf::from(text);
        if has_wildcard(&path.to_string_lossy()) {
            for found in expand_glob(&path) {
                if !out.contains(&found) {
                    out.push(found);
                }
            }
        } else if !out.contains(&path) {
            out.push(path);
        }
    }

    /// Resolves a per-user template (`{local}`, `{roaming}`, `{locallow}`,
    /// `{profile}`, `{temp}`) for every user. Wildcards expand to the paths
    /// that exist; plain paths are returned whether or not they exist yet.
    pub fn user(&self, template: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for user in self.users {
            let text = self
                .expand_machine(template)
                .replace("{local}", &user.local.to_string_lossy())
                .replace("{roaming}", &user.roaming.to_string_lossy())
                .replace("{locallow}", &user.local_low.to_string_lossy())
                .replace("{profile}", &user.profile.to_string_lossy())
                .replace("{temp}", &user.temp.to_string_lossy());
            Self::resolve(text, &mut out);
        }
        out
    }

    /// Resolves a machine-wide template (`{windows}`, `{programdata}`,
    /// `{systemdrive}`, `{programfiles}`, `{programfiles86}`).
    pub fn machine(&self, template: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        Self::resolve(self.expand_machine(template), &mut out);
        out
    }

    /// Resolves several per-user templates at once.
    pub fn users_all(&self, templates: &[&str]) -> Vec<PathBuf> {
        templates.iter().flat_map(|t| self.user(t)).collect()
    }

    /// True when any of the per-user templates resolves to something that exists.
    pub fn any_exists(&self, templates: &[&str]) -> bool {
        templates.iter().any(|t| {
            let per_user = self.user(t);
            per_user.iter().any(|p| p.exists())
        })
    }

    /// A per-user registry key for every user it can be reached for: `HKCU`
    /// for the current user and `HKU\<SID>` for other users whose hive is
    /// loaded (they are logged on).
    pub fn user_keys(&self, path: &str) -> Vec<RegPath> {
        if !self.registry {
            return Vec::new();
        }
        let mut keys = Vec::new();
        for user in self.users {
            if user.is_current {
                keys.push(RegPath::hkcu(path));
            } else if let Some(sid) = &user.sid {
                let hive = RegPath::new(RegRoot::Hku, sid.clone());
                if hive.exists() {
                    keys.push(hive.child(path));
                }
            }
        }
        keys
    }
}

/// Fluent construction of a [`Rule`].
pub(crate) struct RuleBuilder(Rule);

/// Starts a junk rule that is enabled by default.
pub(crate) fn rule(id: &str, name: &str, description: &str) -> RuleBuilder {
    RuleBuilder(Rule {
        id: id.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
        kind: RuleKind::Junk,
        default_enabled: true,
        warning: None,
        requires_admin: false,
        age_filtered: false,
        actions: Vec::new(),
    })
}

impl RuleBuilder {
    /// Marks the rule as a privacy rule; privacy rules are off by default.
    pub fn privacy(mut self) -> Self {
        self.0.kind = RuleKind::Privacy;
        self.0.default_enabled = false;
        self
    }

    /// Turns the rule off by default.
    pub fn off(mut self) -> Self {
        self.0.default_enabled = false;
        self
    }

    /// Marks the rule as needing administrator rights.
    pub fn admin(mut self) -> Self {
        self.0.requires_admin = true;
        self
    }

    /// Applies the user's "only files older than" setting.
    pub fn aged(mut self) -> Self {
        self.0.age_filtered = true;
        self
    }

    /// Adds a confirmation warning.
    pub fn warn(mut self, text: &str) -> Self {
        self.0.warning = Some(text.to_owned());
        self
    }

    /// Deletes files matching `pattern` in each folder.
    pub fn files(mut self, dirs: Vec<PathBuf>, pattern: &str, recursive: bool) -> Self {
        for dir in dirs {
            self.0.actions.push(Action::Files {
                dir,
                pattern: pattern.to_owned(),
                recursive,
                min_age_days: 0,
            });
        }
        self
    }

    /// Like [`RuleBuilder::files`] but only for files older than `days`.
    pub fn old_files(
        mut self,
        dirs: Vec<PathBuf>,
        pattern: &str,
        recursive: bool,
        days: u32,
    ) -> Self {
        for dir in dirs {
            self.0.actions.push(Action::Files {
                dir,
                pattern: pattern.to_owned(),
                recursive,
                min_age_days: days,
            });
        }
        self
    }

    /// Deletes everything inside each folder (recursively), keeping the folder.
    pub fn contents(self, dirs: Vec<PathBuf>) -> Self {
        self.files(dirs, "*", true)
    }

    /// Deletes specific files.
    pub fn file(mut self, files: Vec<PathBuf>) -> Self {
        self.0.actions.extend(files.into_iter().map(Action::File));
        self
    }

    /// Deletes all values of each key (and sub-keys when asked).
    pub fn reg_values(mut self, keys: Vec<RegPath>, include_subkeys: bool) -> Self {
        self.0
            .actions
            .extend(keys.into_iter().map(|key| Action::RegValues {
                key,
                include_subkeys,
            }));
        self
    }

    /// Deletes one named value in each key.
    pub fn reg_value(mut self, keys: Vec<RegPath>, name: &str) -> Self {
        self.0
            .actions
            .extend(keys.into_iter().map(|key| Action::RegValue {
                key,
                name: name.to_owned(),
            }));
        self
    }

    /// Runs a special operation.
    pub fn special(mut self, special: Special) -> Self {
        self.0.actions.push(Action::Special(special));
        self
    }

    /// Finishes the rule.
    pub fn build(self) -> Rule {
        self.0
    }
}

/// Assembles a group, dropping it when it has no rules.
pub(crate) fn group(
    id: &str,
    section: Section,
    name: &str,
    icon: GroupIcon,
    processes: &[&str],
    rules: Vec<Rule>,
) -> Option<Group> {
    if rules.is_empty() {
        return None;
    }
    Some(Group {
        id: id.to_owned(),
        section,
        name: name.to_owned(),
        icon,
        processes: processes.iter().map(|p| (*p).to_owned()).collect(),
        rules,
    })
}

fn custom_group(includes: &[Include]) -> Option<Group> {
    let mut rules = Vec::new();
    for include in includes {
        let path = &include.path;
        let display = path.display().to_string();
        let mut builder = rule(
            &format!("custom.{}", stable_id(&display, include)),
            &display,
            "A location you added under Options > Include.",
        );
        let pattern = if include.pattern.trim().is_empty() {
            "*"
        } else {
            include.pattern.trim()
        };
        let action = if path.is_file() {
            Action::File(path.clone())
        } else {
            match include.mode {
                IncludeMode::FilesOnly => Action::Files {
                    dir: path.clone(),
                    pattern: pattern.to_owned(),
                    recursive: false,
                    min_age_days: 0,
                },
                IncludeMode::FilesAndSubfolders => Action::Files {
                    dir: path.clone(),
                    pattern: pattern.to_owned(),
                    recursive: true,
                    min_age_days: 0,
                },
                IncludeMode::WholeFolder => Action::Folder(path.clone()),
            }
        };
        builder.0.actions.push(action);
        rules.push(builder.build());
    }
    group(
        "custom",
        Section::Custom,
        "Custom files and folders",
        GroupIcon::Custom,
        &[],
        rules,
    )
}

/// A short stable identifier derived from an include's definition, so the
/// rule keeps its id (and its on/off state) when the list is reordered.
fn stable_id(display: &str, include: &Include) -> String {
    // FNV-1a, enough to distinguish a handful of user entries.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let text = format!(
        "{}|{}|{:?}",
        display.to_lowercase(),
        include.pattern,
        include.mode
    );
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl Catalog {
    /// Builds the catalog for the real PC.
    pub fn build(users: &[UserDirs], includes: &[Include]) -> Catalog {
        let machine = MachineDirs::real();
        Self::build_with(
            &Ctx {
                users,
                machine: &machine,
                registry: true,
            },
            includes,
        )
    }

    /// Builds the catalog for an explicit context (used by tests).
    pub fn build_with(ctx: &Ctx, includes: &[Include]) -> Catalog {
        let mut groups = Vec::new();
        groups.extend(windows::groups(ctx));
        groups.extend(browser_rules::groups(&browsers::discover(ctx.users)));
        groups.extend(apps::groups(ctx));
        groups.extend(custom_group(includes));
        Catalog { groups }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn real_catalog_is_well_formed() {
        let users = [UserDirs::current()];
        let catalog = Catalog::build(&users, &[]);
        assert!(
            catalog.groups.len() >= 3,
            "expected at least the Windows groups"
        );

        let mut ids = HashSet::new();
        for (group, rule) in catalog.rules() {
            assert!(ids.insert(rule.id.clone()), "duplicate rule id {}", rule.id);
            assert!(
                !rule.name.is_empty() && !rule.description.is_empty(),
                "{}",
                rule.id
            );
            assert!(!rule.actions.is_empty(), "{} has no actions", rule.id);
            assert!(!group.name.is_empty());
            // Privacy rules are opt-in.
            if rule.kind == RuleKind::Privacy {
                assert!(
                    !rule.default_enabled,
                    "{} is privacy but on by default",
                    rule.id
                );
            }
            // Anything that warns must also be opt-in.
            if rule.warning.is_some() {
                assert!(
                    !rule.default_enabled,
                    "{} warns but is on by default",
                    rule.id
                );
            }
        }
        let group_ids: HashSet<_> = catalog.groups.iter().map(|g| g.id.clone()).collect();
        assert_eq!(group_ids.len(), catalog.groups.len(), "duplicate group id");
    }

    #[test]
    fn no_rule_targets_a_protected_location() {
        // Every folder a rule deletes *inside* must itself be a place where
        // deletion is permitted; otherwise the rule could never work and is
        // a sign of a wrong path.
        let users = [UserDirs::current()];
        let catalog = Catalog::build(&users, &[]);
        for (_, rule) in catalog.rules() {
            for action in &rule.actions {
                match action {
                    Action::Files { dir, .. } => {
                        let probe = dir.join("zest-probe.tmp");
                        assert!(
                            !zest_core::safety::is_protected(&probe),
                            "{}: files in {} are protected",
                            rule.id,
                            dir.display()
                        );
                    }
                    Action::File(path) | Action::Folder(path) => {
                        assert!(
                            !zest_core::safety::is_protected(path),
                            "{}: {} is protected",
                            rule.id,
                            path.display()
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn template_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let machine = MachineDirs::under(tmp.path());
        let users = [
            UserDirs::for_profile(tmp.path().join("Users").join("a"), None),
            UserDirs::for_profile(tmp.path().join("Users").join("b"), None),
        ];
        std::fs::create_dir_all(users[0].local.join(r"Packages\One_x\AC\Temp")).unwrap();
        std::fs::create_dir_all(users[1].local.join(r"Packages\Two_y\AC\Temp")).unwrap();
        let ctx = Ctx {
            users: &users,
            machine: &machine,
            registry: false,
        };
        // Plain templates resolve for every user even when missing.
        assert_eq!(ctx.user(r"{local}\Nothing").len(), 2);
        // Wildcards resolve to what exists.
        assert_eq!(ctx.user(r"{local}\Packages\*\AC\Temp").len(), 2);
        assert_eq!(ctx.user(r"{roaming}\*\Cache").len(), 0);
        assert_eq!(
            ctx.machine(r"{windows}\Temp"),
            vec![tmp.path().join(r"Windows\Temp")]
        );
        assert!(ctx.any_exists(&[r"{local}\Packages"]));
        assert!(!ctx.any_exists(&[r"{roaming}\Nope"]));
        assert!(ctx.user_keys(r"Software\X").is_empty());
    }

    #[test]
    fn custom_includes_become_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("single.log");
        std::fs::write(&file, b"x").unwrap();
        let includes = vec![
            Include {
                path: tmp.path().to_path_buf(),
                pattern: "*.tmp".to_owned(),
                mode: IncludeMode::FilesAndSubfolders,
            },
            Include {
                path: file.clone(),
                pattern: String::new(),
                mode: IncludeMode::FilesOnly,
            },
        ];
        let group = custom_group(&includes).unwrap();
        assert_eq!(group.section, Section::Custom);
        assert_eq!(group.rules.len(), 2);
        assert!(
            matches!(&group.rules[0].actions[0], Action::Files { recursive: true, pattern, .. } if pattern == "*.tmp")
        );
        assert_eq!(group.rules[1].actions[0], Action::File(file));
        assert_ne!(group.rules[0].id, group.rules[1].id);
        // Ids do not depend on position.
        let reversed: Vec<Include> = includes.iter().rev().cloned().collect();
        let regrouped = custom_group(&reversed).unwrap();
        assert_eq!(regrouped.rules[1].id, group.rules[0].id);
        assert!(custom_group(&[]).is_none());
    }
}
