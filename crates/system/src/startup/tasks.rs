//! Scheduled tasks, through the Task Scheduler 2.0 COM API.

use super::{StartupDetails, StartupItem, StartupKind, TaskEntry, resolve_command};
use crate::Scope;
use crate::com::ComGuard;
use crate::util::{
    InfoCache, clsid_server, current_user_sid, is_microsoft, load_indirect_string,
    ole_date_to_local, win_err,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{VARIANT_BOOL, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::TaskScheduler::{
    IComHandlerAction, IExecAction, IRegisteredTask, ITaskFolder, ITaskService,
    TASK_ACTION_COM_HANDLER, TASK_ACTION_EXEC, TASK_ACTION_TYPE, TASK_ENUM_HIDDEN,
    TASK_RUNLEVEL_HIGHEST, TASK_RUNLEVEL_TYPE, TASK_TRIGGER_TYPE2, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, Interface};
use zest_core::reg::RegPath;
use zest_core::{Error, IoContext, Result, elevation, paths, process};

/// `OWNER_SECURITY_INFORMATION`
const OWNER_SECURITY_INFORMATION: i32 = 0x1;
/// HRESULT for "the system cannot find the file specified".
const HR_FILE_NOT_FOUND: u32 = 0x8007_0002;
/// HRESULT for "the system cannot find the path specified".
const HR_PATH_NOT_FOUND: u32 = 0x8007_0003;
/// Folder nesting beyond this is not followed (guards against cycles).
const MAX_FOLDER_DEPTH: usize = 32;

/// What the scheduler is doing with a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskState {
    /// The state could not be determined.
    Unknown,
    /// The task is disabled and will not run.
    Disabled,
    /// An instance is queued to run.
    Queued,
    /// The task is enabled and waiting for its next trigger.
    Ready,
    /// The task is running right now.
    Running,
}

impl TaskState {
    fn from_code(code: i32) -> Self {
        match code {
            1 => TaskState::Disabled,
            2 => TaskState::Queued,
            3 => TaskState::Ready,
            4 => TaskState::Running,
            _ => TaskState::Unknown,
        }
    }

    /// Short label, e.g. `Ready`.
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Unknown => "Unknown",
            TaskState::Disabled => "Disabled",
            TaskState::Queued => "Queued",
            TaskState::Ready => "Ready",
            TaskState::Running => "Running",
        }
    }
}

/// Everything read from the scheduler for one task, as plain data.
#[derive(Clone, Debug, Default)]
struct RawTask {
    name: String,
    path: String,
    enabled: bool,
    state: i32,
    last_run: f64,
    next_run: f64,
    last_result: Option<i32>,
    author: Option<String>,
    description: Option<String>,
    trigger_types: Vec<i32>,
    exec: Option<(String, String)>,
    com_handler: Option<String>,
    action_count: usize,
    user_id: Option<String>,
    highest_privileges: bool,
    hidden: bool,
    owner: Option<String>,
}

fn connect() -> Result<ITaskService> {
    let action = "open the Task Scheduler";
    // SAFETY: standard COM activation; the caller holds a `ComGuard`.
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| win_err(e, action))?;
        let empty = VARIANT::default();
        service
            .Connect(&empty, &empty, &empty, &empty)
            .map_err(|e| win_err(e, action))?;
        Ok(service)
    }
}

