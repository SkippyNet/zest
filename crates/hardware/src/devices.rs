//! Graphics adapters, displays and network adapters.

use crate::model::{Display, GraphicsAdapter, NetworkAdapter, NetworkKind};
use crate::system::clean;
use crate::wmi::{Record, Wmi, parse_date};

/// A graphics adapter as DXGI describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DxgiAdapter {
    pub(crate) name: String,
    pub(crate) vendor_id: u32,
    pub(crate) dedicated_bytes: u64,
}

fn vendor_name(id: u32) -> Option<&'static str> {
    Some(match id {
        0x10DE => "NVIDIA",
        0x8086 => "Intel",
        0x1002 | 0x1022 => "AMD",
        0x5143 | 0x4D4F_4351 => "Qualcomm",
        0x13B5 => "Arm",
        0x1414 => "Microsoft",
        0x15AD => "VMware",
        _ => return None,
    })
}

/// Lists the hardware graphics adapters through DXGI, which reports their
/// memory correctly (the WMI figure stops at 4 GB).
fn dxgi_adapters() -> Vec<DxgiAdapter> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
    };
    let mut adapters = Vec::new();
    // SAFETY: plain DXGI enumeration; every interface is released on drop.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return adapters;
        };
        // The index simply runs until DXGI reports "not found".
        for index in 0..32 {
            let Ok(adapter) = factory.EnumAdapters1(index) else {
                break;
            };
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let len = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..len])
                .trim()
                .to_owned();
            if name.is_empty() || adapters.iter().any(|a: &DxgiAdapter| a.name == name) {
                continue;
            }
            adapters.push(DxgiAdapter {
                name,
                vendor_id: desc.VendorId,
                dedicated_bytes: desc.DedicatedVideoMemory as u64,
            });
        }
    }
    adapters
}

/// Memory sizes the display drivers registered for their adapters, by
/// adapter name. This is the size printed on the box; DXGI reports a little
/// less because the driver keeps part for itself.
fn registered_memory() -> Vec<(String, u64)> {
    const DISPLAY_CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let Ok(class) = zest_core::reg::RegPath::hklm(DISPLAY_CLASS).open() else {
        return Vec::new();
    };
    class
        .enum_keys()
        .flatten()
        .filter_map(|name| {
            let key = class.open_subkey(&name).ok()?;
            let adapter: String = key.get_value("DriverDesc").ok()?;
            let bytes: u64 = key.get_value("HardwareInformation.qwMemorySize").ok()?;
            Some((adapter, bytes))
        })
        .collect()
}

