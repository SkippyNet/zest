//! Batteries, read straight from the battery driver.
//!
//! The battery device answers every user, which the WMI battery classes do
//! not (the design capacity in particular is administrator-only there on
//! many machines).

use crate::model::{Battery, BatteryState};
use crate::system::clean;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Power::{
    BATTERY_INFORMATION, BATTERY_QUERY_INFORMATION, BATTERY_QUERY_INFORMATION_LEVEL,
    BATTERY_STATUS, BATTERY_WAIT_STATUS, BatteryDeviceName, BatteryInformation,
    BatteryManufactureName, GetSystemPowerStatus, IOCTL_BATTERY_QUERY_INFORMATION,
    IOCTL_BATTERY_QUERY_STATUS, IOCTL_BATTERY_QUERY_TAG, SYSTEM_POWER_STATUS,
};
use windows::core::{GUID, PCWSTR};

/// Device interface class of batteries (`GUID_DEVICE_BATTERY`).
const GUID_DEVICE_BATTERY: GUID = GUID::from_u128(0x72631e54_78a4_11d0_bcf7_00aa00b7b32a);
/// The battery is the one that powers the computer.
const BATTERY_SYSTEM_BATTERY: u32 = 0x8000_0000;
/// Capacities are relative figures, not mWh.
const BATTERY_CAPACITY_RELATIVE: u32 = 0x4000_0000;
/// An uninterruptible power supply rather than a built-in battery.
const BATTERY_IS_SHORT_TERM: u32 = 0x2000_0000;
const BATTERY_POWER_ON_LINE: u32 = 0x1;
const BATTERY_DISCHARGING: u32 = 0x2;
const BATTERY_CHARGING: u32 = 0x4;
const UNKNOWN_CAPACITY: u32 = 0xFFFF_FFFF;
const UNKNOWN_VOLTAGE: u32 = 0xFFFF_FFFF;
const UNKNOWN_RATE: i32 = i32::MIN;

/// What the driver reports for one battery, before interpretation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawBattery {
    pub(crate) capabilities: u32,
    pub(crate) chemistry: [u8; 4],
    pub(crate) designed_capacity: u32,
    pub(crate) full_charged_capacity: u32,
    pub(crate) cycle_count: u32,
    /// `(power state, capacity, voltage, rate)` when the status was readable.
    pub(crate) status: Option<(u32, u32, u32, i32)>,
    pub(crate) name: Option<String>,
    pub(crate) manufacturer: Option<String>,
}

fn chemistry_name(code: &[u8; 4]) -> Option<String> {
    let text: String = code
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(
        match text.to_ascii_uppercase().as_str() {
            "LION" | "LI-I" | "LIIO" => "Lithium-ion",
            "LIP" | "LIPO" => "Lithium polymer",
            "NIMH" => "Nickel-metal hydride",
            "NICD" => "Nickel-cadmium",
            "NIZN" => "Nickel-zinc",
            "PBAC" => "Lead-acid",
            "RAM" => "Rechargeable alkaline",
            _ => text,
        }
        .to_owned(),
    )
}