fn non_empty(text: BSTR) -> Option<String> {
    let text = text.to_string();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Task metadata may hold resource references of the form
/// `$(@%SystemRoot%\system32\some.dll,-123)`; resolve them to plain text.
fn resolve_resource_text(text: String) -> String {
    if let Some(inner) = text
        .strip_prefix("$(")
        .and_then(|rest| rest.strip_suffix(')'))
        && let Some(resolved) = load_indirect_string(&paths::expand_env(inner))
    {
        return resolved;
    }
    text
}

/// Extracts the owner from an SDDL string such as `O:BAG:SYD:(...)` or
/// `O:S-1-5-21-1-2-3-1001`.
fn sddl_owner(sddl: &str) -> Option<String> {
    let rest = sddl.strip_prefix("O:")?;
    let end = if rest.starts_with("S-") {
        rest.char_indices()
            .skip(2)
            .find(|(_, c)| !(c.is_ascii_digit() || *c == '-'))
            .map_or(rest.len(), |(i, _)| i)
    } else {
        rest.len().min(2)
    };
    let owner = rest[..end].trim_end_matches('-');
    (!owner.is_empty()).then(|| owner.to_owned())
}

/// Reads one registered task into plain data.
///
/// # Safety
/// Must be called on a thread with COM initialised.
unsafe fn read_task(task: &IRegisteredTask) -> Option<RawTask> {
    // SAFETY: plain COM property reads; every out parameter is a valid
    // pointer to a local.
    unsafe {
        let mut raw = RawTask {
            name: task.Name().ok()?.to_string(),
            path: task.Path().ok()?.to_string(),
            enabled: task.Enabled().map(|b| b.as_bool()).unwrap_or(true),
            state: task.State().map(|s| s.0).unwrap_or(0),
            last_run: task.LastRunTime().unwrap_or(0.0),
            next_run: task.NextRunTime().unwrap_or(0.0),
            last_result: task.LastTaskResult().ok(),
            ..RawTask::default()
        };
        raw.owner = task
            .GetSecurityDescriptor(OWNER_SECURITY_INFORMATION)
            .ok()
            .and_then(|sddl| sddl_owner(&sddl.to_string()));

        // The definition is unavailable for a few protected tasks; such a
        // task is still listed with what is known.
        let Ok(definition) = task.Definition() else {
            return Some(raw);
        };

        if let Ok(info) = definition.RegistrationInfo() {
            let mut text = BSTR::new();
            if info.Author(&mut text).is_ok() {
                raw.author = non_empty(std::mem::take(&mut text)).map(resolve_resource_text);
            }
            if info.Description(&mut text).is_ok() {
                raw.description = non_empty(std::mem::take(&mut text)).map(resolve_resource_text);
            }
        }

        if let Ok(triggers) = definition.Triggers() {
            let mut count = 0i32;
            if triggers.Count(&mut count).is_ok() {
                for index in 1..=count {
                    let Ok(trigger) = triggers.get_Item(index) else {
                        continue;
                    };
                    let mut kind = TASK_TRIGGER_TYPE2::default();
                    if trigger.Type(&mut kind).is_ok() {
                        raw.trigger_types.push(kind.0);
                    }
                }
            }
        }

        if let Ok(actions) = definition.Actions() {
            let mut count = 0i32;
            if actions.Count(&mut count).is_ok() {
                raw.action_count = count.max(0) as usize;
                for index in 1..=count {
                    let Ok(action) = actions.get_Item(index) else {
                        continue;
                    };
                    let mut kind = TASK_ACTION_TYPE::default();
                    if action.Type(&mut kind).is_err() {
                        continue;
                    }
                    if kind == TASK_ACTION_EXEC && raw.exec.is_none() {
                        if let Ok(exec) = action.cast::<IExecAction>() {
                            let mut path = BSTR::new();
                            let mut args = BSTR::new();
                            let _ = exec.Path(&mut path);
                            let _ = exec.Arguments(&mut args);
                            raw.exec = Some((path.to_string(), args.to_string()));
                        }
                    } else if kind == TASK_ACTION_COM_HANDLER
                        && raw.com_handler.is_none()
                        && let Ok(handler) = action.cast::<IComHandlerAction>()
                    {
                        let mut clsid = BSTR::new();
                        if handler.ClassId(&mut clsid).is_ok() {
                            raw.com_handler = non_empty(clsid);
                        }
                    }
                }
            }
        }

        if let Ok(principal) = definition.Principal() {
            let mut user = BSTR::new();
            if principal.UserId(&mut user).is_ok() {
                raw.user_id = non_empty(std::mem::take(&mut user));
            }
            if raw.user_id.is_none() && principal.GroupId(&mut user).is_ok() {
                raw.user_id = non_empty(user);
            }
            let mut level = TASK_RUNLEVEL_TYPE::default();
            if principal.RunLevel(&mut level).is_ok() {
                raw.highest_privileges = level == TASK_RUNLEVEL_HIGHEST;
            }
        }

        if let Ok(settings) = definition.Settings() {
            let mut hidden = VARIANT_BOOL::default();
            if settings.Hidden(&mut hidden).is_ok() {
                raw.hidden = hidden.as_bool();
            }
        }

        Some(raw)
    }
}

/// A Task Scheduler connection together with the COM apartment it lives in.
/// Field order matters: the service must be released before COM is left.
struct Connection {
    service: ITaskService,
    _com: ComGuard,
}

impl Connection {
    fn open() -> Result<Self> {
        let com = ComGuard::new();
        let service = connect()?;
        Ok(Connection { service, _com: com })
    }
}

/// Collects the paths of `folder` and all folders below it.
///
/// # Safety
/// Must be called on a thread with COM initialised.
unsafe fn collect_folders(folder: &ITaskFolder, depth: usize, out: &mut Vec<String>) {
    // SAFETY: plain COM enumeration; collections are 1-based.
    unsafe {
        let Ok(path) = folder.Path() else {
            return;
        };
        out.push(path.to_string());
        if depth >= MAX_FOLDER_DEPTH {
            return;
        }
        if let Ok(folders) = folder.GetFolders(0) {
            let count = folders.Count().unwrap_or(0);
            for index in 1..=count {
                if let Ok(sub) = folders.get_Item(&VARIANT::from(index)) {
                    collect_folders(&sub, depth + 1, out);
                }
            }
        }
    }
}

/// Reads the tasks directly inside one folder (hidden ones included).
///
/// # Safety
/// Must be called on the thread that owns `service`'s apartment.
unsafe fn read_folder(service: &ITaskService, folder_path: &str) -> Vec<RawTask> {
    let mut out = Vec::new();
    // SAFETY: plain COM enumeration; collections are 1-based.
    unsafe {
        let Ok(folder) = service.GetFolder(&BSTR::from(folder_path)) else {
            return out;
        };
        if let Ok(tasks) = folder.GetTasks(TASK_ENUM_HIDDEN.0) {
            let count = tasks.Count().unwrap_or(0);
            for index in 1..=count {
                if let Ok(task) = tasks.get_Item(&VARIANT::from(index))
                    && let Some(raw) = read_task(&task)
                {
                    out.push(raw);
                }
            }
        }
    }
    out
}

/// Short phrase for a trigger type code.
fn trigger_label(code: i32) -> &'static str {
    match code {
        0 => "On an event",
        1 => "One time",
        2 => "Daily",
        3 => "Weekly",
        4 | 5 => "Monthly",
        6 => "On idle",
        7 => "When the task is created",
        8 => "At system startup",
        9 => "At log on",
        11 => "On session change",
        _ => "Custom trigger",
    }
}

