//! The inventory as a report: plain text for people, JSON for tools.

use crate::attention::attention;
use crate::model::{Battery, Inventory, Level, PhysicalDrive};
use std::fmt::Write as _;
use zest_core::fmt::{format_bytes, format_count};

/// Formats a capacity the way hardware is labelled: whole numbers stay
/// whole (`16 GB`), everything else gets what [`format_bytes`] gives.
pub fn format_capacity(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 40, "TB"), (1 << 30, "GB"), (1 << 20, "MB")];
    match UNITS.iter().find(|(size, _)| bytes >= *size) {
        Some((size, unit)) if bytes.is_multiple_of(*size) => format!("{} {unit}", bytes / size),
        _ => format_bytes(bytes),
    }
}

/// Formats a clock speed given in MHz, e.g. `2.40 GHz` or `800 MHz`.
pub fn format_frequency(mhz: u32) -> String {
    if mhz >= 1000 {
        format!("{:.2} GHz", f64::from(mhz) / 1000.0)
    } else {
        format!("{mhz} MHz")
    }
}

/// Formats a link speed given in bits per second, e.g. `1.2 Gbps`.
pub fn format_link_speed(bps: u64) -> String {
    let trim = |value: f64, unit: &str| {
        if (value - value.round()).abs() < 0.05 {
            format!("{value:.0} {unit}")
        } else {
            format!("{value:.1} {unit}")
        }
    };
    if bps >= 1_000_000_000 {
        trim(bps as f64 / 1e9, "Gbps")
    } else if bps >= 1_000_000 {
        trim(bps as f64 / 1e6, "Mbps")
    } else {
        trim(bps as f64 / 1e3, "Kbps")
    }
}

/// Formats how long Windows has been running, e.g. `3 days 4 hours`.
pub fn format_uptime(seconds: u64) -> String {
    let unit = |count: u64, noun: &str| {
        if count == 1 {
            format!("1 {noun}")
        } else {
            format!("{count} {noun}s")
        }
    };
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
    );
    if days > 0 && hours > 0 {
        format!("{} {}", unit(days, "day"), unit(hours, "hour"))
    } else if days > 0 {
        unit(days, "day")
    } else if hours > 0 {
        unit(hours, "hour")
    } else {
        unit(minutes.max(1), "minute")
    }
}

/// Formats a battery capacity given in mWh, e.g. `42.7 Wh`.
pub fn format_energy(mwh: u32) -> String {
    format!("{:.1} Wh", f64::from(mwh) / 1000.0)
}

/// One line that sums a drive up: kind, size and firmware.
pub fn drive_summary(drive: &PhysicalDrive) -> String {
    let mut parts = vec![drive.kind_label()];
    if drive.size_bytes > 0 {
        parts.push(format_bytes(drive.size_bytes));
    }
    if let Some(rpm) = drive.spindle_rpm {
        parts.push(format!("{} rpm", format_count(u64::from(rpm))));
    }
    if let Some(firmware) = &drive.firmware {
        parts.push(format!("firmware {firmware}"));
    }
    parts.join(", ")
}

/// One line on what a battery is doing, e.g. `91% charged, charging`.
pub fn battery_summary(battery: &Battery) -> String {
    let mut parts = Vec::new();
    if let Some(percent) = battery.charge_percent {
        parts.push(format!("{percent:.0}% charged"));
    }
    parts.push(battery.state.label().to_lowercase());
    if let Some(chemistry) = &battery.chemistry {
        parts.push(chemistry.to_lowercase());
    }
    let mut line = parts.join(", ");
    if let Some(first) = line.get(..1) {
        line = first.to_uppercase() + &line[1..];
    }
    line
}

struct Text(String);

impl Text {
    fn heading(&mut self, title: &str) {
        let _ = writeln!(self.0, "\n{title}");
    }

    fn line(&mut self, text: impl AsRef<str>) {
        let _ = writeln!(self.0, "  {}", text.as_ref());
    }

    fn row(&mut self, label: &str, value: impl AsRef<str>) {
        let _ = writeln!(self.0, "  {label:<15} {}", value.as_ref());
    }

    /// A row one level in, for the details of a drive.
    fn sub_row(&mut self, label: &str, value: impl AsRef<str>) {
        let _ = writeln!(self.0, "    {label:<13} {}", value.as_ref());
    }

