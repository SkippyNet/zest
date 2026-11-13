//! Driver updater.
//!
//! Three things happen here:
//!
//! 1. [`list_drivers`] reports which driver every device is using – version,
//!    date, provider, who signed it and whether the device is working.
//! 2. [`check_driver_updates`] asks **Windows Update** whether newer drivers
//!    exist for this exact hardware.
//! 3. [`install_driver_updates`] lets Windows Update download and install the
//!    ones the user picked, and [`backup_drivers`] exports the current
//!    third-party drivers first so they can be rolled back.
//!
//! # Where drivers come from
//!
//! Windows Update is the **only** driver source. Drivers published there are
//! submitted by the hardware vendor, signed by Microsoft and offered only to
//! the hardware ids they were tested against. Zest never downloads driver
//! packages from anywhere else and never matches drivers to devices itself:
//! a wrong driver runs in the kernel and can stop a PC from starting, so the
//! matching is left to the component built for it.
//!
//! # Rights
//!
//! Listing and searching work for any user and change nothing. Installing
//! and backing up require administrator rights and fail with
//! [`zest_core::Error::NeedsElevation`] otherwise.

mod devices;
mod wua;

use crate::exec;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_core::{Error, IoContext, Progress, Result, elevation, process};

pub use chrono::NaiveDate;

/// Default age after which a third-party driver gets an "old" hint: three years.
pub const DEFAULT_OUTDATED_AFTER_DAYS: u32 = 3 * 365;

/// Options for [`list_drivers_with`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriverListOptions {
    /// Also list devices that are not currently connected.
    pub include_disconnected: bool,
    /// Resolve who signed each driver package (adds a moment to the scan).
    pub verify_signatures: bool,
    /// Age in days after which a third-party driver is flagged with
    /// [`DriverInfo::is_outdated_hint`]. Zero disables the hint.
    pub outdated_after_days: u32,
}

impl Default for DriverListOptions {
    fn default() -> Self {
        DriverListOptions {
            include_disconnected: false,
            verify_signatures: true,
            outdated_after_days: DEFAULT_OUTDATED_AFTER_DAYS,
        }
    }
}

/// The driver a device is using.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverInfo {
    /// Device name as shown in Device Manager.
    pub device_name: String,
    /// Setup class name, e.g. `Display`, `Net`, `USB`.
    pub device_class: String,
    /// Friendly class name, e.g. `Display adapters` (in the system language).
    pub class_description: String,
    /// Setup class GUID in registry format, `{XXXXXXXX-...}`.
    pub class_guid: String,
    /// Device manufacturer.
    pub manufacturer: String,
    /// Who supplied the driver package.
    pub provider: String,
    /// Driver version, e.g. `31.0.15.5222`.
    pub driver_version: String,
    /// Driver date as declared by the package.
    pub driver_date: Option<NaiveDate>,
    /// Name of the driver's INF: `oemN.inf` for third-party packages,
    /// anything else for drivers that ship with Windows.
    pub inf_name: String,
    /// Most specific hardware id (empty for devices that have none).
    pub hardware_id: String,
    /// All hardware ids, most specific first.
    pub hardware_ids: Vec<String>,
    /// Compatible ids, most specific first.
    pub compatible_ids: Vec<String>,
    /// Device instance path; unique per device.
    pub instance_id: String,
    /// Whether the driver package carries a valid signature. `None` when
    /// signatures were not checked.
    pub is_signed: Option<bool>,
    /// Who signed the driver package, when known.
    pub signer: Option<String>,
    /// The device is currently connected.
    pub is_present: bool,
    /// The device is started and running.
    pub is_started: bool,
    /// Device Manager problem code; `0` means the device works.
    pub problem_code: u32,
    /// The driver ships with Windows (its INF is not an `oemN.inf`).
    pub is_inbox: bool,
    /// The driver's provider is Microsoft.
    pub is_microsoft: bool,
    /// A third-party driver older than the configured age. This is a hint to
    /// look for an update, not a verdict: old drivers are often perfectly fine.
    pub is_outdated_hint: bool,
}

impl DriverInfo {
    /// True when Device Manager reports a problem with the device.
    pub fn has_problem(&self) -> bool {
        self.problem_code != 0
    }

