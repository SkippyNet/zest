//! Startup entries stored in registry `Run` / `RunOnce` keys.

use super::{RegistryEntry, StartupDetails, StartupItem, StartupKind, approved, resolve_command};
use crate::Scope;
use crate::util::{InfoCache, is_microsoft};
use zest_core::Result;
use zest_core::reg::{RegBackup, RegPath, RegRoot};

const CURRENT_VERSION: &str = r"Software\Microsoft\Windows\CurrentVersion";
const CURRENT_VERSION_32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion";

/// One registry key that Windows runs programs from.
pub(crate) struct RunLocation {
    /// The key holding one value per program.
    pub key: RegPath,
    /// Where the enabled flags for this key live, if Windows supports
    /// disabling entries of this key.
    pub approved: Option<RegPath>,
    /// `RegistryRun` or `RegistryRunOnce`.
    pub kind: StartupKind,
    /// User or machine.
    pub scope: Scope,
}

/// Every Run-style key Windows processes at sign-in.
fn locations() -> Vec<RunLocation> {
    let mut out = Vec::new();
    for (root, scope) in [
        (RegRoot::Hkcu, Scope::User),
        (RegRoot::Hklm, Scope::Machine),
    ] {
        let loc = |path: String, approved: Option<&str>, kind| RunLocation {
            key: RegPath::new(root, path),
            approved: approved.map(|list| approved::approved_key(root, list)),
            kind,
            scope,
        };
        out.push(loc(
            format!("{CURRENT_VERSION}\\Run"),
            Some("Run"),
            StartupKind::RegistryRun,
        ));
        out.push(loc(
            format!("{CURRENT_VERSION}\\RunOnce"),
            None,
            StartupKind::RegistryRunOnce,
        ));
        out.push(loc(
            format!("{CURRENT_VERSION_32}\\Run"),
            Some("Run32"),
            StartupKind::RegistryRun,
        ));
        out.push(loc(
            format!("{CURRENT_VERSION_32}\\RunOnce"),
            None,
            StartupKind::RegistryRunOnce,
        ));
        out.push(loc(
            format!("{CURRENT_VERSION}\\Policies\\Explorer\\Run"),
            None,
            StartupKind::RegistryRun,
        ));
    }
    out
}

/// Lists the entries of every Run-style key.
pub(crate) fn list_all(cache: &mut InfoCache) -> Vec<StartupItem> {
    locations()
        .iter()
        .flat_map(|loc| list_location(loc, cache))
        .collect()
}

/// Lists the entries of one key. A missing or unreadable key yields nothing.
pub(crate) fn list_location(loc: &RunLocation, cache: &mut InfoCache) -> Vec<StartupItem> {
    let Ok(key) = loc.key.open() else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for (name, _) in key.enum_values().flatten() {
        // Only string values are commands; anything else is not a startup entry.
        let Ok(command) = key.get_value::<String, _>(&name) else {
            continue;
        };
        if command.trim().is_empty() {
            continue;
        }
        items.push(make_item(loc, name, command, cache));
    }
    items
}

fn make_item(
    loc: &RunLocation,
    value: String,
    command: String,
    cache: &mut InfoCache,
) -> StartupItem {
    let resolved = resolve_command(&command, cache);
    let enabled = loc
        .approved
        .as_ref()
        .is_none_or(|key| approved::is_enabled(key, &value));
    let machine = loc.scope == Scope::Machine;
    StartupItem {
        id: format!("reg|{}|{}", loc.key, value),
        kind: loc.kind,
        name: if value.is_empty() {
            "(Default)".to_owned()
        } else {
            value.clone()
        },
        command,
        target: resolved.target,
        target_missing: resolved.target_missing,
        is_microsoft: is_microsoft(resolved.publisher.as_deref()),
        publisher: resolved.publisher,
        description: resolved.description,
        location: loc.key.to_string(),
        scope: loc.scope,
        enabled,
        requires_admin: machine,
        toggle_requires_admin: machine,
        can_toggle: loc.approved.is_some(),
        can_delete: true,
        details: StartupDetails::Registry(RegistryEntry {
            key: loc.key.clone(),
            value,
            approved_key: loc.approved.clone(),
        }),
    }
}

