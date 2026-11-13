//! Startup manager: everything that starts automatically on this PC.
//!
//! All sources are presented as one uniform [`StartupItem`] list:
//!
//! | [`StartupKind`]     | Source                                                   |
//! |---------------------|----------------------------------------------------------|
//! | `RegistryRun`       | `Run` and `Policies\Explorer\Run` keys (user + machine)  |
//! | `RegistryRunOnce`   | `RunOnce` keys                                           |
//! | `StartupFolder`     | per-user and all-users Startup folders                   |
//! | `StoreApp`          | startup tasks declared by Microsoft Store apps           |
//! | `ScheduledTask`     | Task Scheduler                                           |
//! | `Service`           | Windows services                                         |
//! | `ContextMenu`       | Explorer context-menu handlers and static verbs          |
//!
//! Enabling and disabling uses the mechanisms Windows itself uses, so the
//! result is visible in Task Manager / Settings and can be undone there:
//!
//! * Run keys and Startup folders: the `Explorer\StartupApproved` flags.
//! * Store apps: the per-user startup task state.
//! * Scheduled tasks: the task's own enabled flag.
//! * Services: the start type (the previous type is remembered and restored).
//! * Shell extensions: the `Shell Extensions\Blocked` list; static verbs:
//!   the `LegacyDisable` marker.
//!
//! Deleting always backs up first (registry export, task definition) or goes
//! to the Recycle Bin (shortcuts). Services can never be deleted.

mod approved;
mod context_menu;
mod folder;
mod registry;
mod services;
mod store_tasks;
mod tasks;

pub use services::{ServiceStartType, ServiceState};
pub use tasks::{TaskBackup, TaskState, restore_task};

use crate::Scope;
use crate::util::InfoCache;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zest_core::reg::{RegBackup, RegPath};
use zest_core::{Error, Progress, Result};

/// The source an automatically started item comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StartupKind {
    /// A value in a registry `Run` key (runs at every sign-in).
    RegistryRun,
    /// A value in a registry `RunOnce` key (runs once, then removes itself).
    RegistryRunOnce,
    /// A file or shortcut in a Startup folder.
    StartupFolder,
    /// A startup task declared by a Microsoft Store app.
    StoreApp,
    /// A scheduled task.
    ScheduledTask,
    /// A Windows service.
    Service,
    /// An Explorer context-menu handler or static menu verb.
    ContextMenu,
}

impl StartupKind {
    /// Every kind, in display order.
    pub const ALL: [StartupKind; 7] = [
        StartupKind::RegistryRun,
        StartupKind::RegistryRunOnce,
        StartupKind::StartupFolder,
        StartupKind::StoreApp,
        StartupKind::ScheduledTask,
        StartupKind::Service,
        StartupKind::ContextMenu,
    ];

    /// The kinds shown on a "Startup apps" page (what Task Manager lists).
    pub const PROGRAMS: [StartupKind; 4] = [
        StartupKind::RegistryRun,
        StartupKind::RegistryRunOnce,
        StartupKind::StartupFolder,
        StartupKind::StoreApp,
    ];

    /// Short label, e.g. `Scheduled task`.
    pub fn label(self) -> &'static str {
        match self {
            StartupKind::RegistryRun => "Registry (Run)",
            StartupKind::RegistryRunOnce => "Registry (RunOnce)",
            StartupKind::StartupFolder => "Startup folder",
            StartupKind::StoreApp => "Store app",
            StartupKind::ScheduledTask => "Scheduled task",
            StartupKind::Service => "Service",
            StartupKind::ContextMenu => "Context menu",
        }
    }
}

