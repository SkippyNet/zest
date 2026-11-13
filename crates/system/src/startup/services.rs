//! Windows services, through the Service Control Manager.

use super::{ServiceEntry, StartupDetails, StartupItem, StartupKind, resolve_command};
use crate::Scope;
use crate::util::{InfoCache, from_wide_ptr, io_err, is_microsoft, wide, win_err};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW,
    ENUM_SERVICE_TYPE, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
    QueryServiceConfig2W, QueryServiceConfigW, SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT,
    SC_MANAGER_ENUMERATE_SERVICE, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR, SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG, SERVICE_START_TYPE, SERVICE_STATE_ALL,
    SERVICE_WIN32,
};
use windows::core::PCWSTR;
use zest_core::reg::{RegPath, RegRoot};
use zest_core::{Error, Result, brand, elevation, paths};

const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";
/// Service type bit marking a per-user instance of a template service.
const SERVICE_USERSERVICE_INSTANCE: u32 = 0x80;
/// HRESULT for `ERROR_SERVICE_DOES_NOT_EXIST`.
const HR_SERVICE_DOES_NOT_EXIST: u32 = 0x8007_0424;
/// HRESULT for `ERROR_MORE_DATA`.
const HR_MORE_DATA: u32 = 0x8007_00EA;
/// HRESULT for `ERROR_INSUFFICIENT_BUFFER`.
const HR_INSUFFICIENT_BUFFER: u32 = 0x8007_007A;

/// Services Windows cannot boot, sign in or stay secure without. They are
/// listed but can never be toggled.
const CRITICAL_SERVICES: &[&str] = &[
    "appinfo",
    "appxsvc",
    "audioendpointbuilder",
    "audiosrv",
    "bfe",
    "brokerinfrastructure",
    "coremessagingregistrar",
    "cryptsvc",
    "dcomlaunch",
    "dhcp",
    "dnscache",
    "eventlog",
    "eventsystem",
    "gpsvc",
    "keyiso",
    "lanmanworkstation",
    "lsm",
    "mpssvc",
    "msiserver",
    "netlogon",
    "netprofm",
    "nsi",
    "plugplay",
    "power",
    "profsvc",
    "rpceptmapper",
    "rpcss",
    "samss",
    "schedule",
    "sense",
    "sgrmbroker",
    "staterepository",
    "systemeventsbroker",
    "themes",
    "timebrokersvc",
    "trustedinstaller",
    "usermanager",
    "vaultsvc",
    "wdnissvc",
    "wincredmgr",
    "windefend",
    "winmgmt",
    "wlansvc",
    "wscsvc",
];

/// How a service is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceStartType {
    /// Started by the boot loader (drivers).
    Boot,
    /// Started during kernel initialisation (drivers).
    System,
    /// Started automatically when Windows starts.
    Automatic,
    /// Started automatically shortly after Windows has started.
    AutomaticDelayed,
    /// Started on demand.
    Manual,
    /// Cannot be started.
    Disabled,
}

impl ServiceStartType {
    /// Short label, e.g. `Automatic (delayed)`.
    pub fn label(self) -> &'static str {
        match self {
            ServiceStartType::Boot => "Boot",
            ServiceStartType::System => "System",
            ServiceStartType::Automatic => "Automatic",
            ServiceStartType::AutomaticDelayed => "Automatic (delayed)",
            ServiceStartType::Manual => "Manual",
            ServiceStartType::Disabled => "Disabled",
        }
    }

    fn from_parts(start: u32, delayed: bool) -> Self {
        match start {
            0 => ServiceStartType::Boot,
            1 => ServiceStartType::System,
            2 if delayed => ServiceStartType::AutomaticDelayed,
            2 => ServiceStartType::Automatic,
            4 => ServiceStartType::Disabled,
            _ => ServiceStartType::Manual,
        }
    }

    /// The Service Control Manager start code.
    fn start_code(self) -> u32 {
        match self {
            ServiceStartType::Boot => 0,
            ServiceStartType::System => 1,
            ServiceStartType::Automatic | ServiceStartType::AutomaticDelayed => 2,
            ServiceStartType::Manual => 3,
            ServiceStartType::Disabled => 4,
        }
    }

    /// Encoding used to remember a start type: start code, plus bit 8 for
    /// "delayed".
    fn to_stored(self) -> u32 {
        let delayed = u32::from(self == ServiceStartType::AutomaticDelayed) << 8;
        self.start_code() | delayed
    }

    fn from_stored(value: u32) -> Option<Self> {
        let start = value & 0xFF;
        (start <= 4).then(|| Self::from_parts(start, value & 0x100 != 0))
    }
}