    /// True for drivers that did not ship with Windows.
    pub fn is_third_party(&self) -> bool {
        !self.is_inbox
    }

    /// Plain-language description of [`problem_code`](Self::problem_code).
    pub fn problem_description(&self) -> Option<&'static str> {
        problem_text(self.problem_code)
    }
}

/// Explains a Device Manager problem code. Returns `None` for `0` (no
/// problem) and for codes without a specific explanation.
pub fn problem_text(code: u32) -> Option<&'static str> {
    Some(match code {
        1 => "The device is not configured correctly.",
        3 => "The driver may be corrupted, or the system is low on memory.",
        10 => "The device cannot start.",
        12 => "The device cannot find enough free resources.",
        14 => "Restart your PC for this device to work.",
        18 => "The drivers for this device need to be reinstalled.",
        19 => "The device's configuration in the registry is damaged.",
        21 => "Windows is removing this device.",
        22 => "The device is disabled.",
        24 => "The device is not present or is not working properly.",
        28 => "No driver is installed for this device.",
        29 => "The device's firmware did not give it the resources it needs.",
        31 => "Windows cannot load the drivers this device needs.",
        32 => "The driver for this device has been disabled.",
        33 => "Windows cannot determine which resources the device needs.",
        34 => "The device needs manual configuration.",
        35 => "The system firmware lacks information to configure the device.",
        37 => "The driver could not be initialised.",
        38 => "A previous instance of the driver is still in memory.",
        39 => "The driver is corrupted or missing.",
        40 => "The driver's service information is missing or wrong.",
        41 => "The driver loaded but the hardware could not be found.",
        42 => "A duplicate device is already running.",
        43 => "The device reported a problem and was stopped.",
        44 => "An application or service shut this device down.",
        45 => "The device is not connected.",
        46 => "The device is unavailable while Windows is shutting down.",
        47 => "The device has been prepared for safe removal.",
        48 => "The driver was blocked because it is known to cause problems.",
        49 => "The system registry is too large to start new devices.",
        50 => "Windows cannot apply all properties for this device.",
        51 => "The device is waiting for another device to start.",
        52 => "The driver's digital signature could not be verified.",
        53 => "The device is reserved for use by the kernel debugger.",
        54 => "The device failed and is being reset.",
        56 => "Windows is still setting up the class configuration for this device.",
        57 => "The device's driver package needs to be reinstalled.",
        _ => return None,
    })
}

/// Lists the driver of every connected device, with signatures verified.
///
/// Equivalent to [`list_drivers_with`] and default options. Read-only; takes
/// roughly a second. Devices without a driver package are left out.
pub fn list_drivers(progress: &Progress) -> Result<Vec<DriverInfo>> {
    list_drivers_with(&DriverListOptions::default(), progress)
}

/// Lists the driver of every device that has one.
///
/// The result is sorted by device class, then device name. While signatures
/// are being verified `progress` counts driver packages.
///
/// # Errors
///
/// [`Error::Cancelled`] when `progress` is cancelled; [`Error::Windows`] when
/// the device list cannot be opened.
pub fn list_drivers_with(
    options: &DriverListOptions,
    progress: &Progress,
) -> Result<Vec<DriverInfo>> {
    devices::enumerate(options, progress)
}

/// A driver package and the devices that use it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverPackage {
    /// INF name of the package.
    pub inf_name: String,
    /// Who supplied the package.
    pub provider: String,
    /// Package version.
    pub driver_version: String,
    /// Package date.
    pub driver_date: Option<NaiveDate>,
    /// Setup class name of the first device using it.
    pub device_class: String,
    /// Friendly class name of the first device using it.
    pub class_description: String,
    /// See [`DriverInfo::is_signed`].
    pub is_signed: Option<bool>,
    /// See [`DriverInfo::signer`].
    pub signer: Option<String>,
    /// See [`DriverInfo::is_inbox`].
    pub is_inbox: bool,
    /// See [`DriverInfo::is_microsoft`].
    pub is_microsoft: bool,
    /// See [`DriverInfo::is_outdated_hint`].
    pub is_outdated_hint: bool,
    /// At least one device using this package reports a problem.
    pub has_problem: bool,
    /// Names of the devices using this package, sorted and de-duplicated.
    pub devices: Vec<String>,
}

