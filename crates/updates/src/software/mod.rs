//! Software updater built on the Windows Package Manager.
//!
//! Zest does not download or host installers. It asks the package manager
//! that ships with Windows 10 and 11 (`winget`) which installed programs have
//! a newer version, and asks it to perform the upgrade. Downloads therefore
//! come from the publisher-verified package source and pass the package
//! manager's own hash verification before anything runs.
//!
//! # Typical flow
//!
//! ```no_run
//! use zest_core::Progress;
//! use zest_updates::software::{self, CheckOptions, UpgradeOptions};
//!
//! let progress = Progress::new();
//! if software::is_available() {
//!     let updates = software::check_updates(&CheckOptions::default(), &progress)?;
//!     for update in &updates {
//!         let outcome = software::upgrade(update, &UpgradeOptions::default(), &progress)?;
//!         println!("{}: {}", outcome.name, outcome.message);
//!     }
//! }
//! # Ok::<(), zest_core::Error>(())
//! ```
//!
//! # Why a text parser
//!
//! The package manager offers no structured output for its upgrade listing,
//! and its programmatic interface is not part of the Windows metadata this
//! project builds against. The listing is therefore read from the table it
//! prints, by column position rather than by (translated) label; see the
//! `table` module for everything that parser guards against. Should a
//! package id ever arrive shortened, it is completed from the package
//! manager's JSON export before it is offered for upgrade.

mod codes;
mod table;

use crate::exec;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use zest_core::{Error, Progress, Result, process};

/// Friendly name of the component this module drives.
pub const PACKAGE_MANAGER_NAME: &str = "Windows Package Manager";

/// Store link for "App Installer", the package that provides the Windows
/// Package Manager. Offer it when [`is_available`] is false.
pub const INSTALL_URL: &str = "https://apps.microsoft.com/detail/9NBLGGH4NNS1";

/// Explanation shown when the package manager is missing.
pub const MISSING_MESSAGE: &str = "The Windows Package Manager (winget) is not installed. \
     It is part of \"App Installer\", available free from the Microsoft Store.";

const WINGET_EXE: &str = "winget.exe";

fn link_exists(path: &std::path::Path) -> bool {
    // The usual location is an app execution alias: a reparse point that
    // ordinary metadata queries cannot follow, hence `symlink_metadata`.
    std::fs::symlink_metadata(path).is_ok()
}

fn packaged_winget() -> Option<PathBuf> {
    // Readable only with administrator rights; a useful fallback when Zest
    // runs elevated under an account that has no alias of its own.
    let apps = zest_core::paths::program_files().join("WindowsApps");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(apps)
        .ok()?
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            name.starts_with("microsoft.desktopappinstaller_") && !name.contains("_neutral_")
        })
        .map(|entry| entry.path().join(WINGET_EXE))
        .filter(|path| path.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

/// Locates `winget.exe`.
///
/// Looks at the per-user app execution alias first
/// (`%LOCALAPPDATA%\Microsoft\WindowsApps`), then at every folder on `PATH`,
/// then at the packaged installation itself.
pub fn winget_path() -> Option<PathBuf> {
    let alias = zest_core::paths::UserDirs::current()
        .local
        .join("Microsoft")
        .join("WindowsApps")
        .join(WINGET_EXE);
    if link_exists(&alias) {
        return Some(alias);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(WINGET_EXE);
            if dir.is_absolute() && link_exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    packaged_winget()
}

/// True when the Windows Package Manager is installed.
pub fn is_available() -> bool {
    winget_path().is_some()
}

/// Version of the installed package manager, e.g. `1.9.25200`.
///
/// Returns `None` when it is missing or does not answer.
pub fn source_version() -> Option<String> {
    let exe = winget_path()?;
    let output = process::run_capture(&exe, ["--version"]).ok()?;
    if !output.success() {
        return None;
    }
    let version = output
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .trim_start_matches(['v', 'V'])
        .to_owned();
    (!version.is_empty()).then_some(version)
}

/// Whether the software updater can work on this PC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManagerStatus {
    /// The package manager was found.
    pub available: bool,
    /// Where it was found.
    pub path: Option<PathBuf>,
    /// Its version, when it answered.
    pub version: Option<String>,
}

/// Probes the package manager. When `available` is false, show
/// [`MISSING_MESSAGE`] and offer [`INSTALL_URL`].
pub fn status() -> PackageManagerStatus {
    let path = winget_path();
    PackageManagerStatus {
        available: path.is_some(),
        version: path.as_ref().and_then(|_| source_version()),
        path,
    }
}

/// Options for [`check_updates`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckOptions {
    /// Also offer programs whose installed version cannot be determined.
    pub include_unknown: bool,
    /// Also offer programs the user pinned to their current version.
    pub include_pinned: bool,
    /// Package ids (case-insensitive) that are never offered.
    pub ignore_ids: Vec<String>,
    /// Give up when the package manager has not answered after this long.
    pub timeout_secs: u64,
}