/// What a service is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceState {
    /// Not running.
    Stopped,
    /// Starting up.
    StartPending,
    /// Shutting down.
    StopPending,
    /// Running.
    Running,
    /// Resuming from pause.
    ContinuePending,
    /// Pausing.
    PausePending,
    /// Paused.
    Paused,
    /// The state could not be determined.
    Unknown,
}

impl ServiceState {
    fn from_code(code: u32) -> Self {
        match code {
            1 => ServiceState::Stopped,
            2 => ServiceState::StartPending,
            3 => ServiceState::StopPending,
            4 => ServiceState::Running,
            5 => ServiceState::ContinuePending,
            6 => ServiceState::PausePending,
            7 => ServiceState::Paused,
            _ => ServiceState::Unknown,
        }
    }

    /// Short label, e.g. `Running`.
    pub fn label(self) -> &'static str {
        match self {
            ServiceState::Stopped => "Stopped",
            ServiceState::StartPending => "Starting",
            ServiceState::StopPending => "Stopping",
            ServiceState::Running => "Running",
            ServiceState::ContinuePending => "Resuming",
            ServiceState::PausePending => "Pausing",
            ServiceState::Paused => "Paused",
            ServiceState::Unknown => "Unknown",
        }
    }
}

/// Owning wrapper that closes a service handle on drop.
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenSCManagerW / OpenServiceW and is
        // closed exactly once.
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

/// A zeroed buffer of at least `bytes` bytes with 8-byte alignment, so the
/// structures the Service Control Manager writes into it are properly aligned.
fn aligned_buffer(bytes: usize) -> Vec<u64> {
    vec![0u64; bytes.div_ceil(8).max(1)]
}

fn as_bytes_mut(buffer: &mut [u64]) -> &mut [u8] {
    // SAFETY: a u64 slice is valid as a byte slice of eight times the length.
    unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), buffer.len() * 8) }
}

fn open_manager(access: u32) -> Result<ScHandle> {
    // SAFETY: null names select the local machine and the active database.
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) }
        .map(ScHandle)
        .map_err(|e| win_err(e, "open the service manager"))
}

/// Plain data read from the Service Control Manager for one service.
#[derive(Clone, Debug, Default)]
struct RawService {
    name: String,
    display_name: String,
    service_type: u32,
    state: u32,
    process_id: u32,
    start: Option<u32>,
    binary_path: String,
    account: Option<String>,
    description: Option<String>,
    delayed: bool,
    /// True when `delayed` was answered by the Service Control Manager
    /// itself rather than read from the registry.
    delayed_known: bool,
    service_dll: Option<PathBuf>,
}

/// Reads start type, binary path, account and description of one service.
///
/// # Safety
/// `manager` must be a valid Service Control Manager handle.
unsafe fn read_config(manager: SC_HANDLE, raw: &mut RawService) {
    let name = wide(&raw.name);
    // SAFETY: all buffers are valid for the sizes passed; strings returned by
    // the API point into the buffers, which outlive their use.
    unsafe {
        let Ok(service) = OpenServiceW(manager, PCWSTR(name.as_ptr()), SERVICE_QUERY_CONFIG) else {
            return;
        };
        let service = ScHandle(service);

        let mut needed = 0u32;
        let _ = QueryServiceConfigW(service.0, None, 0, &mut needed);
        if needed > 0 {
            let mut buffer = aligned_buffer(needed as usize);
            let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
            if QueryServiceConfigW(service.0, Some(config), needed, &mut needed).is_ok() {
                let config = &*config;
                raw.start = Some(config.dwStartType.0);
                raw.binary_path = from_wide_ptr(config.lpBinaryPathName.0);
                let account = from_wide_ptr(config.lpServiceStartName.0);
                raw.account = (!account.is_empty()).then_some(account);
            }
        }

        let mut buffer = aligned_buffer(8192);
        let mut needed = 0u32;
        let mut result = QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some(as_bytes_mut(&mut buffer)),
            &mut needed,
        );
        if let Err(e) = &result
            && e.code().0 as u32 == HR_INSUFFICIENT_BUFFER
        {
            buffer = aligned_buffer(needed as usize);
            result = QueryServiceConfig2W(
                service.0,
                SERVICE_CONFIG_DESCRIPTION,
                Some(as_bytes_mut(&mut buffer)),
                &mut needed,
            );
        }
        if result.is_ok() {
            let description = &*buffer.as_ptr().cast::<SERVICE_DESCRIPTIONW>();
            let text = from_wide_ptr(description.lpDescription.0);
            let text = text.trim();
            raw.description = (!text.is_empty()).then(|| text.to_owned());
        }

        // Per-user service instances inherit "delayed" from their template,
        // which only the Service Control Manager knows about.
        let mut small = aligned_buffer(size_of::<SERVICE_DELAYED_AUTO_START_INFO>());
        let mut needed = 0u32;
        if QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            Some(as_bytes_mut(&mut small)),
            &mut needed,
        )
        .is_ok()
        {
            let info = &*small.as_ptr().cast::<SERVICE_DELAYED_AUTO_START_INFO>();
            raw.delayed = info.fDelayedAutostart.as_bool();
            raw.delayed_known = true;
        }
    }
}