/// Splits `\Folder\Sub\Name` into (`\Folder\Sub`, `Name`).
fn split_task_path(path: &str) -> Result<(String, String)> {
    let trimmed = path.trim();
    let Some(index) = trimmed.rfind('\\') else {
        return Err(Error::Invalid(format!("\"{path}\" is not a task path")));
    };
    let name = &trimmed[index + 1..];
    if !trimmed.starts_with('\\') || name.is_empty() {
        return Err(Error::Invalid(format!("\"{path}\" is not a task path")));
    }
    let folder = if index == 0 { "\\" } else { &trimmed[..index] };
    Ok((folder.to_owned(), name.to_owned()))
}

/// Builds the command line shown for an executable action.
fn exec_command(path: &str, args: &str) -> String {
    let path = path.trim();
    let program = if path.contains(' ') && !path.starts_with('"') {
        format!("\"{path}\"")
    } else {
        path.to_owned()
    };
    if args.trim().is_empty() {
        program
    } else {
        format!("{program} {}", args.trim())
    }
}

fn make_item(raw: RawTask, cache: &mut InfoCache) -> StartupItem {
    let command = match (&raw.exec, &raw.com_handler) {
        (Some((path, args)), _) => exec_command(path, args),
        (None, Some(clsid)) => format!("COM handler {clsid}"),
        (None, None) => String::new(),
    };
    let resolved = if raw.exec.is_some() {
        resolve_command(&command, cache)
    } else {
        // A COM handler runs inside the class's registered server; that file
        // tells who the task belongs to.
        let server = raw
            .com_handler
            .as_deref()
            .and_then(|clsid| clsid_server(&RegPath::hkcr("CLSID"), clsid))
            .unwrap_or_default();
        resolve_command(&server, cache)
    };

    let mut triggers: Vec<String> = Vec::new();
    for code in &raw.trigger_types {
        let label = trigger_label(*code).to_owned();
        if !triggers.contains(&label) {
            triggers.push(label);
        }
    }
    let runs_at_startup = raw.trigger_types.iter().any(|c| *c == 8 || *c == 9);

    let folder = split_task_path(&raw.path)
        .map(|(folder, _)| folder)
        .unwrap_or_else(|_| "\\".to_owned());
    let in_windows_tree = raw
        .path
        .to_lowercase()
        .starts_with("\\microsoft\\windows\\");
    let is_ms = raw.path.to_lowercase().starts_with("\\microsoft\\")
        || is_microsoft(raw.author.as_deref())
        || is_microsoft(resolved.publisher.as_deref());
    let owned_by_user = match (&raw.owner, current_user_sid()) {
        (Some(owner), Some(me)) => owner.eq_ignore_ascii_case(me),
        _ => false,
    };

    StartupItem {
        id: format!("task|{}", raw.path),
        kind: StartupKind::ScheduledTask,
        name: raw.name,
        command,
        target: resolved.target,
        target_missing: resolved.target_missing,
        publisher: resolved.publisher,
        description: raw.description.or(resolved.description),
        location: folder.clone(),
        scope: if owned_by_user {
            Scope::User
        } else {
            Scope::Machine
        },
        enabled: raw.enabled,
        requires_admin: !owned_by_user,
        toggle_requires_admin: !owned_by_user,
        is_microsoft: is_ms,
        can_toggle: true,
        // Tasks that ship with Windows keep the system healthy; they can be
        // disabled but are never offered for deletion.
        can_delete: !in_windows_tree,
        details: StartupDetails::Task(TaskEntry {
            path: raw.path,
            folder,
            triggers,
            runs_at_startup,
            last_run: ole_date_to_local(raw.last_run),
            next_run: ole_date_to_local(raw.next_run),
            last_result: raw.last_result,
            author: raw.author,
            run_as: raw.user_id,
            highest_privileges: raw.highest_privileges,
            hidden: raw.hidden,
            state: TaskState::from_code(raw.state),
            action_count: raw.action_count,
        }),
    }
}

