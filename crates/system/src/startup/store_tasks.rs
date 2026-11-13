//! Startup tasks of Microsoft Store / packaged apps.
//!
//! Packaged apps cannot write to the `Run` keys. They declare a *startup
//! task* in their manifest, and Windows keeps the per-user on/off state under
//! `...\AppModel\SystemAppData\<package family>\<task id>` as a `State`
//! number. Task Manager and Settings list these beside classic entries and
//! flip exactly that value.

use super::{StartupDetails, StartupItem, StartupKind, StoreStartupEntry};
use crate::Scope;
use crate::apps::PackageDisplay;
use crate::util::{filetime_now, io_err, is_microsoft};
use rayon::prelude::*;
use zest_core::reg::RegPath;
use zest_core::{Error, Result};

const SYSTEM_APP_DATA: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData";

/// The app may ask to be enabled; currently off.
const STATE_DISABLED: u32 = 0;
/// Switched off by the user.
const STATE_DISABLED_BY_USER: u32 = 1;
/// On.
const STATE_ENABLED: u32 = 2;
/// Forced off by policy.
const STATE_DISABLED_BY_POLICY: u32 = 3;
/// Forced on by policy.
const STATE_ENABLED_BY_POLICY: u32 = 4;

impl StoreStartupEntry {
    /// What the stored state number means, e.g. `Turned off by you`.
    pub fn state_label(&self) -> &'static str {
        match self.state {
            STATE_DISABLED => "Off",
            STATE_DISABLED_BY_USER => "Turned off by you",
            STATE_ENABLED => "On",
            STATE_DISABLED_BY_POLICY => "Turned off by policy",
            STATE_ENABLED_BY_POLICY => "Turned on by policy",
            _ => "Unknown",
        }
    }
}

fn real_base() -> RegPath {
    RegPath::hkcu(SYSTEM_APP_DATA)
}

/// The app part of a package family name: `Vendor.App_abc123` -> `Vendor.App`.
fn family_identity(family: &str) -> &str {
    family.rsplit_once('_').map_or(family, |(name, _)| name)
}

/// Raw (family, task id, state) triples found below `base`.
fn read_states(base: &RegPath) -> Vec<(String, String, u32)> {
    let Ok(root) = base.open() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for family in root.enum_keys().flatten() {
        let family_key = base.child(&family);
        let Ok(handle) = family_key.open() else {
            continue;
        };
        for task in handle.enum_keys().flatten() {
            // Only sub-keys carrying a `State` number are startup tasks.
            if let Some(state) = family_key.child(&task).read_u32("State") {
                found.push((family.clone(), task, state));
            }
        }
    }
    found
}

fn make_item(
    base: &RegPath,
    family: String,
    task_id: String,
    state: u32,
    display: PackageDisplay,
) -> StartupItem {
    let key = base.child(&family).child(&task_id);
    let name = display
        .name
        .unwrap_or_else(|| family_identity(&family).to_owned());
    let locked = matches!(state, STATE_DISABLED_BY_POLICY | STATE_ENABLED_BY_POLICY);
    StartupItem {
        id: format!("storeapp|{family}|{task_id}"),
        kind: StartupKind::StoreApp,
        name,
        command: String::new(),
        target_missing: false,
        is_microsoft: is_microsoft(display.publisher.as_deref())
            || family.to_lowercase().starts_with("microsoft"),
        publisher: display.publisher,
        description: Some(format!("Startup task \"{task_id}\" of a Store app")),
        location: key.to_string(),
        target: display.location,
        scope: Scope::User,
        enabled: matches!(state, STATE_ENABLED | STATE_ENABLED_BY_POLICY),
        requires_admin: false,
        toggle_requires_admin: false,
        can_toggle: !locked,
        // The entry belongs to the app package; it goes away with the app.
        can_delete: false,
        details: StartupDetails::StoreApp(StoreStartupEntry {
            key,
            package_family_name: family,
            task_id,
            state,
        }),
    }
}