fn read_registry_extras(raw: &mut RawService) {
    let key = RegPath::hklm(format!("{SERVICES_KEY}\\{}", raw.name));
    if !raw.delayed_known {
        raw.delayed = key.read_u32("DelayedAutostart") == Some(1);
    }
    if raw.start.is_none() {
        raw.start = key.read_u32("Start");
    }
    if raw.binary_path.is_empty()
        && let Some(path) = key.read_string("ImagePath")
    {
        raw.binary_path = path;
    }
    raw.service_dll = key
        .child("Parameters")
        .read_string("ServiceDll")
        .or_else(|| key.read_string("ServiceDll"))
        .map(|dll| PathBuf::from(paths::expand_env(dll.trim().trim_matches('"'))))
        .filter(|p| p.is_absolute());
}

fn enumerate() -> Result<Vec<RawService>> {
    let manager = open_manager(SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE)?;
    let mut buffer = aligned_buffer(64 * 1024);
    let mut services = Vec::new();
    // SAFETY: the buffer is 8-byte aligned and valid for the size passed; the
    // returned records and their strings point into it and are copied out
    // before it is reused.
    unsafe {
        let mut resume = 0u32;
        loop {
            let mut needed = 0u32;
            let mut returned = 0u32;
            let result = EnumServicesStatusExW(
                manager.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                Some(as_bytes_mut(&mut buffer)),
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            );
            let more = match &result {
                Ok(()) => false,
                Err(e) if e.code().0 as u32 == HR_MORE_DATA => true,
                Err(e) => return Err(win_err(e.clone(), "list services")),
            };
            let records = std::slice::from_raw_parts(
                buffer.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                returned as usize,
            );
            for record in records {
                services.push(RawService {
                    name: from_wide_ptr(record.lpServiceName.0),
                    display_name: from_wide_ptr(record.lpDisplayName.0),
                    service_type: record.ServiceStatusProcess.dwServiceType.0,
                    state: record.ServiceStatusProcess.dwCurrentState.0,
                    process_id: record.ServiceStatusProcess.dwProcessId,
                    ..RawService::default()
                });
            }
            if !more {
                break;
            }
            if returned == 0 {
                // Not even one record fitted: grow and try again.
                buffer = aligned_buffer(buffer.len() * 8 + needed as usize);
            }
        }
        for raw in &mut services {
            read_config(manager.0, raw);
        }
    }
    for raw in &mut services {
        read_registry_extras(raw);
    }
    Ok(services)
}