/// Collapses a device list into distinct driver packages.
///
/// Many devices share one package (every processor core, every USB hub), so
/// this is the more readable view for a "drivers" page. Sorted by provider,
/// then INF name.
pub fn group_by_package(drivers: &[DriverInfo]) -> Vec<DriverPackage> {
    let mut packages: Vec<DriverPackage> = Vec::new();
    for driver in drivers {
        let existing = packages.iter_mut().find(|p| {
            p.inf_name.eq_ignore_ascii_case(&driver.inf_name)
                && p.driver_version == driver.driver_version
        });
        match existing {
            Some(package) => {
                package.has_problem |= driver.has_problem();
                package.devices.push(driver.device_name.clone());
            }
            None => packages.push(DriverPackage {
                inf_name: driver.inf_name.clone(),
                provider: driver.provider.clone(),
                driver_version: driver.driver_version.clone(),
                driver_date: driver.driver_date,
                device_class: driver.device_class.clone(),
                class_description: driver.class_description.clone(),
                is_signed: driver.is_signed,
                signer: driver.signer.clone(),
                is_inbox: driver.is_inbox,
                is_microsoft: driver.is_microsoft,
                is_outdated_hint: driver.is_outdated_hint,
                has_problem: driver.has_problem(),
                devices: vec![driver.device_name.clone()],
            }),
        }
    }
    for package in &mut packages {
        package.devices.sort_by_key(|name| name.to_lowercase());
        package.devices.dedup();
    }
    packages.sort_by(|a, b| {
        a.provider
            .to_lowercase()
            .cmp(&b.provider.to_lowercase())
            .then_with(|| a.inf_name.to_lowercase().cmp(&b.inf_name.to_lowercase()))
    });
    packages
}

/// Which update service to ask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpdateService {
    /// Whatever this PC is configured to use (Windows Update at home, the
    /// organisation's update server on managed PCs).
    #[default]
    Default,
    /// Windows Update itself, even on a PC that normally uses a managed
    /// server. May be blocked by policy.
    WindowsUpdate,
}

/// Options for [`check_driver_updates_with`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriverSearchOptions {
    /// Update service to search.
    pub service: UpdateService,
}

/// What kind of package a [`DriverUpdate`] delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DriverUpdateKind {
    /// A device driver that replaces the one in use.
    Driver,
    /// An extension that adds vendor customisations on top of the device's
    /// base driver without replacing it.
    Extension,
    /// Device or system firmware (for example a UEFI/BIOS update). Applied
    /// during a restart; the PC must stay powered until it completes.
    Firmware,
    /// A software component that accompanies a driver (control panels,
    /// audio effects and similar).
    SoftwareComponent,
}

/// The driver currently installed for the device a [`DriverUpdate`] targets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledDriverRef {
    /// Device the update applies to.
    pub device_name: String,
    /// Version currently installed.
    pub driver_version: String,
    /// Date of the driver currently installed.
    pub driver_date: Option<NaiveDate>,
    /// Provider of the driver currently installed.
    pub provider: String,
    /// INF of the driver currently installed.
    pub inf_name: String,
}

/// A driver update offered by Windows Update.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverUpdate {
    /// Title as published, e.g. `Vendor - Display - 31.0.101.5522`.
    pub title: String,
    /// Publisher's description.
    pub description: String,
    /// Device model the driver is for.
    pub driver_model: String,
    /// Who supplies the driver.
    pub driver_provider: String,
    /// Hardware manufacturer.
    pub driver_manufacturer: String,
    /// Driver class, e.g. `Display` or `Net`.
    pub driver_class: String,
    /// Hardware id the update was matched to on this PC.
    pub hardware_id: String,
    /// Date of the offered driver.
    pub driver_date: Option<NaiveDate>,
    /// Download size in bytes (upper bound).
    pub size_bytes: u64,
    /// Windows Update identifier of the update.
    pub update_id: String,
    /// Revision of the update.
    pub revision: i32,
    /// Installing it may ask for a restart.
    pub reboot_expected: bool,
    /// Windows Update classifies it as optional rather than automatic.
    pub is_optional: bool,
    /// Already downloaded by Windows Update.
    pub is_downloaded: bool,
    /// The device this update targets and the driver that device uses now,
    /// when it could be identified (best effort, matched by hardware id).
    /// A [`DriverUpdateKind::Driver`] update replaces that driver; the other
    /// kinds are installed alongside it.
    pub applies_to: Option<InstalledDriverRef>,
}

