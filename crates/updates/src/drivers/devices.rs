//! Device and driver enumeration through SetupAPI.
//!
//! This is the same source Device Manager reads, and it answers in a fraction
//! of a second. The digital signer of each driver package is resolved with
//! `SetupVerifyInfFile`, once per distinct INF and in parallel.

use super::{DriverInfo, DriverListOptions};
use chrono::NaiveDate;
use std::collections::HashMap;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_PROB, CR_SUCCESS, DIGCF_ALLCLASSES,
    DIGCF_PRESENT, DN_HAS_PROBLEM, DN_STARTED, HDEVINFO, SETUP_DI_GET_CLASS_DEVS_FLAGS,
    SP_DEVINFO_DATA, SP_INF_SIGNER_INFO_V2_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDescriptionW, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDevicePropertyW, SetupVerifyInfFileW,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_Class, DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_DeviceDesc,
    DEVPKEY_Device_DriverDate, DEVPKEY_Device_DriverInfPath, DEVPKEY_Device_DriverProvider,
    DEVPKEY_Device_DriverVersion, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_HardwareIds,
    DEVPKEY_Device_Manufacturer, DEVPKEY_NAME, DEVPROP_TYPE_FILETIME, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_STRING_LIST, DEVPROPTYPE,
};
use windows::Win32::Foundation::{
    DEVPROPKEY, ERROR_AUTHENTICODE_TRUST_NOT_ESTABLISHED, ERROR_AUTHENTICODE_TRUSTED_PUBLISHER,
    ERROR_INSUFFICIENT_BUFFER, GetLastError,
};
use windows::core::{GUID, PCWSTR};
use zest_core::{Progress, Result};

/// Owns a device information set.
struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the handle came from SetupDiGetClassDevsW and is destroyed once.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

/// Decodes a NUL terminated UTF-16 string property.
fn decode_string(bytes: &[u8]) -> String {
    let units = utf16_units(bytes);
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end]).trim().to_owned()
}