fn make_item(raw: RawService, cache: &mut InfoCache) -> StartupItem {
    let mut resolved = resolve_command(&raw.binary_path, cache);
    // For services hosted in a shared process the DLL is the real program.
    if let Some(dll) = &raw.service_dll
        && dll.is_file()
    {
        let (publisher, description) = cache.publisher_and_description(dll);
        resolved.target = Some(dll.clone());
        resolved.target_missing = false;
        resolved.publisher = publisher.or(resolved.publisher);
        resolved.description = description.or(resolved.description);
    }

    let start_type = ServiceStartType::from_parts(raw.start.unwrap_or(3), raw.delayed);
    let is_ms = is_microsoft(resolved.publisher.as_deref());
    let is_instance = raw.service_type & SERVICE_USERSERVICE_INSTANCE != 0;
    let is_critical = is_ms && CRITICAL_SERVICES.contains(&raw.name.to_lowercase().as_str());
    let display_name = if raw.display_name.trim().is_empty() {
        raw.name.clone()
    } else {
        raw.display_name.trim().to_owned()
    };

    StartupItem {
        id: format!("service|{}", raw.name),
        kind: StartupKind::Service,
        name: display_name.clone(),
        command: raw.binary_path,
        target: resolved.target,
        target_missing: resolved.target_missing,
        publisher: resolved.publisher,
        description: raw.description.or(resolved.description),
        location: format!("HKLM\\{SERVICES_KEY}\\{}", raw.name),
        scope: Scope::Machine,
        enabled: start_type != ServiceStartType::Disabled,
        requires_admin: true,
        toggle_requires_admin: true,
        is_microsoft: is_ms,
        // Per-user instances are recreated from their template at every
        // sign-in, so toggling one would not last.
        can_toggle: !is_critical && !is_instance,
        can_delete: false,
        details: StartupDetails::Service(ServiceEntry {
            service_name: raw.name,
            display_name,
            start_type,
            state: ServiceState::from_code(raw.state),
            account: raw.account,
            service_dll: raw.service_dll,
            process_id: (raw.process_id != 0).then_some(raw.process_id),
            is_critical,
        }),
    }
}

/// Lists every Win32 service.
pub(crate) fn list(cache: &mut InfoCache) -> Result<Vec<StartupItem>> {
    let mut items: Vec<StartupItem> = enumerate()?
        .into_iter()
        .map(|raw| make_item(raw, cache))
        .collect();
    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(items)
}

/// The registry key where the start type of services disabled through
/// Zest is remembered, so enabling restores exactly what was there.
pub(crate) fn state_key() -> RegPath {
    RegPath::hklm(format!(
        "SOFTWARE\\{}\\ServiceStartTypes",
        brand::DATA_DIR_NAME
    ))
}

fn remember(state_key: &RegPath, service: &str, start: ServiceStartType) -> Result<()> {
    let action = "remember the service start type";
    let (key, _) = state_key
        .root
        .key()
        .create_subkey(&state_key.path)
        .map_err(|e| io_err(e, action))?;
    key.set_value(service, &start.to_stored())
        .map_err(|e| io_err(e, action))
}

fn recall(state_key: &RegPath, service: &str) -> Option<ServiceStartType> {
    state_key
        .read_u32(service)
        .and_then(ServiceStartType::from_stored)
        .filter(|start| *start != ServiceStartType::Disabled)
}

fn forget(state_key: &RegPath, service: &str) {
    if let Ok(key) = state_key.open_write() {
        let _ = key.delete_value(service);
    }
}

