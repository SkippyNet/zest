//! Driver updates through the Windows Update Agent.
//!
//! All COM work happens on a dedicated thread (see
//! [`crate::com::run_on_com_thread`]); only plain data crosses back.

use super::{
    DriverInstallOutcome, DriverInstallStatus, DriverSearchOptions, DriverUpdate, UpdateService,
};
use crate::com::run_on_com_thread;
use chrono::NaiveDate;
use windows::Win32::Foundation::{DECIMAL, VARIANT_BOOL, VARIANT_TRUE};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::UpdateAgent::{
    IUpdate, IUpdate3, IUpdateCollection, IUpdateInstaller2, IUpdateSession, IWindowsDriverUpdate,
    UpdateCollection, UpdateSession, irbNeverReboots, orcAborted, orcSucceeded,
    orcSucceededWithErrors, ssWindowsUpdate,
};
use windows::core::{BSTR, Interface};
use zest_core::{Error, Progress, Result};

/// Search filter: driver updates that are applicable and not yet installed.
pub(super) const DRIVER_CRITERIA: &str = "IsInstalled=0 and Type='Driver'";

/// Name Zest identifies itself with in the Windows Update history.
const CLIENT_ID: &str = zest_core::brand::NAME;

fn truthy(value: VARIANT_BOOL) -> bool {
    value.0 != 0
}

/// Converts an OLE automation date (days since 1899-12-30) to a calendar date.
pub(super) fn ole_date(value: f64) -> Option<NaiveDate> {
    // 2958465 is 9999-12-31, the last date the format can express.
    if !value.is_finite() || !(1.0..=2_958_465.0).contains(&value) {
        return None;
    }
    NaiveDate::from_ymd_opt(1899, 12, 30)?.checked_add_days(chrono::Days::new(value.floor() as u64))
}

/// Converts a COM `DECIMAL` byte count to `u64`, saturating on overflow.
pub(super) fn decimal_to_u64(value: &DECIMAL) -> u64 {
    // SAFETY: both unions are plain integers; every bit pattern is valid.
    let (scale, negative, low) = unsafe {
        (
            value.Anonymous1.Anonymous.scale,
            value.Anonymous1.Anonymous.sign != 0,
            value.Anonymous2.Lo64,
        )
    };
    if negative {
        return 0;
    }
    if value.Hi32 != 0 {
        return u64::MAX;
    }
    low / 10u64.saturating_pow(u32::from(scale))
}

/// Plain-language explanations for the Windows Update failures users
/// actually run into.
pub(super) fn describe_error(hresult: i32) -> Option<&'static str> {
    Some(match hresult as u32 {
        0x8007_0422 => "The Windows Update service is disabled.",
        0x8024_002E => "Access to Windows Update is turned off by policy on this PC.",
        0x8024_0016 => "Another installation is in progress, or a restart is pending.",
        0x8024_001E | 0x8024_000B => "Windows Update stopped the operation.",
        0x8024_0438 | 0x8024_402C | 0x8024_401C | 0x8024_4022 | 0x8007_2EE2 | 0x8007_2EE7
        | 0x8007_2EFD | 0x8007_2EFE | 0x8024_0440 => {
            "Windows Update could not be reached. Check your internet connection."
        }
        0x8007_0005 => "Administrator rights are required.",
        0x8024_0024 => "There are no updates to install.",
        0x8024_0022 => "The update could not be installed. Restart your PC and try again.",
        0x8007_0070 => "There is not enough free disk space.",
        _ => return None,
    })
}

fn update_error(action: &str, e: windows::core::Error) -> Error {
    let code = e.code().0;
    let reason = describe_error(code)
        .map(str::to_owned)
        .unwrap_or_else(|| e.message());
    Error::Windows(format!("{action}: {reason} (0x{:08X})", code as u32))
}

/// Maps a Windows Update result code to an install status.
pub(super) fn install_status(result_code: i32, reboot_required: bool) -> DriverInstallStatus {
    if result_code == orcSucceeded.0 || result_code == orcSucceededWithErrors.0 {
        if reboot_required {
            DriverInstallStatus::InstalledRebootRequired
        } else {
            DriverInstallStatus::Installed
        }
    } else if result_code == orcAborted.0 {
        DriverInstallStatus::Aborted
    } else {
        DriverInstallStatus::InstallFailed
    }
}

/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn create_session() -> windows::core::Result<IUpdateSession> {
    // SAFETY: standard in-process activation of the update session.
    unsafe {
        let session: IUpdateSession = CoCreateInstance(&UpdateSession, None, CLSCTX_INPROC_SERVER)?;
        let _ = session.SetClientApplicationID(&BSTR::from(CLIENT_ID));
        Ok(session)
    }
}

