//! Power plans, a few read-only system settings, and a hardware summary.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::System::Power::{
    ACCESS_SCHEME, GetSystemPowerStatus, PowerEnumerate, PowerGetActiveScheme,
    PowerReadFriendlyName, PowerSetActiveScheme, SYSTEM_POWER_STATUS,
};
use windows::core::GUID;
use zest_core::reg::RegPath;
use zest_core::{Error, Result};

const ERROR_SUCCESS: u32 = 0;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_MORE_DATA: u32 = 234;

/// A Windows power plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerPlan {
    /// Identifier in the usual `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` form.
    pub guid: String,
    /// Name shown in Control Panel, e.g. "Balanced".
    pub name: String,
    /// True for the plan currently in effect.
    pub active: bool,
}

fn format_guid(guid: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}

fn parse_guid(text: &str) -> Option<GUID> {
    let trimmed = text.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = trimmed.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    if parts.len() != 5
        || parts
            .iter()
            .zip(lengths)
            .any(|(p, len)| p.len() != len || !p.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    u128::from_str_radix(&parts.concat(), 16)
        .ok()
        .map(GUID::from_u128)
}

fn active_scheme() -> Result<GUID> {
    let mut ptr: *mut GUID = std::ptr::null_mut();
    // SAFETY: the API allocates the GUID and hands ownership to the caller,
    // who must release it with LocalFree; it is copied out before that.
    unsafe {
        let status = PowerGetActiveScheme(None, &mut ptr);
        if status.0 != ERROR_SUCCESS || ptr.is_null() {
            return Err(Error::Windows(format!(
                "could not read the active power plan (code {})",
                status.0
            )));
        }
        let guid = *ptr;
        let _ = LocalFree(Some(HLOCAL(ptr.cast())));
        Ok(guid)
    }
}

fn friendly_name(guid: &GUID) -> Option<String> {
    let mut size = 0u32;
    // SAFETY: first call asks for the required size; the second passes a
    // buffer of exactly that many bytes. The GUID pointer is valid.
    unsafe {
        let status = PowerReadFriendlyName(None, Some(guid), None, None, None, &mut size);
        if (status.0 != ERROR_SUCCESS && status.0 != ERROR_MORE_DATA) || size == 0 {
            return None;
        }
        // u16 elements keep the UTF-16 text aligned.
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let mut bytes = (buf.len() * 2) as u32;
        let status = PowerReadFriendlyName(
            None,
            Some(guid),
            None,
            None,
            Some(buf.as_mut_ptr().cast()),
            &mut bytes,
        );
        if status.0 != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let name = String::from_utf16_lossy(&buf[..len]).trim().to_owned();
        (!name.is_empty()).then_some(name)
    }
}

/// Lists the power plans installed on this PC.
pub fn list_power_plans() -> Result<Vec<PowerPlan>> {
    let active = active_scheme().ok();
    let mut plans = Vec::new();
    for index in 0..64u32 {
        let mut guid = GUID::zeroed();
        let mut size = size_of::<GUID>() as u32;
        // SAFETY: the buffer is a GUID and `size` is its size in bytes.
        let status = unsafe {
            PowerEnumerate(
                None,
                None,
                None,
                ACCESS_SCHEME,
                index,
                Some((&mut guid as *mut GUID).cast()),
                &mut size,
            )
        };
        if status.0 != ERROR_SUCCESS {
            // ERROR_NO_MORE_ITEMS ends the list; anything else also stops it.
            break;
        }
        plans.push(PowerPlan {
            name: friendly_name(&guid).unwrap_or_else(|| "Power plan".to_owned()),
            active: active == Some(guid),
            guid: format_guid(&guid),
        });
    }
    if plans.is_empty() {
        return Err(Error::Windows("no power plans could be listed".to_owned()));
    }
    Ok(plans)
}

/// The power plan currently in effect.
pub fn active_power_plan() -> Result<PowerPlan> {
    let guid = active_scheme()?;
    Ok(PowerPlan {
        name: friendly_name(&guid).unwrap_or_else(|| "Power plan".to_owned()),
        guid: format_guid(&guid),
        active: true,
    })
}

/// Makes the plan with this identifier the active one.
///
/// Only identifiers returned by [`list_power_plans`] are accepted. The
/// change takes effect immediately and can be reversed by activating the
/// previous plan again.
pub fn set_active_power_plan(guid: &str) -> Result<()> {
    let wanted = parse_guid(guid)
        .ok_or_else(|| Error::Invalid(format!("'{guid}' is not a power plan identifier")))?;
    let wanted_text = format_guid(&wanted);
    if !list_power_plans()?.iter().any(|p| p.guid == wanted_text) {
        return Err(Error::NotFound(format!("power plan {wanted_text}")));
    }
    // SAFETY: the GUID pointer is valid for the duration of the call.
    let status = unsafe { PowerSetActiveScheme(None, Some(&wanted)) };
    match status.0 {
        ERROR_SUCCESS => Ok(()),
        ERROR_ACCESS_DENIED => Err(Error::NeedsElevation("change the power plan".to_owned())),
        code => Err(Error::Windows(format!(
            "could not change the power plan (code {code})"
        ))),
    }
}

/// Windows settings that affect background activity. Read-only.
///
/// `None` means the setting has never been changed and Windows applies its
/// default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemTweaks {
    /// Whether Game Mode is switched on (default: on).
    pub game_mode: Option<bool>,
    /// Whether apps from the Store may run in the background (default: yes).
    pub background_apps_allowed: Option<bool>,
    /// Whether Windows may throttle background programs itself (default: yes).
    pub power_throttling: Option<bool>,
}

