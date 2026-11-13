//! System Restore points: list, create and delete.
//!
//! Listing, creating and deleting restore points requires administrator
//! rights; without them these functions return [`Error::NeedsElevation`].
//! Only [`is_protection_enabled`] works for every user.

use crate::com::ComGuard;
use crate::util::{wide, win_err};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoSetProxyBlanket, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_PKT_PRIVACY, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE, WbemLocator,
};
use windows::core::{BSTR, PCSTR, PCWSTR};
use zest_core::reg::RegPath;
use zest_core::{Error, Result, elevation, paths};

/// `RPC_C_AUTHN_WINNT`
const RPC_C_AUTHN_WINNT: u32 = 10;
/// `RPC_C_AUTHZ_NONE`
const RPC_C_AUTHZ_NONE: u32 = 0;
/// WMI: access denied.
const WBEM_E_ACCESS_DENIED: u32 = 0x8004_1003;
/// Win32: access denied, as an HRESULT.
const E_ACCESSDENIED: u32 = 0x8007_0005;
/// `BEGIN_SYSTEM_CHANGE`: the event type that creates a restore point.
const BEGIN_SYSTEM_CHANGE: i32 = 100;
/// Win32 `ERROR_ACCESS_DENIED`.
const ERROR_ACCESS_DENIED: u32 = 5;
/// Win32 `ERROR_INVALID_DATA`: no restore point has that sequence number.
const ERROR_INVALID_DATA: u32 = 13;
/// Win32 `ERROR_SERVICE_DISABLED`: System Restore is turned off.
const ERROR_SERVICE_DISABLED: u32 = 1058;
/// Longest description Windows stores for a restore point.
const MAX_DESCRIPTION_CHARS: usize = 255;

const RESTORE_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\SystemRestore";
const RESTORE_POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\SystemRestore";

/// Why a restore point was created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RestorePointKind {
    /// Before a program was installed.
    ApplicationInstall,
    /// Before a program was removed.
    ApplicationUninstall,
    /// A system restore was performed.
    Restore,
    /// An automatic system checkpoint.
    Checkpoint,
    /// Before a device driver was installed.
    DeviceDriverInstall,
    /// Before settings were changed (also used for manually created points).
    ModifySettings,
    /// An operation was cancelled.
    CancelledOperation,
    /// A backup or recovery operation.
    Backup,
    /// Created manually.
    Manual,
    /// Before a Windows update.
    WindowsUpdate,
    /// Any other type, with its numeric code.
    Other(u32),
}

impl RestorePointKind {
    fn from_code(code: u32) -> Self {
        match code {
            0 => RestorePointKind::ApplicationInstall,
            1 => RestorePointKind::ApplicationUninstall,
            6 => RestorePointKind::Restore,
            7 => RestorePointKind::Checkpoint,
            10 => RestorePointKind::DeviceDriverInstall,
            12 => RestorePointKind::ModifySettings,
            13 => RestorePointKind::CancelledOperation,
            14 | 15 => RestorePointKind::Backup,
            16 => RestorePointKind::Manual,
            17 | 18 => RestorePointKind::WindowsUpdate,
            other => RestorePointKind::Other(other),
        }
    }

    /// Short label, e.g. `Program installation`.
    pub fn label(self) -> &'static str {
        match self {
            RestorePointKind::ApplicationInstall => "Program installation",
            RestorePointKind::ApplicationUninstall => "Program removal",
            RestorePointKind::Restore => "System restore",
            RestorePointKind::Checkpoint => "System checkpoint",
            RestorePointKind::DeviceDriverInstall => "Driver installation",
            RestorePointKind::ModifySettings => "Settings change",
            RestorePointKind::CancelledOperation => "Cancelled operation",
            RestorePointKind::Backup => "Backup",
            RestorePointKind::Manual => "Manual",
            RestorePointKind::WindowsUpdate => "Windows Update",
            RestorePointKind::Other(_) => "Other",
        }
    }
}

/// One System Restore point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePoint {
    /// The number that identifies the restore point.
    pub sequence_number: u32,
    /// Description given when it was created.
    pub description: String,
    /// When it was created (local time).
    pub created: Option<NaiveDateTime>,
    /// Why it was created.
    pub kind: RestorePointKind,
}

