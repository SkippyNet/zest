//! Full system images through the imaging engine built into Windows.
//!
//! Windows ships `wbadmin`, the command line of its own backup engine. An
//! image it writes can be restored from the Windows Recovery Environment even
//! when Windows no longer starts, which no program running inside Windows can
//! offer for the drive Windows itself runs from. This module builds the
//! commands, follows their progress and reads their listings; the imaging
//! and the recovery are done by Windows.
//!
//! Everything here needs administrator rights.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use zest_core::{Error, IoContext, Progress, Result, elevation, process};

/// The Windows tool this module drives.
pub const TOOL: &str = "wbadmin.exe";
/// Folder Windows creates on the target drive to hold images.
pub const IMAGE_FOLDER: &str = "WindowsImageBackup";

/// How to get a system image back, in the order a person does it.
pub const RESTORE_STEPS: [&str; 5] = [
    "Connect the drive that holds the image.",
    "Open Settings > System > Recovery and choose Restart now beside Advanced startup. If Windows does not start, boot from a Windows installation or recovery USB drive and choose Repair your computer.",
    "Choose Troubleshoot > Advanced options > See more recovery options > System Image Recovery.",
    "Pick the image by its date and confirm.",
    "Windows replaces everything on the imaged drives with the contents of the image, then restarts.",
];

/// Full path of the imaging tool.
pub fn tool_path() -> PathBuf {
    process::system_tool(TOOL)
}

/// True when this edition of Windows includes the imaging tool.
pub fn is_available() -> bool {
    tool_path().is_file()
}

/// What to image and where to put it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRequest {
    /// Drive that receives the image, e.g. `E:`.
    pub target: String,
    /// Drives to image besides the ones Windows needs, e.g. `["D:"]`.
    pub include: Vec<String>,
    /// Include every drive Windows needs to start (system, boot and recovery
    /// partitions), so the image can rebuild the PC from nothing.
    pub all_critical: bool,
}

/// Brings `e`, `E:` or `e:\` into the form `E:`. Anything else is refused,
/// so no stray text ever reaches the command line.
pub fn normalize_volume(text: &str) -> Option<String> {
    let trimmed = text.trim().trim_end_matches('\\');
    let mut chars = trimmed.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    match chars.as_str() {
        "" | ":" => Some(format!("{}:", letter.to_ascii_uppercase())),
        _ => None,
    }
}

/// Arguments for `wbadmin` that start the requested image.
pub fn start_backup_args(request: &ImageRequest) -> Result<Vec<String>> {
    let bad = |what: &str| Error::Invalid(format!("{what:?} is not a drive letter"));
    let target = normalize_volume(&request.target).ok_or_else(|| bad(&request.target))?;
    let mut include: Vec<String> = Vec::new();
    for volume in &request.include {
        let volume = normalize_volume(volume).ok_or_else(|| bad(volume))?;
        if volume == target {
            return Err(Error::Invalid(format!(
                "drive {target} cannot be both part of the image and the place it is saved to"
            )));
        }
        if !include.contains(&volume) {
            include.push(volume);
        }
    }
    if include.is_empty() && !request.all_critical {
        return Err(Error::Invalid("choose at least one drive to image".into()));
    }
    let mut args = vec![
        "start".to_owned(),
        "backup".to_owned(),
        format!("-backupTarget:{target}"),
    ];
    if !include.is_empty() {
        args.push(format!("-include:{}", include.join(",")));
    }
    if request.all_critical {
        args.push("-allCritical".to_owned());
    }
    args.push("-quiet".to_owned());
    Ok(args)
}

/// The percentage in a progress line such as
/// `Creating a backup of volume (C:), copied (12%).`
///
/// Only the number in brackets is looked at, so the line may be in any
/// language.
pub fn parse_progress(line: &str) -> Option<u8> {
    let end = line.rfind("%)")?;
    let start = line[..end].rfind('(')?;
    let number = line[start + 1..end].trim();
    let percent: u8 = number.parse().ok()?;
    (percent <= 100).then_some(percent)
}

/// The message of an `ERROR - ...` block in the tool's output, with its line
/// wrapping undone.
pub fn error_text(output: &str) -> Option<String> {
    let start = output.find("ERROR - ")?;
    let mut words: Vec<&str> = Vec::new();
    for line in output[start + "ERROR - ".len()..].lines() {
        if line.trim().is_empty() {
            break;
        }
        words.extend(line.split_whitespace());
    }
    (!words.is_empty()).then(|| words.join(" "))
}