/// Reads the current [`SystemTweaks`].
pub fn system_tweaks() -> SystemTweaks {
    let game_mode = RegPath::hkcu(r"Software\Microsoft\GameBar")
        .read_u32("AutoGameModeEnabled")
        .map(|v| v != 0);

    let policy = RegPath::hklm(r"SOFTWARE\Policies\Microsoft\Windows\AppPrivacy")
        .read_u32("LetAppsRunInBackground");
    let user =
        RegPath::hkcu(r"Software\Microsoft\Windows\CurrentVersion\BackgroundAccessApplications")
            .read_u32("GlobalUserDisabled");
    let background_apps_allowed = match (policy, user) {
        // Policy value 2 forces background apps off for everyone.
        (Some(2), _) => Some(false),
        (Some(1), _) => Some(true),
        (_, Some(disabled)) => Some(disabled == 0),
        _ => None,
    };

    let power_throttling = RegPath::hklm(r"SYSTEM\CurrentControlSet\Control\Power\PowerThrottling")
        .read_u32("PowerThrottlingOff")
        .map(|off| off == 0);

    SystemTweaks {
        game_mode,
        background_apps_allowed,
        power_throttling,
    }
}

/// Mains and battery state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerStatus {
    /// True on mains power, false on battery, `None` when unknown.
    pub on_ac: Option<bool>,
    /// True when the PC has a battery.
    pub has_battery: bool,
    /// Charge level 0–100, when a battery is present and reports it.
    pub battery_percent: Option<u8>,
    /// True while Windows' battery saver is active.
    pub battery_saver: bool,
}

/// Reads the current [`PowerStatus`].
pub fn power_status() -> PowerStatus {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: valid out pointer.
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return PowerStatus::default();
    }
    // BatteryFlag: 128 = no system battery, 255 = unknown.
    let has_battery = status.BatteryFlag != 128 && status.BatteryFlag != 255;
    PowerStatus {
        on_ac: match status.ACLineStatus {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        has_battery,
        battery_percent: (has_battery && status.BatteryLifePercent <= 100)
            .then_some(status.BatteryLifePercent),
        battery_saver: status.SystemStatusFlag == 1,
    }
}

/// Facts about this PC for a dashboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemSummary {
    /// Network name of the PC.
    pub computer_name: String,
    /// Product name, e.g. "Windows 11 Home".
    pub os_name: String,
    /// Feature update, e.g. "24H2" (empty when unknown).
    pub os_version: String,
    /// Build number including the update revision, e.g. "26200.6584".
    pub os_build: String,
    /// Processor model.
    pub cpu_name: String,
    /// Number of physical processor cores.
    pub physical_cores: u32,
    /// Number of logical processors.
    pub logical_cores: u32,
    /// Installed (usable) RAM in bytes.
    pub total_memory: u64,
    /// RAM available right now, in bytes.
    pub available_memory: u64,
    /// Time since Windows started.
    pub uptime: Duration,
    /// When Windows started.
    pub boot_time: SystemTime,
    /// Root of the system drive, e.g. `C:\`.
    pub system_drive: String,
    /// Capacity of the system drive in bytes.
    pub system_drive_total: u64,
    /// Free space on the system drive in bytes.
    pub system_drive_free: u64,
    /// Mains and battery state.
    pub power: PowerStatus,
}

fn physical_core_count() -> u32 {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    let mut bytes = 0u32;
    // SAFETY: the first call only reports the required size. The second
    // receives a buffer of that size, aligned for the record type; records
    // are walked using the size each one declares, never past the end.
    unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut bytes);
        if bytes == 0 {
            return 0;
        }
        let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
        if GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr().cast()),
            &mut bytes,
        )
        .is_err()
        {
            return 0;
        }
        let base = buf.as_ptr() as usize;
        let mut offset = 0usize;
        let mut cores = 0u32;
        while offset + 8 <= bytes as usize {
            let record = &*((base + offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX);
            if record.Size == 0 {
                break;
            }
            if record.Relationship == RelationProcessorCore {
                cores += 1;
            }
            offset += record.Size as usize;
        }
        cores
    }
}