/// Interprets the driver's figures. Returns `None` for batteries that do
/// not power the computer (a UPS, a peripheral).
pub(crate) fn battery_from(raw: &RawBattery) -> Option<Battery> {
    if raw.capabilities & BATTERY_SYSTEM_BATTERY == 0
        || raw.capabilities & BATTERY_IS_SHORT_TERM != 0
    {
        return None;
    }
    let known = |value: u32| (value != UNKNOWN_CAPACITY && value != 0).then_some(value);
    let absolute = raw.capabilities & BATTERY_CAPACITY_RELATIVE == 0;
    let design = known(raw.designed_capacity);
    let full = known(raw.full_charged_capacity);
    let mut battery = Battery {
        name: clean(raw.name.clone()),
        manufacturer: clean(raw.manufacturer.clone()),
        chemistry: chemistry_name(&raw.chemistry),
        design_mwh: design.filter(|_| absolute),
        full_charge_mwh: full.filter(|_| absolute),
        cycle_count: (raw.cycle_count > 0).then_some(raw.cycle_count),
        ..Battery::default()
    };
    if let Some((state, capacity, voltage, rate)) = raw.status {
        let remaining = (capacity != UNKNOWN_CAPACITY).then_some(capacity);
        battery.remaining_mwh = remaining.filter(|_| absolute);
        battery.charge_percent = match (remaining, full) {
            (Some(remaining), Some(full)) => {
                Some((remaining as f32 / full as f32 * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        };
        battery.voltage_mv = (voltage != UNKNOWN_VOLTAGE && voltage != 0).then_some(voltage);
        battery.rate_mw = (rate != UNKNOWN_RATE && absolute).then_some(rate);
        battery.state = if state & BATTERY_CHARGING != 0 {
            BatteryState::Charging
        } else if state & BATTERY_DISCHARGING != 0 {
            BatteryState::Discharging
        } else if state & BATTERY_POWER_ON_LINE != 0 {
            BatteryState::Idle
        } else {
            BatteryState::Unknown
        };
    }
    Some(battery)
}

struct Device(HANDLE);

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful CreateFileW.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct DeviceList(HDEVINFO);

impl Drop for DeviceList {
    fn drop(&mut self) {
        // SAFETY: the list came from a successful SetupDiGetClassDevsW.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

/// Paths of the battery devices that are present.
fn battery_device_paths() -> Vec<Vec<u16>> {
    let mut paths = Vec::new();
    // SAFETY: enumerates present device interfaces of the battery class.
    let Ok(list) = (unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVICE_BATTERY),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }) else {
        return paths;
    };
    let list = DeviceList(list);
    // More than a handful of batteries does not happen; the bound only
    // guards against a driver that never says "no more items".
    for index in 0..16 {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: `interface` is a correctly sized out parameter.
        let found = unsafe {
            SetupDiEnumDeviceInterfaces(list.0, None, &GUID_DEVICE_BATTERY, index, &mut interface)
        };
        if found.is_err() {
            break;
        }
        let mut needed = 0u32;
        // SAFETY: a sizing call; failure with "buffer too small" is expected.
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(list.0, &interface, None, 0, Some(&mut needed), None)
        };
        if needed < 8 {
            continue;
        }
        // u64 elements keep the buffer aligned for the detail structure.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8) + 1];
        let detail = buffer
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: the buffer is large enough for `needed` bytes and aligned;
        // `cbSize` must be the size of the fixed part of the structure.
        let filled = unsafe {
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(list.0, &interface, Some(detail), needed, None, None)
        };
        if filled.is_err() {
            continue;
        }
        // SAFETY: `DevicePath` is a NUL terminated string inside the buffer,
        // which holds `needed` bytes starting at `detail`.
        let path = unsafe {
            let start = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
            let offset = start.cast::<u8>().offset_from(buffer.as_ptr().cast::<u8>()) as usize;
            let max_chars = (needed as usize).saturating_sub(offset) / 2;
            let chars = std::slice::from_raw_parts(start, max_chars);
            let len = chars.iter().position(|&c| c == 0).unwrap_or(max_chars);
            let mut path = chars[..len].to_vec();
            path.push(0);
            path
        };
        paths.push(path);
    }
    paths
}

fn open_battery(path: &[u16]) -> Option<Device> {
    // Read access is enough on current Windows; some older drivers insist on
    // the read/write pair the documentation uses. Neither changes anything.
    for access in [GENERIC_READ.0, GENERIC_READ.0 | GENERIC_WRITE.0] {
        // SAFETY: `path` is NUL terminated.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        if let Ok(handle) = handle {
            return Some(Device(handle));
        }
    }
    None
}

/// Sends one control code with a typed request and a raw answer buffer.
/// Returns the number of bytes answered.
fn control<I>(device: &Device, code: u32, input: &I, output: &mut [u8]) -> Option<usize> {
    let mut returned = 0u32;
    // SAFETY: `input` and `output` are valid for their stated sizes; the
    // battery control codes used here only query.
    unsafe {
        DeviceIoControl(
            device.0,
            code,
            Some(std::ptr::from_ref(input).cast()),
            std::mem::size_of::<I>() as u32,
            Some(output.as_mut_ptr().cast()),
            output.len() as u32,
            Some(&mut returned),
            None,
        )
    }
    .ok()?;
    Some(returned as usize)
}

fn query_text(device: &Device, tag: u32, level: BATTERY_QUERY_INFORMATION_LEVEL) -> Option<String> {
    let request = BATTERY_QUERY_INFORMATION {
        BatteryTag: tag,
        InformationLevel: level,
        AtRate: 0,
    };
    let mut answer = [0u8; 512];
    let len = control(
        device,
        IOCTL_BATTERY_QUERY_INFORMATION,
        &request,
        &mut answer,
    )?;
    let chars: Vec<u16> = answer[..len.min(answer.len())]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&c| c != 0)
        .collect();
    let text = String::from_utf16_lossy(&chars).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn read_battery(path: &[u16]) -> Option<RawBattery> {
    let device = open_battery(path)?;
    // The tag identifies the battery currently in the slot; zero is "none".
    let wait = 0u32;
    let mut tag_bytes = [0u8; 4];
    control(&device, IOCTL_BATTERY_QUERY_TAG, &wait, &mut tag_bytes)?;
    let tag = u32::from_le_bytes(tag_bytes);
    if tag == 0 {
        return None;
    }

    let request = BATTERY_QUERY_INFORMATION {
        BatteryTag: tag,
        InformationLevel: BatteryInformation,
        AtRate: 0,
    };
    let mut info = BATTERY_INFORMATION::default();
    // SAFETY: BATTERY_INFORMATION is plain data; viewing it as bytes lets the
    // driver fill it in place.
    let info_bytes = unsafe {
        std::slice::from_raw_parts_mut(
            std::ptr::from_mut(&mut info).cast::<u8>(),
            std::mem::size_of::<BATTERY_INFORMATION>(),
        )
    };
    let len = control(
        &device,
        IOCTL_BATTERY_QUERY_INFORMATION,
        &request,
        info_bytes,
    )?;
    if len < std::mem::size_of::<BATTERY_INFORMATION>() {
        return None;
    }

    let wait_status = BATTERY_WAIT_STATUS {
        BatteryTag: tag,
        ..Default::default()
    };
    let mut status = BATTERY_STATUS::default();
    // SAFETY: as above, BATTERY_STATUS is plain data.
    let status_bytes = unsafe {
        std::slice::from_raw_parts_mut(
            std::ptr::from_mut(&mut status).cast::<u8>(),
            std::mem::size_of::<BATTERY_STATUS>(),
        )
    };
    let status = control(
        &device,
        IOCTL_BATTERY_QUERY_STATUS,
        &wait_status,
        status_bytes,
    )
    .filter(|&len| len >= std::mem::size_of::<BATTERY_STATUS>())
    .map(|_| {
        (
            status.PowerState,
            status.Capacity,
            status.Voltage,
            status.Rate,
        )
    });

    Some(RawBattery {
        capabilities: info.Capabilities,
        chemistry: info.Chemistry,
        designed_capacity: info.DesignedCapacity,
        full_charged_capacity: info.FullChargedCapacity,
        cycle_count: info.CycleCount,
        status,
        name: query_text(&device, tag, BatteryDeviceName),
        manufacturer: query_text(&device, tag, BatteryManufactureName),
    })
}

/// Reads the batteries that power this PC; empty on a desktop.
pub(crate) fn collect_batteries() -> Vec<Battery> {
    battery_device_paths()
        .iter()
        .filter_map(|path| read_battery(path))
        .filter_map(|raw| battery_from(&raw))
        .collect()
}

/// Whether the PC is on mains power, when Windows can tell.
pub(crate) fn on_mains_power() -> Option<bool> {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid out parameter.
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    match status.ACLineStatus {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laptop_battery() -> RawBattery {
        RawBattery {
            capabilities: BATTERY_SYSTEM_BATTERY,
            chemistry: *b"LION",
            designed_capacity: 53_500,
            full_charged_capacity: 42_800,
            cycle_count: 312,
            status: Some((
                BATTERY_POWER_ON_LINE | BATTERY_CHARGING,
                21_400,
                12_585,
                9_325,
            )),
            name: Some("  Example Pack ".to_owned()),
            manufacturer: Some("Example Cells".to_owned()),
        }
    }

    #[test]
    fn a_worn_laptop_battery() {
        let battery = battery_from(&laptop_battery()).unwrap();
        assert_eq!(battery.name.as_deref(), Some("Example Pack"));
        assert_eq!(battery.chemistry.as_deref(), Some("Lithium-ion"));
        assert_eq!(battery.design_mwh, Some(53_500));
        assert_eq!(battery.full_charge_mwh, Some(42_800));
        assert_eq!(battery.remaining_mwh, Some(21_400));
        assert!((battery.charge_percent.unwrap() - 50.0).abs() < 0.01);
        assert!((battery.wear_percent().unwrap() - 20.0).abs() < 0.01);
        assert!((battery.health_percent().unwrap() - 80.0).abs() < 0.01);
        assert_eq!(battery.cycle_count, Some(312));
        assert_eq!(battery.voltage_mv, Some(12_585));
        assert_eq!(battery.rate_mw, Some(9_325));
        assert_eq!(battery.state, BatteryState::Charging);
    }

    #[test]
    fn missing_figures_stay_unknown() {
        let raw = RawBattery {
            designed_capacity: UNKNOWN_CAPACITY,
            cycle_count: 0,
            chemistry: [0; 4],
            status: Some((
                BATTERY_DISCHARGING,
                UNKNOWN_CAPACITY,
                UNKNOWN_VOLTAGE,
                UNKNOWN_RATE,
            )),
            name: Some("Default string".to_owned()),
            ..laptop_battery()
        };
        let battery = battery_from(&raw).unwrap();
        assert_eq!(battery.design_mwh, None);
        assert_eq!(battery.wear_percent(), None);
        assert_eq!(
            battery.cycle_count, None,
            "zero means the battery does not count"
        );
        assert_eq!(battery.chemistry, None);
        assert_eq!(battery.name, None);
        assert_eq!(battery.charge_percent, None);
        assert_eq!(battery.voltage_mv, None);
        assert_eq!(battery.rate_mw, None);
        assert_eq!(battery.state, BatteryState::Discharging);

        // No status at all still gives the static figures.
        let battery = battery_from(&RawBattery {
            status: None,
            ..laptop_battery()
        })
        .unwrap();
        assert_eq!(battery.state, BatteryState::Unknown);
        assert_eq!(battery.full_charge_mwh, Some(42_800));
    }

    #[test]
    fn a_new_battery_can_exceed_its_design_capacity() {
        let raw = RawBattery {
            full_charged_capacity: 55_000,
            status: Some((BATTERY_POWER_ON_LINE, 55_000, 12_000, 0)),
            ..laptop_battery()
        };
        let battery = battery_from(&raw).unwrap();
        assert_eq!(battery.wear_percent(), Some(0.0));
        assert_eq!(battery.charge_percent, Some(100.0));
        assert_eq!(battery.state, BatteryState::Idle);
    }

    #[test]
    fn relative_capacities_and_foreign_batteries() {
        let relative = RawBattery {
            capabilities: BATTERY_SYSTEM_BATTERY | BATTERY_CAPACITY_RELATIVE,
            designed_capacity: 100,
            full_charged_capacity: 100,
            status: Some((BATTERY_DISCHARGING, 64, 11_000, -500)),
            ..laptop_battery()
        };
        let battery = battery_from(&relative).unwrap();
        assert_eq!(battery.design_mwh, None, "relative figures are not mWh");
        assert_eq!(battery.rate_mw, None);
        assert!((battery.charge_percent.unwrap() - 64.0).abs() < 0.01);

        let ups = RawBattery {
            capabilities: BATTERY_SYSTEM_BATTERY | BATTERY_IS_SHORT_TERM,
            ..laptop_battery()
        };
        assert_eq!(battery_from(&ups), None);
        let peripheral = RawBattery {
            capabilities: 0,
            ..laptop_battery()
        };
        assert_eq!(battery_from(&peripheral), None);
    }

    #[test]
    fn chemistry_names() {
        assert_eq!(chemistry_name(b"LiP\0").as_deref(), Some("Lithium polymer"));
        assert_eq!(
            chemistry_name(b"NiMH").as_deref(),
            Some("Nickel-metal hydride")
        );
        assert_eq!(chemistry_name(b"XyZ\0").as_deref(), Some("XyZ"));
        assert_eq!(chemistry_name(&[0; 4]), None);
    }

    #[test]
    fn this_pc_answers() {
        // A desktop has none; a laptop's figures must be plausible.
        for battery in collect_batteries() {
            if let Some(percent) = battery.charge_percent {
                assert!((0.0..=100.0).contains(&percent));
            }
            if let (Some(design), Some(full)) = (battery.design_mwh, battery.full_charge_mwh) {
                assert!(design > 1_000 && full > 500, "{design} / {full}");
            }
        }
        let _ = on_mains_power();
    }
}