impl DriverUpdate {
    /// Classifies the update so a front end can explain what it does (and
    /// warn before firmware updates).
    pub fn kind(&self) -> DriverUpdateKind {
        let class = self.driver_class.to_ascii_lowercase();
        let hardware_id = self.hardware_id.to_ascii_lowercase();
        if class == "firmware" || hardware_id.starts_with("uefi\\") {
            DriverUpdateKind::Firmware
        } else if class == "extension" {
            DriverUpdateKind::Extension
        } else if class == "softwarecomponent" || hardware_id.starts_with("swc\\") {
            DriverUpdateKind::SoftwareComponent
        } else {
            DriverUpdateKind::Driver
        }
    }
}

/// Finds the installed driver an update with this hardware id applies to.
fn match_installed(
    hardware_id: &str,
    model: &str,
    installed: &[DriverInfo],
) -> Option<InstalledDriverRef> {
    let reference = |driver: &DriverInfo| InstalledDriverRef {
        device_name: driver.device_name.clone(),
        driver_version: driver.driver_version.clone(),
        driver_date: driver.driver_date,
        provider: driver.provider.clone(),
        inf_name: driver.inf_name.clone(),
    };
    let same = |a: &String| a.eq_ignore_ascii_case(hardware_id);
    if !hardware_id.is_empty() {
        if let Some(driver) = installed.iter().find(|d| d.hardware_ids.iter().any(same)) {
            return Some(reference(driver));
        }
        if let Some(driver) = installed.iter().find(|d| d.compatible_ids.iter().any(same)) {
            return Some(reference(driver));
        }
    }
    // Last resort: the device carries exactly the model name of the update.
    let model = model.trim();
    if !model.is_empty() {
        let mut by_name = installed
            .iter()
            .filter(|d| d.device_name.eq_ignore_ascii_case(model));
        if let (Some(driver), None) = (by_name.next(), by_name.next()) {
            return Some(reference(driver));
        }
    }
    None
}

/// Asks Windows Update for driver updates, using this PC's default update
/// service. See [`check_driver_updates_with`].
pub fn check_driver_updates(progress: &Progress) -> Result<Vec<DriverUpdate>> {
    check_driver_updates_with(&DriverSearchOptions::default(), progress)
}

