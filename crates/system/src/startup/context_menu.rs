//! Explorer context-menu entries: COM shell extensions and static verbs.
//!
//! Entries are read from the real registration location (the user's or the
//! machine's `Software\Classes`), never from the merged `HKEY_CLASSES_ROOT`
//! view, so that changes are applied where the entry actually lives.

use super::{
    ContextHandlerKind, ContextMenuEntry, StartupDetails, StartupItem, StartupKind, resolve_command,
};
use crate::Scope;
use crate::util::{InfoCache, io_err, is_microsoft, load_indirect_string};
use winreg::enums::{KEY_READ, KEY_WRITE};
use zest_core::reg::{RegBackup, RegPath, RegRoot};
use zest_core::{Error, Result};

const BLOCKED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Blocked";
/// Marker value that makes Explorer ignore a static verb.
const LEGACY_DISABLE: &str = "LegacyDisable";

/// File classes whose menus are inspected, with a description of what the
/// menu applies to.
const CLASSES: &[(&str, &str)] = &[
    ("*", "All files"),
    ("AllFilesystemObjects", "Files and folders"),
    ("Directory", "Folders"),
    (r"Directory\Background", "Folder background"),
    ("Folder", "Folders and drives"),
    ("Drive", "Drives"),
    ("DesktopBackground", "Desktop background"),
];

/// The registry locations one enumeration pass works on. Tests point these
/// at a scratch key.
pub(crate) struct MenuRoots {
    /// `Software\Classes` of one hive.
    pub classes: RegPath,
    /// Where class identifiers are looked up (`CLSID`).
    pub clsid: RegPath,
    /// The per-user block list.
    pub blocked_user: RegPath,
    /// The machine-wide block list, if any.
    pub blocked_machine: Option<RegPath>,
    /// Scope of entries found under `classes`.
    pub scope: Scope,
}

fn real_roots() -> Vec<MenuRoots> {
    [
        (RegRoot::Hkcu, r"Software\Classes", Scope::User),
        (RegRoot::Hklm, r"SOFTWARE\Classes", Scope::Machine),
    ]
    .into_iter()
    .map(|(root, classes, scope)| MenuRoots {
        classes: RegPath::new(root, classes),
        clsid: RegPath::hkcr("CLSID"),
        blocked_user: RegPath::hkcu(BLOCKED_KEY),
        blocked_machine: Some(RegPath::hklm(BLOCKED_KEY)),
        scope,
    })
    .collect()
}

/// True for text of the form `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`.
fn looks_like_clsid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && bytes[1..37].iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn has_value(key: &RegPath, name: &str) -> bool {
    key.open().is_ok_and(|k| {
        k.enum_values()
            .flatten()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
    })
}

fn is_blocked(roots: &MenuRoots, clsid: &str) -> (bool, bool) {
    let user = has_value(&roots.blocked_user, clsid);
    let machine = roots
        .blocked_machine
        .as_ref()
        .is_some_and(|key| has_value(key, clsid));
    (user, machine)
}

/// The in-process server (DLL) or local server registered for a class.
fn clsid_server(roots: &MenuRoots, clsid: &str) -> Option<String> {
    crate::util::clsid_server(&roots.clsid, clsid)
}

/// The file an indirect resource string (`@some.dll,-123`) loads its text
/// from. For verbs without a command this is the best hint at who owns them.
fn resource_file(text: &str, cache: &mut InfoCache) -> Option<String> {
    let reference = text.trim().strip_prefix('@')?;
    let file = reference.split(',').next()?.trim();
    match zest_core::cmdline::check_path(file) {
        zest_core::cmdline::Target::Exists(path) => cache.publisher_and_description(&path).0,
        _ => None,
    }
}