/// Joins what DXGI knows (name, maker, memory) with what the driver database
/// knows (`Win32_VideoController`: driver version and date). `registered`
/// holds the memory sizes the drivers registered, which win when larger.
pub(crate) fn merge_adapters(
    dxgi: Vec<DxgiAdapter>,
    drivers: &[Record],
    registered: &[(String, u64)],
) -> Vec<GraphicsAdapter> {
    let registered_for = |name: &str| {
        registered
            .iter()
            .find(|(adapter, _)| adapter.eq_ignore_ascii_case(name))
            .map(|(_, bytes)| *bytes)
    };
    let driver_for = |name: &str| {
        drivers.iter().find(|record| {
            record
                .text("Name")
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
    };
    let mut adapters: Vec<GraphicsAdapter> = dxgi
        .iter()
        .map(|adapter| {
            let driver = driver_for(&adapter.name);
            GraphicsAdapter {
                name: adapter.name.clone(),
                vendor: vendor_name(adapter.vendor_id).map(str::to_owned),
                dedicated_memory_bytes: Some(
                    adapter
                        .dedicated_bytes
                        .max(registered_for(&adapter.name).unwrap_or(0)),
                ),
                driver_version: driver.and_then(|d| clean(d.text("DriverVersion"))),
                driver_date: driver
                    .and_then(|d| d.text("DriverDate"))
                    .and_then(|t| parse_date(&t)),
            }
        })
        .collect();
    // Adapters only the driver database knows about (no DXGI, e.g. in a
    // session without graphics access).
    for record in drivers {
        let Some(name) = clean(record.text("Name")) else {
            continue;
        };
        if adapters.iter().any(|a| a.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        adapters.push(GraphicsAdapter {
            name,
            vendor: None,
            dedicated_memory_bytes: record.u64("AdapterRAM").filter(|&bytes| bytes > 0),
            driver_version: clean(record.text("DriverVersion")),
            driver_date: record.text("DriverDate").and_then(|t| parse_date(&t)),
        });
    }
    // The strongest adapter first.
    adapters.sort_by_key(|a| std::cmp::Reverse(a.dedicated_memory_bytes.unwrap_or(0)));
    adapters
}

/// Reads the graphics adapters of this PC.
pub(crate) fn collect_graphics(wmi: Option<&Wmi>) -> Vec<GraphicsAdapter> {
    let drivers = wmi
        .and_then(|wmi| {
            wmi.query(
                "Win32_VideoController",
                &["Name", "DriverVersion", "DriverDate", "AdapterRAM"],
                None,
            )
            .ok()
        })
        .unwrap_or_default();
    merge_adapters(dxgi_adapters(), &drivers, &registered_memory())
}

/// Reads the active displays of this session.
pub(crate) fn collect_displays() -> Vec<Display> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    let mut displays = Vec::new();
    let (mut path_count, mut mode_count) = (0u32, 0u32);
    // SAFETY: both counts are valid out parameters.
    let sized = unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
    };
    if sized != ERROR_SUCCESS || path_count == 0 {
        return displays;
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    // SAFETY: the buffers hold exactly the element counts passed alongside.
    let queried = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    };
    if queried != ERROR_SUCCESS {
        return displays;
    }
    paths.truncate(path_count as usize);
    modes.truncate(mode_count as usize);

    for path in &paths {
        // SAFETY: for paths returned without the virtual-mode flag the union
        // holds the index of the source mode.
        let mode_index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let Some(mode) = modes.get(mode_index) else {
            continue;
        };
        if mode.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
            continue;
        }
        // SAFETY: `infoType` says the union holds a source mode.
        let (width, height) = unsafe {
            (
                mode.Anonymous.sourceMode.width,
                mode.Anonymous.sourceMode.height,
            )
        };
        if width == 0 || height == 0 {
            continue;
        }
        let rate = path.targetInfo.refreshRate;
        let refresh_hz =
            (rate.Denominator != 0).then(|| rate.Numerator as f32 / rate.Denominator as f32);
        let technology = path.targetInfo.outputTechnology;
        let built_in = [
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        ]
        .contains(&technology);

        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        target.header.adapterId = path.targetInfo.adapterId;
        target.header.id = path.targetInfo.id;
        // SAFETY: the header describes the buffer it is part of.
        let named = unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } == 0;
        let friendly = if named {
            let raw = &target.monitorFriendlyDeviceName;
            let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            String::from_utf16_lossy(&raw[..len]).trim().to_owned()
        } else {
            String::new()
        };
        let name = if !friendly.is_empty() {
            friendly
        } else if built_in {
            "Built-in display".to_owned()
        } else {
            "Display".to_owned()
        };
        displays.push(Display {
            name,
            width,
            height,
            refresh_hz: refresh_hz.filter(|hz| *hz > 1.0),
            built_in,
        });
    }
    // Built-in panel first, then by size.
    displays.sort_by_key(|d| (!d.built_in, std::cmp::Reverse(d.width * d.height)));
    displays
}

