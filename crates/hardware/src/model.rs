//! The inventory as plain data.
//!
//! Nothing in here identifies the machine or its owner: no serial numbers,
//! network addresses, product keys, computer or user names. A report can be
//! pasted into a bug tracker as it is.

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

/// Everything Zest knows about the hardware of this PC.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    /// When the inventory was taken (local time).
    pub collected: Option<NaiveDateTime>,
    /// Whether it was taken with administrator rights. Some drive counters
    /// can only be read that way.
    pub elevated: bool,
    pub system: SystemInfo,
    pub processors: Vec<Processor>,
    pub memory: Memory,
    pub graphics: Vec<GraphicsAdapter>,
    pub displays: Vec<Display>,
    pub drives: Vec<PhysicalDrive>,
    /// Volumes that could not be matched to a physical drive.
    pub other_volumes: Vec<Volume>,
    pub batteries: Vec<Battery>,
    /// Whether the PC runs on mains power, when Windows can tell.
    pub on_mains_power: Option<bool>,
    pub network: Vec<NetworkAdapter>,
}

/// Kind of computer, as far as the firmware says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Chassis {
    Desktop,
    Laptop,
    Tablet,
    Server,
    VirtualMachine,
    #[default]
    Unknown,
}

impl Chassis {
    /// Lower-case label, e.g. `laptop`.
    pub fn label(self) -> &'static str {
        match self {
            Chassis::Desktop => "desktop",
            Chassis::Laptop => "laptop",
            Chassis::Tablet => "tablet",
            Chassis::Server => "server",
            Chassis::VirtualMachine => "virtual machine",
            Chassis::Unknown => "computer",
        }
    }
}

/// How the firmware starts the PC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareKind {
    Uefi,
    LegacyBios,
    #[default]
    Unknown,
}

impl FirmwareKind {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            FirmwareKind::Uefi => "UEFI",
            FirmwareKind::LegacyBios => "Legacy BIOS",
            FirmwareKind::Unknown => "Unknown",
        }
    }
}

/// The computer, its firmware and Windows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub chassis: Chassis,
    /// Maker and product name of the mainboard.
    pub mainboard: Option<String>,
    pub firmware_kind: FirmwareKind,
    pub firmware_vendor: Option<String>,
    pub firmware_version: Option<String>,
    pub firmware_date: Option<NaiveDate>,
    /// `None` when the firmware has no Secure Boot or its state is unreadable.
    pub secure_boot: Option<bool>,
    /// TPM specification version (`2.0`, `1.2`), `None` without a TPM.
    pub tpm_version: Option<String>,
    /// Edition name, e.g. `Windows 11 Home`.
    pub os_name: Option<String>,
    /// Feature update, e.g. `25H2`.
    pub os_release: Option<String>,
    /// Full build, e.g. `26200.6584`.
    pub os_build: Option<String>,
    /// `64-bit`, `ARM 64-bit`…
    pub os_architecture: Option<String>,
    /// When this installation (or its last feature update) was set up.
    pub os_installed: Option<NaiveDate>,
    /// When Windows last started from cold (local time).
    pub last_boot: Option<NaiveDateTime>,
    /// Seconds Windows has been running. Shutting down with Fast Startup
    /// does not reset this; a restart does.
    pub uptime_seconds: u64,
}

/// One physical processor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Processor {
    pub name: String,
    pub manufacturer: Option<String>,
    pub cores: Option<u32>,
    pub threads: Option<u32>,
    /// Nominal speed in MHz.
    pub base_mhz: Option<u32>,
    /// `x64`, `ARM64`…
    pub architecture: Option<String>,
    pub l2_cache_kb: Option<u32>,
    pub l3_cache_kb: Option<u32>,
    pub socket: Option<String>,
    /// Whether hardware virtualisation is usable: on in the firmware, or
    /// already in use by Windows.
    pub virtualization: Option<bool>,
}

/// Installed memory.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Memory Windows can use, in bytes.
    pub usable_bytes: u64,
    /// Memory free right now, in bytes.
    pub available_bytes: u64,
    /// Sum of the installed modules, in bytes (zero when not reported).
    pub installed_bytes: u64,
    pub modules: Vec<MemoryModule>,
    /// Number of memory slots on the mainboard, when reported.
    pub slots: Option<u32>,
}

impl Memory {
    /// Slots without a module, when the mainboard reports its slots.
    pub fn free_slots(&self) -> Option<u32> {
        self.slots
            .map(|slots| slots.saturating_sub(self.modules.len() as u32))
    }