impl Default for CheckOptions {
    fn default() -> Self {
        CheckOptions {
            include_unknown: false,
            include_pinned: false,
            ignore_ids: Vec::new(),
            timeout_secs: 180,
        }
    }
}

/// An installed program for which a newer version is available.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SoftwareUpdate {
    /// Display name. May end in `…` when the package manager shortened it.
    pub name: String,
    /// Package identifier, e.g. `Mozilla.Firefox`.
    pub id: String,
    /// Version currently installed (free text; may be `Unknown` or `< 1.2`).
    pub installed_version: String,
    /// Version that would be installed.
    pub available_version: String,
    /// Package source, e.g. `winget` or `msstore`. Empty when not reported.
    pub source: String,
    /// The package manager skips this program in bulk upgrades and only
    /// upgrades it when named explicitly. [`upgrade`] does name it explicitly,
    /// so this is informational (such programs usually update themselves).
    pub requires_explicit_upgrade: bool,
    /// The package id was shortened by the package manager and could not be
    /// completed. Such an entry is shown for information but cannot be
    /// upgraded from here.
    pub id_truncated: bool,
}

impl SoftwareUpdate {
    /// True when [`upgrade`] can act on this entry.
    pub fn can_upgrade(&self) -> bool {
        !self.id_truncated && is_valid_package_id(&self.id)
    }

    /// False when the installed version is reported as unknown or only as an
    /// upper bound.
    pub fn installed_version_known(&self) -> bool {
        let version = self.installed_version.trim();
        !(version.is_empty() || version.starts_with('<') || version.eq_ignore_ascii_case("unknown"))
    }
}

fn is_valid_package_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('-')
        && !id.ends_with(table::ELLIPSIS)
        && !id
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// Parses the text printed by the package manager's upgrade listing.
///
/// Exposed for diagnostics and tests; [`check_updates`] is the normal entry
/// point. Works for any display language.
pub fn parse_upgrade_listing(raw: &str) -> Vec<SoftwareUpdate> {
    table::parse_upgrade_rows(raw)
        .into_iter()
        .map(|row| SoftwareUpdate {
            id_truncated: row.id.ends_with(table::ELLIPSIS),
            name: row.name,
            id: row.id,
            installed_version: row.installed,
            available_version: row.available,
            source: row.source,
            requires_explicit_upgrade: row.secondary,
        })
        .collect()
}

/// Arguments of the read-only command that lists available upgrades.
pub fn check_command_args(options: &CheckOptions) -> Vec<String> {
    let mut args = vec![
        "upgrade".to_owned(),
        "--accept-source-agreements".to_owned(),
        "--disable-interactivity".to_owned(),
    ];
    if options.include_unknown {
        args.push("--include-unknown".to_owned());
    }
    if options.include_pinned {
        args.push("--include-pinned".to_owned());
    }
    args
}

fn tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<String> = table::clean_lines(text)
        .into_iter()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn format_exit(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("0x{:08X}", code as u32),
        None => "no exit code".to_owned(),
    }
}

/// Removes ignored package ids (case-insensitive) and sorts by name.
fn finish_listing(mut updates: Vec<SoftwareUpdate>, ignore_ids: &[String]) -> Vec<SoftwareUpdate> {
    let ignored: Vec<String> = ignore_ids
        .iter()
        .map(|id| id.trim().to_lowercase())
        .collect();
    updates.retain(|update| !ignored.contains(&update.id.to_lowercase()));
    updates.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    updates
}