fn clsid_name(roots: &MenuRoots, clsid: &str) -> Option<String> {
    let name = roots.clsid.child(clsid).read_string("")?;
    let name = load_indirect_string(&name).unwrap_or(name);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn list_shell_extensions(
    roots: &MenuRoots,
    class: &str,
    applies_to: &str,
    cache: &mut InfoCache,
    out: &mut Vec<StartupItem>,
) {
    let handlers = roots
        .classes
        .child(class)
        .child(r"shellex\ContextMenuHandlers");
    let Ok(key) = handlers.open() else {
        return;
    };
    for sub in key.enum_keys().flatten() {
        let handler_key = handlers.child(&sub);
        let default = handler_key.read_string("").unwrap_or_default();
        let default = default.trim();
        let key_name = sub.trim();
        // The class identifier is the default value, or the key name itself.
        let clsid = if looks_like_clsid(default) {
            Some(default.to_owned())
        } else if looks_like_clsid(key_name) {
            Some(key_name.to_owned())
        } else {
            None
        };

        let server = clsid.as_deref().and_then(|c| clsid_server(roots, c));
        let command = server.clone().unwrap_or_default();
        let resolved = resolve_command(&command, cache);
        let (blocked_user, blocked_machine) = clsid
            .as_deref()
            .map_or((false, false), |c| is_blocked(roots, c));

        let name = if looks_like_clsid(key_name) {
            clsid
                .as_deref()
                .and_then(|c| clsid_name(roots, c))
                .unwrap_or_else(|| key_name.to_owned())
        } else {
            key_name.to_owned()
        };
        let machine = roots.scope == Scope::Machine;

        out.push(StartupItem {
            id: format!("ctx|{handler_key}"),
            kind: StartupKind::ContextMenu,
            name,
            command,
            target: resolved.target,
            target_missing: resolved.target_missing,
            is_microsoft: is_microsoft(resolved.publisher.as_deref()),
            publisher: resolved.publisher,
            description: clsid
                .as_deref()
                .and_then(|c| clsid_name(roots, c))
                .or(resolved.description),
            location: handler_key.to_string(),
            scope: roots.scope,
            enabled: !(blocked_user || blocked_machine),
            requires_admin: machine || blocked_machine,
            // Blocking happens in the user's own block list; only lifting a
            // machine-wide block needs administrator rights.
            toggle_requires_admin: blocked_machine,
            can_toggle: clsid.is_some(),
            can_delete: true,
            details: StartupDetails::ContextMenu(ContextMenuEntry {
                key: handler_key,
                handler: ContextHandlerKind::ShellExtension,
                clsid,
                applies_to: applies_to.to_owned(),
            }),
        });
    }
}

fn list_static_verbs(
    roots: &MenuRoots,
    class: &str,
    applies_to: &str,
    cache: &mut InfoCache,
    out: &mut Vec<StartupItem>,
) {
    let shell = roots.classes.child(class).child("shell");
    let Ok(key) = shell.open() else {
        return;
    };
    for sub in key.enum_keys().flatten() {
        let verb_key = shell.child(&sub);
        // Verbs marked for programmatic use only never appear in a menu.
        if has_value(&verb_key, "ProgrammaticAccessOnly") {
            continue;
        }
        let command_key = verb_key.child("command");
        let command = command_key.read_string("").unwrap_or_default();
        let command = command.trim().to_owned();

        // Verbs may be implemented by a COM object instead of a command line.
        let clsid = [
            command_key.read_string("DelegateExecute"),
            verb_key.read_string("ExplorerCommandHandler"),
            verb_key.read_string("DelegateExecute"),
            verb_key.child("DropTarget").read_string("CLSID"),
        ]
        .into_iter()
        .flatten()
        .map(|c| c.trim().to_owned())
        .find(|c| looks_like_clsid(c));

        let mut resolved = resolve_command(&command, cache);
        if resolved.target.is_none()
            && let Some(server) = clsid.as_deref().and_then(|c| clsid_server(roots, c))
        {
            resolved = resolve_command(&server, cache);
        }

        let raw_label = verb_key
            .read_string("MUIVerb")
            .or_else(|| verb_key.read_string(""));
        // No command and no class: fall back to the owner of the label text.
        if resolved.publisher.is_none()
            && let Some(publisher) = raw_label
                .as_deref()
                .and_then(|text| resource_file(text, cache))
        {
            resolved.publisher = Some(publisher);
        }
        let label = raw_label
            .map(|text| load_indirect_string(&text).unwrap_or(text))
            .map(|text| text.replace('&', "").trim().to_owned())
            .filter(|text| !text.is_empty() && !text.starts_with('@'))
            .unwrap_or_else(|| sub.trim().to_owned());

        let machine = roots.scope == Scope::Machine;
        let shown_command = if command.is_empty() {
            clsid
                .as_deref()
                .map(|c| format!("COM handler {c}"))
                .unwrap_or_default()
        } else {
            command
        };

        out.push(StartupItem {
            id: format!("ctxverb|{verb_key}"),
            kind: StartupKind::ContextMenu,
            name: label,
            command: shown_command,
            target: resolved.target,
            target_missing: resolved.target_missing,
            is_microsoft: is_microsoft(resolved.publisher.as_deref()),
            publisher: resolved.publisher,
            description: resolved.description,
            location: verb_key.to_string(),
            scope: roots.scope,
            enabled: !has_value(&verb_key, LEGACY_DISABLE),
            requires_admin: machine,
            toggle_requires_admin: machine,
            can_toggle: true,
            can_delete: true,
            details: StartupDetails::ContextMenu(ContextMenuEntry {
                key: verb_key,
                handler: ContextHandlerKind::StaticVerb,
                clsid,
                applies_to: applies_to.to_owned(),
            }),
        });
    }
}

/// Lists the entries registered below one set of roots.
pub(crate) fn list_roots(roots: &MenuRoots, cache: &mut InfoCache) -> Vec<StartupItem> {
    let mut out = Vec::new();
    for (class, applies_to) in CLASSES {
        list_shell_extensions(roots, class, applies_to, cache, &mut out);
        list_static_verbs(roots, class, applies_to, cache, &mut out);
    }
    out
}

/// Lists the context-menu entries of the current user and the machine.
pub(crate) fn list_all(cache: &mut InfoCache) -> Vec<StartupItem> {
    real_roots()
        .iter()
        .flat_map(|roots| list_roots(roots, cache))
        .collect()
}

fn set_string_value(key: &RegPath, name: &str, data: &str, action: &str) -> Result<()> {
    let (handle, _) = key
        .root
        .key()
        .create_subkey_with_flags(&key.path, KEY_READ | KEY_WRITE)
        .map_err(|e| io_err(e, action))?;
    handle.set_value(name, &data).map_err(|e| io_err(e, action))
}

fn remove_value(key: &RegPath, name: &str, action: &str) -> Result<()> {
    if !has_value(key, name) {
        return Ok(());
    }
    key.open_write()
        .and_then(|handle| handle.delete_value(name))
        .map_err(|e| io_err(e, action))
}

/// Blocks or unblocks a shell extension class.
pub(crate) fn set_extension_enabled(
    clsid: &str,
    enabled: bool,
    blocked_user: &RegPath,
    blocked_machine: Option<&RegPath>,
) -> Result<()> {
    if !looks_like_clsid(clsid) {
        return Err(Error::Invalid(format!("{clsid} is not a class identifier")));
    }
    let action = "change the context menu entry";
    if enabled {
        remove_value(blocked_user, clsid, action)?;
        if let Some(machine) = blocked_machine {
            remove_value(machine, clsid, action)?;
        }
        Ok(())
    } else {
        set_string_value(
            blocked_user,
            clsid,
            &format!("Disabled by {}", zest_core::brand::NAME),
            action,
        )
    }
}

/// Hides or shows a static verb through its `LegacyDisable` marker.
pub(crate) fn set_verb_enabled(verb_key: &RegPath, enabled: bool) -> Result<()> {
    let action = "change the context menu entry";
    if !verb_key.exists() {
        return Err(Error::NotFound(verb_key.to_string()));
    }
    if enabled {
        remove_value(verb_key, LEGACY_DISABLE, action)
    } else {
        set_string_value(verb_key, LEGACY_DISABLE, "", action)
    }
}

/// Enables or disables an entry found in the real registry.
pub(crate) fn set_enabled(entry: &ContextMenuEntry, enabled: bool) -> Result<()> {
    match entry.handler {
        ContextHandlerKind::ShellExtension => {
            let Some(clsid) = &entry.clsid else {
                return Err(Error::Other(
                    "this context menu entry has no class identifier and cannot be disabled"
                        .to_owned(),
                ));
            };
            set_extension_enabled(
                clsid,
                enabled,
                &RegPath::hkcu(BLOCKED_KEY),
                Some(&RegPath::hklm(BLOCKED_KEY)),
            )
        }
        ContextHandlerKind::StaticVerb => set_verb_enabled(&entry.key, enabled),
    }
}

/// True when `key` is a registration this module may remove: a handler below
/// `shellex\ContextMenuHandlers` or a verb directly below a `shell` key.
fn is_menu_registration(key: &RegPath) -> bool {
    let lower = key.path.to_lowercase();
    let Some((parent, leaf)) = lower.rsplit_once('\\') else {
        return false;
    };
    !leaf.is_empty()
        && (parent.ends_with(r"\shellex\contextmenuhandlers") || parent.ends_with(r"\shell"))
}

/// Deletes the registration key after exporting it to `backup`.
pub(crate) fn delete(entry: &ContextMenuEntry, backup: Option<&mut RegBackup>) -> Result<()> {
    if !is_menu_registration(&entry.key) {
        return Err(Error::Other(format!(
            "refused to delete {}: not a context menu registration",
            entry.key
        )));
    }
    zest_core::reg::delete_key(&entry.key, backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use zest_core::paths;

    const CLSID_A: &str = "{11111111-2222-3333-4444-555555555555}";
    const CLSID_B: &str = "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}";

    fn scratch_roots(scratch: &Scratch) -> MenuRoots {
        MenuRoots {
            classes: scratch.child("Classes"),
            clsid: scratch.child("CLSID"),
            blocked_user: scratch.child("BlockedUser"),
            blocked_machine: Some(scratch.child("BlockedMachine")),
            scope: Scope::User,
        }
    }

    fn create(path: &RegPath) -> winreg::RegKey {
        RegRoot::Hkcu.key().create_subkey(&path.path).unwrap().0
    }

    #[test]
    fn clsid_shapes() {
        assert!(looks_like_clsid(CLSID_A));
        assert!(looks_like_clsid("{09A47860-11B0-4DA5-AFA5-26D86198A780}"));
        assert!(!looks_like_clsid("{09A47860-11B0-4DA5-AFA5-26D86198A78}"));
        assert!(!looks_like_clsid("09A47860-11B0-4DA5-AFA5-26D86198A780"));
        assert!(!looks_like_clsid("{09A47860x11B0-4DA5-AFA5-26D86198A780}"));
        assert!(!looks_like_clsid("Some Handler"));
        assert!(!looks_like_clsid(""));
    }

    #[test]
    fn only_menu_registrations_may_be_deleted() {
        assert!(is_menu_registration(&RegPath::hklm(
            r"SOFTWARE\Classes\*\shellex\ContextMenuHandlers\Tool"
        )));
        assert!(is_menu_registration(&RegPath::hkcu(
            r"Software\Classes\Directory\Background\shell\OpenHere"
        )));
        assert!(!is_menu_registration(&RegPath::hklm(
            r"SOFTWARE\Classes\*\shellex"
        )));
        assert!(!is_menu_registration(&RegPath::hklm(
            r"SOFTWARE\Classes\Directory"
        )));
        assert!(!is_menu_registration(&RegPath::hklm(
            r"SOFTWARE\Classes\Directory\shell\Open\command"
        )));
        assert!(!is_menu_registration(&RegPath::hklm("")));

        let bogus = ContextMenuEntry {
            key: RegPath::hkcu(r"Software\ZestTests"),
            handler: ContextHandlerKind::StaticVerb,
            clsid: None,
            applies_to: String::new(),
        };
        assert!(matches!(delete(&bogus, None), Err(Error::Other(_))));
    }

    #[test]
    fn lists_toggles_and_deletes_scratch_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::new("ctx");
        let roots = scratch_roots(&scratch);
        let shell32 = r"%SystemRoot%\System32\shell32.dll";
        let drive = paths::system_drive();

        // A shell extension named by a friendly key, served by a real DLL.
        create(
            &roots
                .classes
                .child(r"*\shellex\ContextMenuHandlers\ Good Tool"),
        )
        .set_value("", &CLSID_A)
        .unwrap();
        let class_a = create(&roots.clsid.child(CLSID_A));
        class_a.set_value("", &"Good Tool Shell Extension").unwrap();
        create(&roots.clsid.child(CLSID_A).child("InprocServer32"))
            .set_value("", &shell32)
            .unwrap();

        // A shell extension registered under its class identifier whose DLL is gone.
        create(
            &roots
                .classes
                .child(&format!(r"Directory\shellex\ContextMenuHandlers\{CLSID_B}")),
        );
        create(&roots.clsid.child(CLSID_B))
            .set_value("", &"Removed Tool")
            .unwrap();
        create(&roots.clsid.child(CLSID_B).child("InprocServer32"))
            .set_value("", &format!(r"{}Zest No Such Dir\ext.dll", drive.display()))
            .unwrap();

        // A handler without any class identifier.
        create(
            &roots
                .classes
                .child(r"Drive\shellex\ContextMenuHandlers\Nameless"),
        );

        // A static verb with a command, and one implemented by a COM class.
        let verb = roots.classes.child(r"Directory\Background\shell\OpenHere");
        create(&verb)
            .set_value("MUIVerb", &"Open &tool here")
            .unwrap();
        create(&verb.child("command"))
            .set_value("", &r#""%SystemRoot%\System32\cmd.exe" /k cd "%V""#)
            .unwrap();
        let com_verb = roots.classes.child(r"Folder\shell\Pin");
        create(&com_verb)
            .set_value("ExplorerCommandHandler", &CLSID_A)
            .unwrap();
        // A verb for programmatic use only (never shown) and one that only
        // has a label loaded from a system file.
        create(&roots.classes.child(r"*\shell\hiddenverb"))
            .set_value("ProgrammaticAccessOnly", &"Apartment")
            .unwrap();
        create(&roots.classes.child(r"*\shell\LabelOnly"))
            .set_value("MUIVerb", &r"@%SystemRoot%\system32\shell32.dll,-8506")
            .unwrap();

        let mut cache = InfoCache::default();
        let items = list_roots(&roots, &mut cache);
        assert_eq!(items.len(), 6, "{items:#?}");
        let find = |name: &str| items.iter().find(|i| i.name == name).unwrap();
        assert!(items.iter().all(|i| !i.id.ends_with("hiddenverb")));
        let label_only = items.iter().find(|i| i.id.ends_with("LabelOnly")).unwrap();
        assert!(label_only.is_microsoft && label_only.target.is_none());
        assert!(!label_only.name.starts_with('@'), "{}", label_only.name);

        let good = find("Good Tool");
        assert!(good.enabled && good.can_toggle && good.can_delete);
        assert!(!good.toggle_requires_admin && !good.target_missing);
        assert!(good.is_microsoft, "shell32 is a Microsoft file");
        assert_eq!(good.command, shell32);
        assert!(good.target.as_ref().is_some_and(|t| t.is_file()));
        assert_eq!(
            good.description.as_deref(),
            Some("Good Tool Shell Extension")
        );
        assert!(matches!(
            &good.details,
            StartupDetails::ContextMenu(e)
                if e.handler == ContextHandlerKind::ShellExtension
                    && e.clsid.as_deref() == Some(CLSID_A)
                    && e.applies_to == "All files"
        ));

        let removed = find("Removed Tool");
        assert!(removed.target_missing, "{removed:?}");
        assert!(!removed.is_microsoft);

        let nameless = find("Nameless");
        assert!(!nameless.can_toggle);
        assert!(matches!(
            super::super::set_enabled(nameless, false),
            Err(Error::Other(_))
        ));

        let open_here = find("Open tool here");
        assert!(open_here.enabled && open_here.is_microsoft);
        assert!(open_here.command.contains("cmd.exe"));
        assert!(open_here.id.starts_with("ctxverb|"));

        let pin = find("Pin");
        assert_eq!(pin.command, format!("COM handler {CLSID_A}"));
        assert!(
            pin.target
                .as_ref()
                .is_some_and(|t| t.ends_with("shell32.dll"))
        );

        // Block and unblock a shell extension in the per-user list.
        let machine_list = roots.blocked_machine.as_ref().unwrap();
        set_extension_enabled(CLSID_A, false, &roots.blocked_user, Some(machine_list)).unwrap();
        assert!(
            !list_roots(&roots, &mut cache)
                .iter()
                .find(|i| i.name == "Good Tool")
                .unwrap()
                .enabled
        );
        assert!(has_value(&roots.blocked_user, CLSID_A));
        assert!(!has_value(machine_list, CLSID_A));
        set_extension_enabled(CLSID_A, true, &roots.blocked_user, Some(machine_list)).unwrap();
        assert!(!has_value(&roots.blocked_user, CLSID_A));

        // A machine-wide block is recognised and lifted when enabling.
        create(machine_list).set_value(CLSID_B, &"blocked").unwrap();
        let blocked = list_roots(&roots, &mut cache);
        let removed = blocked.iter().find(|i| i.name == "Removed Tool").unwrap();
        assert!(!removed.enabled && removed.toggle_requires_admin);
        set_extension_enabled(CLSID_B, true, &roots.blocked_user, Some(machine_list)).unwrap();
        assert!(!has_value(machine_list, CLSID_B));
        assert!(set_extension_enabled("not-a-clsid", false, &roots.blocked_user, None).is_err());

        // Hide and show a static verb.
        set_verb_enabled(&verb, false).unwrap();
        assert!(has_value(&verb, LEGACY_DISABLE));
        assert!(
            !list_roots(&roots, &mut cache)
                .iter()
                .find(|i| i.name == "Open tool here")
                .unwrap()
                .enabled
        );
        set_verb_enabled(&verb, true).unwrap();
        assert!(!has_value(&verb, LEGACY_DISABLE));
        set_verb_enabled(&verb, true).unwrap();
        assert!(matches!(
            set_verb_enabled(&roots.classes.child(r"Drive\shell\Nope"), false),
            Err(Error::NotFound(_))
        ));

        // Delete with backup, then restore.
        let mut backup = RegBackup::at(tmp.path().join("menu.reg"));
        super::super::delete(open_here, Some(&mut backup), None).unwrap();
        assert!(!verb.exists());
        assert_eq!(list_roots(&roots, &mut cache).len(), 5);
        let saved = backup.save().unwrap().unwrap();
        zest_core::reg::import_reg_file(&saved).unwrap();
        assert!(verb.child("command").exists());
        assert_eq!(list_roots(&roots, &mut cache).len(), 6);
        assert!(!paths::expand_env(shell32).contains('%'));
    }

    #[test]
    fn enumerates_real_menus_read_only() {
        let items = list_all(&mut InfoCache::default());
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            assert_eq!(item.kind, StartupKind::ContextMenu);
            assert!(ids.insert(item.id.clone()), "duplicate id {}", item.id);
            let StartupDetails::ContextMenu(entry) = &item.details else {
                panic!("wrong details");
            };
            assert!(is_menu_registration(&entry.key), "{}", entry.key);
        }
        // Windows always registers verbs for folders.
        assert!(!items.is_empty());
    }
}