    /// The larger of installed and usable memory: what to call "the memory".
    pub fn total_bytes(&self) -> u64 {
        self.installed_bytes.max(self.usable_bytes)
    }

    /// Fraction of usable memory in use right now, `0.0..=1.0`.
    pub fn used_fraction(&self) -> f32 {
        if self.usable_bytes == 0 {
            return 0.0;
        }
        let used = self.usable_bytes.saturating_sub(self.available_bytes);
        (used as f64 / self.usable_bytes as f64) as f32
    }
}

/// One memory module.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryModule {
    /// Slot name printed on the mainboard, e.g. `DIMM A1`.
    pub slot: Option<String>,
    pub capacity_bytes: u64,
    /// Speed the module runs at, in MHz.
    pub speed_mhz: Option<u32>,
    /// Speed the module is rated for, in MHz.
    pub rated_mhz: Option<u32>,
    /// `DDR4`, `DDR5`, `LPDDR5`…
    pub kind: Option<String>,
    /// `DIMM`, `SO-DIMM`…
    pub form: Option<String>,
    pub manufacturer: Option<String>,
    /// The maker's model number of the module.
    pub part_number: Option<String>,
}

/// One graphics adapter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphicsAdapter {
    pub name: String,
    pub vendor: Option<String>,
    /// Memory of its own, in bytes. Integrated graphics have little or none
    /// and borrow main memory instead.
    pub dedicated_memory_bytes: Option<u64>,
    pub driver_version: Option<String>,
    pub driver_date: Option<NaiveDate>,
}

/// One connected screen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Refresh rate in Hz.
    pub refresh_hz: Option<f32>,
    /// Built into the PC (laptop panel) rather than plugged in.
    pub built_in: bool,
}

/// How a drive is attached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bus {
    Nvme,
    Sata,
    Usb,
    Sas,
    Raid,
    SdCard,
    Virtual,
    Other,
    #[default]
    Unknown,
}

impl Bus {
    /// Display label; empty when unknown.
    pub fn label(self) -> &'static str {
        match self {
            Bus::Nvme => "NVMe",
            Bus::Sata => "SATA",
            Bus::Usb => "USB",
            Bus::Sas => "SAS",
            Bus::Raid => "RAID",
            Bus::SdCard => "SD card",
            Bus::Virtual => "Virtual",
            Bus::Other | Bus::Unknown => "",
        }
    }
}

/// What a drive stores data on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Media {
    Ssd,
    Hdd,
    #[default]
    Unknown,
}

impl Media {
    /// Display label; empty when unknown.
    pub fn label(self) -> &'static str {
        match self {
            Media::Ssd => "SSD",
            Media::Hdd => "Hard disk",
            Media::Unknown => "",
        }
    }
}

/// Windows' own verdict on a drive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Health {
    Healthy,
    /// Still working, but Windows expects trouble.
    Warning,
    /// Failing or failed.
    Unhealthy,
    #[default]
    Unknown,
}

impl Health {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            Health::Healthy => "Healthy",
            Health::Warning => "Warning",
            Health::Unhealthy => "Failing",
            Health::Unknown => "Health unknown",
        }
    }
}

/// Wear and error counters kept by a drive itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DriveCounters {
    pub temperature_c: Option<i32>,
    /// Share of the drive's rated life that is used up, in percent. Can pass
    /// 100 on a drive that outlives its rating.
    pub wear_percent: Option<u32>,
    /// Spare blocks still available, in percent (solid-state drives).
    pub spare_percent: Option<u32>,
    /// Level of spare blocks below which the drive reports a problem.
    pub spare_threshold_percent: Option<u32>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    /// Errors the drive could not correct.
    pub media_errors: Option<u64>,
    pub read_errors: Option<u64>,
    pub write_errors: Option<u64>,
    pub bytes_read: Option<u64>,
    pub bytes_written: Option<u64>,
    /// The drive's own warning flags (NVMe "critical warning"); zero is good.
    pub critical_warning: Option<u8>,
}

impl DriveCounters {
    /// True when the drive reported nothing at all.
    pub fn is_empty(&self) -> bool {
        *self == DriveCounters::default()
    }
}

