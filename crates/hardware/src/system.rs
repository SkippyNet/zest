//! The computer itself: maker and model, firmware, Windows, processors and
//! memory.

use crate::model::{Chassis, FirmwareKind, Memory, MemoryModule, Processor, SystemInfo};
use crate::wmi::{Record, Wmi, parse_date, parse_time};
use windows::Win32::System::SystemInformation::{
    FIRMWARE_TYPE, FirmwareTypeBios, FirmwareTypeUefi, GetFirmwareType, GetTickCount64,
    GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use zest_core::reg::RegPath;

/// Texts that board makers leave in firmware fields they never filled in.
const PLACEHOLDERS: [&str; 22] = [
    "to be filled by o.e.m.",
    "to be filled by oem",
    "default string",
    "system product name",
    "system manufacturer",
    "system version",
    "system serial number",
    "base board product name",
    "base board manufacturer",
    "not specified",
    "not applicable",
    "not available",
    "no asset tag",
    "undefined",
    "unknown",
    "invalid",
    "none",
    "n/a",
    "oem",
    "o.e.m.",
    "empty",
    "x.x",
];

/// Tidies a firmware-provided text: collapses runs of spaces and drops
/// placeholders, so `None` means "the PC does not say".
pub(crate) fn clean(text: Option<String>) -> Option<String> {
    let text = text?;
    let tidy = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = tidy.to_ascii_lowercase();
    if tidy.is_empty()
        || PLACEHOLDERS.contains(&lower.as_str())
        || tidy.chars().all(|c| c == '0' || c == '-' || c == '.')
    {
        return None;
    }
    Some(tidy)
}

/// Shortens the legal names some makers put in the firmware.
fn short_maker(name: Option<String>) -> Option<String> {
    let name = clean(name)?;
    const SUFFIXES: [&str; 12] = [
        " international co., ltd.",
        " computer corporation",
        " computer inc.",
        " technology co., ltd.",
        " electronics co., ltd.",
        " co., ltd.",
        " corporation",
        " incorporated",
        ", inc.",
        " inc.",
        " corp.",
        ", llc.",
    ];
    let lower = name.to_ascii_lowercase();
    for suffix in SUFFIXES {
        if lower.ends_with(suffix) && name.len() > suffix.len() {
            return Some(name[..name.len() - suffix.len()].trim().to_owned());
        }
    }
    Some(name)
}

fn chassis_of(system: &Record, maker: Option<&str>, model: Option<&str>) -> Chassis {
    let haystack = format!(
        "{} {}",
        maker.unwrap_or_default().to_ascii_lowercase(),
        model.unwrap_or_default().to_ascii_lowercase()
    );
    const VIRTUAL: [&str; 10] = [
        "virtual",
        "vmware",
        "kvm",
        "qemu",
        "xen",
        "hvm domu",
        "parallels",
        "bochs",
        "bhyve",
        "google compute engine",
    ];
    if VIRTUAL.iter().any(|mark| haystack.contains(mark)) {
        return Chassis::VirtualMachine;
    }
    match system.u32("PCSystemType") {
        Some(1 | 3 | 6) => Chassis::Desktop,
        Some(2) => Chassis::Laptop,
        Some(4 | 5 | 7) => Chassis::Server,
        Some(8) => Chassis::Tablet,
        _ => Chassis::Unknown,
    }
}

/// Builds the system description from the WMI records that describe it.
pub(crate) fn system_from(
    computer: Option<&Record>,
    bios: Option<&Record>,
    board: Option<&Record>,
    os: Option<&Record>,
) -> SystemInfo {
    let mut info = SystemInfo::default();
    if let Some(computer) = computer {
        info.manufacturer = short_maker(computer.text("Manufacturer"));
        info.model = clean(computer.text("Model"));
        info.chassis = chassis_of(
            computer,
            info.manufacturer.as_deref(),
            info.model.as_deref(),
        );
    }
    if let Some(bios) = bios {
        info.firmware_vendor = short_maker(bios.text("Manufacturer"));
        info.firmware_version = clean(bios.text("SMBIOSBIOSVersion"));
        info.firmware_date = bios.text("ReleaseDate").and_then(|t| parse_date(&t));
    }
    if let Some(board) = board {
        let maker = short_maker(board.text("Manufacturer"));
        let product = clean(board.text("Product"));
        info.mainboard = match (maker, product) {
            (Some(maker), Some(product)) => Some(format!("{maker} {product}")),
            (None, Some(product)) => Some(product),
            (maker, None) => maker,
        };
    }
    if let Some(os) = os {
        info.os_name = os.text("Caption").map(|caption| {
            caption
                .trim_start_matches("Microsoft ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
        info.os_architecture = os.text("OSArchitecture").map(|arch| {
            let lower = arch.to_ascii_lowercase();
            if lower.contains("arm") {
                "ARM 64-bit".to_owned()
            } else if lower.contains("64") {
                "64-bit".to_owned()
            } else if lower.contains("32") {
                "32-bit".to_owned()
            } else {
                arch
            }
        });
        info.os_installed = os.text("InstallDate").and_then(|t| parse_date(&t));
        info.last_boot = os.text("LastBootUpTime").and_then(|t| parse_time(&t));
        info.os_build = os.text("BuildNumber");
    }
    info
}

/// Reads the system description from this PC.
pub(crate) fn collect_system(wmi: Option<&Wmi>) -> SystemInfo {
    let one = |class: &str, properties: &[&str]| -> Option<Record> {
        wmi?.query(class, properties, None).ok()?.into_iter().next()
    };
    let computer = one(
        "Win32_ComputerSystem",
        &["Manufacturer", "Model", "PCSystemType"],
    );
    let bios = one(
        "Win32_BIOS",
        &["Manufacturer", "SMBIOSBIOSVersion", "ReleaseDate"],
    );
    let board = one("Win32_BaseBoard", &["Manufacturer", "Product"]);
    let os = one(
        "Win32_OperatingSystem",
        &[
            "Caption",
            "BuildNumber",
            "OSArchitecture",
            "InstallDate",
            "LastBootUpTime",
        ],
    );
    let mut info = system_from(
        computer.as_ref(),
        bios.as_ref(),
        board.as_ref(),
        os.as_ref(),
    );

    let version = RegPath::hklm(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    info.os_release = version
        .read_string("DisplayVersion")
        .or_else(|| version.read_string("ReleaseId"))
        .filter(|release| !release.trim().is_empty());
    let build = version
        .read_string("CurrentBuildNumber")
        .or(info.os_build.take());
    info.os_build = match (build, version.read_u32("UBR")) {
        (Some(build), Some(revision)) => Some(format!("{build}.{revision}")),
        (build, _) => build,
    };
    if info.os_name.is_none() {
        info.os_name = version.read_string("ProductName");
    }
    if info.os_installed.is_none() {
        info.os_installed = version
            .read_u32("InstallDate")
            .and_then(|seconds| chrono::DateTime::from_timestamp(i64::from(seconds), 0))
            .map(|utc| utc.with_timezone(&chrono::Local).date_naive());
    }

    info.firmware_kind = firmware_kind();
    info.secure_boot = RegPath::hklm(r"SYSTEM\CurrentControlSet\Control\SecureBoot\State")
        .read_u32("UEFISecureBootEnabled")
        .map(|state| state != 0);
    info.tpm_version = tpm_version();
    // SAFETY: no arguments, no failure mode.
    info.uptime_seconds = unsafe { GetTickCount64() } / 1000;
    info
}

fn firmware_kind() -> FirmwareKind {
    let mut kind = FIRMWARE_TYPE::default();
    // SAFETY: `kind` is a valid out parameter.
    if unsafe { GetFirmwareType(&mut kind) }.is_err() {
        return FirmwareKind::Unknown;
    }
    if kind == FirmwareTypeUefi {
        FirmwareKind::Uefi
    } else if kind == FirmwareTypeBios {
        FirmwareKind::LegacyBios
    } else {
        FirmwareKind::Unknown
    }
}

/// Version of the TPM, asked from the TPM base services (works without
/// administrator rights). `None` when the PC has no usable TPM.
fn tpm_version() -> Option<String> {
    use windows::Win32::System::TpmBaseServices::{TPM_DEVICE_INFO, Tbsi_GetDeviceInfo};
    let mut info = TPM_DEVICE_INFO::default();
    // SAFETY: the buffer is a valid TPM_DEVICE_INFO of the stated size.
    let status = unsafe {
        Tbsi_GetDeviceInfo(
            std::mem::size_of::<TPM_DEVICE_INFO>() as u32,
            std::ptr::from_mut(&mut info).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    match info.tpmVersion {
        1 => Some("1.2".to_owned()),
        2 => Some("2.0".to_owned()),
        _ => None,
    }
}

fn maker_of_processor(raw: Option<String>) -> Option<String> {
    let raw = clean(raw)?;
    Some(
        match raw.as_str() {
            "GenuineIntel" => "Intel",
            "AuthenticAMD" => "AMD",
            other => other,
        }
        .to_owned(),
    )
}

/// Builds the processor list from `Win32_Processor` records.
/// `hypervisor_running` is `Win32_ComputerSystem.HypervisorPresent`.
pub(crate) fn processors_from(
    records: &[Record],
    hypervisor_running: Option<bool>,
) -> Vec<Processor> {
    records
        .iter()
        .filter_map(|record| {
            let name = clean(record.text("Name"))?;
            // While Windows itself runs on its hypervisor the firmware flag
            // reads "off", although virtualisation is plainly on.
            let virtualization = match (
                hypervisor_running,
                record.bool("VirtualizationFirmwareEnabled"),
            ) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (_, Some(false)) => Some(false),
                _ => None,
            };
            Some(Processor {
                name,
                manufacturer: maker_of_processor(record.text("Manufacturer")),
                cores: record.u32("NumberOfCores").filter(|&n| n > 0),
                threads: record.u32("NumberOfLogicalProcessors").filter(|&n| n > 0),
                base_mhz: record.u32("MaxClockSpeed").filter(|&n| n > 0),
                architecture: record.u32("Architecture").and_then(|code| {
                    Some(
                        match code {
                            0 => "x86",
                            5 => "ARM",
                            9 => "x64",
                            12 => "ARM64",
                            _ => return None,
                        }
                        .to_owned(),
                    )
                }),
                l2_cache_kb: record.u32("L2CacheSize").filter(|&n| n > 0),
                l3_cache_kb: record.u32("L3CacheSize").filter(|&n| n > 0),
                socket: clean(record.text("SocketDesignation")),
                virtualization,
            })
        })
        .collect()
}

/// Reads the processors of this PC.
pub(crate) fn collect_processors(wmi: Option<&Wmi>) -> Vec<Processor> {
    let Some(wmi) = wmi else {
        return Vec::new();
    };
    let hypervisor = wmi
        .query("Win32_ComputerSystem", &["HypervisorPresent"], None)
        .ok()
        .and_then(|rows| rows.first().and_then(|row| row.bool("HypervisorPresent")));
    let records = wmi
        .query(
            "Win32_Processor",
            &[
                "Name",
                "Manufacturer",
                "NumberOfCores",
                "NumberOfLogicalProcessors",
                "MaxClockSpeed",
                "Architecture",
                "L2CacheSize",
                "L3CacheSize",
                "SocketDesignation",
                "VirtualizationFirmwareEnabled",
            ],
            None,
        )
        .unwrap_or_default();
    processors_from(&records, hypervisor)
}

/// Names for the SMBIOS memory type codes in use today.
fn memory_kind(code: u32) -> Option<&'static str> {
    Some(match code {
        20 => "DDR",
        21 => "DDR2",
        24 => "DDR3",
        26 => "DDR4",
        27 => "LPDDR",
        28 => "LPDDR2",
        29 => "LPDDR3",
        30 => "LPDDR4",
        34 => "DDR5",
        35 => "LPDDR5",
        _ => return None,
    })
}

fn memory_form(code: u32) -> Option<&'static str> {
    Some(match code {
        8 => "DIMM",
        12 => "SO-DIMM",
        _ => return None,
    })
}

/// Module makers often leave their JEDEC identifier instead of a name.
fn memory_maker(raw: Option<String>) -> Option<String> {
    let raw = clean(raw)?;
    let code = raw.trim_start_matches("0x").to_ascii_uppercase();
    let looks_like_code = code.len() >= 4 && code.chars().all(|c| c.is_ascii_hexdigit());
    if !looks_like_code {
        return Some(raw);
    }
    const JEDEC: [(&str, &str); 10] = [
        ("80AD", "SK Hynix"),
        ("AD00", "SK Hynix"),
        ("802C", "Micron"),
        ("2C00", "Micron"),
        ("80CE", "Samsung"),
        ("CE00", "Samsung"),
        ("859B", "Crucial"),
        ("0198", "Kingston"),
        ("029E", "Corsair"),
        ("04CD", "G.Skill"),
    ];
    JEDEC
        .iter()
        .find(|(prefix, _)| code.starts_with(prefix))
        .map(|(_, name)| (*name).to_owned())
}

/// Builds the module list from `Win32_PhysicalMemory` records.
pub(crate) fn modules_from(records: &[Record]) -> Vec<MemoryModule> {
    records
        .iter()
        .filter_map(|record| {
            let capacity_bytes = record.u64("Capacity").filter(|&bytes| bytes > 0)?;
            let rated = record.u32("Speed").filter(|&mhz| mhz > 0);
            let running = record.u32("ConfiguredClockSpeed").filter(|&mhz| mhz > 0);
            Some(MemoryModule {
                slot: clean(record.text("DeviceLocator"))
                    .or_else(|| clean(record.text("BankLabel"))),
                capacity_bytes,
                speed_mhz: running.or(rated),
                rated_mhz: rated,
                kind: record
                    .u32("SMBIOSMemoryType")
                    .and_then(memory_kind)
                    .map(str::to_owned),
                form: record
                    .u32("FormFactor")
                    .and_then(memory_form)
                    .map(str::to_owned),
                manufacturer: memory_maker(record.text("Manufacturer")),
                part_number: clean(record.text("PartNumber")),
            })
        })
        .collect()
}

/// Reads the memory of this PC.
pub(crate) fn collect_memory(wmi: Option<&Wmi>) -> Memory {
    let mut memory = Memory::default();
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a valid, correctly sized out parameter.
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
        memory.usable_bytes = status.ullTotalPhys;
        memory.available_bytes = status.ullAvailPhys;
    }
    if let Some(wmi) = wmi {
        let records = wmi
            .query(
                "Win32_PhysicalMemory",
                &[
                    "DeviceLocator",
                    "BankLabel",
                    "Capacity",
                    "Speed",
                    "ConfiguredClockSpeed",
                    "SMBIOSMemoryType",
                    "FormFactor",
                    "Manufacturer",
                    "PartNumber",
                ],
                None,
            )
            .unwrap_or_default();
        memory.modules = modules_from(&records);
        memory.installed_bytes = memory.modules.iter().map(|m| m.capacity_bytes).sum();
        let slots: u32 = wmi
            .query("Win32_PhysicalMemoryArray", &["MemoryDevices"], None)
            .unwrap_or_default()
            .iter()
            .filter_map(|array| array.u32("MemoryDevices"))
            .sum();
        // A board never has fewer slots than modules; such a figure is noise.
        memory.slots = (slots as usize >= memory.modules.len() && slots > 0).then_some(slots);
    }
    memory
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi::Value;

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    #[test]
    fn placeholders_and_spacing_are_cleaned() {
        assert_eq!(
            clean(Some("  Example   Model  9 ".to_owned())).as_deref(),
            Some("Example Model 9")
        );
        for junk in [
            "To Be Filled By O.E.M.",
            "Default string",
            "System Product Name",
            "",
            "   ",
            "None",
            "0000-0000",
        ] {
            assert_eq!(clean(Some(junk.to_owned())), None, "{junk:?}");
        }
        assert_eq!(clean(None), None);
        assert_eq!(
            short_maker(Some("Example International Co., Ltd.".to_owned())).as_deref(),
            Some("Example")
        );
        assert_eq!(
            short_maker(Some("Sample Megatrends, LLC.".to_owned())).as_deref(),
            Some("Sample Megatrends")
        );
        assert_eq!(
            short_maker(Some("Inc.".to_owned())).as_deref(),
            Some("Inc.")
        );
    }

    #[test]
    fn a_laptop_is_described_from_its_records() {
        let computer = Record::from_pairs(&[
            ("Manufacturer", text("Example Computer Inc.")),
            ("Model", text("Featherbook  14")),
            ("PCSystemType", Value::Int(2)),
        ]);
        let bios = Record::from_pairs(&[
            ("Manufacturer", text("Example Firmware Corp.")),
            ("SMBIOSBIOSVersion", text("F.42")),
            ("ReleaseDate", text("20250314000000.000000+000")),
        ]);
        let board = Record::from_pairs(&[
            ("Manufacturer", text("Example Computer Inc.")),
            ("Product", text("FB14-MB")),
        ]);
        let os = Record::from_pairs(&[
            ("Caption", text("Microsoft Windows 11 Pro")),
            ("BuildNumber", text("26200")),
            ("OSArchitecture", text("64-bit")),
            ("InstallDate", text("20250612103150.000000-240")),
        ]);
        let info = system_from(Some(&computer), Some(&bios), Some(&board), Some(&os));
        assert_eq!(info.manufacturer.as_deref(), Some("Example"));
        assert_eq!(info.model.as_deref(), Some("Featherbook 14"));
        assert_eq!(info.chassis, Chassis::Laptop);
        assert_eq!(info.mainboard.as_deref(), Some("Example FB14-MB"));
        assert_eq!(info.firmware_vendor.as_deref(), Some("Example Firmware"));
        assert_eq!(info.firmware_version.as_deref(), Some("F.42"));
        assert_eq!(info.firmware_date.unwrap().to_string(), "2025-03-14");
        assert_eq!(info.os_name.as_deref(), Some("Windows 11 Pro"));
        assert_eq!(info.os_architecture.as_deref(), Some("64-bit"));
        assert_eq!(info.os_installed.unwrap().to_string(), "2025-06-12");
    }

    #[test]
    fn home_built_desktops_and_virtual_machines() {
        let diy = Record::from_pairs(&[
            ("Manufacturer", text("System manufacturer")),
            ("Model", text("System Product Name")),
            ("PCSystemType", Value::Int(1)),
        ]);
        let info = system_from(Some(&diy), None, None, None);
        assert_eq!(info.manufacturer, None);
        assert_eq!(info.model, None);
        assert_eq!(info.chassis, Chassis::Desktop);

        let vm = Record::from_pairs(&[
            ("Manufacturer", text("Microsoft Corporation")),
            ("Model", text("Virtual Machine")),
            ("PCSystemType", Value::Int(1)),
        ]);
        assert_eq!(
            system_from(Some(&vm), None, None, None).chassis,
            Chassis::VirtualMachine
        );

        // Nothing at all still yields a usable, empty description.
        let empty = system_from(None, None, None, None);
        assert_eq!(empty, SystemInfo::default());

        let arm = Record::from_pairs(&[("OSArchitecture", text("ARM 64-bit Processor"))]);
        assert_eq!(
            system_from(None, None, None, Some(&arm))
                .os_architecture
                .as_deref(),
            Some("ARM 64-bit")
        );
    }

    #[test]
    fn processors() {
        let record = Record::from_pairs(&[
            ("Name", text("Example(R) Core(TM)   i7-0000H  ")),
            ("Manufacturer", text("GenuineIntel")),
            ("NumberOfCores", Value::Int(10)),
            ("NumberOfLogicalProcessors", Value::Int(16)),
            ("MaxClockSpeed", Value::Int(2400)),
            ("Architecture", Value::Int(9)),
            ("L2CacheSize", Value::Int(9728)),
            ("L3CacheSize", Value::Int(24576)),
            ("SocketDesignation", text("U3E1")),
            ("VirtualizationFirmwareEnabled", Value::Bool(false)),
        ]);
        let list = processors_from(std::slice::from_ref(&record), Some(true));
        assert_eq!(list.len(), 1);
        let cpu = &list[0];
        assert_eq!(cpu.name, "Example(R) Core(TM) i7-0000H");
        assert_eq!(cpu.manufacturer.as_deref(), Some("Intel"));
        assert_eq!((cpu.cores, cpu.threads), (Some(10), Some(16)));
        assert_eq!(cpu.architecture.as_deref(), Some("x64"));
        assert_eq!(
            cpu.virtualization,
            Some(true),
            "a running hypervisor proves virtualisation is on"
        );

        let off = processors_from(std::slice::from_ref(&record), Some(false));
        assert_eq!(off[0].virtualization, Some(false));
        let unknown = Record::from_pairs(&[("Name", text("Mystery CPU"))]);
        let list = processors_from(&[unknown], None);
        assert_eq!(list[0].virtualization, None);
        assert_eq!(list[0].cores, None);
        // A record without a name is not a processor.
        assert!(processors_from(&[Record::default()], None).is_empty());
    }

    #[test]
    fn memory_modules() {
        let records = [
            Record::from_pairs(&[
                ("DeviceLocator", text("Controller0-ChannelA-DIMM0")),
                ("BankLabel", text("BANK 0")),
                ("Capacity", text("8589934592")),
                ("Speed", Value::Int(5600)),
                ("ConfiguredClockSpeed", Value::Int(5200)),
                ("SMBIOSMemoryType", Value::Int(34)),
                ("FormFactor", Value::Int(12)),
                ("Manufacturer", text("80CE000080CE")),
                ("PartNumber", text("M425R1GB4BB0-CWMOD   ")),
            ]),
            // An empty slot is reported with no capacity and must be skipped.
            Record::from_pairs(&[("DeviceLocator", text("DIMM 2")), ("Capacity", text("0"))]),
            Record::from_pairs(&[
                ("BankLabel", text("BANK 1")),
                ("Capacity", text("17179869184")),
                ("Speed", Value::Int(3200)),
                ("SMBIOSMemoryType", Value::Int(26)),
                ("FormFactor", Value::Int(8)),
                ("Manufacturer", text("Example Memory")),
            ]),
        ];
        let modules = modules_from(&records);
        assert_eq!(modules.len(), 2);
        assert_eq!(
            modules[0].slot.as_deref(),
            Some("Controller0-ChannelA-DIMM0")
        );
        assert_eq!(modules[0].capacity_bytes, 8 << 30);
        assert_eq!(
            (modules[0].speed_mhz, modules[0].rated_mhz),
            (Some(5200), Some(5600))
        );
        assert_eq!(modules[0].kind.as_deref(), Some("DDR5"));
        assert_eq!(modules[0].form.as_deref(), Some("SO-DIMM"));
        assert_eq!(modules[0].manufacturer.as_deref(), Some("Samsung"));
        assert_eq!(
            modules[0].part_number.as_deref(),
            Some("M425R1GB4BB0-CWMOD")
        );
        assert_eq!(modules[1].slot.as_deref(), Some("BANK 1"));
        assert_eq!(modules[1].speed_mhz, Some(3200));
        assert_eq!(modules[1].kind.as_deref(), Some("DDR4"));
        assert_eq!(modules[1].manufacturer.as_deref(), Some("Example Memory"));
        assert_eq!(
            memory_maker(Some("FFFF00000000".to_owned())),
            None,
            "unknown code"
        );
    }

    #[test]
    fn this_pc_answers() {
        let wmi = Wmi::connect(r"ROOT\CIMV2").ok();
        let system = collect_system(wmi.as_ref());
        assert!(
            system
                .os_name
                .as_deref()
                .is_some_and(|n| n.contains("Windows"))
        );
        assert!(system.os_build.is_some());
        assert!(system.uptime_seconds > 0);
        let processors = collect_processors(wmi.as_ref());
        assert!(!processors.is_empty());
        assert!(processors[0].threads.unwrap_or(0) >= 1);
        let memory = collect_memory(wmi.as_ref());
        assert!(memory.usable_bytes > 512 << 20);
        assert!(memory.available_bytes <= memory.usable_bytes);
        assert!(memory.used_fraction() > 0.0 && memory.used_fraction() <= 1.0);
        // Without WMI the Win32 parts still work.
        let offline = collect_memory(None);
        assert!(offline.usable_bytes > 0 && offline.modules.is_empty());
        assert!(collect_system(None).uptime_seconds > 0);
    }
}