/// One automatically started item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StartupItem {
    /// Stable identifier, unique across all kinds (e.g. `task|\Vendor\Updater`).
    pub id: String,
    /// Where the item comes from.
    pub kind: StartupKind,
    /// Display name.
    pub name: String,
    /// The command that is run (may be empty for COM based handlers).
    pub command: String,
    /// The file the command resolves to, whether or not it exists.
    pub target: Option<PathBuf>,
    /// True when the target is positively known to be gone (an orphan).
    pub target_missing: bool,
    /// Publisher from the target's version information.
    pub publisher: Option<String>,
    /// Friendly description (file description, service or task description).
    pub description: Option<String>,
    /// Human readable location, e.g. a registry key or a task folder.
    pub location: String,
    /// Current user or whole machine.
    pub scope: Scope,
    /// Whether the item currently starts automatically.
    pub enabled: bool,
    /// True when changing this item in any way may need administrator rights.
    pub requires_admin: bool,
    /// True when [`set_enabled`] specifically needs administrator rights
    /// (for some machine-wide items a per-user switch exists that does not).
    pub toggle_requires_admin: bool,
    /// True for items published by Microsoft / part of Windows; front ends
    /// hide these by default.
    pub is_microsoft: bool,
    /// Whether [`set_enabled`] is supported for this item.
    pub can_toggle: bool,
    /// Whether [`delete`] is supported for this item.
    pub can_delete: bool,
    /// Kind specific details.
    pub details: StartupDetails,
}

/// Kind specific details of a [`StartupItem`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StartupDetails {
    /// A `Run` / `RunOnce` registry value.
    Registry(RegistryEntry),
    /// A file in a Startup folder.
    Folder(FolderEntry),
    /// A startup task of a Microsoft Store app.
    StoreApp(StoreStartupEntry),
    /// A scheduled task.
    Task(TaskEntry),
    /// A Windows service.
    Service(ServiceEntry),
    /// An Explorer context-menu entry.
    ContextMenu(ContextMenuEntry),
}

/// Details of a registry startup value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// The key that holds the value.
    pub key: RegPath,
    /// The value name.
    pub value: String,
    /// The `StartupApproved` key that stores the enabled flag, if this
    /// location supports enabling and disabling.
    pub approved_key: Option<RegPath>,
}

/// Details of a Startup folder entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    /// The file inside the Startup folder (usually a `.lnk` shortcut).
    pub file: PathBuf,
    /// Arguments stored in the shortcut.
    pub arguments: String,
    /// The `StartupApproved` key that stores the enabled flag.
    pub approved_key: Option<RegPath>,
}

/// Details of a Microsoft Store app's startup task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreStartupEntry {
    /// The registry key that stores the task's state.
    pub key: RegPath,
    /// Package family the task belongs to.
    pub package_family_name: String,
    /// Identifier of the task inside the package.
    pub task_id: String,
    /// Raw state number: 0 off, 1 turned off by the user, 2 on, 3 turned
    /// off by policy, 4 turned on by policy.
    pub state: u32,
}

/// Details of a scheduled task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEntry {
    /// Full task path, e.g. `\Vendor\Updater`.
    pub path: String,
    /// Folder part of the path, e.g. `\Vendor` (`\` for the root).
    pub folder: String,
    /// One short phrase per trigger, e.g. `At log on`, `Daily`.
    pub triggers: Vec<String>,
    /// True when a trigger fires at system start or at sign-in.
    pub runs_at_startup: bool,
    /// Last time the task ran (local time), if ever.
    pub last_run: Option<NaiveDateTime>,
    /// Next scheduled run (local time), if any.
    pub next_run: Option<NaiveDateTime>,
    /// Result code of the last run.
    pub last_result: Option<i32>,
    /// Author recorded in the task.
    pub author: Option<String>,
    /// Account the task runs as.
    pub run_as: Option<String>,
    /// True when the task runs with highest privileges.
    pub highest_privileges: bool,
    /// True when the task is flagged hidden.
    pub hidden: bool,
    /// Scheduler state.
    pub state: TaskState,
    /// Number of actions the task performs (only the first is shown as the command).
    pub action_count: usize,
}