    fn row_opt(&mut self, label: &str, value: Option<impl AsRef<str>>) {
        if let Some(value) = value {
            self.row(label, value);
        }
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

/// Renders the inventory as a plain-text report.
///
/// Like the inventory itself the report holds nothing that identifies the
/// machine or its owner.
pub fn to_text(inventory: &Inventory) -> String {
    let mut out = Text(String::from("Zest system report\n"));
    if let Some(taken) = inventory.collected {
        let _ = writeln!(out.0, "Taken {}", taken.format("%Y-%m-%d %H:%M"));
    }

    let findings = attention(inventory);
    if !findings.is_empty() {
        out.heading("Needs attention");
        for finding in &findings {
            let mark = match finding.level {
                Level::Critical => "!!",
                Level::Warning => "!",
                Level::Note => "-",
            };
            out.line(format!("{mark} {}", finding.title));
            out.line(format!("  {}", finding.detail));
        }
    }

    let system = &inventory.system;
    out.heading("System");
    let computer = match (&system.manufacturer, &system.model) {
        (Some(maker), Some(model)) => Some(format!("{maker} {model}")),
        (None, Some(model)) => Some(model.clone()),
        (Some(maker), None) => Some(maker.clone()),
        (None, None) => None,
    };
    out.row(
        "Computer",
        match computer {
            Some(name) => format!("{name} ({})", system.chassis.label()),
            None => format!("Unbranded {}", system.chassis.label()),
        },
    );
    out.row_opt("Mainboard", system.mainboard.as_ref());
    let mut firmware = vec![system.firmware_kind.label().to_owned()];
    match (&system.firmware_vendor, &system.firmware_version) {
        (Some(vendor), Some(version)) => firmware.push(format!("{vendor} {version}")),
        (None, Some(version)) => firmware.push(version.clone()),
        (Some(vendor), None) => firmware.push(vendor.clone()),
        (None, None) => {}
    }
    if let Some(date) = system.firmware_date {
        firmware.push(date.to_string());
    }
    out.row("Firmware", firmware.join(", "));
    out.row_opt("Secure Boot", system.secure_boot.map(on_off));
    out.row("TPM", system.tpm_version.as_deref().unwrap_or("Not found"));
    let mut windows = Vec::new();
    windows.extend(system.os_name.clone());
    if let Some(release) = &system.os_release {
        windows.push(release.clone());
    }
    let mut windows = windows.join(" ");
    if let Some(build) = &system.os_build {
        let _ = write!(windows, ", build {build}");
    }
    if let Some(architecture) = &system.os_architecture {
        let _ = write!(windows, ", {architecture}");
    }
    out.row("Windows", windows);
    out.row_opt("Installed", system.os_installed.map(|d| d.to_string()));
    out.row("Running for", format_uptime(system.uptime_seconds));

    out.heading(if inventory.processors.len() > 1 {
        "Processors"
    } else {
        "Processor"
    });
    for processor in &inventory.processors {
        out.line(&processor.name);
        let mut facts = Vec::new();
        if let Some(cores) = processor.cores {
            facts.push(format!("{cores} cores"));
        }
        if let Some(threads) = processor.threads {
            facts.push(format!("{threads} threads"));
        }
        if let Some(mhz) = processor.base_mhz {
            facts.push(format_frequency(mhz));
        }
        facts.extend(processor.architecture.clone());
        if !facts.is_empty() {
            out.line(facts.join(", "));
        }
        let mut cache = Vec::new();
        if let Some(l2) = processor.l2_cache_kb {
            cache.push(format!("{} L2", format_capacity(u64::from(l2) * 1024)));
        }
        if let Some(l3) = processor.l3_cache_kb {
            cache.push(format!("{} L3", format_capacity(u64::from(l3) * 1024)));
        }
        if !cache.is_empty() {
            out.row("Cache", cache.join(", "));
        }
        out.row_opt("Virtualisation", processor.virtualization.map(on_off));
    }
    if inventory.processors.is_empty() {
        out.line("Not reported");
    }

    let memory = &inventory.memory;
    out.heading("Memory");
    let mut summary = Vec::new();
    if memory.installed_bytes > 0 {
        summary.push(format!(
            "{} installed",
            format_capacity(memory.installed_bytes)
        ));
    }
    summary.push(format!("{} usable", format_bytes(memory.usable_bytes)));
    summary.push(format!("{} free", format_bytes(memory.available_bytes)));
    out.line(summary.join(", "));
    if let Some(slots) = memory.slots {
        out.row("Slots", format!("{} of {slots} used", memory.modules.len()));
    }
    for module in &memory.modules {
        let mut facts = vec![format_capacity(module.capacity_bytes)];
        facts.extend(module.kind.clone());
        facts.extend(module.form.clone());
        let mut line = facts.join(" ");
        match (module.speed_mhz, module.rated_mhz) {
            (Some(speed), Some(rated)) if rated != speed => {
                let _ = write!(line, ", {speed} MHz (rated {rated})");
            }
            (Some(speed), _) => {
                let _ = write!(line, ", {speed} MHz");
            }
            _ => {}
        }
        let maker: Vec<&str> = [
            module.manufacturer.as_deref(),
            module.part_number.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !maker.is_empty() {
            let _ = write!(line, ", {}", maker.join(" "));
        }
        out.row(module.slot.as_deref().unwrap_or("Module"), line);
    }

    out.heading("Graphics");
    for adapter in &inventory.graphics {
        let mut facts = Vec::new();
        if let Some(bytes) = adapter.dedicated_memory_bytes.filter(|&b| b >= 256 << 20) {
            facts.push(format!("{} of its own memory", format_capacity(bytes)));
        }
        if let Some(version) = &adapter.driver_version {
            match adapter.driver_date {
                Some(date) => facts.push(format!("driver {version} ({date})")),
                None => facts.push(format!("driver {version}")),
            }
        }
        out.line(&adapter.name);
        if !facts.is_empty() {
            out.line(format!("  {}", facts.join(", ")));
        }
    }
    if inventory.graphics.is_empty() {
        out.line("Not reported");
    }

    if !inventory.displays.is_empty() {
        out.heading("Displays");
        for display in &inventory.displays {
            let mut line = format!("{}: {} x {}", display.name, display.width, display.height);
            if let Some(hz) = display.refresh_hz {
                let _ = write!(line, ", {hz:.0} Hz");
            }
            out.line(line);
        }
    }

    out.heading("Storage");
    for drive in &inventory.drives {
        let number = drive
            .number
            .map(|n| format!("Disk {n}: "))
            .unwrap_or_default();
        out.line(format!(
            "{number}{} ({})",
            drive.model,
            drive_summary(drive)
        ));
        out.sub_row("Health", drive.health.label());
        let counters = &drive.counters;
        if let Some(celsius) = counters.temperature_c {
            out.sub_row("Temperature", format!("{celsius} °C"));
        }
        if let Some(wear) = counters.wear_percent {
            let mut line = format!("{wear}% of rated life used");
            if let Some(spare) = counters.spare_percent {
                let _ = write!(line, ", {spare}% spare left");
            }
            out.sub_row("Wear", line);
        }
        if let Some(hours) = counters.power_on_hours {
            let mut line = format!("{} hours", format_count(hours));
            if let Some(cycles) = counters.power_cycles {
                let _ = write!(line, ", switched on {} times", format_count(cycles));
            }
            out.sub_row("Powered on", line);
        }
        if let Some(written) = counters.bytes_written {
            out.sub_row("Written", format_bytes(written));
        }
        let errors = counters.media_errors.unwrap_or(0)
            + counters.read_errors.unwrap_or(0)
            + counters.write_errors.unwrap_or(0);
        if counters.media_errors.is_some()
            || counters.read_errors.is_some()
            || counters.write_errors.is_some()
        {
            out.sub_row("Errors", format_count(errors));
        }
        if drive.counters_need_admin {
            out.sub_row(
                "Counters",
                "Wear and error counters need administrator rights",
            );
        }
        for volume in &drive.volumes {
            out.sub_row(
                &volume.display_name(),
                format!(
                    "{}, {} free of {}",
                    volume.file_system,
                    format_bytes(volume.free_bytes),
                    format_bytes(volume.total_bytes)
                ),
            );
        }
        if drive.hidden_partitions > 0 {
            out.line(format!(
                "  {} without a drive letter",
                if drive.hidden_partitions == 1 {
                    "1 partition".to_owned()
                } else {
                    format!("{} partitions", drive.hidden_partitions)
                }
            ));
        }
    }
    for volume in &inventory.other_volumes {
        out.line(format!(
            "{}: {}, {} free of {}",
            volume.display_name(),
            volume.file_system,
            format_bytes(volume.free_bytes),
            format_bytes(volume.total_bytes)
        ));
    }
    if inventory.drives.is_empty() && inventory.other_volumes.is_empty() {
        out.line("Not reported");
    }

    out.heading("Battery");
    if inventory.batteries.is_empty() {
        out.line("No battery");
    }
    for battery in &inventory.batteries {
        out.line(battery_summary(battery));
        if let Some(full) = battery.full_charge_mwh {
            let line = match (battery.design_mwh, battery.health_percent()) {
                (Some(design), Some(health)) => format!(
                    "{} of {} when new ({health:.0}%)",
                    format_energy(full),
                    format_energy(design)
                ),
                _ => format_energy(full),
            };
            out.row("Full charge", line);
        }
        out.row_opt("Cycles", battery.cycle_count.map(|c| c.to_string()));
    }
    out.row_opt(
        "Power",
        inventory.on_mains_power.map(|mains| {
            if mains {
                "Plugged in"
            } else {
                "Running on battery"
            }
        }),
    );

    if !inventory.network.is_empty() {
        out.heading("Network");
        for adapter in &inventory.network {
            let mut line = format!("{}: {}", adapter.name, adapter.kind.label());
            line.push_str(if adapter.connected {
                ", connected"
            } else {
                ", not connected"
            });
            if let Some(bps) = adapter.link_bps {
                let _ = write!(line, ", {}", format_link_speed(bps));
            }
            out.line(line);
        }
    }
    out.0
}

/// Renders the inventory, with its attention list, as pretty-printed JSON.
pub fn to_json(inventory: &Inventory) -> String {
    #[derive(serde::Serialize)]
    struct Report<'a> {
        attention: Vec<crate::model::Attention>,
        #[serde(flatten)]
        inventory: &'a Inventory,
    }
    serde_json::to_string_pretty(&Report {
        attention: attention(inventory),
        inventory,
    })
    .unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;

    #[test]
    fn formatting() {
        assert_eq!(format_capacity(8 << 30), "8 GB");
        assert_eq!(format_capacity(2 << 40), "2 TB");
        assert_eq!(format_capacity(512 << 20), "512 MB");
        assert_eq!(format_capacity(16_868_962_304), "15.7 GB");
        assert_eq!(
            format_capacity(5920 << 20),
            "5.78 GB",
            "not a whole number of GB"
        );
        assert_eq!(format_capacity(9728 << 10), "9.50 MB");
        assert_eq!(format_frequency(2400), "2.40 GHz");
        assert_eq!(format_frequency(800), "800 MHz");
        assert_eq!(format_link_speed(1_201_000_000), "1.2 Gbps");
        assert_eq!(format_link_speed(1_000_000_000), "1 Gbps");
        assert_eq!(format_link_speed(866_700_000), "866.7 Mbps");
        assert_eq!(format_link_speed(100_000_000), "100 Mbps");
        assert_eq!(format_link_speed(56_000), "56 Kbps");
        assert_eq!(format_uptime(30), "1 minute");
        assert_eq!(format_uptime(45 * 60), "45 minutes");
        assert_eq!(format_uptime(3600), "1 hour");
        assert_eq!(format_uptime(86_400), "1 day");
        assert_eq!(format_uptime(3 * 86_400 + 4 * 3600 + 59), "3 days 4 hours");
        assert_eq!(format_energy(42_682), "42.7 Wh");
    }

    #[test]
    fn the_text_report_covers_every_section() {
        let inventory = sample();
        let text = to_text(&inventory);
        for heading in [
            "Zest system report",
            "\nNeeds attention\n",
            "\nSystem\n",
            "\nProcessor\n",
            "\nMemory\n",
            "\nGraphics\n",
            "\nDisplays\n",
            "\nStorage\n",
            "\nBattery\n",
            "\nNetwork\n",
        ] {
            assert!(text.contains(heading), "missing {heading:?} in:\n{text}");
        }
        assert!(text.contains("Lumen Vela 15 (laptop)"), "{text}");
        assert!(
            text.contains("8 cores, 16 threads, 3.30 GHz, x64"),
            "{text}"
        );
        assert!(text.contains("32 GB installed"), "{text}");
        assert!(text.contains("2 of 2 used"), "{text}");
        assert!(
            text.contains("16 GB DDR5 SO-DIMM, 5200 MHz (rated 5600)"),
            "{text}"
        );
        assert!(text.contains("8 GB of its own memory"), "{text}");
        assert!(
            text.contains("Built-in display: 2560 x 1600, 165 Hz"),
            "{text}"
        );
        assert!(
            text.contains("Disk 0: Kestrel Pro 990 1TB (NVMe SSD"),
            "{text}"
        );
        assert!(
            text.contains("3% of rated life used, 100% spare left"),
            "{text}"
        );
        assert!(
            text.contains("Wear and error counters need administrator rights"),
            "{text}"
        );
        assert!(text.contains("may fail soon"), "{text}");
        assert!(text.contains("of 80.0 Wh when new"), "{text}");
        assert!(text.contains("Wi-Fi, connected, 1.2 Gbps"), "{text}");
        // Every line ends cleanly.
        assert!(text.lines().all(|line| line == line.trim_end()), "{text}");
    }

    #[test]
    fn an_empty_inventory_still_makes_a_report() {
        let text = to_text(&Inventory::default());
        assert!(text.contains("Unbranded computer"));
        assert!(text.contains("No battery"));
        assert!(!text.contains("Needs attention"));
        assert!(!text.contains("Network"));
        let json = to_json(&Inventory::default());
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["attention"].as_array().unwrap().is_empty());
        assert_eq!(value["drives"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn json_round_trips_and_carries_the_attention_list() {
        let inventory = sample();
        let json = to_json(&inventory);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(!value["attention"].as_array().unwrap().is_empty());
        assert_eq!(value["system"]["model"], "Vela 15");
        // The inventory alone deserialises back to the same value.
        let plain = serde_json::to_string(&inventory).unwrap();
        let back: Inventory = serde_json::from_str(&plain).unwrap();
        assert_eq!(back, inventory);
    }
}