/// Extracts `(source, package id)` pairs from the package manager's JSON export.
fn parse_export_ids(json: &str) -> Vec<(String, String)> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json.trim_start_matches('\u{feff}'))
    else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for source in root["Sources"].as_array().into_iter().flatten() {
        let name = source["SourceDetails"]["Name"].as_str().unwrap_or_default();
        for package in source["Packages"].as_array().into_iter().flatten() {
            if let Some(id) = package["PackageIdentifier"].as_str() {
                ids.push((name.to_owned(), id.to_owned()));
            }
        }
    }
    ids
}

/// Completes shortened package ids from the full ids in `known`.
/// An id is only completed when exactly one known id starts with it.
fn resolve_truncated_ids(updates: &mut [SoftwareUpdate], known: &[(String, String)]) {
    for update in updates.iter_mut().filter(|u| u.id_truncated) {
        let prefix = update.id.trim_end_matches(table::ELLIPSIS);
        if prefix.is_empty() {
            continue;
        }
        let source_known = !update.source.is_empty() && !update.source.ends_with(table::ELLIPSIS);
        let mut matches: Vec<&str> = known
            .iter()
            .filter(|(source, _)| !source_known || source.eq_ignore_ascii_case(&update.source))
            .map(|(_, id)| id.as_str())
            .filter(|id| id.starts_with(prefix))
            .collect();
        matches.sort_unstable();
        matches.dedup();
        if let [only] = matches.as_slice() {
            update.id = (*only).to_owned();
            update.id_truncated = false;
        }
    }
}

/// Arguments of the command that writes the list of installed packages to
/// `file` as JSON. It changes nothing but that file.
fn export_command_args(file: &std::path::Path) -> Vec<std::ffi::OsString> {
    vec![
        "export".into(),
        "--output".into(),
        file.as_os_str().to_owned(),
        "--accept-source-agreements".into(),
        "--disable-interactivity".into(),
    ]
}

fn full_package_ids(exe: &std::path::Path, progress: &Progress) -> Vec<(String, String)> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let file =
        std::env::temp_dir().join(format!("zest-packages-{}-{stamp}.json", std::process::id()));
    let mut command = process::hidden_command(exe);
    command.args(export_command_args(&file));
    let ran = exec::run_cancellable(
        command,
        progress,
        Some(Duration::from_secs(120)),
        &mut |_| {},
    );
    let ids = match ran {
        Ok(_) => std::fs::read_to_string(&file)
            .map(|json| parse_export_ids(&json))
            .unwrap_or_default(),
        Err(e) => {
            log::warn!("could not export the package list: {e}");
            Vec::new()
        }
    };
    // Our own scratch file.
    let _ = std::fs::remove_file(&file);
    ids
}

/// Asks the package manager which installed programs can be upgraded.
///
/// This only reads: it refreshes the package manager's own source index and
/// compares versions. Nothing is downloaded or installed.
///
/// The result is sorted by name. Entries whose id is on
/// [`CheckOptions::ignore_ids`] are left out.
///
/// # Errors
///
/// * [`Error::NotFound`] with [`MISSING_MESSAGE`] when the package manager is
///   not installed (check [`is_available`] first to show a friendlier panel).
/// * [`Error::Cancelled`] when `progress` is cancelled.
/// * [`Error::Command`] when the package manager fails or times out.
pub fn check_updates(options: &CheckOptions, progress: &Progress) -> Result<Vec<SoftwareUpdate>> {
    let exe = winget_path().ok_or_else(|| Error::NotFound(MISSING_MESSAGE.to_owned()))?;
    progress.set_message("Checking for software updates…");
    progress.check()?;

    let mut command = process::hidden_command(&exe);
    command.args(check_command_args(options));
    let timeout = Duration::from_secs(options.timeout_secs.max(10));
    let output = exec::run_cancellable(command, progress, Some(timeout), &mut |_| {})?;

    let mut updates = parse_upgrade_listing(&output.stdout);
    let benign = matches!(
        output.code,
        Some(0) | Some(codes::UPDATE_NOT_APPLICABLE) | Some(codes::NO_APPLICATIONS_FOUND)
    );
    if updates.is_empty() && !benign {
        let detail = tail(&format!("{}\n{}", output.stdout, output.stderr), 4);
        return Err(Error::Command(format!(
            "the package manager could not list updates ({}): {detail}",
            format_exit(output.code)
        )));
    }

    if updates.iter().any(|u| u.id_truncated) {
        progress.set_message("Resolving package names…");
        let known = full_package_ids(&exe, progress);
        progress.check()?;
        resolve_truncated_ids(&mut updates, &known);
    }

    let updates = finish_listing(updates, &options.ignore_ids);
    progress.set_message(match updates.len() {
        0 => "All software is up to date".to_owned(),
        1 => "1 software update available".to_owned(),
        n => format!("{n} software updates available"),
    });
    Ok(updates)
}