/// Details of a Windows service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEntry {
    /// Internal service name (the registry key name).
    pub service_name: String,
    /// Friendly name.
    pub display_name: String,
    /// How the service starts.
    pub start_type: ServiceStartType,
    /// What the service is doing right now.
    pub state: ServiceState,
    /// Account the service runs as.
    pub account: Option<String>,
    /// The DLL implementing the service when it is hosted in a shared process.
    pub service_dll: Option<PathBuf>,
    /// Process identifier while running.
    pub process_id: Option<u32>,
    /// True for services Windows cannot run without; these cannot be toggled.
    pub is_critical: bool,
}

/// Which mechanism adds a context-menu entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContextHandlerKind {
    /// A COM shell extension registered under `shellex\ContextMenuHandlers`.
    ShellExtension,
    /// A static verb registered under `shell`.
    StaticVerb,
}

/// Details of an Explorer context-menu entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMenuEntry {
    /// The registry key that registers the entry.
    pub key: RegPath,
    /// Shell extension or static verb.
    pub handler: ContextHandlerKind,
    /// Class identifier of the COM object, when one is involved.
    pub clsid: Option<String>,
    /// What the menu entry applies to, e.g. `All files`, `Folders`.
    pub applies_to: String,
}

/// Lists Run-key, Startup-folder and Store-app startup entries – the
/// equivalent of Task Manager's "Startup apps" list. Never fails; unreadable
/// locations are skipped.
pub fn list_startup_programs() -> Vec<StartupItem> {
    let mut cache = InfoCache::default();
    let mut items = registry::list_all(&mut cache);
    items.extend(folder::list_all(&mut cache));
    items.extend(store_tasks::list_all());
    items
}

/// Lists every scheduled task (hidden ones included).
pub fn list_scheduled_tasks() -> Result<Vec<StartupItem>> {
    tasks::list(&mut InfoCache::default())
}

/// Lists every Windows service (kernel drivers are not included).
pub fn list_services() -> Result<Vec<StartupItem>> {
    services::list(&mut InfoCache::default())
}

/// Lists Explorer context-menu handlers and static verbs for files, folders,
/// drives and the desktop background.
pub fn list_context_menu() -> Vec<StartupItem> {
    context_menu::list_all(&mut InfoCache::default())
}

/// Lists the requested kinds in one pass.
///
/// The independent sources are read in parallel. A source that cannot be
/// read (for example the Task Scheduler service being stopped) is logged and
/// skipped so the remaining sources still show. Returns
/// [`Error::Cancelled`] when `progress` is cancelled.
pub fn list(kinds: &[StartupKind], progress: &Progress) -> Result<Vec<StartupItem>> {
    let wants = |kind: StartupKind| kinds.contains(&kind);
    progress.set_total(4);
    progress.set_message("Reading startup items");

    let programs = || {
        let mut cache = InfoCache::default();
        let mut items = Vec::new();
        if wants(StartupKind::RegistryRun) || wants(StartupKind::RegistryRunOnce) {
            items.extend(
                registry::list_all(&mut cache)
                    .into_iter()
                    .filter(|item| wants(item.kind)),
            );
        }
        if wants(StartupKind::StartupFolder) {
            items.extend(folder::list_all(&mut cache));
        }
        if wants(StartupKind::StoreApp) {
            items.extend(store_tasks::list_all());
        }
        progress.inc(1);
        items
    };
    let scheduled = || {
        let mut items = Vec::new();
        if wants(StartupKind::ScheduledTask) && !progress.is_cancelled() {
            match tasks::list(&mut InfoCache::default()) {
                Ok(found) => items = found,
                Err(e) => log::warn!("scheduled tasks could not be listed: {e}"),
            }
        }
        progress.inc(1);
        items
    };
    let system_services = || {
        let mut items = Vec::new();
        if wants(StartupKind::Service) && !progress.is_cancelled() {
            match services::list(&mut InfoCache::default()) {
                Ok(found) => items = found,
                Err(e) => log::warn!("services could not be listed: {e}"),
            }
        }
        progress.inc(1);
        items
    };
    let menus = || {
        let mut items = Vec::new();
        if wants(StartupKind::ContextMenu) && !progress.is_cancelled() {
            items = context_menu::list_all(&mut InfoCache::default());
        }
        progress.inc(1);
        items
    };

    let ((mut items, tasks), (services, menus)) = rayon::join(
        || rayon::join(programs, scheduled),
        || rayon::join(system_services, menus),
    );
    progress.check()?;
    items.extend(tasks);
    items.extend(services);
    items.extend(menus);
    Ok(items)
}