/// Asks Windows Update which newer drivers it has for this PC's hardware.
///
/// This is a search only: nothing is downloaded or installed and no
/// administrator rights are needed. The search contacts the update service,
/// so it needs a network connection and can take anywhere from a few seconds
/// to a couple of minutes; `progress` stays indeterminate meanwhile. Each result is matched, where possible, to the device it
/// targets and the driver that device uses now.
///
/// Cancelling `progress` returns promptly with [`Error::Cancelled`]; the
/// request already sent to the update service is left to finish on its own.
///
/// # Errors
///
/// [`Error::Windows`] with a plain-language reason when the update service
/// is disabled, blocked by policy or unreachable.
pub fn check_driver_updates_with(
    options: &DriverSearchOptions,
    progress: &Progress,
) -> Result<Vec<DriverUpdate>> {
    progress.check()?;
    let mut updates = wua::search_driver_updates(options, progress)?;
    progress.check()?;

    if !updates.is_empty() {
        // Matching is a convenience; a failed device listing must not hide
        // the updates themselves.
        let quiet = Progress::new();
        let listing = DriverListOptions {
            verify_signatures: false,
            ..DriverListOptions::default()
        };
        match devices::enumerate(&listing, &quiet) {
            Ok(installed) => {
                for update in &mut updates {
                    update.applies_to =
                        match_installed(&update.hardware_id, &update.driver_model, &installed);
                }
            }
            Err(e) => log::warn!("could not match driver updates to devices: {e}"),
        }
    }

    updates.sort_by(|a, b| {
        a.driver_class
            .to_lowercase()
            .cmp(&b.driver_class.to_lowercase())
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    progress.set_message(match updates.len() {
        0 => "No driver updates available".to_owned(),
        1 => "1 driver update available".to_owned(),
        n => format!("{n} driver updates available"),
    });
    Ok(updates)
}

/// How installing one driver update ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DriverInstallStatus {
    /// Installed and active.
    Installed,
    /// Installed; takes effect after a restart.
    InstalledRebootRequired,
    /// The download failed; nothing was changed.
    DownloadFailed,
    /// Windows Update could not install the driver.
    InstallFailed,
    /// Windows Update stopped the installation.
    Aborted,
    /// Windows Update no longer offers this update.
    NoLongerOffered,
    /// Not attempted because the operation was cancelled first.
    Skipped,
}

impl DriverInstallStatus {
    /// True when the driver was installed (possibly pending a restart).
    pub fn is_success(self) -> bool {
        matches!(
            self,
            DriverInstallStatus::Installed | DriverInstallStatus::InstalledRebootRequired
        )
    }
}

/// Result of installing one driver update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverInstallOutcome {
    /// Windows Update identifier of the update.
    pub update_id: String,
    /// Title of the update.
    pub title: String,
    /// What happened.
    pub status: DriverInstallStatus,
    /// Result code reported by Windows Update (`0` when none).
    pub hresult: i32,
    /// A restart is needed to finish.
    pub reboot_required: bool,
    /// One plain-language sentence for the user.
    pub message: String,
}

/// Downloads and installs the selected driver updates through Windows Update.
///
/// Updates are processed one at a time; `progress` counts finished updates
/// and carries the current step. Cancelling takes effect *between* updates –
/// an installation in flight is always allowed to finish – and the remaining
/// updates are reported as [`DriverInstallStatus::Skipped`].
///
/// Returns one outcome per entry of `updates`, in the same order. Consider
/// offering [`backup_drivers`] first.
///
/// # Errors
///
/// * [`Error::NeedsElevation`] when Zest is not running as administrator
///   (nothing is attempted).
/// * [`Error::Windows`] when Windows Update cannot be reached at all.
pub fn install_driver_updates(
    updates: &[DriverUpdate],
    progress: &Progress,
) -> Result<Vec<DriverInstallOutcome>> {
    if updates.is_empty() {
        return Ok(Vec::new());
    }
    if !elevation::is_elevated() {
        return Err(Error::NeedsElevation("install driver updates".to_owned()));
    }
    progress.check()?;
    wua::install(updates.to_vec(), progress)
}

/// What [`backup_drivers`] produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverBackupReport {
    /// Folder the driver packages were exported to.
    pub dest: PathBuf,
    /// Number of driver packages exported.
    pub packages: u64,
    /// Total size of the exported files in bytes.
    pub bytes: u64,
    /// Exit code of the export tool.
    pub exit_code: Option<i32>,
}

/// Arguments of the Windows tool invocation that exports every third-party
/// driver package to `dest`.
pub fn backup_command_args(dest: &Path) -> Vec<OsString> {
    vec![
        "/export-driver".into(),
        "*".into(),
        dest.as_os_str().to_owned(),
    ]
}

/// Counts exported packages (sub-folders containing an INF) and their size.
fn measure_backup(dest: &Path) -> (u64, u64) {
    let mut packages = 0u64;
    for entry in std::fs::read_dir(dest).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let has_inf = std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .flatten()
            .any(|file| {
                file.path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("inf"))
            });
        if has_inf {
            packages += 1;
        }
    }
    (packages, zest_core::walk::tree_size(dest, None).bytes)
}