/// Collects the [`SystemSummary`]. Never fails; fields that cannot be read
/// are left empty or zero.
pub fn system_summary() -> SystemSummary {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::core::HSTRING;

    let version = RegPath::hklm(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let build_number = version
        .read_string("CurrentBuildNumber")
        .unwrap_or_default();
    let mut os_name = version
        .read_string("ProductName")
        .unwrap_or_else(|| "Windows".to_owned());
    // The product name was never updated in the registry for Windows 11.
    if build_number.parse::<u32>().is_ok_and(|b| b >= 22000) {
        os_name = os_name.replace("Windows 10", "Windows 11");
    }
    let os_build = match version.read_u32("UBR") {
        Some(revision) if !build_number.is_empty() => format!("{build_number}.{revision}"),
        _ => build_number,
    };

    let logical_cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let physical = physical_core_count();

    let mut memory = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `memory` is a correctly sized, initialised structure.
    let _ = unsafe { GlobalMemoryStatusEx(&mut memory) };

    let drive = zest_core::paths::system_drive();
    let (mut total, mut free) = (0u64, 0u64);
    // SAFETY: the path is a NUL terminated string owned by `HSTRING`; both
    // out pointers are valid.
    let _ = unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(drive.as_os_str()),
            None,
            Some(&mut total),
            Some(&mut free),
        )
    };

    let uptime = crate::monitor::uptime();
    SystemSummary {
        computer_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
        os_name,
        os_version: version.read_string("DisplayVersion").unwrap_or_default(),
        os_build,
        cpu_name: RegPath::hklm(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
            .read_string("ProcessorNameString")
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
        physical_cores: if physical == 0 {
            logical_cores
        } else {
            physical
        },
        logical_cores,
        total_memory: memory.ullTotalPhys,
        available_memory: memory.ullAvailPhys,
        uptime,
        boot_time: SystemTime::now()
            .checked_sub(uptime)
            .unwrap_or(SystemTime::UNIX_EPOCH),
        system_drive: drive.display().to_string(),
        system_drive_total: total,
        system_drive_free: free,
        power: power_status(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_text_round_trip() {
        let text = "381b4222-f694-41f0-9685-ff5bb260df2e";
        let guid = parse_guid(text).unwrap();
        assert_eq!(format_guid(&guid), text);
        assert_eq!(
            parse_guid(&format!("{{{}}}", text.to_uppercase())),
            Some(guid)
        );
        for bad in [
            "",
            "balanced",
            "381b4222f69441f09685ff5bb260df2e",
            "381b4222-f694-41f0-9685-ff5bb260df2",
            "zzzzzzzz-f694-41f0-9685-ff5bb260df2e",
        ] {
            assert_eq!(parse_guid(bad), None, "{bad}");
        }
    }

    #[test]
    fn exactly_one_plan_is_active() {
        let plans = list_power_plans().unwrap();
        assert!(!plans.is_empty());
        assert_eq!(plans.iter().filter(|p| p.active).count(), 1, "{plans:?}");
        assert!(
            plans
                .iter()
                .all(|p| !p.name.is_empty() && parse_guid(&p.guid).is_some())
        );
        let active = active_power_plan().unwrap();
        assert!(active.active);
        assert!(
            plans
                .iter()
                .any(|p| p.guid == active.guid && p.name == active.name)
        );
    }

    #[test]
    fn changing_the_plan_validates_before_touching_anything() {
        // Neither call reaches the system: the identifier is rejected first.
        assert!(matches!(
            set_active_power_plan("not-a-guid"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            set_active_power_plan("00000000-1111-2222-3333-444444444444"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn summary_is_plausible() {
        let s = system_summary();
        assert!(s.os_name.starts_with("Windows"), "{}", s.os_name);
        assert!(
            s.os_build
                .split('.')
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap()
                >= 10240
        );
        assert!(!s.cpu_name.is_empty());
        assert!(s.logical_cores >= 1 && s.physical_cores >= 1);
        assert!(s.physical_cores <= s.logical_cores);
        assert!(s.total_memory > 512 * 1024 * 1024);
        assert!(s.available_memory <= s.total_memory);
        assert!(s.system_drive_total > 0 && s.system_drive_free <= s.system_drive_total);
        assert!(s.uptime > Duration::from_secs(1));
        assert!(s.boot_time < SystemTime::now());
        if let Some(percent) = s.power.battery_percent {
            assert!(percent <= 100 && s.power.has_battery);
        }
        // Reading the tweaks never fails, whatever is configured.
        let _ = system_tweaks();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<SystemSummary>(&json).unwrap(), s);
    }
}