fn failure(output: &str, code: Option<i32>) -> Error {
    let detail = error_text(output).unwrap_or_else(|| {
        let tail: Vec<&str> = output
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .rev()
            .take(3)
            .collect();
        let mut tail: Vec<&str> = tail.into_iter().rev().collect();
        if tail.is_empty() {
            tail.push("the Windows backup tool reported a failure");
        }
        tail.join(" ")
    });
    Error::Command(match code {
        Some(code) => format!("{detail} (code {code})"),
        None => detail,
    })
}

/// How an imaging run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageOutcome {
    /// Folder on the target drive that holds the image.
    pub folder: PathBuf,
    /// The last lines the tool printed: its own summary of the run.
    pub summary: Vec<String>,
}

fn require_elevation(action: &str) -> Result<()> {
    if elevation::is_elevated() {
        Ok(())
    } else {
        Err(Error::NeedsElevation(action.to_owned()))
    }
}

/// Creates a system image and waits for it to finish.
///
/// `progress` shows the percentage of the drive being copied and the tool's
/// current line. Cancelling asks Windows to stop the job.
pub fn create_image(request: &ImageRequest, progress: &Progress) -> Result<ImageOutcome> {
    let args = start_backup_args(request)?;
    if !is_available() {
        return Err(Error::NotFound(
            "this edition of Windows does not include the system image tool".into(),
        ));
    }
    require_elevation("create a system image")?;
    let target = normalize_volume(&request.target).unwrap_or_default();

    progress.set_total(100);
    progress.set_message("Starting");
    let mut child = process::hidden_command(tool_path())
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ctx("starting the Windows backup tool")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("the Windows backup tool gave no output"))?;

    let finished = AtomicBool::new(false);
    let mut lines: Vec<String> = Vec::new();
    std::thread::scope(|scope| {
        // The reader below blocks on the tool's output, so a second thread
        // watches for cancellation and tells Windows to stop the job.
        scope.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                if progress.is_cancelled() {
                    if let Err(e) = stop_running_job() {
                        log::warn!("could not stop the system image job: {e}");
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });

        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            pending.extend_from_slice(&buffer[..read]);
            // Progress lines may end in a bare carriage return.
            while let Some(end) = pending.iter().position(|b| *b == b'\n' || *b == b'\r') {
                let raw: Vec<u8> = pending.drain(..=end).collect();
                note_line(&process::decode_console(&raw), progress, &mut lines);
            }
        }
        note_line(&process::decode_console(&pending), progress, &mut lines);
        finished.store(true, Ordering::Relaxed);
    });

    let status = child.wait().ctx("waiting for the Windows backup tool")?;
    if progress.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if !status.success() {
        return Err(failure(&lines.join("\n"), status.code()));
    }
    progress.set_done(100);
    let summary = lines.iter().rev().take(8).rev().cloned().collect();
    Ok(ImageOutcome {
        folder: PathBuf::from(format!("{target}\\")).join(IMAGE_FOLDER),
        summary,
    })
}

fn note_line(text: &str, progress: &Progress, lines: &mut Vec<String>) {
    let line = text.trim();
    if line.is_empty() {
        return;
    }
    if let Some(percent) = parse_progress(line) {
        progress.set_done(u64::from(percent));
    }
    progress.set_message(line);
    // Repeated progress lines are not worth keeping.
    if lines.last().map(String::as_str) != Some(line) {
        lines.push(line.to_owned());
    }
}

/// Asks Windows to stop the imaging job that is running.
pub fn stop_running_job() -> Result<()> {
    require_elevation("stop a system image job")?;
    let output = process::run_capture(tool_path(), ["stop", "job", "-quiet"])?;
    if output.success() {
        Ok(())
    } else {
        Err(failure(&output.stdout, output.code))
    }
}

/// One image Windows knows about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageVersion {
    /// Identifier Windows uses for the image, `MM/DD/YYYY-HH:MM` in UTC.
    pub identifier: String,
    /// When it was made, as Windows prints it.
    pub time: String,
    /// Where it is stored, as Windows prints it.
    pub target: String,
    /// The remaining details, such as what can be recovered from it.
    pub details: Vec<String>,
}

fn is_version_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 16
        && bytes.iter().enumerate().all(|(i, b)| match i {
            2 | 5 => *b == b'/',
            10 => *b == b'-',
            13 => *b == b':',
            _ => b.is_ascii_digit(),
        })
}