/// Reads the current start type of an open service.
///
/// # Safety
/// `service` must be a valid handle opened with `SERVICE_QUERY_CONFIG`.
unsafe fn current_start_type(service: SC_HANDLE, name: &str) -> Result<ServiceStartType> {
    // SAFETY: the buffer is aligned and sized as the API asks.
    unsafe {
        let mut needed = 0u32;
        let _ = QueryServiceConfigW(service, None, 0, &mut needed);
        let mut buffer = aligned_buffer(needed.max(64) as usize);
        let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
        QueryServiceConfigW(
            service,
            Some(config),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
        .map_err(|e| win_err(e, &format!("read the service \"{name}\"")))?;
        let start = (*config).dwStartType.0;
        let delayed = RegPath::new(RegRoot::Hklm, format!("{SERVICES_KEY}\\{name}"))
            .read_u32("DelayedAutostart")
            == Some(1);
        Ok(ServiceStartType::from_parts(start, delayed))
    }
}

/// Applies a start type to an open service.
///
/// # Safety
/// `service` must be a valid handle opened with `SERVICE_CHANGE_CONFIG`.
unsafe fn apply_start_type(service: SC_HANDLE, name: &str, start: ServiceStartType) -> Result<()> {
    let action = format!("change the service \"{name}\"");
    // SAFETY: null strings and SERVICE_NO_CHANGE leave everything but the
    // start type untouched; `info` outlives the second call.
    unsafe {
        ChangeServiceConfigW(
            service,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(start.start_code()),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
        .map_err(|e| win_err(e, &action))?;
        if matches!(
            start,
            ServiceStartType::Automatic | ServiceStartType::AutomaticDelayed
        ) {
            let info = SERVICE_DELAYED_AUTO_START_INFO {
                fDelayedAutostart: (start == ServiceStartType::AutomaticDelayed).into(),
            };
            ChangeServiceConfig2W(
                service,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                Some(std::ptr::from_ref(&info).cast()),
            )
            .map_err(|e| win_err(e, &action))?;
        }
    }
    Ok(())
}

/// Enables or disables a service by changing its start type.
///
/// Disabling remembers the current start type under `state_key`; enabling
/// restores it (falling back to "Manual" when nothing was remembered). A
/// running service is not stopped.
pub(crate) fn set_enabled(name: &str, enabled: bool, state_key: &RegPath) -> Result<()> {
    if CRITICAL_SERVICES.contains(&name.to_lowercase().as_str()) {
        return Err(Error::Other(format!(
            "the service \"{name}\" is required by Windows and cannot be changed"
        )));
    }
    if !elevation::is_elevated() {
        return Err(Error::NeedsElevation(format!(
            "change the service \"{name}\""
        )));
    }
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let wide_name = wide(name);
    // SAFETY: `wide_name` is NUL terminated; handles are closed by `ScHandle`.
    unsafe {
        let service = OpenServiceW(
            manager.0,
            PCWSTR(wide_name.as_ptr()),
            SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG,
        )
        .map(ScHandle)
        .map_err(|e| {
            if e.code().0 as u32 == HR_SERVICE_DOES_NOT_EXIST {
                Error::NotFound(format!("service {name}"))
            } else {
                win_err(e, &format!("change the service \"{name}\""))
            }
        })?;

        let current = current_start_type(service.0, name)?;
        if enabled {
            if current != ServiceStartType::Disabled {
                return Ok(());
            }
            let target = recall(state_key, name).unwrap_or(ServiceStartType::Manual);
            apply_start_type(service.0, name, target)?;
            forget(state_key, name);
        } else {
            if current == ServiceStartType::Disabled {
                return Ok(());
            }
            remember(state_key, name, current)?;
            apply_start_type(service.0, name, ServiceStartType::Disabled)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    #[test]
    fn start_type_encoding_round_trips() {
        for start in [
            ServiceStartType::Boot,
            ServiceStartType::System,
            ServiceStartType::Automatic,
            ServiceStartType::AutomaticDelayed,
            ServiceStartType::Manual,
            ServiceStartType::Disabled,
        ] {
            assert_eq!(
                ServiceStartType::from_stored(start.to_stored()),
                Some(start)
            );
            assert!(!start.label().is_empty());
        }
        assert_eq!(
            ServiceStartType::from_stored(0x102),
            Some(ServiceStartType::AutomaticDelayed)
        );
        assert_eq!(ServiceStartType::from_stored(99), None);
        assert_eq!(
            ServiceStartType::from_parts(2, true),
            ServiceStartType::AutomaticDelayed
        );
        // "Delayed" only has a meaning for automatic services.
        assert_eq!(
            ServiceStartType::from_parts(3, true),
            ServiceStartType::Manual
        );
        assert_eq!(ServiceStartType::Automatic.start_code(), 2);
        assert_eq!(ServiceStartType::AutomaticDelayed.start_code(), 2);
    }

    #[test]
    fn state_codes() {
        assert_eq!(ServiceState::from_code(1), ServiceState::Stopped);
        assert_eq!(ServiceState::from_code(4), ServiceState::Running);
        assert_eq!(ServiceState::from_code(42), ServiceState::Unknown);
        assert_eq!(ServiceState::Running.label(), "Running");
    }

    #[test]
    fn previous_start_type_is_remembered() {
        let scratch = Scratch::new("services");
        let state = scratch.0.child("ServiceStartTypes");
        assert_eq!(recall(&state, "Updater"), None);

        remember(&state, "Updater", ServiceStartType::AutomaticDelayed).unwrap();
        remember(&state, "Helper", ServiceStartType::Manual).unwrap();
        assert_eq!(
            recall(&state, "Updater"),
            Some(ServiceStartType::AutomaticDelayed)
        );
        assert_eq!(recall(&state, "Helper"), Some(ServiceStartType::Manual));

        // "Disabled" is never a sensible thing to restore to.
        remember(&state, "Odd", ServiceStartType::Disabled).unwrap();
        assert_eq!(recall(&state, "Odd"), None);

        forget(&state, "Updater");
        assert_eq!(recall(&state, "Updater"), None);
        assert_eq!(recall(&state, "Helper"), Some(ServiceStartType::Manual));
        // Forgetting twice, or in a key that does not exist, is harmless.
        forget(&state, "Updater");
        forget(&scratch.0.child("Nope").child("Deeper"), "Updater");
    }

    #[test]
    fn real_state_key_is_app_owned() {
        let key = state_key();
        assert_eq!(key.root, RegRoot::Hklm);
        assert!(key.path.contains(brand::DATA_DIR_NAME));
        assert!(key.path.ends_with("ServiceStartTypes"));
    }

    #[test]
    fn item_from_raw_data() {
        let mut cache = InfoCache::default();
        let svchost = paths::system32_dir().join("svchost.exe");
        let item = make_item(
            RawService {
                name: "ExampleSvc".to_owned(),
                display_name: " Example Service ".to_owned(),
                service_type: 0x20,
                state: 4,
                process_id: 1234,
                start: Some(2),
                binary_path: format!("{} -k netsvcs -p", svchost.display()),
                account: Some("LocalSystem".to_owned()),
                description: Some("Does example things.".to_owned()),
                delayed: true,
                delayed_known: true,
                service_dll: None,
            },
            &mut cache,
        );
        assert_eq!(item.id, "service|ExampleSvc");
        assert_eq!(item.name, "Example Service");
        assert!(item.enabled && item.requires_admin && item.can_toggle);
        assert!(!item.can_delete, "services are never deletable");
        assert!(item.is_microsoft);
        assert!(!item.target_missing);
        let StartupDetails::Service(service) = &item.details else {
            panic!("wrong details");
        };
        assert_eq!(service.start_type, ServiceStartType::AutomaticDelayed);
        assert_eq!(service.state, ServiceState::Running);
        assert_eq!(service.process_id, Some(1234));
        assert!(!service.is_critical);

        let critical = make_item(
            RawService {
                name: "RpcSs".to_owned(),
                display_name: "Remote Procedure Call (RPC)".to_owned(),
                state: 4,
                start: Some(2),
                binary_path: format!("{} -k rpcss -p", svchost.display()),
                ..RawService::default()
            },
            &mut cache,
        );
        assert!(!critical.can_toggle);
        assert!(matches!(&critical.details, StartupDetails::Service(s) if s.is_critical));

        let drive = paths::system_drive();
        let orphan = make_item(
            RawService {
                name: "GoneSvc".to_owned(),
                state: 1,
                start: Some(4),
                binary_path: format!("\"{}Zest No Such Dir\\svc.exe\" --run", drive.display()),
                ..RawService::default()
            },
            &mut cache,
        );
        assert_eq!(orphan.name, "GoneSvc");
        assert!(!orphan.enabled);
        assert!(orphan.target_missing);
        assert!(!orphan.is_microsoft);

        let instance = make_item(
            RawService {
                name: "ExampleUserSvc_1a2b3".to_owned(),
                service_type: 0xE0,
                state: 4,
                start: Some(3),
                binary_path: format!("{} -k UnistackSvcGroup", svchost.display()),
                ..RawService::default()
            },
            &mut cache,
        );
        assert!(!instance.can_toggle);
    }

    #[test]
    fn enumerates_real_services_read_only() {
        let items = list(&mut InfoCache::default()).expect("services should be listable");
        assert!(items.len() > 50, "only {} services found", items.len());
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            assert!(ids.insert(item.id.clone()), "duplicate id {}", item.id);
            assert_eq!(item.kind, StartupKind::Service);
            assert!(!item.can_delete);
        }
        let rpc = items
            .iter()
            .find(|i| i.id.eq_ignore_ascii_case("service|RpcSs"))
            .expect("the RPC service always exists");
        assert!(rpc.enabled && rpc.is_microsoft && !rpc.can_toggle);
        assert!(matches!(
            &rpc.details,
            StartupDetails::Service(s) if s.state == ServiceState::Running && s.is_critical
        ));
    }

    #[test]
    fn changes_are_refused_without_touching_real_services() {
        let scratch = Scratch::new("services-refuse");
        let state = scratch.0.child("ServiceStartTypes");
        // A critical service is refused before anything else is looked at.
        assert!(matches!(
            set_enabled("RpcSs", false, &state),
            Err(Error::Other(_))
        ));
        // A service that does not exist: elevation is asked for first, and
        // even when elevated nothing can change.
        let result = set_enabled("ZestNoSuchService", false, &state);
        if elevation::is_elevated() {
            assert!(matches!(result, Err(Error::NotFound(_))), "{result:?}");
        } else {
            assert!(
                matches!(result, Err(Error::NeedsElevation(_))),
                "{result:?}"
            );
        }
        assert_eq!(recall(&state, "ZestNoSuchService"), None);
    }
}