/// Options for [`upgrade`] and [`upgrade_all`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpgradeOptions {
    /// Ask installers to run without showing their own windows.
    pub silent: bool,
    /// Upgrade even when the installed version cannot be determined. Applied
    /// automatically to entries whose version is reported as unknown.
    pub include_unknown: bool,
    /// Continue past non-security problems (for example a version mismatch
    /// reported by the installer).
    pub force: bool,
    /// Give up on one program after this long.
    pub timeout_secs: u64,
}

impl Default for UpgradeOptions {
    fn default() -> Self {
        UpgradeOptions {
            silent: true,
            include_unknown: false,
            force: false,
            timeout_secs: 30 * 60,
        }
    }
}

/// Arguments of the command that upgrades one program.
pub fn upgrade_command_args(update: &SoftwareUpdate, options: &UpgradeOptions) -> Vec<String> {
    let mut args = vec![
        "upgrade".to_owned(),
        "--id".to_owned(),
        update.id.clone(),
        "--exact".to_owned(),
    ];
    if !update.source.is_empty() && !update.source.ends_with(table::ELLIPSIS) {
        args.push("--source".to_owned());
        args.push(update.source.clone());
    }
    if options.silent {
        args.push("--silent".to_owned());
    }
    args.extend([
        "--accept-package-agreements".to_owned(),
        "--accept-source-agreements".to_owned(),
        "--disable-interactivity".to_owned(),
    ]);
    if options.include_unknown || !update.installed_version_known() {
        args.push("--include-unknown".to_owned());
    }
    if options.force {
        args.push("--force".to_owned());
    }
    args
}

/// How an upgrade attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpgradeStatus {
    /// The new version is installed.
    Upgraded,
    /// The new version is installed and takes effect after a restart.
    RebootRequired,
    /// Nothing to do: already current, or no installer fits this PC.
    NoUpdateAvailable,
    /// The program (or one of its files) is in use; close it and retry.
    InUse,
    /// Another installation is running; retry later.
    InstallerBusy,
    /// The user cancelled, or declined the administrator prompt.
    Cancelled,
    /// Administrator rights are required.
    NeedsAdmin,
    /// Blocked by group policy.
    BlockedByPolicy,
    /// The program is pinned to its current version.
    Pinned,
    /// The package manager does not recognise the program (any more).
    NotFound,
    /// The installer could not be downloaded.
    DownloadFailed,
    /// The download failed its integrity check and was not run.
    VerificationFailed,
    /// The installer or the package manager reported an error.
    Failed,
    /// Not attempted because the batch was cancelled first.
    Skipped,
}

impl UpgradeStatus {
    /// True for [`Upgraded`](Self::Upgraded) and
    /// [`RebootRequired`](Self::RebootRequired).
    pub fn is_success(self) -> bool {
        matches!(
            self,
            UpgradeStatus::Upgraded | UpgradeStatus::RebootRequired
        )
    }

    /// Short label for lists and badges.
    pub fn label(self) -> &'static str {
        match self {
            UpgradeStatus::Upgraded => "Updated",
            UpgradeStatus::RebootRequired => "Restart required",
            UpgradeStatus::NoUpdateAvailable => "Up to date",
            UpgradeStatus::InUse => "In use",
            UpgradeStatus::InstallerBusy => "Installer busy",
            UpgradeStatus::Cancelled => "Cancelled",
            UpgradeStatus::NeedsAdmin => "Needs administrator",
            UpgradeStatus::BlockedByPolicy => "Blocked by policy",
            UpgradeStatus::Pinned => "Pinned",
            UpgradeStatus::NotFound => "Not found",
            UpgradeStatus::DownloadFailed => "Download failed",
            UpgradeStatus::VerificationFailed => "Verification failed",
            UpgradeStatus::Failed => "Failed",
            UpgradeStatus::Skipped => "Skipped",
        }
    }
}