/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn search(
    session: &IUpdateSession,
    service: UpdateService,
) -> windows::core::Result<IUpdateCollection> {
    // SAFETY: plain method calls on a live interface.
    unsafe {
        let searcher = session.CreateUpdateSearcher()?;
        // Always a fresh scan. Searching only the local cache was measured
        // to be far slower than asking the service, not faster.
        searcher.SetOnline(VARIANT_TRUE)?;
        if service == UpdateService::WindowsUpdate {
            searcher.SetServerSelection(ssWindowsUpdate)?;
        }
        searcher.Search(&BSTR::from(DRIVER_CRITERIA))?.Updates()
    }
}

/// # Safety
/// COM must be initialised on the calling thread; `update` must be live.
unsafe fn describe(update: &IUpdate) -> windows::core::Result<DriverUpdate> {
    // SAFETY: plain property reads on live interfaces.
    unsafe {
        let identity = update.Identity()?;
        let text = |value: windows::core::Result<BSTR>| {
            value
                .map(|b| b.to_string().trim().to_owned())
                .unwrap_or_default()
        };
        let driver: Option<IWindowsDriverUpdate> = update.cast().ok();
        let is_optional = update
            .cast::<IUpdate3>()
            .ok()
            .and_then(|u| u.BrowseOnly().ok())
            .map(truthy)
            .unwrap_or(false);
        let reboot_expected = update
            .InstallationBehavior()
            .and_then(|behavior| behavior.RebootBehavior())
            .map(|behavior| behavior != irbNeverReboots)
            .unwrap_or(false);
        Ok(DriverUpdate {
            title: text(update.Title()),
            description: text(update.Description()),
            driver_model: driver
                .as_ref()
                .map(|d| text(d.DriverModel()))
                .unwrap_or_default(),
            driver_provider: driver
                .as_ref()
                .map(|d| text(d.DriverProvider()))
                .unwrap_or_default(),
            driver_manufacturer: driver
                .as_ref()
                .map(|d| text(d.DriverManufacturer()))
                .unwrap_or_default(),
            driver_class: driver
                .as_ref()
                .map(|d| text(d.DriverClass()))
                .unwrap_or_default(),
            hardware_id: driver
                .as_ref()
                .map(|d| text(d.DriverHardwareID()))
                .unwrap_or_default(),
            driver_date: driver
                .as_ref()
                .and_then(|d| d.DriverVerDate().ok())
                .and_then(ole_date),
            size_bytes: update
                .MaxDownloadSize()
                .map(|size| decimal_to_u64(&size))
                .unwrap_or(0),
            update_id: text(identity.UpdateID()),
            revision: identity.RevisionNumber().unwrap_or(0),
            reboot_expected,
            is_optional,
            is_downloaded: update.IsDownloaded().map(truthy).unwrap_or(false),
            applies_to: None,
        })
    }
}

/// Searches Windows Update for driver updates. Read-only.
pub(super) fn search_driver_updates(
    options: &DriverSearchOptions,
    progress: &Progress,
) -> Result<Vec<DriverUpdate>> {
    let service = options.service;
    run_on_com_thread("driver-search", progress, true, move |progress| {
        progress.set_message("Asking Windows Update for driver updates…");
        // SAFETY: this closure runs on a thread with COM initialised.
        unsafe {
            let session =
                create_session().map_err(|e| update_error("Starting Windows Update", e))?;
            let offered = search(&session, service)
                .map_err(|e| update_error("Searching Windows Update", e))?;
            progress.check()?;
            let count = offered.Count().unwrap_or(0);
            let mut updates = Vec::with_capacity(count.max(0) as usize);
            for index in 0..count {
                let Ok(update) = offered.get_Item(index) else {
                    continue;
                };
                match describe(&update) {
                    Ok(found) if !found.update_id.is_empty() => updates.push(found),
                    Ok(_) => {}
                    Err(e) => log::warn!("skipping unreadable driver update: {e}"),
                }
            }
            Ok(updates)
        }
    })
}