/// Lists every scheduled task, hidden ones included.
///
/// Reading a task takes several round trips to the scheduler service, so the
/// folders are read in parallel, each worker over its own connection.
pub(crate) fn list(cache: &mut InfoCache) -> Result<Vec<StartupItem>> {
    let main = Connection::open()?;
    let mut folders = Vec::new();
    // SAFETY: COM is initialised by `main`, which outlives every interface.
    unsafe {
        let root = main
            .service
            .GetFolder(&BSTR::from("\\"))
            .map_err(|e| win_err(e, "read scheduled tasks"))?;
        collect_folders(&root, 0, &mut folders);
    }

    let per_folder: Vec<Option<Vec<RawTask>>> = folders
        .par_iter()
        .map_init(
            || Connection::open().ok(),
            |connection, folder| {
                connection
                    .as_ref()
                    // SAFETY: the connection was opened on this worker thread
                    // and is only used here.
                    .map(|connection| unsafe { read_folder(&connection.service, folder) })
            },
        )
        .collect();

    let mut raw = Vec::new();
    for (folder, tasks) in folders.iter().zip(per_folder) {
        match tasks {
            Some(tasks) => raw.extend(tasks),
            // A worker could not connect: read that folder here instead.
            // SAFETY: `main` owns this thread's apartment.
            None => raw.extend(unsafe { read_folder(&main.service, folder) }),
        }
    }

    let mut items: Vec<StartupItem> = raw.into_iter().map(|r| make_item(r, cache)).collect();
    items.sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()));
    Ok(items)
}

fn task_not_found(e: &windows::core::Error) -> bool {
    matches!(e.code().0 as u32, HR_FILE_NOT_FOUND | HR_PATH_NOT_FOUND)
}

/// Finds a registered task by its full path.
///
/// # Safety
/// Must be called on a thread with COM initialised.
unsafe fn open_task(service: &ITaskService, path: &str) -> Result<IRegisteredTask> {
    // SAFETY: plain COM calls on valid interfaces.
    unsafe {
        let root = service
            .GetFolder(&BSTR::from("\\"))
            .map_err(|e| win_err(e, "read scheduled tasks"))?;
        root.GetTask(&BSTR::from(path)).map_err(|e| {
            if task_not_found(&e) {
                Error::NotFound(format!("scheduled task {path}"))
            } else {
                win_err(e, &format!("open the scheduled task {path}"))
            }
        })
    }
}