/// Lists the startup tasks registered below `base`, using `lookup` to turn a
/// package family name into display details.
pub(crate) fn list_at(
    base: &RegPath,
    lookup: &(dyn Fn(&str) -> PackageDisplay + Sync),
) -> Vec<StartupItem> {
    let states = read_states(base);
    // Display names load package resources; do the distinct families in parallel.
    let mut families: Vec<&str> = states
        .iter()
        .map(|(family, _, _)| family.as_str())
        .collect();
    families.sort_unstable();
    families.dedup();
    let displays: Vec<(String, PackageDisplay)> = families
        .par_iter()
        .map(|family| ((*family).to_owned(), lookup(family)))
        .collect();
    let mut items: Vec<StartupItem> = states
        .into_iter()
        .map(|(family, task, state)| {
            let display = displays
                .iter()
                .find(|(name, _)| *name == family)
                .map(|(_, display)| display.clone())
                .unwrap_or_default();
            make_item(base, family, task, state, display)
        })
        .collect();
    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    items
}

/// Lists the startup tasks of the current user's Store apps.
pub(crate) fn list_all() -> Vec<StartupItem> {
    list_at(&real_base(), &crate::apps::store_display)
}

/// Switches a startup task on or off, the way Task Manager does.
pub(crate) fn set_enabled(entry: &StoreStartupEntry, enabled: bool) -> Result<()> {
    let action = "change the startup state of the app";
    let current = entry
        .key
        .read_u32("State")
        .ok_or_else(|| Error::NotFound(entry.key.to_string()))?;
    if matches!(current, STATE_DISABLED_BY_POLICY | STATE_ENABLED_BY_POLICY) {
        return Err(Error::Other(
            "this startup entry is controlled by a policy and cannot be changed".to_owned(),
        ));
    }
    let key = entry.key.open_write().map_err(|e| io_err(e, action))?;
    if enabled {
        key.set_value("State", &STATE_ENABLED)
            .map_err(|e| io_err(e, action))?;
        key.set_value("UserEnabledStartupOnce", &1u32)
            .map_err(|e| io_err(e, action))
    } else {
        key.set_value("State", &STATE_DISABLED_BY_USER)
            .map_err(|e| io_err(e, action))?;
        // Seconds since 1970, as Windows records it.
        let unix_seconds = (filetime_now() / 10_000_000).saturating_sub(11_644_473_600) as u32;
        key.set_value("LastDisabledTime", &unix_seconds)
            .map_err(|e| io_err(e, action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use std::path::PathBuf;
    use zest_core::reg::RegRoot;

    fn add_task(base: &RegPath, family: &str, task: &str, state: Option<u32>) {
        let key = base.child(family).child(task);
        let (handle, _) = RegRoot::Hkcu.key().create_subkey(&key.path).unwrap();
        if let Some(state) = state {
            handle.set_value("State", &state).unwrap();
        }
    }

    fn lookup(family: &str) -> PackageDisplay {
        if family.starts_with("Example.Player") {
            PackageDisplay {
                name: Some("Example Player".to_owned()),
                publisher: Some("Example Corp".to_owned()),
                location: Some(PathBuf::from(
                    r"C:\Program Files\WindowsApps\Example.Player",
                )),
            }
        } else {
            PackageDisplay::default()
        }
    }

    #[test]
    fn family_names() {
        assert_eq!(family_identity("Vendor.App_8wekyb3d8bbwe"), "Vendor.App");
        assert_eq!(family_identity("NoSuffix"), "NoSuffix");
        let entry = |state| StoreStartupEntry {
            key: RegPath::hkcu("Software\\ZestTests\\none"),
            package_family_name: String::new(),
            task_id: String::new(),
            state,
        };
        assert_eq!(entry(STATE_DISABLED).state_label(), "Off");
        assert_eq!(entry(STATE_ENABLED).state_label(), "On");
        assert_eq!(entry(77).state_label(), "Unknown");
    }

    #[test]
    fn lists_and_toggles_scratch_tasks() {
        let scratch = Scratch::new("store-tasks");
        let base = scratch.child("SystemAppData");
        add_task(
            &base,
            "Example.Player_abc123",
            "PlayerStartup",
            Some(STATE_ENABLED),
        );
        add_task(
            &base,
            "Example.Player_abc123",
            "HelperTask",
            Some(STATE_DISABLED),
        );
        add_task(
            &base,
            "Microsoft.Sample_8wekyb3d8bbwe",
            "Start",
            Some(STATE_DISABLED_BY_USER),
        );
        add_task(
            &base,
            "Managed.App_xyz",
            "Start",
            Some(STATE_ENABLED_BY_POLICY),
        );
        // Other per-package data without a State value is not a startup task.
        add_task(&base, "Example.Player_abc123", "Schemas", None);
        add_task(&base, "No.Tasks_000", "SplashScreen", None);

        let items = list_at(&base, &lookup);
        assert_eq!(items.len(), 4, "{items:#?}");
        let by_id = |suffix: &str| items.iter().find(|i| i.id.ends_with(suffix)).unwrap();

        let player = by_id("|PlayerStartup");
        assert_eq!(player.name, "Example Player");
        assert_eq!(player.kind, StartupKind::StoreApp);
        assert!(player.enabled && player.can_toggle && !player.can_delete);
        assert!(!player.requires_admin && !player.is_microsoft);
        assert_eq!(player.publisher.as_deref(), Some("Example Corp"));
        assert!(!by_id("|HelperTask").enabled);

        let sample = by_id("Microsoft.Sample_8wekyb3d8bbwe|Start");
        assert_eq!(
            sample.name, "Microsoft.Sample",
            "falls back to the identity name"
        );
        assert!(!sample.enabled && sample.is_microsoft);

        let managed = by_id("Managed.App_xyz|Start");
        assert!(managed.enabled && !managed.can_toggle);
        assert!(matches!(
            super::super::set_enabled(managed, false),
            Err(Error::Other(_))
        ));
        assert!(matches!(
            super::super::delete(player, None, None),
            Err(Error::Other(_))
        ));

        // Disable, then enable again, through the public entry point.
        super::super::set_enabled(player, false).unwrap();
        let StartupDetails::StoreApp(entry) = &player.details else {
            panic!("wrong details");
        };
        assert_eq!(entry.key.read_u32("State"), Some(STATE_DISABLED_BY_USER));
        assert!(entry.key.read_u32("LastDisabledTime").unwrap() > 1_700_000_000);
        assert!(
            !list_at(&base, &lookup)
                .iter()
                .find(|i| i.id.ends_with("|PlayerStartup"))
                .unwrap()
                .enabled
        );

        super::super::set_enabled(player, true).unwrap();
        assert_eq!(entry.key.read_u32("State"), Some(STATE_ENABLED));
        assert_eq!(entry.key.read_u32("UserEnabledStartupOnce"), Some(1));

        // A policy-controlled entry is refused even when called directly.
        let StartupDetails::StoreApp(managed_entry) = &managed.details else {
            panic!("wrong details");
        };
        assert!(set_enabled(managed_entry, false).is_err());
        assert_eq!(
            managed_entry.key.read_u32("State"),
            Some(STATE_ENABLED_BY_POLICY)
        );
    }

    #[test]
    fn missing_entries_are_reported() {
        let scratch = Scratch::new("store-tasks-missing");
        let entry = StoreStartupEntry {
            key: scratch.0.child("Gone").child("Task"),
            package_family_name: "Gone".to_owned(),
            task_id: "Task".to_owned(),
            state: STATE_ENABLED,
        };
        assert!(matches!(
            set_enabled(&entry, false),
            Err(Error::NotFound(_))
        ));
        assert!(list_at(&scratch.0.child("Nope"), &lookup).is_empty());
    }

    #[test]
    fn real_listing_is_sane() {
        let items = list_all();
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            assert_eq!(item.kind, StartupKind::StoreApp);
            assert!(ids.insert(item.id.clone()));
            assert!(!item.name.trim().is_empty());
            assert!(!item.can_delete && !item.requires_admin);
        }
    }
}