/// What happened when a restore point was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreateOutcome {
    /// A new restore point with this sequence number now exists.
    Created(u32),
    /// Windows accepted the request but made no new restore point. By
    /// default it creates at most one per 24 hours; the most recent one
    /// already covers the current state.
    SkippedByWindows,
}

fn needs_elevation(action: &str) -> Error {
    Error::NeedsElevation(action.to_owned())
}

fn map_wmi(e: windows::core::Error, action: &str) -> Error {
    match e.code().0 as u32 {
        WBEM_E_ACCESS_DENIED | E_ACCESSDENIED => needs_elevation(action),
        _ => win_err(e, action),
    }
}

/// Whether System Restore (system protection) is switched on.
///
/// Reads the configuration only and works without administrator rights.
/// When this returns `false`, no restore points exist and none can be made.
pub fn is_protection_enabled() -> Result<bool> {
    let policy = RegPath::hklm(RESTORE_POLICY_KEY);
    if policy.read_u32("DisableSR") == Some(1) {
        return Ok(false);
    }
    let key = RegPath::hklm(RESTORE_KEY);
    if !key.exists() {
        return Err(Error::NotFound(
            "System Restore is not available on this edition of Windows".to_owned(),
        ));
    }
    // RPSessionInterval is 1 while protection is on for at least one drive.
    Ok(key.read_u32("RPSessionInterval") == Some(1))
}

/// Connects to the WMI namespace that hosts System Restore.
///
/// # Safety
/// Must be called on a thread with COM initialised.
unsafe fn connect(action: &str) -> Result<IWbemServices> {
    // SAFETY: standard WMI connection sequence on valid interfaces.
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| map_wmi(e, action))?;
        let services = locator
            .ConnectServer(
                &BSTR::from("ROOT\\DEFAULT"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )
            .map_err(|e| map_wmi(e, action))?;
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .map_err(|e| map_wmi(e, action))?;
        Ok(services)
    }
}

/// Reads a property of a WMI object as a variant.
///
/// # Safety
/// Must be called on a thread with COM initialised.
unsafe fn property(object: &IWbemClassObject, name: &str) -> Option<VARIANT> {
    let name = wide(name);
    let mut value = VARIANT::default();
    // SAFETY: `name` is NUL terminated and `value` is a valid out parameter.
    unsafe {
        object
            .Get(PCWSTR(name.as_ptr()), 0, &mut value, None, None)
            .ok()?;
    }
    Some(value)
}

/// Parses a WMI timestamp (`yyyymmddHHMMSS.mmmmmm±UUU`, offset in minutes)
/// into local time.
fn parse_wmi_time(text: &str) -> Option<NaiveDateTime> {
    let stamp = text.get(..14)?;
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S").ok()?;
    let offset_minutes: i64 = text
        .get(21..)
        .and_then(|o| o.trim().parse().ok())
        .unwrap_or(0);
    let utc = naive.checked_sub_signed(chrono::Duration::minutes(offset_minutes))?;
    Some(utc.and_utc().with_timezone(&chrono::Local).naive_local())
}

/// Lists all restore points, oldest first.
///
/// Requires administrator rights.
pub fn list_restore_points() -> Result<Vec<RestorePoint>> {
    let action = "list restore points";
    let _com = ComGuard::new();
    let mut points = Vec::new();
    // SAFETY: COM is initialised by `_com`, which outlives every interface.
    unsafe {
        let services = connect(action)?;
        let enumerator = services
            .ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT * FROM SystemRestore"),
                WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0),
                None,
            )
            .map_err(|e| map_wmi(e, action))?;
        loop {
            let mut row = [None];
            let mut returned = 0u32;
            let status = enumerator.Next(WBEM_INFINITE, &mut row, &mut returned);
            if status.is_err() {
                return Err(map_wmi(status.into(), action));
            }
            let Some(object) = row[0].take().filter(|_| returned == 1) else {
                break;
            };
            let number = |name: &str| {
                property(&object, name).and_then(|v| {
                    u32::try_from(&v)
                        .ok()
                        .or_else(|| i32::try_from(&v).ok().map(|n| n as u32))
                })
            };
            let text = |name: &str| {
                property(&object, name)
                    .and_then(|v| BSTR::try_from(&v).ok())
                    .map(|b| b.to_string())
            };
            let Some(sequence_number) = number("SequenceNumber") else {
                continue;
            };
            points.push(RestorePoint {
                sequence_number,
                description: text("Description").unwrap_or_default().trim().to_owned(),
                created: text("CreationTime").and_then(|t| parse_wmi_time(&t)),
                kind: RestorePointKind::from_code(number("RestorePointType").unwrap_or(u32::MAX)),
            });
        }
    }
    points.sort_by_key(|p| p.sequence_number);
    Ok(points)
}