/// Exports every third-party driver package to `dest` so the current drivers
/// can be reinstalled later (through Device Manager's "Update driver" →
/// "Browse my computer", pointing at this folder).
///
/// Uses the Windows driver store tool (`pnputil /export-driver`); drivers
/// that ship with Windows are not exported because Windows can always
/// restore them itself. `dest` is created if needed.
///
/// # Errors
///
/// * [`Error::NeedsElevation`] when not running as administrator.
/// * [`Error::Invalid`] when `dest` is not an absolute path.
/// * [`Error::Command`] when the export tool fails.
pub fn backup_drivers(dest: &Path, progress: &Progress) -> Result<DriverBackupReport> {
    if !dest.is_absolute() {
        return Err(Error::Invalid(format!(
            "{} is not an absolute folder path",
            dest.display()
        )));
    }
    if !elevation::is_elevated() {
        return Err(Error::NeedsElevation("back up drivers".to_owned()));
    }
    progress.check()?;
    std::fs::create_dir_all(dest).ctx(format!("creating folder {}", dest.display()))?;
    progress.set_message("Exporting driver packages…");

    let mut command = process::hidden_command(process::system_tool("pnputil.exe"));
    command.args(backup_command_args(dest));
    let output = exec::run_cancellable(
        command,
        progress,
        Some(Duration::from_secs(30 * 60)),
        &mut |_| {},
    )?;
    let (packages, bytes) = measure_backup(dest);
    if output.code != Some(0) && packages == 0 {
        let detail = output
            .stdout
            .lines()
            .chain(output.stderr.lines())
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("no output")
            .to_owned();
        return Err(Error::Command(format!(
            "the driver export failed: {detail}"
        )));
    }
    progress.set_message(format!("{packages} driver packages exported"));
    Ok(DriverBackupReport {
        dest: dest.to_path_buf(),
        packages,
        bytes,
        exit_code: output.code,
    })
}

