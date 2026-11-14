//! Hardware inventory, drive health and battery reports for Zest.
//!
//! [`inventory`] takes a read-only look at the PC: the computer and its
//! firmware, Windows, processors, memory, graphics, displays, drives with
//! their health and wear, batteries and network adapters. [`attention`]
//! picks out what a person should act on, and [`to_text`] / [`to_json`]
//! turn the result into a report.
//!
//! Nothing here changes the system, and nothing that identifies the machine
//! or its owner is read: no serial numbers, network addresses, product
//! keys, computer or user names.
//!
//! Every part degrades on its own. A desktop has no battery, a virtual
//! machine has no memory modules, some drive counters are reserved for
//! administrators: the affected fields are simply empty and the rest of the
//! inventory is unaffected.

#![cfg(windows)]

mod attention;
mod battery;
mod devices;
mod model;
mod report;
mod storage;
mod system;
mod wmi;

pub use attention::attention;
pub use model::{
    Attention, Battery, BatteryState, Bus, Chassis, Display, DriveCounters, FirmwareKind,
    GraphicsAdapter, Health, Inventory, Level, Media, Memory, MemoryModule, NetworkAdapter,
    NetworkKind, PhysicalDrive, Processor, SystemInfo, Volume,
};
pub use report::{
    battery_summary, drive_summary, format_capacity, format_energy, format_frequency,
    format_link_speed, format_uptime, to_json, to_text,
};

use zest_core::Progress;

/// Takes the inventory of this PC. Read-only; takes about a second.
pub fn inventory() -> Inventory {
    inventory_with_progress(&Progress::new())
}

/// Like [`inventory`], reporting progress.
///
/// # Progress
/// `total` = groups of hardware to look at, `done` = groups finished.
pub fn inventory_with_progress(progress: &Progress) -> Inventory {
    progress.set_total(4);
    progress.set_done(0);
    progress.set_message("Looking at the hardware");
    let elevated = zest_core::elevation::is_elevated();
    let mut inventory = Inventory {
        collected: Some(chrono::Local::now().naive_local()),
        elevated,
        ..Inventory::default()
    };

    // The groups talk to different parts of Windows and do not depend on
    // each other, so they run side by side; each thread opens its own WMI
    // connection because those cannot be shared between threads.
    std::thread::scope(|scope| {
        let base = scope.spawn(|| {
            let wmi = wmi::Wmi::connect(r"ROOT\CIMV2").ok();
            let found = (
                system::collect_system(wmi.as_ref()),
                system::collect_processors(wmi.as_ref()),
                system::collect_memory(wmi.as_ref()),
                devices::collect_graphics(wmi.as_ref()),
            );
            progress.inc(1);
            found
        });
        let drives = scope.spawn(|| {
            let found = storage::collect_drives(elevated);
            progress.inc(1);
            found
        });
        let network = scope.spawn(|| {
            let found = devices::collect_network();
            progress.inc(1);
            found
        });
        let local = scope.spawn(|| {
            let found = (
                devices::collect_displays(),
                battery::collect_batteries(),
                battery::on_mains_power(),
            );
            progress.inc(1);
            found
        });

        // A group that panics leaves its part of the inventory empty.
        if let Ok((system, processors, memory, graphics)) = base.join() {
            inventory.system = system;
            inventory.processors = processors;
            inventory.memory = memory;
            inventory.graphics = graphics;
        }
        if let Ok((drives, other_volumes)) = drives.join() {
            inventory.drives = drives;
            inventory.other_volumes = other_volumes;
        }
        if let Ok(network) = network.join() {
            inventory.network = network;
        }
        if let Ok((displays, batteries, on_mains_power)) = local.join() {
            inventory.displays = displays;
            inventory.batteries = batteries;
            inventory.on_mains_power = on_mains_power;
        }
    });
    progress.set_message(String::new());
    inventory
}