/// The part of a `label: value` line after the label.
fn value_of(line: &str) -> &str {
    line.find(": ").map_or(line, |i| &line[i + 2..]).trim()
}

/// Reads the listing printed by `wbadmin get versions`.
///
/// The listing is a run of blocks separated by blank lines. A block is
/// recognised by the shape of its version identifier rather than by its
/// labels, which are translated on non-English systems.
pub fn parse_versions(output: &str) -> Vec<ImageVersion> {
    let mut versions = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut flush = |block: &mut Vec<&str>| {
        let values: Vec<&str> = block.iter().map(|line| value_of(line)).collect();
        if let Some(at) = values.iter().position(|v| is_version_identifier(v)) {
            versions.push(ImageVersion {
                identifier: values[at].to_owned(),
                time: if at > 0 {
                    values[0].to_owned()
                } else {
                    String::new()
                },
                target: if at > 1 {
                    values[at - 1].to_owned()
                } else {
                    String::new()
                },
                details: values[at + 1..].iter().map(|v| (*v).to_owned()).collect(),
            });
        }
        block.clear();
    };
    for line in output.lines() {
        if line.trim().is_empty() {
            flush(&mut block);
        } else {
            block.push(line.trim());
        }
    }
    flush(&mut block);
    versions
}

/// Lists the images Windows knows about, optionally those stored on one
/// drive (needed for images made on another PC).
pub fn list_images(target: Option<&str>) -> Result<Vec<ImageVersion>> {
    if !is_available() {
        return Err(Error::NotFound(
            "this edition of Windows does not include the system image tool".into(),
        ));
    }
    require_elevation("list system images")?;
    let mut args = vec!["get".to_owned(), "versions".to_owned()];
    if let Some(target) = target {
        let volume = normalize_volume(target)
            .ok_or_else(|| Error::Invalid(format!("{target:?} is not a drive letter")))?;
        args.push(format!("-backupTarget:{volume}"));
    }
    let output = process::run_capture(tool_path(), &args)?;
    let versions = parse_versions(&output.stdout);
    if !output.success() && versions.is_empty() {
        // The tool also exits unsuccessfully when there simply are no images.
        return match error_text(&output.stdout) {
            Some(message) if message.to_lowercase().contains("no backup") => Ok(Vec::new()),
            _ => Err(failure(&output.stdout, output.code)),
        };
    }
    Ok(versions)
}

/// The image folder on a drive, when one exists. Works without administrator
/// rights: only the presence of the folder is checked, not what is inside.
pub fn image_folder_on(drive_root: &Path) -> Option<PathBuf> {
    let folder = drive_root.join(IMAGE_FOLDER);
    folder.is_dir().then_some(folder)
}

/// What is known about a drive considered as the place to save an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetFacts {
    /// Drive letter form, e.g. `E:`.
    pub volume: String,
    /// File system name, e.g. `NTFS`.
    pub file_system: String,
    /// Free space in bytes.
    pub free_bytes: u64,
    /// False when there is no disk in the drive or it cannot be read.
    pub is_ready: bool,
    /// True for CD/DVD drives.
    pub is_optical: bool,
}

/// Why a drive cannot hold the image, or `None` when it can.
///
/// `needed_bytes` is the space in use on the drives being imaged; the image
/// takes about that much.
pub fn target_problem(
    target: &TargetFacts,
    imaged: &[String],
    needed_bytes: u64,
) -> Option<String> {
    let volume = normalize_volume(&target.volume)?;
    if imaged
        .iter()
        .filter_map(|v| normalize_volume(v))
        .any(|v| v == volume)
    {
        return Some("This drive is part of the image, so the image cannot be saved on it.".into());
    }
    if !target.is_ready {
        return Some("There is no disk in this drive.".into());
    }
    if target.is_optical {
        return Some("Images cannot be saved to CD or DVD drives here.".into());
    }
    if !target.file_system.eq_ignore_ascii_case("NTFS") {
        let file_system = if target.file_system.is_empty() {
            "an unknown file system"
        } else {
            target.file_system.as_str()
        };
        return Some(format!(
            "Windows saves images only to drives formatted with NTFS. This one uses {file_system}."
        ));
    }
    if target.free_bytes < needed_bytes {
        return Some(format!(
            "Not enough room: the image needs about {} and {} is free.",
            zest_core::fmt::format_bytes(needed_bytes),
            zest_core::fmt::format_bytes(target.free_bytes)
        ));
    }
    None
}