fn outcome(
    wanted: &DriverUpdate,
    status: DriverInstallStatus,
    hresult: i32,
    reboot_required: bool,
) -> DriverInstallOutcome {
    let message = match status {
        DriverInstallStatus::Installed => "Driver installed.".to_owned(),
        DriverInstallStatus::InstalledRebootRequired => {
            "Driver installed. Restart your PC to start using it.".to_owned()
        }
        DriverInstallStatus::NoLongerOffered => {
            "Windows Update no longer offers this driver (it may already be installed).".to_owned()
        }
        DriverInstallStatus::Skipped => {
            "Not attempted because the operation was cancelled.".to_owned()
        }
        DriverInstallStatus::Aborted => "Windows Update stopped the installation.".to_owned(),
        DriverInstallStatus::DownloadFailed | DriverInstallStatus::InstallFailed => {
            let what = if status == DriverInstallStatus::DownloadFailed {
                "The driver could not be downloaded"
            } else {
                "The driver could not be installed"
            };
            match describe_error(hresult) {
                Some(reason) => format!("{what}. {reason}"),
                None if hresult != 0 => format!("{what} (0x{:08X}).", hresult as u32),
                None => format!("{what}."),
            }
        }
    };
    DriverInstallOutcome {
        update_id: wanted.update_id.clone(),
        title: wanted.title.clone(),
        status,
        hresult,
        reboot_required,
        message,
    }
}

/// # Safety
/// COM must be initialised on the calling thread; interfaces must be live.
unsafe fn install_one(
    session: &IUpdateSession,
    update: &IUpdate,
    wanted: &DriverUpdate,
    progress: &Progress,
) -> DriverInstallOutcome {
    let attempt = || -> windows::core::Result<DriverInstallOutcome> {
        // SAFETY: plain method calls on live interfaces.
        unsafe {
            if !truthy(update.EulaAccepted()?) {
                update.AcceptEula()?;
            }
            let batch: IUpdateCollection =
                CoCreateInstance(&UpdateCollection, None, CLSCTX_INPROC_SERVER)?;
            batch.Add(update)?;

            if !truthy(update.IsDownloaded()?) {
                progress.set_message(format!("Downloading {}…", wanted.title));
                let downloader = session.CreateUpdateDownloader()?;
                downloader.SetUpdates(&batch)?;
                let result = downloader.Download()?;
                let code = result.ResultCode()?;
                let downloaded = (code == orcSucceeded || code == orcSucceededWithErrors)
                    && truthy(update.IsDownloaded()?);
                if !downloaded {
                    let hresult = result.HResult().unwrap_or(0);
                    return Ok(outcome(
                        wanted,
                        DriverInstallStatus::DownloadFailed,
                        hresult,
                        false,
                    ));
                }
            }

            progress.set_message(format!("Installing {}…", wanted.title));
            let installer = session.CreateUpdateInstaller()?;
            if let Ok(quiet) = installer.cast::<IUpdateInstaller2>() {
                let _ = quiet.SetForceQuiet(VARIANT_TRUE);
            }
            installer.SetUpdates(&batch)?;
            let result = installer.Install()?;
            let single = result.GetUpdateResult(0)?;
            let reboot = truthy(single.RebootRequired()?)
                || result.RebootRequired().map(truthy).unwrap_or(false);
            let status = install_status(single.ResultCode()?.0, reboot);
            Ok(outcome(
                wanted,
                status,
                single.HResult().unwrap_or(0),
                reboot,
            ))
        }
    };
    match attempt() {
        Ok(done) => done,
        Err(e) => outcome(
            wanted,
            DriverInstallStatus::InstallFailed,
            e.code().0,
            false,
        ),
    }
}