/// A made-up PC, for documentation screenshots and tests. It has one of
/// everything, including a drive that deserves attention.
pub fn sample() -> Inventory {
    use chrono::NaiveDate;
    let date = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
    let volume = |letter, label: &str, total_gb: u64, free_gb: u64, is_system| Volume {
        letter,
        label: label.to_owned(),
        file_system: "NTFS".to_owned(),
        total_bytes: total_gb << 30,
        free_bytes: free_gb << 30,
        is_system,
    };
    let module = |slot: &str| MemoryModule {
        slot: Some(slot.to_owned()),
        capacity_bytes: 16 << 30,
        speed_mhz: Some(5200),
        rated_mhz: Some(5600),
        kind: Some("DDR5".to_owned()),
        form: Some("SO-DIMM".to_owned()),
        manufacturer: Some("Kestrel".to_owned()),
        part_number: Some("KS5600S16".to_owned()),
    };
    Inventory {
        collected: date(2026, 11, 2).and_then(|d| d.and_hms_opt(9, 30, 0)),
        elevated: false,
        system: SystemInfo {
            manufacturer: Some("Lumen".to_owned()),
            model: Some("Vela 15".to_owned()),
            chassis: Chassis::Laptop,
            mainboard: Some("Lumen VL15-MB".to_owned()),
            firmware_kind: FirmwareKind::Uefi,
            firmware_vendor: Some("Lumen".to_owned()),
            firmware_version: Some("1.14.0".to_owned()),
            firmware_date: date(2026, 3, 18),
            secure_boot: Some(true),
            tpm_version: Some("2.0".to_owned()),
            os_name: Some("Windows 11 Home".to_owned()),
            os_release: Some("25H2".to_owned()),
            os_build: Some("26200.7019".to_owned()),
            os_architecture: Some("64-bit".to_owned()),
            os_installed: date(2025, 9, 4),
            last_boot: date(2026, 10, 30).and_then(|d| d.and_hms_opt(8, 12, 0)),
            uptime_seconds: 3 * 86_400 + 4_680,
        },
        processors: vec![Processor {
            name: "Orion X7-1380H".to_owned(),
            manufacturer: Some("Orion".to_owned()),
            cores: Some(8),
            threads: Some(16),
            base_mhz: Some(3300),
            architecture: Some("x64".to_owned()),
            l2_cache_kb: Some(8 * 1024),
            l3_cache_kb: Some(24 * 1024),
            socket: None,
            virtualization: Some(true),
        }],
        memory: Memory {
            usable_bytes: (32 << 30) - (420 << 20),
            available_bytes: 19 << 30,
            installed_bytes: 32 << 30,
            modules: vec![module("DIMM A"), module("DIMM B")],
            slots: Some(2),
        },
        graphics: vec![
            GraphicsAdapter {
                name: "Vantage RX 4070 Laptop".to_owned(),
                vendor: Some("Vantage".to_owned()),
                dedicated_memory_bytes: Some(8 << 30),
                driver_version: Some("32.0.15.6614".to_owned()),
                driver_date: date(2026, 9, 22),
            },
            GraphicsAdapter {
                name: "Orion Iris Graphics".to_owned(),
                vendor: Some("Orion".to_owned()),
                dedicated_memory_bytes: Some(128 << 20),
                driver_version: Some("32.0.101.6078".to_owned()),
                driver_date: date(2026, 6, 3),
            },
        ],
        displays: vec![
            Display {
                name: "Built-in display".to_owned(),
                width: 2560,
                height: 1600,
                refresh_hz: Some(165.0),
                built_in: true,
            },
            Display {
                name: "Halo 27Q".to_owned(),
                width: 2560,
                height: 1440,
                refresh_hz: Some(75.0),
                built_in: false,
            },
        ],
        drives: vec![
            PhysicalDrive {
                number: Some(0),
                model: "Kestrel Pro 990 1TB".to_owned(),
                bus: Bus::Nvme,
                media: Media::Ssd,
                size_bytes: 1_000_204_886_016,
                firmware: Some("4B2QJXD7".to_owned()),
                spindle_rpm: None,
                health: Health::Healthy,
                counters: DriveCounters {
                    temperature_c: Some(41),
                    wear_percent: Some(3),
                    spare_percent: Some(100),
                    spare_threshold_percent: Some(10),
                    power_on_hours: Some(2_184),
                    power_cycles: Some(611),
                    unsafe_shutdowns: Some(9),
                    media_errors: Some(0),
                    read_errors: None,
                    write_errors: None,
                    bytes_read: Some(21 << 40),
                    bytes_written: Some(14 << 40),
                    critical_warning: Some(0),
                },
                counters_need_admin: false,
                volumes: vec![volume('C', "Windows", 930, 412, true)],
                hidden_partitions: 3,
            },
            PhysicalDrive {
                number: Some(1),
                model: "Tundra T2 2TB".to_owned(),
                bus: Bus::Sata,
                media: Media::Hdd,
                size_bytes: 2_000_398_934_016,
                firmware: Some("CC43".to_owned()),
                spindle_rpm: Some(5400),
                health: Health::Warning,
                counters: DriveCounters {
                    temperature_c: Some(36),
                    ..DriveCounters::default()
                },
                counters_need_admin: true,
                volumes: vec![volume('D', "Archive", 1862, 121, false)],
                hidden_partitions: 0,
            },
        ],
        other_volumes: Vec::new(),
        batteries: vec![Battery {
            name: None,
            manufacturer: Some("Lumen".to_owned()),
            chemistry: Some("Lithium polymer".to_owned()),
            design_mwh: Some(80_000),
            full_charge_mwh: Some(68_400),
            remaining_mwh: Some(48_600),
            charge_percent: Some(71.0),
            cycle_count: Some(214),
            voltage_mv: Some(15_480),
            rate_mw: Some(-14_200),
            state: BatteryState::Discharging,
        }],
        on_mains_power: Some(false),
        network: vec![
            NetworkAdapter {
                name: "Orion Wi-Fi 7 BE200".to_owned(),
                kind: NetworkKind::WiFi,
                connected: true,
                link_bps: Some(1_201_000_000),
            },
            NetworkAdapter {
                name: "Lumen 2.5GbE Controller".to_owned(),
                kind: NetworkKind::Ethernet,
                connected: false,
                link_bps: None,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_pc_shows_every_feature() {
        let sample = sample();
        let findings = attention(&sample);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert_eq!(findings[0].level, Level::Warning);
        assert_eq!(findings[0].title, "Tundra T2 2TB may fail soon");
        assert_eq!(findings[1].title, "Archive (D:) is almost full");
        assert_eq!(sample.memory.free_slots(), Some(0));
        assert_eq!(sample.memory.total_bytes(), 32 << 30);
        assert!((sample.batteries[0].wear_percent().unwrap() - 14.5).abs() < 0.01);
    }

    #[test]
    fn the_inventory_of_this_pc_is_complete_and_anonymous() {
        let progress = Progress::new();
        let inventory = inventory_with_progress(&progress);
        assert_eq!(progress.fraction(), Some(1.0));
        assert!(inventory.collected.is_some());
        assert!(inventory.system.os_name.is_some());
        assert!(!inventory.processors.is_empty());
        assert!(inventory.memory.usable_bytes > 0);
        assert!(!inventory.drives.is_empty());

        // Nothing that names this machine or its user may appear.
        let report = to_text(&inventory) + &to_json(&inventory);
        for secret in ["COMPUTERNAME", "USERNAME"] {
            let Ok(value) = std::env::var(secret) else {
                continue;
            };
            if value.len() >= 4 {
                assert!(
                    !report.to_lowercase().contains(&value.to_lowercase()),
                    "the report contains %{secret}%"
                );
            }
        }
    }
}
