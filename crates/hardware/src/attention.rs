//! What about the hardware deserves the user's attention.
//!
//! Every finding is something a person can act on: back up a failing drive,
//! free space, restart, replace a worn battery. Facts that cannot be acted
//! on stay in the inventory and out of this list.

use crate::model::{Attention, FirmwareKind, Health, Inventory, Level, PhysicalDrive, Volume};
use zest_core::fmt::{format_bytes, plural};

/// Wear, in percent of rated life, from which a drive is worth watching.
const WEAR_WARNING_PERCENT: u32 = 90;
/// Temperature from which a drive is running hot.
const HOT_DRIVE_CELSIUS: i32 = 70;
/// Free share below which a volume is nearly full.
const LOW_SPACE_FRACTION: f32 = 0.10;
/// Volumes smaller than this are recovery or tool partitions, full by design.
const SMALL_VOLUME_BYTES: u64 = 8 << 30;
/// Battery wear from which a note is shown.
const BATTERY_NOTE_PERCENT: f32 = 25.0;
/// Battery wear from which replacing the battery is suggested.
const BATTERY_WARNING_PERCENT: f32 = 45.0;
/// Days of uptime from which a restart is suggested.
const LONG_UPTIME_DAYS: u64 = 14;

fn item(level: Level, title: impl Into<String>, detail: impl Into<String>) -> Attention {
    Attention {
        level,
        title: title.into(),
        detail: detail.into(),
    }
}

fn drive_findings(drive: &PhysicalDrive, out: &mut Vec<Attention>) {
    let name = &drive.model;
    match drive.health {
        Health::Unhealthy => out.push(item(
            Level::Critical,
            format!("{name} is failing"),
            "Windows reports this drive as failing. Copy what is on it somewhere else now, then replace it.",
        )),
        Health::Warning => out.push(item(
            Level::Warning,
            format!("{name} may fail soon"),
            "Windows expects trouble with this drive. Back up what is on it and plan to replace it.",
        )),
        Health::Healthy | Health::Unknown => {}
    }
    let counters = &drive.counters;
    if counters.critical_warning.is_some_and(|flags| flags != 0) && drive.health == Health::Healthy
    {
        out.push(item(
            Level::Warning,
            format!("{name} reports a problem"),
            "The drive's own monitoring has raised a warning. Back up what is on it and check it with the maker's tool.",
        ));
    }
    if let (Some(spare), Some(threshold)) =
        (counters.spare_percent, counters.spare_threshold_percent)
        && threshold > 0
        && spare <= threshold
    {
        out.push(item(
            Level::Warning,
            format!("{name} is running out of spare blocks"),
            format!("{spare}% of its reserve is left. The drive is close to the end of its life; back up and replace it."),
        ));
    }
    if let Some(wear) = counters.wear_percent
        && wear >= WEAR_WARNING_PERCENT
    {
        out.push(item(
            Level::Warning,
            format!("{name} has used {wear}% of its rated life"),
            "It can keep working past its rating, but failures become more likely. Keep a backup of what is on it.",
        ));
    }
    let errors = counters.media_errors.unwrap_or(0)
        + counters.read_errors.unwrap_or(0)
        + counters.write_errors.unwrap_or(0);
    if errors > 0 {
        out.push(item(
            Level::Warning,
            format!("{name} has logged {}", plural(errors, "uncorrectable error")),
            "Data was read or written incorrectly at least once. Back up what is on the drive and consider replacing it.",
        ));
    }
    if let Some(celsius) = counters.temperature_c
        && celsius >= HOT_DRIVE_CELSIUS
    {
        out.push(item(
            Level::Warning,
            format!("{name} is running hot at {celsius} °C"),
            "Heat shortens a drive's life and slows it down. Check that air can flow around it.",
        ));
    }
}