/// Result of upgrading one program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeOutcome {
    /// Package id that was upgraded.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What happened.
    pub status: UpgradeStatus,
    /// Exit code of the package manager, when it ran.
    pub exit_code: Option<i32>,
    /// One plain-language sentence for the user.
    pub message: String,
    /// The last lines the package manager printed (in the system language),
    /// for troubleshooting.
    pub detail: String,
}

impl UpgradeOutcome {
    /// True when the new version is installed (possibly pending a restart).
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// True when a restart is needed to finish.
    pub fn reboot_required(&self) -> bool {
        self.status == UpgradeStatus::RebootRequired
    }

    fn without_running(update: &SoftwareUpdate, status: UpgradeStatus, message: &str) -> Self {
        UpgradeOutcome {
            id: update.id.clone(),
            name: update.name.clone(),
            status,
            exit_code: None,
            message: message.to_owned(),
            detail: String::new(),
        }
    }
}

fn outcome_from_exit(update: &SoftwareUpdate, code: Option<i32>, output: &str) -> UpgradeOutcome {
    let (status, message) = codes::classify(code);
    let message = if status == UpgradeStatus::Failed {
        format!("{message} (code {})", format_exit(code))
    } else {
        message.to_owned()
    };
    UpgradeOutcome {
        id: update.id.clone(),
        name: update.name.clone(),
        status,
        exit_code: code,
        message,
        detail: tail(output, 12),
    }
}

fn shorten(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        line.to_owned()
    } else {
        let mut out: String = line.chars().take(max_chars.saturating_sub(1)).collect();
        out.push(table::ELLIPSIS);
        out
    }
}

/// Upgrades one program to its latest version.
///
/// The package manager downloads the installer from the package source,
/// verifies it and runs it. Installers that need administrator rights raise
/// the usual Windows elevation prompt themselves unless Zest already runs
/// elevated; declining it yields [`UpgradeStatus::Cancelled`].
///
/// While running, `progress` carries the package manager's latest status
/// line. Cancelling `progress` stops the package manager; an installer that
/// has already been started may still complete.
///
/// Installer-level problems are reported through the returned
/// [`UpgradeOutcome`], not as errors.
///
/// # Errors
///
/// * [`Error::NotFound`] when the package manager is not installed.
/// * [`Error::Cancelled`] when `progress` is cancelled.
/// * [`Error::Command`] when the package manager cannot be started or
///   exceeds [`UpgradeOptions::timeout_secs`].
pub fn upgrade(
    update: &SoftwareUpdate,
    options: &UpgradeOptions,
    progress: &Progress,
) -> Result<UpgradeOutcome> {
    if !update.can_upgrade() {
        return Ok(UpgradeOutcome::without_running(
            update,
            UpgradeStatus::Failed,
            "The package manager did not report a complete identifier for this program, \
             so it cannot be updated from here.",
        ));
    }
    let exe = winget_path().ok_or_else(|| Error::NotFound(MISSING_MESSAGE.to_owned()))?;
    progress.check()?;
    progress.set_message(format!("Updating {}…", update.name));

    let mut command = process::hidden_command(&exe);
    command.args(upgrade_command_args(update, options));
    let timeout = Duration::from_secs(options.timeout_secs.max(60));
    let name = update.name.clone();
    let output = exec::run_cancellable(command, progress, Some(timeout), &mut |text| {
        let latest = table::clean_lines(text)
            .into_iter()
            .map(|line| line.trim().to_owned())
            .rfind(|line| !line.is_empty());
        if let Some(line) = latest {
            progress.set_message(format!("{name}: {}", shorten(&line, 110)));
        }
    })?;

    let combined = if output.stderr.trim().is_empty() {
        output.stdout
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };
    let outcome = outcome_from_exit(update, output.code, &combined);
    log::info!(
        "software update {} ({}): {:?}, exit {}",
        update.id,
        update.available_version,
        outcome.status,
        format_exit(output.code)
    );
    progress.set_message(format!("{}: {}", update.name, outcome.message));
    Ok(outcome)
}