fn network_kind(medium: Option<u32>, name: &str) -> NetworkKind {
    match medium {
        Some(1 | 9) => return NetworkKind::WiFi,
        Some(8) => return NetworkKind::Cellular,
        Some(10) => return NetworkKind::Bluetooth,
        Some(14) => return NetworkKind::Ethernet,
        _ => {}
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("wi-fi") || lower.contains("wireless") || lower.contains("802.11") {
        NetworkKind::WiFi
    } else if lower.contains("bluetooth") {
        NetworkKind::Bluetooth
    } else if lower.contains("ethernet") || lower.contains("gbe") || lower.contains("gigabit") {
        NetworkKind::Ethernet
    } else {
        NetworkKind::Other
    }
}

/// Builds the adapter list from `MSFT_NetAdapter` records, keeping only
/// real hardware.
pub(crate) fn network_from(records: &[Record]) -> Vec<NetworkAdapter> {
    let mut adapters: Vec<NetworkAdapter> = records
        .iter()
        .filter(|record| {
            record.bool("HardwareInterface").unwrap_or(false)
                && !record.bool("Virtual").unwrap_or(false)
        })
        .filter_map(|record| {
            let name = clean(record.text("InterfaceDescription"))
                .or_else(|| clean(record.text("Name")))?;
            let connected = record.u32("MediaConnectState") == Some(1);
            Some(NetworkAdapter {
                kind: network_kind(record.u32("NdisPhysicalMedium"), &name),
                link_bps: record
                    .u64("Speed")
                    // Disconnected adapters report zero or an all-ones filler.
                    .filter(|&bps| connected && bps > 0 && bps < u64::MAX / 2),
                connected,
                name,
            })
        })
        .collect();
    adapters.sort_by(|a, b| {
        (!a.connected, a.kind as u8, &a.name).cmp(&(!b.connected, b.kind as u8, &b.name))
    });
    adapters
}

/// Reads the physical network adapters of this PC.
pub(crate) fn collect_network() -> Vec<NetworkAdapter> {
    let Ok(wmi) = Wmi::connect(r"ROOT\StandardCimv2") else {
        return Vec::new();
    };
    let records = wmi
        .query(
            "MSFT_NetAdapter",
            &[
                "Name",
                "InterfaceDescription",
                "NdisPhysicalMedium",
                "MediaConnectState",
                "HardwareInterface",
                "Virtual",
                "Speed",
            ],
            None,
        )
        .unwrap_or_default();
    network_from(&records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi::Value;

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    #[test]
    fn adapters_get_their_memory_from_dxgi_and_their_driver_from_wmi() {
        let dxgi = vec![
            DxgiAdapter {
                name: "Example UHD Graphics".to_owned(),
                vendor_id: 0x8086,
                dedicated_bytes: 128 << 20,
            },
            DxgiAdapter {
                name: "Example RTX 0000".to_owned(),
                vendor_id: 0x10DE,
                // What is left after the driver's own share.
                dedicated_bytes: 5920 << 20,
            },
        ];
        let drivers = [
            Record::from_pairs(&[
                ("Name", text("example rtx 0000")),
                ("DriverVersion", text("32.0.16.1078")),
                ("DriverDate", text("20260711000000.000000-000")),
                // WMI stops at 4 GB; DXGI knows better.
                ("AdapterRAM", Value::Int(4_293_918_720)),
            ]),
            Record::from_pairs(&[
                ("Name", text("Example Remote Display")),
                ("DriverVersion", text("10.0.1")),
                ("AdapterRAM", Value::Int(0)),
            ]),
        ];
        let registered = [
            ("EXAMPLE RTX 0000".to_owned(), 6141u64 << 20),
            ("Example UHD Graphics".to_owned(), 0),
        ];
        let adapters = merge_adapters(dxgi, &drivers, &registered);
        assert_eq!(adapters.len(), 3);
        assert_eq!(adapters[0].name, "Example RTX 0000", "strongest first");
        assert_eq!(adapters[0].vendor.as_deref(), Some("NVIDIA"));
        assert_eq!(
            adapters[0].dedicated_memory_bytes,
            Some(6141 << 20),
            "the registered size wins over the smaller DXGI figure"
        );
        assert_eq!(adapters[1].dedicated_memory_bytes, Some(128 << 20));
        assert_eq!(adapters[0].driver_version.as_deref(), Some("32.0.16.1078"));
        assert_eq!(adapters[0].driver_date.unwrap().to_string(), "2026-07-11");
        assert_eq!(adapters[1].vendor.as_deref(), Some("Intel"));
        assert_eq!(adapters[1].driver_version, None);
        assert_eq!(adapters[2].name, "Example Remote Display");
        assert_eq!(adapters[2].dedicated_memory_bytes, None);
        assert!(merge_adapters(Vec::new(), &[], &[]).is_empty());
    }

    #[test]
    fn only_physical_network_adapters_are_listed() {
        let records = [
            Record::from_pairs(&[
                ("Name", text("Ethernet")),
                (
                    "InterfaceDescription",
                    text("Example GbE Family Controller"),
                ),
                ("NdisPhysicalMedium", Value::Int(14)),
                ("MediaConnectState", Value::Int(2)),
                ("HardwareInterface", Value::Bool(true)),
                ("Virtual", Value::Bool(false)),
                ("Speed", text("18446744073709551615")),
            ]),
            Record::from_pairs(&[
                ("Name", text("Wi-Fi")),
                ("InterfaceDescription", text("Example Wi-Fi 6 AX000")),
                ("NdisPhysicalMedium", Value::Int(9)),
                ("MediaConnectState", Value::Int(1)),
                ("HardwareInterface", Value::Bool(true)),
                ("Virtual", Value::Bool(false)),
                ("Speed", text("1201000000")),
            ]),
            Record::from_pairs(&[
                ("Name", text("vEthernet (Default Switch)")),
                ("InterfaceDescription", text("Virtual Ethernet Adapter")),
                ("MediaConnectState", Value::Int(1)),
                ("HardwareInterface", Value::Bool(false)),
                ("Virtual", Value::Bool(true)),
                ("Speed", text("10000000000")),
            ]),
            Record::from_pairs(&[
                ("Name", text("Bluetooth Network Connection")),
                (
                    "InterfaceDescription",
                    text("Bluetooth Device (Personal Area Network)"),
                ),
                ("NdisPhysicalMedium", Value::Int(10)),
                ("MediaConnectState", Value::Int(2)),
                ("HardwareInterface", Value::Bool(true)),
                ("Virtual", Value::Bool(false)),
            ]),
        ];
        let adapters = network_from(&records);
        assert_eq!(adapters.len(), 3);
        assert_eq!(adapters[0].name, "Example Wi-Fi 6 AX000", "connected first");
        assert_eq!(adapters[0].kind, NetworkKind::WiFi);
        assert_eq!(adapters[0].link_bps, Some(1_201_000_000));
        assert_eq!(adapters[1].kind, NetworkKind::Ethernet);
        assert!(!adapters[1].connected);
        assert_eq!(adapters[1].link_bps, None);
        assert_eq!(adapters[2].kind, NetworkKind::Bluetooth);
        assert_eq!(
            network_kind(None, "Example 802.11ac Adapter"),
            NetworkKind::WiFi
        );
        assert_eq!(
            network_kind(Some(0), "Example Gigabit Adapter"),
            NetworkKind::Ethernet
        );
        assert_eq!(network_kind(None, "Mystery"), NetworkKind::Other);
    }

    #[test]
    fn this_pc_answers() {
        // Counts depend on the machine; the calls must simply not fail.
        let wmi = Wmi::connect(r"ROOT\CIMV2").ok();
        for adapter in collect_graphics(wmi.as_ref()) {
            assert!(!adapter.name.is_empty());
        }
        for display in collect_displays() {
            assert!(display.width > 0 && display.height > 0);
        }
        for adapter in collect_network() {
            assert!(!adapter.name.is_empty());
        }
    }
}