/// Deletes the value (and its enabled flag) after exporting both to `backup`.
pub(crate) fn delete(entry: &RegistryEntry, mut backup: Option<&mut RegBackup>) -> Result<()> {
    zest_core::reg::delete_value(&entry.key, &entry.value, backup.as_deref_mut())?;
    if let Some(approved_key) = &entry.approved_key {
        approved::forget(approved_key, &entry.value, backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use zest_core::Error;

    fn scratch_location(scratch: &Scratch, with_approval: bool) -> RunLocation {
        RunLocation {
            key: scratch.child("Run"),
            approved: with_approval.then(|| scratch.child("StartupApproved")),
            kind: StartupKind::RegistryRun,
            scope: Scope::User,
        }
    }

    #[test]
    fn real_locations_are_well_formed() {
        let all = locations();
        assert_eq!(all.len(), 10);
        // RunOnce and policy keys have no enabled flag.
        assert!(
            all.iter()
                .filter(|l| l.kind == StartupKind::RegistryRunOnce)
                .all(|l| l.approved.is_none())
        );
        let wow = all
            .iter()
            .find(|l| {
                l.key.root == RegRoot::Hklm
                    && l.key
                        .path
                        .ends_with(r"WOW6432Node\Microsoft\Windows\CurrentVersion\Run")
            })
            .unwrap();
        assert!(wow.approved.as_ref().unwrap().path.ends_with("Run32"));
        // Enumerating the real keys must never panic and yields only run kinds.
        let mut cache = InfoCache::default();
        for item in list_all(&mut cache) {
            assert!(matches!(
                item.kind,
                StartupKind::RegistryRun | StartupKind::RegistryRunOnce
            ));
            assert!(item.id.starts_with("reg|"));
            assert!(item.can_delete);
        }
    }

    #[test]
    fn lists_toggles_and_deletes_scratch_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::new("run");
        let loc = scratch_location(&scratch, true);
        let cmd = zest_core::paths::system32_dir().join("cmd.exe");
        let drive = zest_core::paths::system_drive();
        {
            let key = loc.key.open_write().unwrap();
            key.set_value("Good", &format!("\"{}\" /c rem", cmd.display()))
                .unwrap();
            key.set_value(
                "Orphan",
                &format!("\"{}Zest No Such Dir\\tray.exe\" -silent", drive.display()),
            )
            .unwrap();
            key.set_value("Number", &7u32).unwrap();
            key.set_value("Blank", &"   ").unwrap();
        }

        let mut cache = InfoCache::default();
        let mut items = list_location(&loc, &mut cache);
        items.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(items.len(), 2, "{items:?}");
        let (good, orphan) = (&items[0], &items[1]);
        assert_eq!(good.name, "Good");
        assert!(good.enabled && good.can_toggle && !good.requires_admin);
        assert!(!good.target_missing);
        assert!(good.is_microsoft);
        assert_eq!(orphan.name, "Orphan");
        assert!(orphan.target_missing);
        assert!(orphan.publisher.is_none());

        // Toggle through the public entry point.
        super::super::set_enabled(good, false).unwrap();
        let items = list_location(&loc, &mut cache);
        assert!(!items.iter().find(|i| i.name == "Good").unwrap().enabled);
        assert!(items.iter().find(|i| i.name == "Orphan").unwrap().enabled);
        super::super::set_enabled(good, true).unwrap();
        assert!(list_location(&loc, &mut cache).iter().all(|i| i.enabled));

        // Delete with backup, then restore from the backup.
        super::super::set_enabled(orphan, false).unwrap();
        let mut backup = RegBackup::at(tmp.path().join("startup.reg"));
        super::super::delete(orphan, Some(&mut backup), None).unwrap();
        assert_eq!(backup.len(), 2, "value and its flag should both be saved");
        let items = list_location(&loc, &mut cache);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Good");

        let saved = backup.save().unwrap().unwrap();
        zest_core::reg::import_reg_file(&saved).unwrap();
        let items = list_location(&loc, &mut cache);
        let restored = items.iter().find(|i| i.name == "Orphan").unwrap();
        assert!(!restored.enabled, "restored entry should still be disabled");
        assert!(restored.target_missing);
    }

    #[test]
    fn locations_without_flags_cannot_be_toggled() {
        let scratch = Scratch::new("runonce");
        let loc = scratch_location(&scratch, false);
        loc.key
            .open_write()
            .unwrap()
            .set_value("Once", &"notepad.exe")
            .unwrap();
        let mut cache = InfoCache::default();
        let items = list_location(&loc, &mut cache);
        assert_eq!(items.len(), 1);
        assert!(items[0].enabled);
        assert!(!items[0].can_toggle);
        assert!(matches!(
            super::super::set_enabled(&items[0], false),
            Err(Error::Other(_))
        ));
        // Deleting works without a backup when the caller insists.
        super::super::delete(&items[0], None, None).unwrap();
        assert!(list_location(&loc, &mut cache).is_empty());
    }

    #[test]
    fn missing_key_lists_nothing() {
        let scratch = Scratch::new("run-missing");
        let loc = RunLocation {
            key: scratch.0.child("DoesNotExist"),
            approved: None,
            kind: StartupKind::RegistryRun,
            scope: Scope::User,
        };
        assert!(list_location(&loc, &mut InfoCache::default()).is_empty());
    }
}