/// Enables or disables a task through its own enabled flag.
pub(crate) fn set_enabled(path: &str, enabled: bool) -> Result<()> {
    split_task_path(path)?;
    let _com = ComGuard::new();
    let service = connect()?;
    let flag = if enabled { VARIANT_TRUE } else { VARIANT_FALSE };
    // SAFETY: COM is initialised by `_com`.
    unsafe {
        let task = open_task(&service, path)?;
        task.SetEnabled(flag)
            .map_err(|e| win_err(e, &format!("change the scheduled task {path}")))
    }
}

/// A saved task definition, written before a task is deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBackup {
    /// Full path the task was registered under, e.g. `\Vendor\Updater`.
    pub path: String,
    /// The complete task definition as Task Scheduler XML.
    pub xml: String,
    /// When the backup was taken (local time, `YYYY-MM-DD HH:MM:SS`).
    pub saved_at: String,
}

impl TaskBackup {
    /// Reads a backup file written by the startup manager.
    pub fn load(file: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(file).ctx(format!("reading task backup {}", file.display()))?;
        let backup: TaskBackup = serde_json::from_str(&text)?;
        split_task_path(&backup.path)?;
        if !backup.xml.contains("<Task") {
            return Err(Error::Invalid(format!(
                "{} does not contain a task definition",
                file.display()
            )));
        }
        Ok(backup)
    }

    /// Writes the backup into `dir` and returns the file path.
    fn save(&self, dir: &Path) -> Result<PathBuf> {
        paths::ensure_dir(dir)?;
        let safe: String = self
            .path
            .trim_start_matches('\\')
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let mut file = dir.join(format!("{stamp}-{safe}.task.json"));
        let mut n = 1;
        while file.exists() {
            n += 1;
            file = dir.join(format!("{stamp}-{safe}-{n}.task.json"));
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&file, text).ctx(format!("writing task backup {}", file.display()))?;
        Ok(file)
    }
}