/// Checks a description for a new restore point and returns the text that
/// will be stored.
fn validate_description(description: &str) -> Result<String> {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return Err(Error::Invalid(
            "the restore point needs a description".to_owned(),
        ));
    }
    Ok(trimmed.chars().take(MAX_DESCRIPTION_CHARS).collect())
}

/// Creates a restore point with the given description.
///
/// Requires administrator rights and enabled system protection. Windows by
/// default refuses to create more than one restore point per 24 hours; that
/// case is reported as [`CreateOutcome::SkippedByWindows`], not as an error.
pub fn create_restore_point(description: &str) -> Result<CreateOutcome> {
    let action = "create a restore point";
    let description = validate_description(description)?;
    if !elevation::is_elevated() {
        return Err(needs_elevation(action));
    }
    if !is_protection_enabled()? {
        return Err(Error::Other(
            "System Restore is turned off; turn on system protection for the system drive first"
                .to_owned(),
        ));
    }
    let newest_before = list_restore_points()?.last().map(|p| p.sequence_number);

    let _com = ComGuard::new();
    // SAFETY: COM is initialised by `_com`; every pointer passed is valid for
    // the duration of its call.
    let return_value = unsafe {
        let services = connect(action)?;
        let class_path = BSTR::from("SystemRestore");
        let mut class: Option<IWbemClassObject> = None;
        services
            .GetObject(
                &class_path,
                WBEM_GENERIC_FLAG_TYPE(0),
                None,
                Some(&mut class),
                None,
            )
            .map_err(|e| map_wmi(e, action))?;
        let class = class.ok_or_else(|| Error::Windows(format!("could not {action}")))?;

        let method = wide("CreateRestorePoint");
        let mut signature: Option<IWbemClassObject> = None;
        class
            .GetMethod(
                PCWSTR(method.as_ptr()),
                0,
                &mut signature,
                std::ptr::null_mut(),
            )
            .map_err(|e| map_wmi(e, action))?;
        let arguments = signature
            .ok_or_else(|| Error::Windows(format!("could not {action}")))?
            .SpawnInstance(0)
            .map_err(|e| map_wmi(e, action))?;

        let put = |name: &str, value: VARIANT| {
            let name = wide(name);
            arguments
                .Put(PCWSTR(name.as_ptr()), 0, &value, 0)
                .map_err(|e| map_wmi(e, action))
        };
        put("Description", VARIANT::from(description.as_str()))?;
        // 12 = MODIFY_SETTINGS, the type Windows uses for manual points.
        put("RestorePointType", VARIANT::from(12i32))?;
        put("EventType", VARIANT::from(BEGIN_SYSTEM_CHANGE))?;

        let mut output: Option<IWbemClassObject> = None;
        services
            .ExecMethod(
                &class_path,
                &BSTR::from("CreateRestorePoint"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None,
                &arguments,
                Some(&mut output),
                None,
            )
            .map_err(|e| map_wmi(e, action))?;
        output
            .as_ref()
            .and_then(|out| property(out, "ReturnValue"))
            .and_then(|v| {
                u32::try_from(&v)
                    .ok()
                    .or_else(|| i32::try_from(&v).ok().map(|n| n as u32))
            })
            .unwrap_or(0)
    };

    match return_value {
        0 => {}
        ERROR_ACCESS_DENIED => return Err(needs_elevation(action)),
        ERROR_SERVICE_DISABLED => {
            return Err(Error::Other("System Restore is turned off".to_owned()));
        }
        code => {
            return Err(Error::Windows(format!("could not {action} (error {code})")));
        }
    }

    let newest_after = list_restore_points()?.last().map(|p| p.sequence_number);
    Ok(match newest_after {
        Some(sequence) if newest_after != newest_before => CreateOutcome::Created(sequence),
        _ => CreateOutcome::SkippedByWindows,
    })
}

/// Deletes the restore point with the given sequence number.
///
/// Requires administrator rights. Deleting cannot be undone.
pub fn delete_restore_point(sequence_number: u32) -> Result<()> {
    let action = "delete a restore point";
    if !elevation::is_elevated() {
        return Err(needs_elevation(action));
    }
    type RemoveRestorePoint = unsafe extern "system" fn(u32) -> u32;

    // The library is loaded on demand (and only from System32) because it
    // does not exist on Windows editions without System Restore.
    let library = wide(
        &paths::system32_dir()
            .join("srclient.dll")
            .display()
            .to_string(),
    );
    // SAFETY: the library is loaded by absolute path from the system folder,
    // the exported function has the documented signature, and the module is
    // released again before returning.
    let code = unsafe {
        let module = LoadLibraryExW(PCWSTR(library.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            .map_err(|_| {
            Error::NotFound("System Restore is not available on this edition of Windows".to_owned())
        })?;
        let code = match GetProcAddress(module, PCSTR(c"SRRemoveRestorePoint".as_ptr().cast())) {
            Some(function) => {
                let remove: RemoveRestorePoint = std::mem::transmute(function);
                Some(remove(sequence_number))
            }
            None => None,
        };
        let _ = FreeLibrary(module);
        code
    };

    match code {
        Some(0) => Ok(()),
        Some(ERROR_INVALID_DATA) => {
            Err(Error::NotFound(format!("restore point {sequence_number}")))
        }
        Some(ERROR_ACCESS_DENIED) => Err(needs_elevation(action)),
        Some(code) => Err(Error::Windows(format!("could not {action} (error {code})"))),
        None => Err(Error::Windows(
            "System Restore does not support deleting restore points on this system".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(
            RestorePointKind::from_code(0),
            RestorePointKind::ApplicationInstall
        );
        assert_eq!(
            RestorePointKind::from_code(12),
            RestorePointKind::ModifySettings
        );
        assert_eq!(RestorePointKind::from_code(99), RestorePointKind::Other(99));
        for code in 0..20 {
            assert!(!RestorePointKind::from_code(code).label().is_empty());
        }
    }

    #[test]
    fn wmi_timestamps() {
        // Restore points are stamped in UTC.
        let parsed = parse_wmi_time("20260115093000.123456-000").unwrap();
        let expected = NaiveDateTime::parse_from_str("20260115093000", "%Y%m%d%H%M%S")
            .unwrap()
            .and_utc()
            .with_timezone(&chrono::Local)
            .naive_local();
        assert_eq!(parsed, expected);
        // An explicit offset is honoured: 10:30 at +060 is 09:30 UTC.
        assert_eq!(parse_wmi_time("20260115103000.000000+060"), Some(expected));
        // Missing fraction/offset still parses.
        assert_eq!(parse_wmi_time("20260115093000"), Some(expected));
        assert_eq!(parse_wmi_time("garbage"), None);
        assert_eq!(parse_wmi_time(""), None);
        assert_eq!(parse_wmi_time("20261340990000.000000-000"), None);
    }

    #[test]
    fn descriptions() {
        assert_eq!(
            validate_description("  Before cleanup  ").unwrap(),
            "Before cleanup"
        );
        assert!(matches!(
            validate_description("   "),
            Err(Error::Invalid(_))
        ));
        let long = "x".repeat(400);
        assert_eq!(
            validate_description(&long).unwrap().chars().count(),
            MAX_DESCRIPTION_CHARS
        );
    }

    #[test]
    fn protection_state_is_readable_by_anyone() {
        // Read-only; must not need elevation on client editions of Windows.
        match is_protection_enabled() {
            Ok(_) | Err(Error::NotFound(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn everything_else_asks_for_elevation() {
        if elevation::is_elevated() {
            // Elevated: listing is read-only and must work when protection
            // is available. Creating and deleting are never exercised.
            if is_protection_enabled().is_ok() {
                list_restore_points().expect("listing should work when elevated");
            }
            return;
        }
        assert!(matches!(
            list_restore_points(),
            Err(Error::NeedsElevation(_))
        ));
        assert!(matches!(
            create_restore_point("Zest test (never created)"),
            Err(Error::NeedsElevation(_))
        ));
        assert!(matches!(
            delete_restore_point(u32::MAX),
            Err(Error::NeedsElevation(_))
        ));
        // Validation comes first, whatever the rights.
        assert!(matches!(create_restore_point("  "), Err(Error::Invalid(_))));
    }
}