/// One physical drive.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhysicalDrive {
    /// Windows' disk number (`Disk 0`), when known.
    pub number: Option<u32>,
    pub model: String,
    pub bus: Bus,
    pub media: Media,
    pub size_bytes: u64,
    pub firmware: Option<String>,
    /// Rotation speed of a hard disk, in rpm.
    pub spindle_rpm: Option<u32>,
    pub health: Health,
    /// What the drive reports about itself; fields it does not report, or
    /// that Windows would not hand over, are `None`.
    pub counters: DriveCounters,
    /// True when wear and error counters exist but Windows only gives them
    /// to administrators, and this inventory was taken without those rights.
    pub counters_need_admin: bool,
    /// Volumes with a drive letter that live on this drive.
    pub volumes: Vec<Volume>,
    /// Partitions without a drive letter (boot, recovery and similar).
    pub hidden_partitions: u32,
}

impl PhysicalDrive {
    /// A short description such as `NVMe SSD` or `USB drive`.
    pub fn kind_label(&self) -> String {
        match (self.bus.label(), self.media.label()) {
            ("", "") => "Drive".to_owned(),
            ("", media) => media.to_owned(),
            (bus, "") => format!("{bus} drive"),
            (bus, media) => format!("{bus} {media}"),
        }
    }
}

/// A volume with a drive letter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    pub letter: char,
    pub label: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// The volume Windows is installed on.
    pub is_system: bool,
}

impl Volume {
    /// Bytes in use.
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    /// Used fraction in `0.0..=1.0`.
    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.used_bytes() as f64 / self.total_bytes as f64) as f32
        }
    }

    /// Free fraction in `0.0..=1.0`.
    pub fn free_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.free_bytes as f64 / self.total_bytes as f64) as f32
        }
    }

    /// A caption such as `Windows (C:)`.
    pub fn display_name(&self) -> String {
        if self.label.is_empty() {
            format!("Local disk ({}:)", self.letter)
        } else {
            format!("{} ({}:)", self.label, self.letter)
        }
    }
}

/// What a battery is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatteryState {
    Charging,
    Discharging,
    /// Plugged in and not charging (full, or held at a charge limit).
    Idle,
    #[default]
    Unknown,
}

impl BatteryState {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            BatteryState::Charging => "Charging",
            BatteryState::Discharging => "On battery",
            BatteryState::Idle => "Plugged in",
            BatteryState::Unknown => "Unknown",
        }
    }
}

/// One battery.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Battery {
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    /// `Lithium-ion`, `Lithium polymer`…
    pub chemistry: Option<String>,
    /// Capacity when new, in mWh.
    pub design_mwh: Option<u32>,
    /// Capacity of a full charge today, in mWh.
    pub full_charge_mwh: Option<u32>,
    /// Charge held right now, in mWh.
    pub remaining_mwh: Option<u32>,
    /// Charge held right now, in percent of a full charge.
    pub charge_percent: Option<f32>,
    /// Charge cycles so far. Many batteries do not count them.
    pub cycle_count: Option<u32>,
    pub voltage_mv: Option<u32>,
    /// Power flowing in (positive) or out (negative), in mW.
    pub rate_mw: Option<i32>,
    pub state: BatteryState,
}

impl Battery {
    /// Share of the original capacity that is gone, in percent.
    pub fn wear_percent(&self) -> Option<f32> {
        let design = self.design_mwh.filter(|&d| d > 0)? as f32;
        let full = self.full_charge_mwh? as f32;
        Some(((1.0 - full / design) * 100.0).clamp(0.0, 100.0))
    }

    /// Share of the original capacity a full charge still holds, in percent.
    pub fn health_percent(&self) -> Option<f32> {
        self.wear_percent().map(|wear| 100.0 - wear)
    }
}

/// What kind of network a physical adapter connects to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkKind {
    Ethernet,
    WiFi,
    Bluetooth,
    Cellular,
    #[default]
    Other,
}

impl NetworkKind {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            NetworkKind::Ethernet => "Ethernet",
            NetworkKind::WiFi => "Wi-Fi",
            NetworkKind::Bluetooth => "Bluetooth",
            NetworkKind::Cellular => "Cellular",
            NetworkKind::Other => "Network",
        }
    }
}

/// One physical network adapter. Addresses are deliberately left out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkAdapter {
    /// The device name, e.g. `Intel(R) Wi-Fi 6 AX201 160MHz`.
    pub name: String,
    pub kind: NetworkKind,
    pub connected: bool,
    /// Link speed in bits per second, while connected.
    pub link_bps: Option<u64>,
}

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    /// Worth knowing.
    Note,
    /// Should be looked at.
    Warning,
    /// Data is at risk.
    Critical,
}

/// Something about the hardware that deserves the user's attention.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub level: Level,
    /// One sentence saying what is wrong.
    pub title: String,
    /// What it means and what to do about it.
    pub detail: String,
}