/// Lists every kind. See [`list`].
pub fn list_all(progress: &Progress) -> Result<Vec<StartupItem>> {
    list(&StartupKind::ALL, progress)
}

/// Enables or disables an item.
///
/// The change is reversible and uses the same switch Windows uses. Fails
/// with [`Error::NeedsElevation`] when administrator rights are required
/// and missing, and with [`Error::Other`] for items that cannot be toggled
/// (`can_toggle == false`).
///
/// Context-menu changes take effect once Explorer restarts (sign out and in,
/// or restart `explorer.exe`). Disabling a service does not stop it; it no
/// longer starts from the next boot on.
pub fn set_enabled(item: &StartupItem, enabled: bool) -> Result<()> {
    if !item.can_toggle {
        return Err(Error::Other(format!(
            "\"{}\" cannot be enabled or disabled",
            item.name
        )));
    }
    match &item.details {
        StartupDetails::Registry(entry) => match &entry.approved_key {
            Some(key) => approved::set_enabled(key, &entry.value, enabled),
            None => Err(Error::Other(format!(
                "\"{}\" can only be deleted, not disabled",
                item.name
            ))),
        },
        StartupDetails::Folder(entry) => {
            let name = folder::approved_name(&entry.file);
            match &entry.approved_key {
                Some(key) => approved::set_enabled(key, &name, enabled),
                None => Err(Error::Other(format!(
                    "\"{}\" can only be deleted, not disabled",
                    item.name
                ))),
            }
        }
        StartupDetails::StoreApp(entry) => store_tasks::set_enabled(entry, enabled),
        StartupDetails::Task(task) => tasks::set_enabled(&task.path, enabled),
        StartupDetails::Service(service) => {
            if service.is_critical {
                return Err(Error::Other(format!(
                    "\"{}\" is required by Windows and cannot be disabled",
                    item.name
                )));
            }
            services::set_enabled(&service.service_name, enabled, &services::state_key())
        }
        StartupDetails::ContextMenu(entry) => context_menu::set_enabled(entry, enabled),
    }
}

/// Deletes an item permanently, after securing a way back:
///
/// * registry values and context-menu keys are exported to `backup` first
///   (pass `None` only if the caller really wants no backup);
/// * Startup-folder files go to the Recycle Bin;
/// * scheduled tasks have their full definition saved to `task_backup_dir`
///   (as `<name>.task.json`, restorable with [`restore_task`]) before removal.
///
/// Services are never deleted; the call fails with [`Error::Other`].
/// Returns the path of the task backup file when one was written.
pub fn delete(
    item: &StartupItem,
    backup: Option<&mut RegBackup>,
    task_backup_dir: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if !item.can_delete {
        return Err(Error::Other(format!("\"{}\" cannot be deleted", item.name)));
    }
    match &item.details {
        StartupDetails::Registry(entry) => {
            registry::delete(entry, backup)?;
            Ok(None)
        }
        StartupDetails::Folder(entry) => {
            folder::delete(entry, backup)?;
            Ok(None)
        }
        StartupDetails::StoreApp(_) => Err(Error::Other(
            "this entry belongs to a Store app; turn it off or uninstall the app".to_owned(),
        )),
        StartupDetails::Task(task) => tasks::delete(&task.path, task_backup_dir),
        StartupDetails::Service(_) => Err(Error::Other(
            "services cannot be deleted; disable the service instead".to_owned(),
        )),
        StartupDetails::ContextMenu(entry) => {
            context_menu::delete(entry, backup)?;
            Ok(None)
        }
    }
}