/// Opens Device Manager.
///
/// # Errors
///
/// [`Error::Windows`] when the shell refuses to open it.
pub fn open_device_manager() -> Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    let console = HSTRING::from(process::system_tool("devmgmt.msc").as_os_str());
    // SAFETY: both strings are NUL terminated and outlive the call.
    let instance = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &console,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = instance.0 as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(Error::Windows(format!(
            "could not open Device Manager (code {code})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver(name: &str, inf: &str, version: &str, hardware_ids: &[&str]) -> DriverInfo {
        DriverInfo {
            device_name: name.to_owned(),
            device_class: "Display".to_owned(),
            class_description: "Display adapters".to_owned(),
            class_guid: "{4D36E968-E325-11CE-BFC1-08002BE10318}".to_owned(),
            manufacturer: "Vendor".to_owned(),
            provider: "Vendor".to_owned(),
            driver_version: version.to_owned(),
            driver_date: NaiveDate::from_ymd_opt(2023, 5, 1),
            inf_name: inf.to_owned(),
            hardware_id: hardware_ids.first().copied().unwrap_or_default().to_owned(),
            hardware_ids: hardware_ids.iter().map(|s| (*s).to_owned()).collect(),
            compatible_ids: vec!["PCI\\CC_0300".to_owned()],
            instance_id: format!("PCI\\{name}"),
            is_signed: Some(true),
            signer: Some("Microsoft Windows Hardware Compatibility Publisher".to_owned()),
            is_present: true,
            is_started: true,
            problem_code: 0,
            is_inbox: false,
            is_microsoft: false,
            is_outdated_hint: false,
        }
    }

    #[test]
    fn real_listing_is_plausible() {
        // Read-only: the same data Device Manager shows.
        let progress = Progress::new();
        let drivers = list_drivers(&progress).unwrap();
        assert!(drivers.len() > 10, "only {} devices found", drivers.len());
        assert!(drivers.iter().all(|d| !d.inf_name.is_empty()));
        assert!(
            drivers
                .iter()
                .all(|d| d.is_inbox != devices::is_oem_inf(&d.inf_name))
        );
        assert!(
            drivers
                .iter()
                .all(|d| !d.is_outdated_hint || d.is_third_party())
        );
        assert!(drivers.iter().any(|d| d.is_microsoft && d.is_inbox));
        assert!(drivers.iter().all(|d| d.is_signed.is_some()));
        // Drivers that ship with Windows are signed by Microsoft.
        assert!(
            drivers
                .iter()
                .any(|d| d.is_inbox && d.is_signed == Some(true) && d.signer.is_some())
        );
        assert!(drivers.iter().all(|d| d.is_present));
        assert!(drivers.iter().any(|d| d.driver_date.is_some()));
        assert!(drivers.iter().any(|d| !d.class_description.is_empty()));

        let packages = group_by_package(&drivers);
        assert!(!packages.is_empty() && packages.len() <= drivers.len());
        let devices_in_packages: usize = packages.iter().map(|p| p.devices.len()).sum();
        assert!(devices_in_packages <= drivers.len());
    }

    #[test]
    fn listing_without_signatures_and_cancellation() {
        let options = DriverListOptions {
            verify_signatures: false,
            ..DriverListOptions::default()
        };
        let drivers = list_drivers_with(&options, &Progress::new()).unwrap();
        assert!(
            drivers
                .iter()
                .all(|d| d.is_signed.is_none() && d.signer.is_none())
        );

        let cancelled = Progress::new();
        cancelled.cancel();
        assert!(matches!(list_drivers(&cancelled), Err(Error::Cancelled)));
        assert!(matches!(
            check_driver_updates(&cancelled),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn grouping_collapses_shared_packages() {
        let mut broken = driver("GPU B", "oem7.inf", "1.0", &["PCI\\VEN_1&DEV_2"]);
        broken.problem_code = 43;
        let drivers = vec![
            driver("GPU A", "oem7.inf", "1.0", &["PCI\\VEN_1&DEV_1"]),
            broken,
            driver("GPU A", "OEM7.INF", "1.0", &["PCI\\VEN_1&DEV_1"]),
            driver("Audio", "oem9.inf", "2.0", &["HDAUDIO\\X"]),
            driver("Old GPU", "oem7.inf", "0.9", &["PCI\\VEN_1&DEV_0"]),
        ];
        let packages = group_by_package(&drivers);
        assert_eq!(packages.len(), 3);
        let shared = packages
            .iter()
            .find(|p| p.inf_name == "oem7.inf" && p.driver_version == "1.0")
            .unwrap();
        assert_eq!(shared.devices, ["GPU A", "GPU B"]);
        assert!(shared.has_problem);
        assert!(
            !packages
                .iter()
                .find(|p| p.inf_name == "oem9.inf")
                .unwrap()
                .has_problem
        );
    }

    #[test]
    fn updates_are_matched_by_hardware_id() {
        let installed = vec![
            driver(
                "Graphics Adapter",
                "oem7.inf",
                "1.0",
                &["PCI\\VEN_10DE&DEV_2684&SUBSYS_1", "PCI\\VEN_10DE&DEV_2684"],
            ),
            driver(
                "Network Adapter",
                "oem3.inf",
                "5.5",
                &["PCI\\VEN_8086&DEV_15F3"],
            ),
        ];
        let hit = match_installed("pci\\ven_10de&dev_2684", "", &installed).unwrap();
        assert_eq!(hit.device_name, "Graphics Adapter");
        assert_eq!(hit.driver_version, "1.0");
        assert_eq!(hit.inf_name, "oem7.inf");
        // Compatible ids are a weaker, second-choice match.
        assert_eq!(
            match_installed("PCI\\CC_0300", "", &installed)
                .unwrap()
                .device_name,
            "Graphics Adapter"
        );
        // Model name only when unambiguous.
        assert_eq!(
            match_installed("", "network adapter", &installed)
                .unwrap()
                .inf_name,
            "oem3.inf"
        );
        assert!(match_installed("USB\\VID_0000", "Something Else", &installed).is_none());
        assert!(match_installed("", "", &installed).is_none());
        let twins = vec![
            driver("Twin", "oem1.inf", "1", &["A"]),
            driver("Twin", "oem2.inf", "2", &["B"]),
        ];
        assert!(match_installed("", "Twin", &twins).is_none());
    }

    #[test]
    fn update_kinds() {
        let update = |class: &str, hardware_id: &str| DriverUpdate {
            driver_class: class.to_owned(),
            hardware_id: hardware_id.to_owned(),
            ..DriverUpdate::default()
        };
        assert_eq!(
            update("Display", "pci\\ven_10de&dev_2684").kind(),
            DriverUpdateKind::Driver
        );
        assert_eq!(update("Net", "").kind(), DriverUpdateKind::Driver);
        assert_eq!(
            update("Extension", "pci\\ven_8086&dev_a7a8").kind(),
            DriverUpdateKind::Extension
        );
        assert_eq!(
            update("Firmware", "uefi\\res_{0000}").kind(),
            DriverUpdateKind::Firmware
        );
        assert_eq!(
            update("OtherHardware", "UEFI\\RES_{0000}").kind(),
            DriverUpdateKind::Firmware
        );
        assert_eq!(
            update("OtherHardware", "swc\\ven_10ec&sid_0001").kind(),
            DriverUpdateKind::SoftwareComponent
        );
        assert_eq!(
            update("SoftwareComponent", "").kind(),
            DriverUpdateKind::SoftwareComponent
        );
        assert_eq!(DriverUpdate::default().kind(), DriverUpdateKind::Driver);
    }

    #[test]
    fn problem_codes() {
        assert_eq!(problem_text(0), None);
        assert!(problem_text(22).unwrap().contains("disabled"));
        assert!(problem_text(28).unwrap().contains("No driver"));
        assert!(problem_text(43).unwrap().contains("stopped"));
        assert_eq!(problem_text(9999), None);
        let mut d = driver("X", "oem1.inf", "1", &[]);
        assert!(!d.has_problem() && d.problem_description().is_none());
        d.problem_code = 22;
        assert!(d.has_problem() && d.problem_description().is_some());
    }

    #[test]
    fn backup_arguments_and_guards() {
        let args = backup_command_args(Path::new(r"D:\Driver Backup"));
        assert_eq!(args, ["/export-driver", "*", r"D:\Driver Backup"]);
        // Relative paths are rejected before anything else happens.
        let err = backup_drivers(Path::new("relative\\folder"), &Progress::new()).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
        // Without administrator rights nothing is created or run.
        if !elevation::is_elevated() {
            let tmp = tempfile::tempdir().unwrap();
            let dest = tmp.path().join("backup");
            let err = backup_drivers(&dest, &Progress::new()).unwrap_err();
            assert!(matches!(err, Error::NeedsElevation(_)));
            assert!(!dest.exists());
        }
    }

    #[test]
    fn install_guards() {
        // Nothing to do is not an error and needs no rights.
        assert!(
            install_driver_updates(&[], &Progress::new())
                .unwrap()
                .is_empty()
        );
        // Without administrator rights nothing is attempted.
        if !elevation::is_elevated() {
            let update = DriverUpdate {
                title: "Vendor - Display - 1.0".to_owned(),
                update_id: "00000000-0000-0000-0000-000000000000".to_owned(),
                ..DriverUpdate::default()
            };
            let err = install_driver_updates(&[update], &Progress::new()).unwrap_err();
            assert!(matches!(err, Error::NeedsElevation(_)));
        }
    }

    #[test]
    fn backup_measurement_counts_package_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("oem1.inf_amd64_abc");
        let b = tmp.path().join("oem2.inf_amd64_def");
        let stray = tmp.path().join("not-a-package");
        for dir in [&a, &b, &stray] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(a.join("oem1.inf"), vec![0u8; 100]).unwrap();
        std::fs::write(a.join("driver.sys"), vec![0u8; 900]).unwrap();
        std::fs::write(b.join("OEM2.INF"), vec![0u8; 50]).unwrap();
        std::fs::write(stray.join("readme.txt"), vec![0u8; 5]).unwrap();
        assert_eq!(measure_backup(tmp.path()), (2, 1055));
        assert_eq!(measure_backup(&tmp.path().join("missing")), (0, 0));
    }

    #[test]
    fn status_helpers() {
        assert!(DriverInstallStatus::Installed.is_success());
        assert!(DriverInstallStatus::InstalledRebootRequired.is_success());
        assert!(!DriverInstallStatus::DownloadFailed.is_success());
        assert!(!DriverInstallStatus::Skipped.is_success());
        assert_eq!(
            DriverSearchOptions::default().service,
            UpdateService::Default
        );
        let defaults = DriverListOptions::default();
        assert!(defaults.verify_signatures && !defaults.include_disconnected);
        assert_eq!(defaults.outdated_after_days, DEFAULT_OUTDATED_AFTER_DAYS);
    }
}