fn volume_findings(volume: &Volume, out: &mut Vec<Attention>) {
    if volume.total_bytes < SMALL_VOLUME_BYTES || volume.free_fraction() >= LOW_SPACE_FRACTION {
        return;
    }
    let free = format_bytes(volume.free_bytes);
    if volume.is_system {
        out.push(item(
            Level::Warning,
            format!("{} is almost full", volume.display_name()),
            format!("{free} is free. Windows needs room for updates and slows down without it. Custom clean and the Disk analyzer show what can go."),
        ));
    } else {
        out.push(item(
            Level::Note,
            format!("{} is almost full", volume.display_name()),
            format!("{free} is free. The Disk analyzer shows what takes up the space."),
        ));
    }
}

/// Lists what deserves attention, most serious first.
pub fn attention(inventory: &Inventory) -> Vec<Attention> {
    let mut out = Vec::new();
    for drive in &inventory.drives {
        drive_findings(drive, &mut out);
        for volume in &drive.volumes {
            volume_findings(volume, &mut out);
        }
    }
    for volume in &inventory.other_volumes {
        volume_findings(volume, &mut out);
    }
    for battery in &inventory.batteries {
        let Some(wear) = battery.wear_percent() else {
            continue;
        };
        let holds = (100.0 - wear).round();
        if wear >= BATTERY_WARNING_PERCENT {
            out.push(item(
                Level::Warning,
                format!("The battery holds {holds:.0}% of its original charge"),
                "It will run for noticeably less time than when it was new. Replacing the battery restores the original run time.",
            ));
        } else if wear >= BATTERY_NOTE_PERCENT {
            out.push(item(
                Level::Note,
                format!("The battery holds {holds:.0}% of its original charge"),
                "Some loss with age is normal. Expect shorter run times than when it was new.",
            ));
        }
    }
    let days = inventory.system.uptime_seconds / 86_400;
    if days >= LONG_UPTIME_DAYS {
        out.push(item(
            Level::Note,
            format!("Windows has been running for {days} days"),
            "A restart finishes pending updates and clears out what long-running programs leave behind. Shutting down does not count when Fast Startup is on.",
        ));
    }
    if inventory.system.firmware_kind == FirmwareKind::Uefi
        && inventory.system.secure_boot == Some(false)
    {
        out.push(item(
            Level::Note,
            "Secure Boot is switched off",
            "Secure Boot stops malware from loading before Windows starts. It is switched on in the firmware settings.",
        ));
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.level));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Battery, DriveCounters, SystemInfo};

    fn healthy_drive() -> PhysicalDrive {
        PhysicalDrive {
            number: Some(0),
            model: "Example SSD".to_owned(),
            health: Health::Healthy,
            counters: DriveCounters {
                temperature_c: Some(38),
                wear_percent: Some(4),
                spare_percent: Some(100),
                spare_threshold_percent: Some(10),
                media_errors: Some(0),
                critical_warning: Some(0),
                ..DriveCounters::default()
            },
            volumes: vec![Volume {
                letter: 'C',
                label: "Windows".to_owned(),
                file_system: "NTFS".to_owned(),
                total_bytes: 500 << 30,
                free_bytes: 200 << 30,
                is_system: true,
            }],
            ..PhysicalDrive::default()
        }
    }

    fn inventory_with(drive: PhysicalDrive) -> Inventory {
        Inventory {
            drives: vec![drive],
            system: SystemInfo {
                uptime_seconds: 3 * 86_400,
                firmware_kind: FirmwareKind::Uefi,
                secure_boot: Some(true),
                ..SystemInfo::default()
            },
            ..Inventory::default()
        }
    }

    #[test]
    fn a_healthy_pc_needs_no_attention() {
        assert!(attention(&inventory_with(healthy_drive())).is_empty());
        assert!(attention(&Inventory::default()).is_empty());
    }

    #[test]
    fn failing_drives_come_first() {
        let mut inventory = inventory_with(PhysicalDrive {
            health: Health::Unhealthy,
            ..healthy_drive()
        });
        inventory.system.uptime_seconds = 30 * 86_400;
        let found = attention(&inventory);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].level, Level::Critical);
        assert_eq!(found[0].title, "Example SSD is failing");
        assert_eq!(found[1].level, Level::Note);
        assert!(found[1].title.contains("30 days"));

        let warned = attention(&inventory_with(PhysicalDrive {
            health: Health::Warning,
            ..healthy_drive()
        }));
        assert_eq!(warned[0].level, Level::Warning);
        assert!(warned[0].title.contains("may fail soon"));
    }

    #[test]
    fn drive_counters_raise_warnings() {
        let mut drive = healthy_drive();
        drive.counters = DriveCounters {
            temperature_c: Some(74),
            wear_percent: Some(97),
            spare_percent: Some(8),
            spare_threshold_percent: Some(10),
            media_errors: Some(2),
            read_errors: Some(1),
            critical_warning: Some(1),
            ..DriveCounters::default()
        };
        let titles: Vec<String> = attention(&inventory_with(drive))
            .into_iter()
            .map(|a| a.title)
            .collect();
        assert_eq!(titles.len(), 5, "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("reports a problem")));
        assert!(titles.iter().any(|t| t.contains("spare blocks")));
        assert!(titles.iter().any(|t| t.contains("97% of its rated life")));
        assert!(titles.iter().any(|t| t.contains("3 uncorrectable errors")));
        assert!(titles.iter().any(|t| t.contains("74 °C")));

        // A warning flag on a drive Windows already calls failing is not
        // reported twice.
        let mut failing = healthy_drive();
        failing.health = Health::Unhealthy;
        failing.counters.critical_warning = Some(4);
        assert_eq!(attention(&inventory_with(failing)).len(), 1);
    }

    #[test]
    fn nearly_full_volumes() {
        let mut drive = healthy_drive();
        drive.volumes[0].free_bytes = 20 << 30; // 4% of 500 GB
        drive.volumes.push(Volume {
            letter: 'D',
            label: String::new(),
            file_system: "NTFS".to_owned(),
            total_bytes: 1000 << 30,
            free_bytes: 50 << 30,
            is_system: false,
        });
        // A small tools partition that is full by design.
        drive.volumes.push(Volume {
            letter: 'R',
            label: "Recovery".to_owned(),
            file_system: "NTFS".to_owned(),
            total_bytes: 1 << 30,
            free_bytes: 10 << 20,
            is_system: false,
        });
        let mut inventory = inventory_with(drive);
        inventory.other_volumes.push(Volume {
            letter: 'E',
            label: "Photos".to_owned(),
            file_system: "exFAT".to_owned(),
            total_bytes: 64 << 30,
            free_bytes: 1 << 30,
            is_system: false,
        });
        let found = attention(&inventory);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].level, Level::Warning);
        assert_eq!(found[0].title, "Windows (C:) is almost full");
        assert!(found[0].detail.starts_with("20.0 GB is free"));
        assert_eq!(found[1].title, "Local disk (D:) is almost full");
        assert_eq!(found[1].level, Level::Note);
        assert_eq!(found[2].title, "Photos (E:) is almost full");
    }

    #[test]
    fn battery_wear_and_secure_boot() {
        let battery = |full: u32| Battery {
            design_mwh: Some(50_000),
            full_charge_mwh: Some(full),
            ..Battery::default()
        };
        let mut inventory = inventory_with(healthy_drive());
        inventory.batteries = vec![battery(45_000)];
        assert!(attention(&inventory).is_empty(), "10% wear is ordinary");
        inventory.batteries = vec![battery(35_000)];
        let found = attention(&inventory);
        assert_eq!(found[0].level, Level::Note);
        assert_eq!(
            found[0].title,
            "The battery holds 70% of its original charge"
        );
        inventory.batteries = vec![battery(25_000)];
        assert_eq!(attention(&inventory)[0].level, Level::Warning);
        // A battery that reports no design capacity cannot be judged.
        inventory.batteries = vec![Battery::default()];
        assert!(attention(&inventory).is_empty());

        inventory.system.secure_boot = Some(false);
        assert_eq!(
            attention(&inventory)[0].title,
            "Secure Boot is switched off"
        );
        // Legacy firmware has no Secure Boot to switch on.
        inventory.system.firmware_kind = FirmwareKind::LegacyBios;
        assert!(attention(&inventory).is_empty());
    }
}