/// Decodes a double-NUL terminated list of UTF-16 strings.
fn decode_string_list(bytes: &[u8]) -> Vec<String> {
    utf16_units(bytes)
        .split(|&u| u == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf16_lossy(part).trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

/// Converts a `FILETIME` property (100 ns ticks since 1601-01-01) to a date.
fn decode_filetime_date(bytes: &[u8]) -> Option<NaiveDate> {
    const TICKS_PER_DAY: u64 = 24 * 60 * 60 * 10_000_000;
    let ticks = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
    if ticks == 0 {
        return None;
    }
    NaiveDate::from_ymd_opt(1601, 1, 1)?.checked_add_days(chrono::Days::new(ticks / TICKS_PER_DAY))
}

fn format_guid(guid: &GUID) -> String {
    format!("{{{guid:?}}}")
}

/// Reads one device property as raw bytes.
///
/// # Safety
/// `set` must be a live device information set and `device` one of its members.
unsafe fn property(
    set: HDEVINFO,
    device: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    let mut kind = DEVPROPTYPE(0);
    let mut required = 0u32;
    let mut buffer = vec![0u8; 512];
    // SAFETY: all pointers reference live locals; the buffer length is passed
    // by the wrapper.
    let first = unsafe {
        SetupDiGetDevicePropertyW(
            set,
            device,
            key,
            &mut kind,
            Some(&mut buffer),
            Some(&mut required),
            0,
        )
    };
    match first {
        Ok(()) => {}
        Err(e)
            if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult()
                && required as usize > buffer.len() =>
        {
            buffer = vec![0u8; required as usize];
            // SAFETY: as above, with a buffer of the size the API asked for.
            unsafe {
                SetupDiGetDevicePropertyW(
                    set,
                    device,
                    key,
                    &mut kind,
                    Some(&mut buffer),
                    Some(&mut required),
                    0,
                )
            }
            .ok()?;
        }
        Err(_) => return None,
    }
    buffer.truncate(required as usize);
    Some((kind, buffer))
}

/// # Safety
/// Same contract as [`property`].
unsafe fn string_property(set: HDEVINFO, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> String {
    // SAFETY: forwarded contract.
    match unsafe { property(set, device, key) } {
        Some((kind, bytes)) if kind == DEVPROP_TYPE_STRING => decode_string(&bytes),
        _ => String::new(),
    }
}

/// # Safety
/// Same contract as [`property`].
unsafe fn list_property(set: HDEVINFO, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Vec<String> {
    // SAFETY: forwarded contract.
    match unsafe { property(set, device, key) } {
        Some((kind, bytes)) if kind == DEVPROP_TYPE_STRING_LIST => decode_string_list(&bytes),
        Some((kind, bytes)) if kind == DEVPROP_TYPE_STRING => {
            let single = decode_string(&bytes);
            if single.is_empty() {
                Vec::new()
            } else {
                vec![single]
            }
        }
        _ => Vec::new(),
    }
}

fn class_description(guid: &GUID) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: the buffer is valid for its length; the GUID is a live reference.
    let ok = unsafe { SetupDiGetClassDescriptionW(guid, &mut buffer, None) }.is_ok();
    if !ok {
        return String::new();
    }
    let end = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_owned()
}

/// Interprets the result of `SetupVerifyInfFile`.
///
/// The function only returns success for packages signed through the
/// Windows hardware programme. A package with a valid Authenticode signature
/// from its vendor "fails" with one of two dedicated codes; such a package
/// is signed all the same, and reporting it as unsigned would be a false
/// alarm.
pub(super) fn is_signed_verdict(succeeded: bool, last_error: u32) -> bool {
    succeeded
        || last_error == ERROR_AUTHENTICODE_TRUSTED_PUBLISHER.0
        || last_error == ERROR_AUTHENTICODE_TRUST_NOT_ESTABLISHED.0
}

/// Asks Windows who signed the driver package behind an INF in the system
/// INF folder. Returns `(is_signed, signer)`.
pub(super) fn verify_inf(inf_name: &str) -> (bool, Option<String>) {
    let wide: Vec<u16> = inf_name.encode_utf16().chain(Some(0)).collect();
    let mut info = SP_INF_SIGNER_INFO_V2_W {
        cbSize: size_of::<SP_INF_SIGNER_INFO_V2_W>() as u32,
        ..Default::default()
    };
    // SAFETY: `wide` is NUL terminated and `info` carries its own size. The
    // thread's last error is read immediately after the call it belongs to.
    let signed = unsafe {
        let succeeded = SetupVerifyInfFileW(PCWSTR(wide.as_ptr()), None, &mut info).as_bool();
        let last_error = if succeeded { 0 } else { GetLastError().0 };
        is_signed_verdict(succeeded, last_error)
    };
    let end = info
        .DigitalSigner
        .iter()
        .position(|&u| u == 0)
        .unwrap_or(info.DigitalSigner.len());
    let signer = String::from_utf16_lossy(&info.DigitalSigner[..end])
        .trim()
        .to_owned();
    (signed, (!signer.is_empty()).then_some(signer))
}

/// Verifies every distinct INF once, spreading the work over a few threads.
fn verify_all(
    inf_names: &[String],
    progress: &Progress,
) -> HashMap<String, (bool, Option<String>)> {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let chunk = inf_names.len().div_ceil(workers).max(1);
    let mut results = HashMap::with_capacity(inf_names.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = inf_names
            .chunks(chunk)
            .map(|names| {
                scope.spawn(move || {
                    names
                        .iter()
                        .take_while(|_| !progress.is_cancelled())
                        .map(|name| {
                            let verdict = verify_inf(name);
                            progress.inc(1);
                            (name.clone(), verdict)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            if let Ok(part) = handle.join() {
                results.extend(part);
            }
        }
    });
    results
}

/// True for third-party driver packages, which Windows stores as `oemN.inf`.
pub(super) fn is_oem_inf(inf_name: &str) -> bool {
    let lower = inf_name.to_ascii_lowercase();
    lower
        .strip_prefix("oem")
        .and_then(|rest| rest.strip_suffix(".inf"))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// Decides whether a driver's age is worth pointing out.
///
/// Only third-party drivers qualify, and placeholder dates are ignored:
/// some vendors deliberately stamp drivers with dates decades in the past so
/// that any real driver outranks them.
pub(super) fn outdated_hint(
    is_inbox: bool,
    is_microsoft: bool,
    date: Option<NaiveDate>,
    today: NaiveDate,
    outdated_after_days: u32,
) -> bool {
    use chrono::Datelike;
    if is_inbox || is_microsoft || outdated_after_days == 0 {
        return false;
    }
    let Some(date) = date else {
        return false;
    };
    if date.year() < 2000 || date > today {
        return false;
    }
    (today - date).num_days() > i64::from(outdated_after_days)
}

/// Enumerates devices that have a driver installed.
pub(super) fn enumerate(
    options: &DriverListOptions,
    progress: &Progress,
) -> Result<Vec<DriverInfo>> {
    progress.set_message("Reading installed drivers…");
    let flags = if options.include_disconnected {
        DIGCF_ALLCLASSES
    } else {
        SETUP_DI_GET_CLASS_DEVS_FLAGS(DIGCF_ALLCLASSES.0 | DIGCF_PRESENT.0)
    };
    // SAFETY: no class filter, no enumerator, no parent window.
    let set = DeviceSet(unsafe { SetupDiGetClassDevsW(None, PCWSTR::null(), None, flags) }?);
    let today = chrono::Local::now().date_naive();

    let mut drivers: Vec<DriverInfo> = Vec::new();
    let mut class_names: HashMap<String, String> = HashMap::new();
    let mut index = 0u32;
    loop {
        progress.check()?;
        let mut device = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: `device` is initialised with its size as the API requires.
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut device) }.is_err() {
            break;
        }
        index += 1;

        // SAFETY (all property reads below): `set` is alive and `device` was
        // just filled in by SetupDiEnumDeviceInfo for this set.
        let inf_name = unsafe { string_property(set.0, &device, &DEVPKEY_Device_DriverInfPath) };
        if inf_name.is_empty() {
            // No driver package bound to this device.
            continue;
        }
        let mut device_name =
            unsafe { string_property(set.0, &device, &DEVPKEY_Device_FriendlyName) };
        if device_name.is_empty() {
            device_name = unsafe { string_property(set.0, &device, &DEVPKEY_Device_DeviceDesc) };
        }
        if device_name.is_empty() {
            device_name = unsafe { string_property(set.0, &device, &DEVPKEY_NAME) };
        }
        let driver_date = match unsafe { property(set.0, &device, &DEVPKEY_Device_DriverDate) } {
            Some((kind, bytes)) if kind == DEVPROP_TYPE_FILETIME => decode_filetime_date(&bytes),
            _ => None,
        };
        let hardware_ids = unsafe { list_property(set.0, &device, &DEVPKEY_Device_HardwareIds) };
        let compatible_ids =
            unsafe { list_property(set.0, &device, &DEVPKEY_Device_CompatibleIds) };
        let provider = unsafe { string_property(set.0, &device, &DEVPKEY_Device_DriverProvider) };

        let mut instance = [0u16; 512];
        // SAFETY: buffer valid for its length.
        let instance_id =
            if unsafe { SetupDiGetDeviceInstanceIdW(set.0, &device, Some(&mut instance), None) }
                .is_ok()
            {
                let end = instance
                    .iter()
                    .position(|&u| u == 0)
                    .unwrap_or(instance.len());
                String::from_utf16_lossy(&instance[..end])
            } else {
                String::new()
            };

        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        // SAFETY: out-pointers reference live locals.
        let present =
            unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, device.DevInst, 0) }
                == CR_SUCCESS;
        let has_problem = present && status.0 & DN_HAS_PROBLEM.0 != 0;

        let class_guid = format_guid(&device.ClassGuid);
        let class_description = class_names
            .entry(class_guid.clone())
            .or_insert_with(|| class_description(&device.ClassGuid))
            .clone();

        let is_inbox = !is_oem_inf(&inf_name);
        let is_microsoft = provider.to_lowercase().contains("microsoft");
        drivers.push(DriverInfo {
            device_name,
            device_class: unsafe { string_property(set.0, &device, &DEVPKEY_Device_Class) },
            class_description,
            class_guid,
            manufacturer: unsafe { string_property(set.0, &device, &DEVPKEY_Device_Manufacturer) },
            driver_version: unsafe {
                string_property(set.0, &device, &DEVPKEY_Device_DriverVersion)
            },
            driver_date,
            hardware_id: hardware_ids.first().cloned().unwrap_or_default(),
            hardware_ids,
            compatible_ids,
            instance_id,
            is_signed: None,
            signer: None,
            is_present: present,
            is_started: present && status.0 & DN_STARTED.0 != 0,
            problem_code: if has_problem { problem.0 } else { 0 },
            is_outdated_hint: outdated_hint(
                is_inbox,
                is_microsoft,
                driver_date,
                today,
                options.outdated_after_days,
            ),
            is_inbox,
            is_microsoft,
            provider,
            inf_name,
        });
    }
    drop(set);

    if options.verify_signatures {
        let mut inf_names: Vec<String> =
            drivers.iter().map(|d| d.inf_name.to_lowercase()).collect();
        inf_names.sort_unstable();
        inf_names.dedup();
        progress.set_message("Checking driver signatures…");
        progress.set_total(inf_names.len() as u64);
        progress.set_done(0);
        let verdicts = verify_all(&inf_names, progress);
        progress.check()?;
        for driver in &mut drivers {
            if let Some((signed, signer)) = verdicts.get(&driver.inf_name.to_lowercase()) {
                driver.is_signed = Some(*signed);
                driver.signer = signer.clone();
            }
        }
    }

    drivers.sort_by(|a, b| {
        a.class_description
            .to_lowercase()
            .cmp(&b.class_description.to_lowercase())
            .then_with(|| {
                a.device_name
                    .to_lowercase()
                    .cmp(&b.device_name.to_lowercase())
            })
            .then_with(|| a.instance_id.cmp(&b.instance_id))
    });
    progress.set_message(format!("{} devices with drivers", drivers.len()));
    Ok(drivers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide_bytes(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn string_decoding() {
        let text: Vec<u16> = "oem42.inf\0garbage".encode_utf16().collect();
        assert_eq!(decode_string(&wide_bytes(&text)), "oem42.inf");
        assert_eq!(decode_string(&[]), "");
        // An odd trailing byte is ignored rather than misread.
        let mut odd = wide_bytes(&"ab".encode_utf16().collect::<Vec<_>>());
        odd.push(0x41);
        assert_eq!(decode_string(&odd), "ab");
    }

    #[test]
    fn string_list_decoding() {
        let text: Vec<u16> = "PCI\\VEN_8086&DEV_1234\0PCI\\VEN_8086\0\0"
            .encode_utf16()
            .collect();
        assert_eq!(
            decode_string_list(&wide_bytes(&text)),
            vec![
                "PCI\\VEN_8086&DEV_1234".to_owned(),
                "PCI\\VEN_8086".to_owned()
            ]
        );
        assert!(decode_string_list(&[0, 0, 0, 0]).is_empty());
    }

    #[test]
    fn filetime_decoding() {
        // 2006-06-21, the date Windows stamps on its in-box drivers.
        let ticks: u64 = 127_953_216_000_000_000;
        assert_eq!(
            decode_filetime_date(&ticks.to_le_bytes()),
            NaiveDate::from_ymd_opt(2006, 6, 21)
        );
        assert_eq!(decode_filetime_date(&0u64.to_le_bytes()), None);
        assert_eq!(decode_filetime_date(&[1, 2, 3]), None);
        // Absurd values must not panic.
        let _ = decode_filetime_date(&u64::MAX.to_le_bytes());
    }

    #[test]
    fn signature_verdicts() {
        assert!(is_signed_verdict(true, 0));
        // Vendor-signed (Authenticode) packages are signed.
        assert!(is_signed_verdict(false, 0xE000_0241));
        assert!(is_signed_verdict(false, 0xE000_0242));
        // Distrusted publishers, missing catalogs and plain failures are not.
        assert!(!is_signed_verdict(false, 0xE000_0240));
        assert!(!is_signed_verdict(false, 0xE000_0243));
        assert!(!is_signed_verdict(false, 0x800B_0100));
        assert!(!is_signed_verdict(false, 2));
        assert!(!is_signed_verdict(false, 0));
    }

    #[test]
    fn inbox_packages_verify_as_signed_by_windows() {
        // Read-only signature query for a driver that ships with every
        // Windows installation.
        let (signed, signer) = verify_inf("machine.inf");
        assert!(signed, "machine.inf should be signed");
        assert!(signer.is_some_and(|s| s.contains("Microsoft")));
        // A package that does not exist is simply not signed.
        assert_eq!(verify_inf("zest-no-such-driver.inf"), (false, None));
    }

    #[test]
    fn oem_inf_detection() {
        assert!(is_oem_inf("oem0.inf"));
        assert!(is_oem_inf("OEM123.INF"));
        assert!(!is_oem_inf("oem.inf"));
        assert!(!is_oem_inf("oemsetup.inf"));
        assert!(!is_oem_inf("machine.inf"));
        assert!(!is_oem_inf("oem12.inf.bak"));
        assert!(!is_oem_inf(""));
    }

    #[test]
    fn age_hint_rules() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let old = NaiveDate::from_ymd_opt(2019, 1, 1);
        let recent = NaiveDate::from_ymd_opt(2025, 12, 1);
        assert!(outdated_hint(false, false, old, today, 1095));
        assert!(!outdated_hint(false, false, recent, today, 1095));
        // In-box and Microsoft drivers are serviced by Windows itself.
        assert!(!outdated_hint(true, false, old, today, 1095));
        assert!(!outdated_hint(false, true, old, today, 1095));
        // Placeholder and missing dates say nothing about age.
        assert!(!outdated_hint(
            false,
            false,
            NaiveDate::from_ymd_opt(1968, 7, 18),
            today,
            1095
        ));
        assert!(!outdated_hint(
            false,
            false,
            NaiveDate::from_ymd_opt(1970, 1, 1),
            today,
            1095
        ));
        assert!(!outdated_hint(false, false, None, today, 1095));
        assert!(!outdated_hint(
            false,
            false,
            NaiveDate::from_ymd_opt(2030, 1, 1),
            today,
            1095
        ));
        // Zero disables the hint.
        assert!(!outdated_hint(false, false, old, today, 0));
    }

    #[test]
    fn guid_formatting() {
        let guid = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);
        assert_eq!(format_guid(&guid), "{4D36E968-E325-11CE-BFC1-08002BE10318}");
    }
}