/// Downloads and installs the given driver updates, one at a time.
/// The caller has already verified that the process is elevated.
pub(super) fn install(
    wanted: Vec<DriverUpdate>,
    progress: &Progress,
) -> Result<Vec<DriverInstallOutcome>> {
    // Never abandon the worker: an installation must be waited for.
    run_on_com_thread("driver-install", progress, false, move |progress| {
        progress.set_total(wanted.len() as u64);
        progress.set_done(0);
        progress.set_message("Contacting Windows Update…");
        // SAFETY: this closure runs on a thread with COM initialised.
        unsafe {
            let session =
                create_session().map_err(|e| update_error("Starting Windows Update", e))?;
            let offered = search(&session, UpdateService::Default)
                .map_err(|e| update_error("Searching Windows Update", e))?;
            let mut available: Vec<(String, i32, IUpdate)> = Vec::new();
            for index in 0..offered.Count().unwrap_or(0) {
                let Ok(update) = offered.get_Item(index) else {
                    continue;
                };
                let Ok(identity) = update.Identity() else {
                    continue;
                };
                let id = identity
                    .UpdateID()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                available.push((id, identity.RevisionNumber().unwrap_or(0), update));
            }

            let mut outcomes = Vec::with_capacity(wanted.len());
            for item in &wanted {
                if progress.is_cancelled() {
                    outcomes.push(outcome(item, DriverInstallStatus::Skipped, 0, false));
                    continue;
                }
                let exact = available.iter().find(|(id, revision, _)| {
                    id.eq_ignore_ascii_case(&item.update_id) && *revision == item.revision
                });
                let any_revision = || {
                    available
                        .iter()
                        .find(|(id, _, _)| id.eq_ignore_ascii_case(&item.update_id))
                };
                let result = match exact.or_else(any_revision) {
                    Some((_, _, update)) => install_one(&session, update, item, progress),
                    None => outcome(item, DriverInstallStatus::NoLongerOffered, 0, false),
                };
                log::info!("driver update '{}': {:?}", item.title, result.status);
                outcomes.push(result);
                progress.inc(1);
            }
            Ok(outcomes)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{DECIMAL_0, DECIMAL_0_0, DECIMAL_1};

    fn decimal(low: u64, hi: u32, scale: u8, sign: u8) -> DECIMAL {
        DECIMAL {
            wReserved: 0,
            Anonymous1: DECIMAL_0 {
                Anonymous: DECIMAL_0_0 { scale, sign },
            },
            Hi32: hi,
            Anonymous2: DECIMAL_1 { Lo64: low },
        }
    }

    #[test]
    fn decimal_conversion() {
        assert_eq!(decimal_to_u64(&decimal(0, 0, 0, 0)), 0);
        assert_eq!(decimal_to_u64(&decimal(52_428_800, 0, 0, 0)), 52_428_800);
        assert_eq!(decimal_to_u64(&decimal(123_456, 0, 3, 0)), 123);
        assert_eq!(decimal_to_u64(&decimal(5, 1, 0, 0)), u64::MAX);
        assert_eq!(decimal_to_u64(&decimal(5, 0, 0, 0x80)), 0);
        // A nonsensical scale must not panic.
        assert_eq!(decimal_to_u64(&decimal(5, 0, 200, 0)), 0);
    }

    #[test]
    fn ole_dates() {
        assert_eq!(ole_date(1.0), NaiveDate::from_ymd_opt(1899, 12, 31));
        assert_eq!(ole_date(2.0), NaiveDate::from_ymd_opt(1900, 1, 1));
        // 2024-03-15 is serial day 45366.
        assert_eq!(ole_date(45366.0), NaiveDate::from_ymd_opt(2024, 3, 15));
        assert_eq!(ole_date(45366.75), NaiveDate::from_ymd_opt(2024, 3, 15));
        assert_eq!(ole_date(0.0), None);
        assert_eq!(ole_date(-5.0), None);
        assert_eq!(ole_date(f64::NAN), None);
        assert_eq!(ole_date(f64::INFINITY), None);
        assert_eq!(ole_date(1e12), None);
    }

    #[test]
    fn result_codes() {
        use DriverInstallStatus as S;
        assert_eq!(install_status(2, false), S::Installed);
        assert_eq!(install_status(2, true), S::InstalledRebootRequired);
        assert_eq!(install_status(3, false), S::Installed);
        assert_eq!(install_status(4, false), S::InstallFailed);
        assert_eq!(install_status(5, false), S::Aborted);
        assert_eq!(install_status(0, false), S::InstallFailed);
        assert_eq!(install_status(1, true), S::InstallFailed);
        assert_eq!(install_status(99, false), S::InstallFailed);
    }

    #[test]
    fn known_errors_are_explained() {
        assert!(
            describe_error(0x8007_0422u32 as i32)
                .unwrap()
                .contains("disabled")
        );
        assert!(
            describe_error(0x8024_402Cu32 as i32)
                .unwrap()
                .contains("internet")
        );
        assert_eq!(describe_error(0x1234_5678), None);
        assert_eq!(describe_error(0), None);
    }

    #[test]
    fn outcome_messages() {
        let update = DriverUpdate {
            title: "Vendor - Display - 1.2.3.4".to_owned(),
            update_id: "11111111-2222-3333-4444-555555555555".to_owned(),
            ..DriverUpdate::default()
        };
        let ok = outcome(
            &update,
            DriverInstallStatus::InstalledRebootRequired,
            0,
            true,
        );
        assert!(ok.reboot_required && ok.message.contains("Restart"));
        assert_eq!(ok.update_id, update.update_id);
        let failed = outcome(
            &update,
            DriverInstallStatus::InstallFailed,
            0x8007_0070u32 as i32,
            false,
        );
        assert!(failed.message.contains("disk space"), "{}", failed.message);
        let unknown = outcome(
            &update,
            DriverInstallStatus::DownloadFailed,
            0x8024_9999u32 as i32,
            false,
        );
        assert!(
            unknown.message.contains("0x80249999"),
            "{}",
            unknown.message
        );
        let plain = outcome(&update, DriverInstallStatus::InstallFailed, 0, false);
        assert_eq!(plain.message, "The driver could not be installed.");
    }

    #[test]
    fn search_filter_only_reads_driver_updates() {
        assert_eq!(DRIVER_CRITERIA, "IsInstalled=0 and Type='Driver'");
    }
}