/// Fills in target, publisher and description for an item from its command
/// line. Shared by all sources.
pub(crate) struct Resolved {
    pub target: Option<PathBuf>,
    pub target_missing: bool,
    pub publisher: Option<String>,
    pub description: Option<String>,
}

/// Resolves what `command` launches, conservatively (unknown is never "missing").
pub(crate) fn resolve_command(command: &str, cache: &mut InfoCache) -> Resolved {
    use zest_core::cmdline::{self, Target};

    if command.trim().is_empty() {
        return Resolved {
            target: None,
            target_missing: false,
            publisher: None,
            description: None,
        };
    }
    let (target, target_missing) = match cmdline::check_command(command) {
        Target::Exists(path) => (Some(path), false),
        Target::Missing(path) => (Some(path), true),
        // The real payload is unknown (script host, bare name, network
        // path); show the launching program when that can be found.
        Target::Unknown => match cmdline::check_program(command) {
            Target::Exists(path) => (Some(path), false),
            _ => (None, false),
        },
    };
    let (publisher, description) = match (&target, target_missing) {
        (Some(path), false) => cache.publisher_and_description(path),
        _ => (None, None),
    };
    Resolved {
        target,
        target_missing,
        publisher,
        description,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_labels() {
        for kind in StartupKind::ALL {
            assert!(!kind.label().is_empty());
        }
        assert!(
            StartupKind::PROGRAMS
                .iter()
                .all(|k| StartupKind::ALL.contains(k))
        );
    }

    #[test]
    fn command_resolution_is_conservative() {
        let mut cache = InfoCache::default();
        let cmd = zest_core::paths::system32_dir().join("cmd.exe");

        let found = resolve_command(&format!("\"{}\" /c echo hi", cmd.display()), &mut cache);
        assert!(found.target.is_some());
        assert!(!found.target_missing);
        assert!(crate::util::is_microsoft(found.publisher.as_deref()));

        let drive = zest_core::paths::system_drive();
        let gone = resolve_command(
            &format!("\"{}Zest No Such Dir\\agent.exe\" --tray", drive.display()),
            &mut cache,
        );
        assert!(gone.target_missing);
        assert!(gone.publisher.is_none());

        let unknown = resolve_command("some-unknown-zest-tool --flag", &mut cache);
        assert!(!unknown.target_missing);
        assert!(unknown.target.is_none());

        let empty = resolve_command("   ", &mut cache);
        assert!(empty.target.is_none() && !empty.target_missing);
    }

    #[test]
    fn untoggleable_and_undeletable_items_are_refused() {
        let item = StartupItem {
            id: "service|ZestNoSuchService".to_owned(),
            kind: StartupKind::Service,
            name: "Imaginary".to_owned(),
            command: String::new(),
            target: None,
            target_missing: false,
            publisher: None,
            description: None,
            location: "Services".to_owned(),
            scope: Scope::Machine,
            enabled: true,
            requires_admin: true,
            toggle_requires_admin: true,
            is_microsoft: false,
            can_toggle: false,
            can_delete: false,
            details: StartupDetails::Service(ServiceEntry {
                service_name: "ZestNoSuchService".to_owned(),
                display_name: "Imaginary".to_owned(),
                start_type: ServiceStartType::Manual,
                state: ServiceState::Stopped,
                account: None,
                service_dll: None,
                process_id: None,
                is_critical: false,
            }),
        };
        assert!(matches!(set_enabled(&item, false), Err(Error::Other(_))));
        assert!(matches!(delete(&item, None, None), Err(Error::Other(_))));

        // Even when flagged deletable, a service is never deleted.
        let mut forced = item.clone();
        forced.can_delete = true;
        assert!(matches!(delete(&forced, None, None), Err(Error::Other(_))));
    }

    #[test]
    fn items_serialize() {
        let items = list_startup_programs();
        let json = serde_json::to_string(&items).unwrap();
        let back: Vec<StartupItem> = serde_json::from_str(&json).unwrap();
        assert_eq!(items, back);
    }
}