/// Opens the Recovery page of Windows Settings, where "Advanced startup"
/// restarts into the recovery environment.
pub fn open_recovery_settings() -> Result<()> {
    process::hidden_command("explorer.exe")
        .arg("ms-settings:recovery")
        .spawn()
        .map(drop)
        .ctx("opening Windows Settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target: &str, include: &[&str], all_critical: bool) -> ImageRequest {
        ImageRequest {
            target: target.into(),
            include: include.iter().map(|s| (*s).to_owned()).collect(),
            all_critical,
        }
    }

    #[test]
    fn volumes_are_normalised_or_refused() {
        assert_eq!(normalize_volume("e"), Some("E:".into()));
        assert_eq!(normalize_volume("E:"), Some("E:".into()));
        assert_eq!(normalize_volume(" e:\\ "), Some("E:".into()));
        for bad in [
            "",
            "1:",
            "EE:",
            r"E:\folder",
            "E: -quiet",
            r"\\server\share",
            "é:",
        ] {
            assert_eq!(normalize_volume(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_command_for_a_bare_metal_image() {
        let args = start_backup_args(&request("e:", &["c", "D:\\", "C:"], true)).unwrap();
        assert_eq!(
            args,
            [
                "start",
                "backup",
                "-backupTarget:E:",
                "-include:C:,D:",
                "-allCritical",
                "-quiet"
            ]
        );
        let critical_only = start_backup_args(&request("F", &[], true)).unwrap();
        assert_eq!(
            critical_only,
            [
                "start",
                "backup",
                "-backupTarget:F:",
                "-allCritical",
                "-quiet"
            ]
        );
    }

    #[test]
    fn unusable_requests_are_refused() {
        assert!(start_backup_args(&request("E:", &["E:"], true)).is_err());
        assert!(start_backup_args(&request("E:", &[], false)).is_err());
        assert!(start_backup_args(&request("E: -include:C:", &["C:"], true)).is_err());
        assert!(start_backup_args(&request("E:", &["C: & calc"], true)).is_err());
    }

    #[test]
    fn progress_lines() {
        assert_eq!(
            parse_progress("Creating a backup of volume (C:), copied (12%)."),
            Some(12)
        );
        assert_eq!(
            parse_progress(
                "Creating a backup of volume System Reserved (100.00 MB), copied (100%)."
            ),
            Some(100)
        );
        assert_eq!(
            parse_progress("Sicherung von Volume (C:) wird erstellt, (7%) kopiert."),
            Some(7)
        );
        assert_eq!(
            parse_progress("The backup operation to E: is starting."),
            None
        );
        assert_eq!(parse_progress("Volume (C:) done"), None);
        assert_eq!(parse_progress("odd (250%)"), None);
    }

    /// Output of `wbadmin get versions` run without administrator rights on
    /// Windows 11, captured on a real PC.
    const ACCESS_DENIED: &str = "wbadmin 1.0 - Backup command-line tool\r\n(C) Copyright Microsoft Corporation. All rights reserved.\r\n\r\nERROR - Access denied. You must be a member of the Administrators group or \r\nBackup Operators group to use Windows Server Backup.  In addition, you must \r\nrun WBADMIN from an elevated command prompt. (To open an elevated command \r\nprompt, click Start, right-click Command Prompt, and then click Run \r\nas administrator.)\r\n\r\n";

    #[test]
    fn error_blocks_are_unwrapped() {
        let message = error_text(ACCESS_DENIED).unwrap();
        assert!(message.starts_with("Access denied. You must be a member"));
        assert!(message.ends_with("click Run as administrator.)"));
        assert!(!message.contains('\n'));
        assert_eq!(error_text("all fine\r\n"), None);
        assert!(parse_versions(ACCESS_DENIED).is_empty());

        let error = failure(ACCESS_DENIED, Some(-1));
        assert!(error.to_string().contains("Access denied"));
        assert!(failure("", None).to_string().contains("reported a failure"));
    }

    #[test]
    fn version_listings() {
        let listing = "wbadmin 1.0 - Backup command-line tool\r\n(C) Copyright Microsoft Corporation. All rights reserved.\r\n\r\nBackup time: 9/14/2026 10:35 AM\r\nBackup target: Fixed Disk labeled Backup(E:)\r\nVersion identifier: 09/14/2026-17:35\r\nCan recover: Volume(s), File(s), Application(s), Bare Metal Recovery, System State\r\nSnapshot ID: {a8d2ea3c-0000-4000-8000-123456789abc}\r\n\r\nBackup time: 10/2/2026 8:00 PM\r\nBackup target: Fixed Disk labeled Backup(E:)\r\nVersion identifier: 10/03/2026-03:00\r\nCan recover: Volume(s), File(s), Application(s), Bare Metal Recovery, System State\r\nSnapshot ID: {b1b2b3b4-0000-4000-8000-123456789abc}\r\n\r\n";
        let versions = parse_versions(listing);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].identifier, "09/14/2026-17:35");
        assert_eq!(versions[0].time, "9/14/2026 10:35 AM");
        assert_eq!(versions[0].target, "Fixed Disk labeled Backup(E:)");
        assert_eq!(versions[0].details.len(), 2);
        assert!(versions[0].details[0].contains("Bare Metal Recovery"));
        assert_eq!(versions[1].identifier, "10/03/2026-03:00");
    }

    #[test]
    fn version_listings_in_another_language() {
        let listing = "wbadmin 1.0 - Sicherungs-Befehlszeilentool\r\n\r\nSicherungszeit: 14.09.2026 10:35\r\nSicherungsziel: Festplatte mit der Bezeichnung Backup(E:)\r\nVersions-ID: 09/14/2026-17:35\r\nWiederherstellbar: Volume(s), Datei(en)\r\n";
        let versions = parse_versions(listing);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].identifier, "09/14/2026-17:35");
        assert_eq!(versions[0].time, "14.09.2026 10:35");
        assert_eq!(
            versions[0].target,
            "Festplatte mit der Bezeichnung Backup(E:)"
        );
    }

    #[test]
    fn identifiers_have_one_shape() {
        assert!(is_version_identifier("09/14/2026-17:35"));
        assert!(!is_version_identifier("9/14/2026 10:35 AM"));
        assert!(!is_version_identifier("09/14/2026-17:3"));
        assert!(!is_version_identifier("09-14-2026-17:35"));
    }

    fn facts(volume: &str, file_system: &str, free_gb: u64) -> TargetFacts {
        TargetFacts {
            volume: volume.into(),
            file_system: file_system.into(),
            free_bytes: free_gb << 30,
            is_ready: true,
            is_optical: false,
        }
    }

    #[test]
    fn targets_are_checked_before_windows_is_asked() {
        let imaged = vec!["C:".to_owned()];
        assert_eq!(
            target_problem(&facts("E:", "NTFS", 500), &imaged, 100 << 30),
            None
        );
        assert!(
            target_problem(&facts("c", "NTFS", 500), &imaged, 100 << 30)
                .unwrap()
                .contains("part of the image")
        );
        assert!(
            target_problem(&facts("E:", "exFAT", 500), &imaged, 100 << 30)
                .unwrap()
                .contains("NTFS")
        );
        assert!(
            target_problem(&facts("E:", "NTFS", 50), &imaged, 100 << 30)
                .unwrap()
                .contains("Not enough room")
        );
        let mut empty = facts("F:", "", 0);
        empty.is_ready = false;
        assert!(
            target_problem(&empty, &imaged, 1)
                .unwrap()
                .contains("no disk")
        );
        let mut optical = facts("G:", "UDF", 500);
        optical.is_optical = true;
        assert!(
            target_problem(&optical, &imaged, 1)
                .unwrap()
                .contains("CD or DVD")
        );
    }

    #[test]
    fn without_administrator_rights_nothing_is_started() {
        if elevation::is_elevated() {
            return;
        }
        let progress = Progress::new();
        let result = create_image(&request("Z:", &[], true), &progress);
        if is_available() {
            assert!(matches!(result, Err(Error::NeedsElevation(_))));
            assert!(matches!(list_images(None), Err(Error::NeedsElevation(_))));
            assert!(matches!(stop_running_job(), Err(Error::NeedsElevation(_))));
        } else {
            assert!(matches!(result, Err(Error::NotFound(_))));
        }
    }

    #[test]
    fn image_folders_are_found_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(image_folder_on(tmp.path()), None);
        std::fs::create_dir(tmp.path().join(IMAGE_FOLDER)).unwrap();
        assert_eq!(
            image_folder_on(tmp.path()),
            Some(tmp.path().join(IMAGE_FOLDER))
        );
    }
}