/// Upgrades several programs one after another.
///
/// Returns one outcome per entry of `updates`, in the same order. `progress`
/// counts finished programs. When cancelled, the program being upgraded is
/// reported as [`UpgradeStatus::Cancelled`] and the remaining ones as
/// [`UpgradeStatus::Skipped`].
///
/// # Errors
///
/// [`Error::NotFound`] when the package manager is not installed; every other
/// problem is reported per program.
pub fn upgrade_all(
    updates: &[SoftwareUpdate],
    options: &UpgradeOptions,
    progress: &Progress,
) -> Result<Vec<UpgradeOutcome>> {
    if winget_path().is_none() {
        return Err(Error::NotFound(MISSING_MESSAGE.to_owned()));
    }
    progress.set_total(updates.len() as u64);
    progress.set_done(0);
    let mut outcomes = Vec::with_capacity(updates.len());
    let mut cancelled = false;
    for update in updates {
        if cancelled || progress.is_cancelled() {
            cancelled = true;
            outcomes.push(UpgradeOutcome::without_running(
                update,
                UpgradeStatus::Skipped,
                "Not attempted because the operation was cancelled.",
            ));
            continue;
        }
        let outcome = match upgrade(update, options, progress) {
            Ok(outcome) => outcome,
            Err(Error::Cancelled) => {
                cancelled = true;
                UpgradeOutcome::without_running(
                    update,
                    UpgradeStatus::Cancelled,
                    "The update was cancelled.",
                )
            }
            Err(e) => {
                UpgradeOutcome::without_running(update, UpgradeStatus::Failed, &e.to_string())
            }
        };
        outcomes.push(outcome);
        progress.inc(1);
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(name: &str, id: &str, installed: &str) -> SoftwareUpdate {
        SoftwareUpdate {
            name: name.to_owned(),
            id: id.to_owned(),
            installed_version: installed.to_owned(),
            available_version: "9.9".to_owned(),
            source: "winget".to_owned(),
            requires_explicit_upgrade: false,
            id_truncated: id.ends_with(table::ELLIPSIS),
        }
    }

    #[test]
    fn listing_becomes_public_entries() {
        let updates = parse_upgrade_listing(&table::fixtures::typical());
        assert_eq!(updates.len(), 7);
        let git = updates.iter().find(|u| u.id == "Git.Git").unwrap();
        assert_eq!(git.name, "Git");
        assert_eq!(git.installed_version, "2.53.0");
        assert_eq!(git.available_version, "2.56.0.4");
        assert_eq!(git.source, "winget");
        assert!(git.can_upgrade());
        assert!(!git.id_truncated && !git.requires_explicit_upgrade);
    }

    #[test]
    fn check_arguments() {
        assert_eq!(
            check_command_args(&CheckOptions::default()),
            [
                "upgrade",
                "--accept-source-agreements",
                "--disable-interactivity"
            ]
        );
        let all = check_command_args(&CheckOptions {
            include_unknown: true,
            include_pinned: true,
            ..CheckOptions::default()
        });
        assert!(all.contains(&"--include-unknown".to_owned()));
        assert!(all.contains(&"--include-pinned".to_owned()));
        // The listing command must never contain anything that installs.
        for args in [check_command_args(&CheckOptions::default()), all] {
            assert!(
                !args
                    .iter()
                    .any(|a| a == "--all" || a == "--id" || a == "-r" || a == "--recurse")
            );
        }
    }

    #[test]
    fn upgrade_arguments() {
        let args = upgrade_command_args(
            &update("Git", "Git.Git", "2.53.0"),
            &UpgradeOptions::default(),
        );
        assert_eq!(
            args,
            [
                "upgrade",
                "--id",
                "Git.Git",
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
            ]
        );

        // Unknown installed versions need the explicit opt-in.
        let args = upgrade_command_args(
            &update("Tool", "Vendor.Tool", "Unknown"),
            &UpgradeOptions::default(),
        );
        assert!(args.contains(&"--include-unknown".to_owned()));
        let args = upgrade_command_args(
            &update("Tool", "Vendor.Tool", "< 2.0"),
            &UpgradeOptions::default(),
        );
        assert!(args.contains(&"--include-unknown".to_owned()));

        let custom = UpgradeOptions {
            silent: false,
            force: true,
            ..UpgradeOptions::default()
        };
        let mut no_source = update("Tool", "Vendor.Tool", "1.0");
        no_source.source.clear();
        let args = upgrade_command_args(&no_source, &custom);
        assert!(!args.contains(&"--silent".to_owned()));
        assert!(!args.contains(&"--source".to_owned()));
        assert!(args.contains(&"--force".to_owned()));
    }

    #[test]
    fn package_id_validation() {
        assert!(is_valid_package_id("Git.Git"));
        assert!(is_valid_package_id("Notepad++.Notepad++"));
        assert!(is_valid_package_id("Microsoft.VCRedist.2015+.x64"));
        assert!(is_valid_package_id("9NCBCSZSJRSB"));
        assert!(!is_valid_package_id(""));
        assert!(!is_valid_package_id("--manifest"));
        assert!(!is_valid_package_id("Vendor Tool"));
        assert!(!is_valid_package_id("Vendor.Tool\u{2026}"));
        assert!(!is_valid_package_id("Vendor\".Tool"));
        assert!(!update("Tool", "Vendor.To\u{2026}", "1.0").can_upgrade());
    }

    #[test]
    fn incomplete_ids_are_never_run() {
        let truncated = update("Tool", "Vendor.To\u{2026}", "1.0");
        // Returns before looking for (or starting) the package manager.
        let outcome = upgrade(&truncated, &UpgradeOptions::default(), &Progress::new()).unwrap();
        assert_eq!(outcome.status, UpgradeStatus::Failed);
        assert_eq!(outcome.exit_code, None);
        assert!(!outcome.is_success());
    }

    #[test]
    fn ignore_list_and_sorting() {
        let updates = vec![
            update("zeta", "Vendor.Zeta", "1"),
            update("Alpha", "Vendor.Alpha", "1"),
            update("beta", "Vendor.Beta", "1"),
        ];
        let kept = finish_listing(updates, &["vendor.BETA".to_owned(), "  ".to_owned()]);
        let names: Vec<&str> = kept.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta"]);
    }

    #[test]
    fn export_ids_are_parsed() {
        let json = r#"{
            "$schema": "https://aka.ms/winget-packages.schema.2.0.json",
            "CreationDate": "2026-01-01T00:00:00.000-00:00",
            "Sources": [
                {
                    "Packages": [
                        { "PackageIdentifier": "Git.Git" },
                        { "PackageIdentifier": "Microsoft.VisualStudio.2022.BuildTools", "Version": "17.14.23" },
                        { "PackageIdentifier": "Microsoft.VisualStudio.2022.Community" }
                    ],
                    "SourceDetails": { "Argument": "https://cdn.winget.microsoft.com/cache", "Identifier": "Microsoft.Winget.Source_8wekyb3d8bbwe", "Name": "winget", "Type": "Microsoft.PreIndexed.Package" }
                },
                {
                    "Packages": [ { "PackageIdentifier": "9NCBCSZSJRSB" } ],
                    "SourceDetails": { "Name": "msstore" }
                }
            ]
        }"#;
        let ids = parse_export_ids(json);
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&("winget".to_owned(), "Git.Git".to_owned())));
        assert!(ids.contains(&("msstore".to_owned(), "9NCBCSZSJRSB".to_owned())));
        assert!(parse_export_ids("not json").is_empty());
        assert!(parse_export_ids("{}").is_empty());
    }

    #[test]
    fn truncated_ids_are_completed_only_when_unambiguous() {
        let known = vec![
            (
                "winget".to_owned(),
                "Microsoft.VisualStudio.2022.BuildTools".to_owned(),
            ),
            (
                "winget".to_owned(),
                "Microsoft.VisualStudio.2022.Community".to_owned(),
            ),
            (
                "winget".to_owned(),
                "Microsoft.VCRedist.2015+.x64".to_owned(),
            ),
            (
                "msstore".to_owned(),
                "Microsoft.VCRedist.2015+.x64".to_owned(),
            ),
        ];
        let mut updates = vec![
            update(
                "Build Tools",
                "Microsoft.VisualStudio.2022.B\u{2026}",
                "17.1",
            ),
            update("Ambiguous", "Microsoft.VisualStudio.2022.\u{2026}", "17.1"),
            update("Redist", "Microsoft.VCRedist.2015+\u{2026}", "14.1"),
            update("Unknown", "Vendor.Nothing\u{2026}", "1.0"),
            update("Complete", "Git.Git", "2.0"),
        ];
        resolve_truncated_ids(&mut updates, &known);
        assert_eq!(updates[0].id, "Microsoft.VisualStudio.2022.BuildTools");
        assert!(!updates[0].id_truncated && updates[0].can_upgrade());
        assert!(
            updates[1].id_truncated,
            "two candidates must stay unresolved"
        );
        assert!(!updates[1].can_upgrade());
        // Same id offered by two sources: the row's own source decides.
        assert_eq!(updates[2].id, "Microsoft.VCRedist.2015+.x64");
        assert!(updates[3].id_truncated);
        assert_eq!(updates[4].id, "Git.Git");
    }

    #[test]
    fn export_arguments_only_write_the_given_file() {
        let args = export_command_args(std::path::Path::new(r"C:\Temp\x.json"));
        assert_eq!(args[0], "export");
        assert_eq!(args[2], r"C:\Temp\x.json");
    }

    #[test]
    fn outcomes_from_exit_codes() {
        let git = update("Git", "Git.Git", "2.53.0");
        let ok = outcome_from_exit(&git, Some(0), "Found Git\nSuccessfully installed\n");
        assert!(ok.is_success() && !ok.reboot_required());
        assert_eq!(ok.detail, "Found Git\nSuccessfully installed");

        let reboot = outcome_from_exit(&git, Some(3010), "");
        assert!(reboot.is_success() && reboot.reboot_required());

        let failed = outcome_from_exit(&git, Some(1603), "Installer failed with exit code: 1603");
        assert_eq!(failed.status, UpgradeStatus::Failed);
        assert!(failed.message.contains("0x00000643"), "{}", failed.message);

        let current = outcome_from_exit(&git, Some(codes::UPDATE_NOT_APPLICABLE), "");
        assert_eq!(current.status, UpgradeStatus::NoUpdateAvailable);
        assert!(!current.is_success());
    }

    #[test]
    fn helpers() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("abcdefghij", 5), "abcd\u{2026}");
        assert_eq!(format_exit(Some(-1978335189)), "0x8A15002B");
        assert_eq!(tail("a\r\n\r\nb\r\nc\r\n", 2), "b\nc");
        assert!(update("x", "A.B", "1.0").installed_version_known());
        assert!(!update("x", "A.B", "Unknown").installed_version_known());
        assert!(!update("x", "A.B", "< 3").installed_version_known());
        for status in [
            UpgradeStatus::Upgraded,
            UpgradeStatus::Failed,
            UpgradeStatus::Skipped,
        ] {
            assert!(!status.label().is_empty());
        }
    }

    #[test]
    fn status_is_self_consistent() {
        let status = status();
        assert_eq!(status.available, status.path.is_some());
        assert_eq!(status.available, is_available());
        if !status.available {
            assert!(status.version.is_none());
            let err = check_updates(&CheckOptions::default(), &Progress::new()).unwrap_err();
            assert!(matches!(err, Error::NotFound(_)));
        }
    }

    #[test]
    fn cancelled_batch_reports_every_entry() {
        if !is_available() {
            return;
        }
        // Cancelled before it starts: nothing is run, everything is skipped.
        let progress = Progress::new();
        progress.cancel();
        let updates = vec![update("A", "Vendor.A", "1"), update("B", "Vendor.B", "1")];
        let outcomes = upgrade_all(&updates, &UpgradeOptions::default(), &progress).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|o| o.status == UpgradeStatus::Skipped));
        assert!(outcomes.iter().all(|o| o.exit_code.is_none()));
    }
}