/// Deletes a task. When `backup_dir` is given, the full definition is saved
/// there first and the backup file is returned. A task that is already gone
/// is not an error.
pub(crate) fn delete(path: &str, backup_dir: Option<&Path>) -> Result<Option<PathBuf>> {
    let (folder_path, name) = split_task_path(path)?;
    if path.to_lowercase().starts_with("\\microsoft\\windows\\") {
        return Err(Error::Other(format!(
            "{path} is part of Windows and cannot be deleted; disable it instead"
        )));
    }
    let _com = ComGuard::new();
    let service = connect()?;
    // SAFETY: COM is initialised by `_com`.
    unsafe {
        let task = match open_task(&service, path) {
            Ok(task) => task,
            Err(Error::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let saved = match backup_dir {
            Some(dir) => {
                let xml = task
                    .Xml()
                    .map_err(|e| win_err(e, &format!("read the scheduled task {path}")))?
                    .to_string();
                let backup = TaskBackup {
                    path: path.to_owned(),
                    xml,
                    saved_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                };
                Some(backup.save(dir)?)
            }
            None => None,
        };
        let folder = service
            .GetFolder(&BSTR::from(folder_path.as_str()))
            .map_err(|e| win_err(e, &format!("open the task folder {folder_path}")))?;
        folder
            .DeleteTask(&BSTR::from(name.as_str()), 0)
            .map_err(|e| win_err(e, &format!("delete the scheduled task {path}")))?;
        Ok(saved)
    }
}

/// Re-creates a task from a backup file written when it was deleted.
///
/// Fails when a task with the same path already exists. Restoring tasks that
/// run for other accounts or with highest privileges needs administrator
/// rights.
pub fn restore_task(backup_file: &Path) -> Result<()> {
    let backup = TaskBackup::load(backup_file)?;
    // The scheduler's XML declares UTF-16, so the file must be UTF-16 too.
    let temp = std::env::temp_dir().join(format!(
        "zest-task-restore-{}-{}.xml",
        std::process::id(),
        crate::util::filetime_now()
    ));
    let mut bytes = vec![0xFF, 0xFE];
    for unit in backup.xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&temp, bytes).ctx("writing the task definition")?;
    let result = process::run_checked(
        process::system_tool("schtasks.exe"),
        [
            std::ffi::OsStr::new("/Create"),
            std::ffi::OsStr::new("/TN"),
            std::ffi::OsStr::new(&backup.path),
            std::ffi::OsStr::new("/XML"),
            temp.as_os_str(),
        ],
    );
    let _ = std::fs::remove_file(&temp);
    match result {
        Ok(_) => Ok(()),
        Err(_) if !elevation::is_elevated() => Err(Error::NeedsElevation(format!(
            "restore the scheduled task {}",
            backup.path
        ))),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_paths() {
        assert_eq!(
            split_task_path(r"\Vendor\Sub\Updater").unwrap(),
            (r"\Vendor\Sub".to_owned(), "Updater".to_owned())
        );
        assert_eq!(
            split_task_path(r"\RootTask").unwrap(),
            ("\\".to_owned(), "RootTask".to_owned())
        );
        assert!(split_task_path("NoSlash").is_err());
        assert!(split_task_path(r"\Trailing\").is_err());
        assert!(split_task_path(r"Relative\Task").is_err());
        assert!(split_task_path("").is_err());
    }

    #[test]
    fn sddl_owner_parsing() {
        assert_eq!(sddl_owner("O:BAG:SYD:(A;;FA;;;BA)").as_deref(), Some("BA"));
        assert_eq!(
            sddl_owner("O:S-1-5-21-111-222-333-1001G:S-1-5-21-111-222-333-513").as_deref(),
            Some("S-1-5-21-111-222-333-1001")
        );
        assert_eq!(
            sddl_owner("O:S-1-5-21-111-222-333-1001").as_deref(),
            Some("S-1-5-21-111-222-333-1001")
        );
        assert_eq!(sddl_owner("O:SY").as_deref(), Some("SY"));
        assert_eq!(sddl_owner("D:(A;;FA;;;BA)"), None);
        assert_eq!(sddl_owner(""), None);
    }

    #[test]
    fn command_building() {
        assert_eq!(exec_command(r"C:\Tools\a.exe", ""), r"C:\Tools\a.exe");
        assert_eq!(
            exec_command(r"C:\Program Files\App\a.exe", "/silent"),
            r#""C:\Program Files\App\a.exe" /silent"#
        );
        assert_eq!(
            exec_command(r#""C:\Program Files\App\a.exe""#, " -x "),
            r#""C:\Program Files\App\a.exe" -x"#
        );
    }

    #[test]
    fn trigger_labels_cover_all_codes() {
        for code in -1..=13 {
            assert!(!trigger_label(code).is_empty());
        }
        assert_eq!(trigger_label(8), "At system startup");
        assert_eq!(trigger_label(9), "At log on");
    }

    #[test]
    fn item_from_raw_data() {
        let mut cache = InfoCache::default();
        let cmd = paths::system32_dir().join("cmd.exe");
        let item = make_item(
            RawTask {
                name: "Updater".to_owned(),
                path: r"\Vendor\Updater".to_owned(),
                enabled: false,
                state: 1,
                last_run: 45_000.5,
                next_run: 0.0,
                last_result: Some(0),
                author: Some("Vendor Inc.".to_owned()),
                trigger_types: vec![9, 2, 2],
                exec: Some((cmd.display().to_string(), "/c rem".to_owned())),
                action_count: 1,
                user_id: Some("SYSTEM".to_owned()),
                highest_privileges: true,
                owner: Some("BA".to_owned()),
                ..RawTask::default()
            },
            &mut cache,
        );
        assert_eq!(item.id, r"task|\Vendor\Updater");
        assert_eq!(item.kind, StartupKind::ScheduledTask);
        assert!(!item.enabled);
        assert!(item.requires_admin && item.can_toggle && item.can_delete);
        assert_eq!(item.location, r"\Vendor");
        assert!(!item.target_missing);
        let StartupDetails::Task(task) = &item.details else {
            panic!("wrong details");
        };
        assert_eq!(task.triggers, ["At log on", "Daily"]);
        assert!(task.runs_at_startup);
        assert_eq!(task.state, TaskState::Disabled);
        assert!(task.last_run.is_some());
        assert!(task.next_run.is_none());
        assert!(task.highest_privileges);

        // A Windows task with a COM handler: Microsoft, not deletable.
        let system = make_item(
            RawTask {
                name: "Maintenance".to_owned(),
                path: r"\Microsoft\Windows\Example\Maintenance".to_owned(),
                enabled: true,
                state: 3,
                com_handler: Some("{00000000-0000-0000-0000-000000000000}".to_owned()),
                action_count: 1,
                ..RawTask::default()
            },
            &mut cache,
        );
        assert!(system.is_microsoft);
        assert!(!system.can_delete);
        assert!(system.command.starts_with("COM handler {"));
        assert!(system.target.is_none() && !system.target_missing);

        // A task whose program is gone is an orphan.
        let drive = paths::system_drive();
        let orphan = make_item(
            RawTask {
                name: "Old".to_owned(),
                path: r"\Old".to_owned(),
                enabled: true,
                state: 3,
                exec: Some((
                    format!(r"{}Zest No Such Dir\updater.exe", drive.display()),
                    String::new(),
                )),
                owner: current_user_sid().map(str::to_owned),
                ..RawTask::default()
            },
            &mut cache,
        );
        assert!(orphan.target_missing);
        assert_eq!(orphan.location, "\\");
        if current_user_sid().is_some() {
            assert!(!orphan.requires_admin);
            assert_eq!(orphan.scope, Scope::User);
        }
    }

    #[test]
    fn enumerates_real_tasks_read_only() {
        let items = list(&mut InfoCache::default()).expect("Task Scheduler should be reachable");
        assert!(!items.is_empty(), "Windows always has scheduled tasks");
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            assert!(item.id.starts_with("task|\\"), "{}", item.id);
            assert!(ids.insert(item.id.clone()), "duplicate id {}", item.id);
            let StartupDetails::Task(task) = &item.details else {
                panic!("wrong details");
            };
            assert!(task.path.ends_with(&item.name));
        }
        assert!(items.iter().any(|i| i.is_microsoft));
    }

    #[test]
    fn unknown_task_is_reported_and_nothing_changes() {
        let missing = r"\Zest Tests\No Such Task";
        assert!(matches!(
            set_enabled(missing, false),
            Err(Error::NotFound(_))
        ));
        // Deleting what does not exist is a harmless no-op.
        assert_eq!(delete(missing, None).unwrap(), None);
        assert!(matches!(
            set_enabled("not a path", true),
            Err(Error::Invalid(_))
        ));
        // Windows' own tasks are refused before the scheduler is even asked.
        assert!(matches!(
            delete(r"\Microsoft\Windows\Zest Tests\Nothing", None),
            Err(Error::Other(_))
        ));
    }

    #[test]
    fn backup_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let backup = TaskBackup {
            path: r"\Vendor\My Updater".to_owned(),
            xml: "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\"></Task>"
                .to_owned(),
            saved_at: "2026-01-01 00:00:00".to_owned(),
        };
        let file = backup.save(tmp.path()).unwrap();
        assert!(
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("Vendor-My-Updater.task.json")
        );
        assert_eq!(TaskBackup::load(&file).unwrap(), backup);
        // A second backup in the same second gets its own file.
        let second = backup.save(tmp.path()).unwrap();
        assert_ne!(file, second);

        let bogus = tmp.path().join("bogus.task.json");
        std::fs::write(&bogus, r#"{"path":"\\X","xml":"not a task","saved_at":""}"#).unwrap();
        assert!(matches!(TaskBackup::load(&bogus), Err(Error::Invalid(_))));
        std::fs::write(&bogus, "not json").unwrap();
        assert!(TaskBackup::load(&bogus).is_err());
        assert!(restore_task(&tmp.path().join("missing.task.json")).is_err());
    }

    #[test]
    fn resource_text_resolution() {
        assert_eq!(
            resolve_resource_text("Plain author".to_owned()),
            "Plain author"
        );
        let resolved =
            resolve_resource_text(r"$(@%SystemRoot%\system32\shell32.dll,-8506)".to_owned());
        assert!(!resolved.starts_with("$("), "{resolved}");
        // Unresolvable references are kept as they are.
        let kept = resolve_resource_text(r"$(@C:\no\such.dll,-1)".to_owned());
        assert_eq!(kept, r"$(@C:\no\such.dll,-1)");
    }
}
