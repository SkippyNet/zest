//! Zest's own updates, from the releases of its public repository.
//!
//! The flow has three steps, each usable on its own:
//!
//! 1. [`check`] asks the repository's release feed for the latest release
//!    and compares its version with the running one.
//! 2. [`download`] fetches the package that fits this copy of Zest and
//!    verifies it against the SHA-256 sums published with the release.
//!    Nothing is kept unless the sums match.
//! 3. [`apply`] hands over to the installer (installed copies) or swaps the
//!    program files in place (portable copies).
//!
//! # What a release must look like
//!
//! * a tag `v<version>` with a [semantic version](https://semver.org);
//! * `Zest-<version>-x64-setup.exe` for installed copies;
//! * `Zest-<version>-x64-portable.zip` for portable copies, with the
//!   executables at the top level of the archive;
//! * `SHA256SUMS.txt` with one `<hex digest>  <file name>` line per package.
//!
//! # Which copy is this
//!
//! An installed copy has the uninstaller (`unins000.exe`) beside the
//! executable; a portable copy has the `zest.portable` marker. Anything
//! else (a build from source, a hand-copied executable) is told where to
//! download the new version and is never modified.
//!
//! # Privacy
//!
//! The check is one anonymous HTTPS request that carries the Zest version
//! and nothing about the PC or its user. It can be
//! switched off in the options, after which Zest only checks when asked.

mod swap;

use crate::net::{self, Client, NetError};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zest_core::{Progress, brand, paths};

/// Seconds between automatic checks.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// File name of the published checksum list.
pub const CHECKSUM_FILE: &str = "SHA256SUMS.txt";

/// The uninstaller the installer places beside the executable.
const UNINSTALLER: &str = "unins000.exe";

/// No package of Zest is anywhere near this size; larger downloads are refused.
const MAX_PACKAGE_BYTES: u64 = 400 * 1024 * 1024;

/// Where releases are looked up and downloaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Base address of the release feed's API.
    pub api_base: String,
    /// Repository as `owner/name`.
    pub repo: String,
    /// Download addresses must start with this; anything else found in a
    /// release description is ignored.
    pub download_prefix: String,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            api_base: "https://api.github.com".to_owned(),
            repo: brand::REPO.to_owned(),
            download_prefix: format!("https://github.com/{}/releases/download/", brand::REPO),
        }
    }
}

impl Endpoint {
    /// Address answering with the latest published release.
    pub fn latest_release_url(&self) -> String {
        format!(
            "{}/repos/{}/releases/latest",
            self.api_base.trim_end_matches('/'),
            self.repo
        )
    }

    /// Page that lists every release, for people to download from by hand.
    pub fn releases_page(&self) -> String {
        format!("https://github.com/{}/releases/latest", self.repo)
    }
}

/// How this copy of Zest got onto the PC, which decides how it is updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CopyKind {
    /// Set up by the installer.
    Installed,
    /// Unpacked from the portable archive.
    Portable,
    /// Neither: built from source or copied by hand. Not updated in place.
    Unmanaged,
}

impl CopyKind {
    /// Detects the kind of the copy whose executables live in `dir`.
    pub fn of_dir(dir: &Path) -> CopyKind {
        if dir.join(UNINSTALLER).is_file() {
            CopyKind::Installed
        } else if dir.join(paths::PORTABLE_MARKER).exists() {
            CopyKind::Portable
        } else {
            CopyKind::Unmanaged
        }
    }

    /// The kind of the running copy.
    pub fn current() -> CopyKind {
        CopyKind::of_dir(&paths::exe_dir())
    }

    /// Ending of the release package made for this kind of copy.
    fn asset_suffix(self) -> Option<&'static str> {
        match self {
            CopyKind::Installed => Some("-x64-setup.exe"),
            CopyKind::Portable => Some("-x64-portable.zip"),
            CopyKind::Unmanaged => None,
        }
    }
}

/// One downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// File name, e.g. `Zest-1.2.0-x64-setup.exe`.
    pub name: String,
    /// Download address.
    pub url: String,
    /// Size in bytes as announced by the release feed.
    pub size: u64,
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Version parsed from the tag.
    pub version: Version,
    /// The tag as published, e.g. `v1.2.0`.
    pub tag: String,
    /// Title of the release.
    pub title: String,
    /// Release notes (Markdown as written by the publisher).
    pub notes: String,
    /// Web page of the release.
    pub page_url: String,
    /// Publication time (ISO 8601), when given.
    pub published: Option<String>,
    /// Files attached to the release whose address is trusted.
    pub assets: Vec<Asset>,
}

impl Release {
    /// The package that updates a copy of the given kind.
    pub fn package_for(&self, kind: CopyKind) -> Option<&Asset> {
        let suffix = kind.asset_suffix()?;
        let prefix = format!("{}-", brand::NAME.to_lowercase());
        self.assets.iter().find(|asset| {
            let name = asset.name.to_lowercase();
            name.starts_with(&prefix) && name.ends_with(suffix)
        })
    }

    /// The published checksum list.
    pub fn checksums(&self) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.name.eq_ignore_ascii_case(CHECKSUM_FILE))
    }

    /// The release notes as plain lines, without Markdown decoration, for
    /// interfaces that cannot render Markdown.
    pub fn notes_plain(&self) -> String {
        plain_notes(&self.notes)
    }
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The running version is the newest one published. `latest` is `None`
    /// when nothing has been published yet.
    UpToDate {
        /// The newest published version.
        latest: Option<Version>,
    },
    /// A newer version is published.
    Available(Box<Release>),
}

impl CheckOutcome {
    /// The newest published version, if any release exists.
    pub fn latest(&self) -> Option<&Version> {
        match self {
            CheckOutcome::UpToDate { latest } => latest.as_ref(),
            CheckOutcome::Available(release) => Some(&release.version),
        }
    }
}

/// Why an update step failed. The messages are written for end users.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// No connection to the release feed.
    #[error(
        "The update server could not be reached. Check your internet connection and try again."
    )]
    Offline(String),
    /// The release feed limits anonymous requests per hour and network.
    #[error("{}", rate_limit_message(*.retry_after_secs))]
    RateLimited {
        /// Seconds until the next request is allowed, when announced.
        retry_after_secs: Option<u64>,
    },
    /// The release feed answered with an unexpected status.
    #[error("The update server answered with an error (status {0}). Try again later.")]
    Server(u16),
    /// The answer could not be understood.
    #[error("The update information could not be read: {0}")]
    Malformed(String),
    /// The release has no package for this kind of copy.
    #[error(
        "This release has no download that fits this copy of {}. Get it from the project page.",
        brand::NAME
    )]
    NoPackage,
    /// The release has no checksum list, or the list lacks the package.
    #[error(
        "The release does not list a checksum for {0}, so the download cannot be verified and was not used."
    )]
    NoChecksum(String),
    /// The download does not match its published checksum.
    #[error("The download of {file} does not match its published checksum and was deleted.")]
    ChecksumMismatch {
        /// File that was downloaded.
        file: String,
        /// Digest published with the release.
        expected: String,
        /// Digest of what arrived.
        actual: String,
    },
    /// The copy is neither installed nor portable.
    #[error(
        "This copy of {} was not set up by the installer or unpacked from the portable package, so it cannot update itself. Download the new version from the project page.",
        brand::NAME
    )]
    Unmanaged,
    /// The downloaded package is not what a release package looks like.
    #[error("The downloaded package is damaged: {0}")]
    BadPackage(String),
    /// A file operation failed.
    #[error("{0}")]
    Io(String),
    /// The step was cancelled.
    #[error("The update was cancelled.")]
    Cancelled,
}

fn rate_limit_message(retry_after_secs: Option<u64>) -> String {
    let when = match retry_after_secs {
        Some(secs) if secs > 90 => format!("in about {} minutes", secs.div_ceil(60)),
        Some(_) => "in a minute".to_owned(),
        None => "later".to_owned(),
    };
    format!("The update server is limiting requests from this network. Try again {when}.")
}

impl From<NetError> for UpdateError {
    fn from(error: NetError) -> Self {
        match error {
            NetError::Cancelled => UpdateError::Cancelled,
            NetError::Status {
                status: 403 | 429,
                retry_after_secs,
            } => UpdateError::RateLimited { retry_after_secs },
            NetError::Status { status, .. } => UpdateError::Server(status),
            NetError::Unreachable(detail) | NetError::Interrupted(detail) => {
                UpdateError::Offline(detail)
            }
            NetError::File { path, reason } => {
                UpdateError::Io(format!("Could not write {path}: {reason}"))
            }
            other @ (NetError::TooLarge { .. } | NetError::Insecure(_)) => {
                UpdateError::BadPackage(other.to_string())
            }
        }
    }
}

/// The version of the running program.
pub fn current_version() -> Version {
    parse_version(brand::VERSION).unwrap_or_else(|| Version::new(0, 0, 0))
}

/// Parses a release tag or version text: `v1.2.3`, `1.2.3-beta.1`, and, for
/// leniency, `1.2` (read as `1.2.0`).
pub fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    if let Ok(version) = Version::parse(text) {
        return Some(version);
    }
    // Pad a missing minor or patch number, keeping any pre-release suffix.
    let split = text.find(['-', '+']).unwrap_or(text.len());
    let (numbers, suffix) = text.split_at(split);
    let parts = numbers.split('.').count();
    if (1..3).contains(&parts) && !numbers.is_empty() {
        let padded = format!("{numbers}{}{suffix}", ".0".repeat(3 - parts));
        return Version::parse(&padded).ok();
    }
    None
}

/// True when an automatic check is due: none was made yet, the last one is
/// older than [`CHECK_INTERVAL_SECS`], or the clock was set back since.
pub fn check_is_due(last_checked: Option<i64>, now: i64) -> bool {
    match last_checked {
        None => true,
        Some(last) => last > now || now - last >= CHECK_INTERVAL_SECS,
    }
}

/// True when `latest` should be offered: it is newer than the running
/// version and the user did not choose to skip exactly that version.
pub fn should_offer(latest: &Version, current: &Version, skipped: Option<&str>) -> bool {
    if latest <= current {
        return false;
    }
    match skipped.and_then(parse_version) {
        Some(skipped) => &skipped != latest,
        None => true,
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: String,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<RawAsset>,
}

fn parse_release(json: &str, endpoint: &Endpoint) -> Result<Option<Release>, UpdateError> {
    let raw: RawRelease =
        serde_json::from_str(json).map_err(|e| UpdateError::Malformed(e.to_string()))?;
    if raw.draft || raw.prerelease {
        // The "latest" feed never lists these; a server that does is ignored.
        return Ok(None);
    }
    let version = parse_version(&raw.tag_name).ok_or_else(|| {
        UpdateError::Malformed(format!("\"{}\" is not a version number", raw.tag_name))
    })?;
    let assets = raw
        .assets
        .into_iter()
        .filter(|asset| {
            let trusted = asset
                .browser_download_url
                .starts_with(&endpoint.download_prefix)
                && !asset.name.contains(['/', '\\'])
                && !asset.name.is_empty();
            if !trusted {
                log::warn!(
                    "ignoring release file with an unexpected address: {}",
                    asset.name
                );
            }
            trusted
        })
        .map(|asset| Asset {
            name: asset.name,
            url: asset.browser_download_url,
            size: asset.size,
        })
        .collect();
    let page_url = if raw.html_url.starts_with("https://") {
        raw.html_url
    } else {
        endpoint.releases_page()
    };
    Ok(Some(Release {
        title: raw
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("{} {version}", brand::NAME)),
        notes: raw.body.unwrap_or_default(),
        tag: raw.tag_name,
        page_url,
        published: raw.published_at,
        assets,
        version,
    }))
}

fn check_with(
    client: &Client,
    endpoint: &Endpoint,
    current: &Version,
) -> Result<CheckOutcome, UpdateError> {
    let json = match client.get_text(
        &endpoint.latest_release_url(),
        Some("application/vnd.github+json"),
        2 * 1024 * 1024,
    ) {
        Ok(json) => json,
        // The feed answers "not found" while nothing has been published.
        Err(error) if error.is_not_found() => return Ok(CheckOutcome::UpToDate { latest: None }),
        Err(error) => return Err(error.into()),
    };
    let Some(release) = parse_release(&json, endpoint)? else {
        return Ok(CheckOutcome::UpToDate { latest: None });
    };
    if &release.version > current {
        Ok(CheckOutcome::Available(Box::new(release)))
    } else {
        Ok(CheckOutcome::UpToDate {
            latest: Some(release.version),
        })
    }
}

/// Asks the repository whether a newer version than the running one has
/// been published. One anonymous request; changes nothing on the PC.
///
/// # Errors
///
/// See [`UpdateError`]: offline, rate limited, or an unreadable answer.
pub fn check() -> Result<CheckOutcome, UpdateError> {
    let outcome = check_with(&Client::new(), &Endpoint::default(), &current_version());
    match &outcome {
        Ok(CheckOutcome::Available(release)) => {
            log::info!("update check: version {} is available", release.version);
        }
        Ok(CheckOutcome::UpToDate { .. }) => log::info!("update check: up to date"),
        Err(error) => log::warn!("update check failed: {error:?}"),
    }
    outcome
}

/// A verified package, ready for [`apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    /// The downloaded file.
    pub path: PathBuf,
    /// The kind of copy it updates.
    pub kind: CopyKind,
    /// The version it contains.
    pub version: Version,
}

/// Finds the digest published for `file_name` in a checksum list.
///
/// Accepts the formats written by common tools: `<hex>  <name>` and
/// `<hex> *<name>`. Names are compared without regard to case.
pub fn find_checksum(sums: &str, file_name: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        let Some((digest, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim().trim_start_matches('*');
        let is_sha256 = digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit());
        if is_sha256 && name.eq_ignore_ascii_case(file_name) {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

/// Folder that holds downloaded packages until they are applied.
fn staging_root() -> PathBuf {
    std::env::temp_dir().join(format!("{}-update", brand::NAME))
}

fn download_with(
    client: &Client,
    release: &Release,
    kind: CopyKind,
    folder: &Path,
    progress: &Progress,
) -> Result<Download, UpdateError> {
    if kind == CopyKind::Unmanaged {
        return Err(UpdateError::Unmanaged);
    }
    let package = release.package_for(kind).ok_or(UpdateError::NoPackage)?;
    if progress.is_cancelled() {
        return Err(UpdateError::Cancelled);
    }
    let sums_asset = release
        .checksums()
        .ok_or_else(|| UpdateError::NoChecksum(package.name.clone()))?;

    progress.set_message("Reading the published checksums…");
    let sums = client.get_text(&sums_asset.url, None, 1024 * 1024)?;
    let expected = find_checksum(&sums, &package.name)
        .ok_or_else(|| UpdateError::NoChecksum(package.name.clone()))?;

    progress.set_message(format!("Downloading {}…", package.name));
    let path = folder.join(&package.name);
    let downloaded = client.download(&package.url, &path, MAX_PACKAGE_BYTES, progress)?;
    if downloaded.sha256 != expected {
        // Never leave an unverified package where it could be run.
        let _ = std::fs::remove_file(&path);
        return Err(UpdateError::ChecksumMismatch {
            file: package.name.clone(),
            expected,
            actual: downloaded.sha256,
        });
    }
    progress.set_message("Download verified");
    log::info!(
        "downloaded and verified {} ({} bytes)",
        package.name,
        downloaded.bytes
    );
    Ok(Download {
        path,
        kind,
        version: release.version.clone(),
    })
}

/// Downloads the package of `release` that fits this copy of Zest and
/// verifies it against the published SHA-256 sums.
///
/// The file goes to a folder of its own inside the temporary directory.
/// `progress` reports bytes and can cancel.
///
/// # Errors
///
/// [`UpdateError::Unmanaged`] for copies that cannot update themselves,
/// [`UpdateError::NoPackage`] / [`UpdateError::NoChecksum`] when the release
/// is incomplete, [`UpdateError::ChecksumMismatch`] when the download does
/// not match (the file is deleted), plus the network errors.
pub fn download(release: &Release, progress: &Progress) -> Result<Download, UpdateError> {
    let folder = staging_root().join(release.version.to_string());
    download_with(
        &Client::new(),
        release,
        CopyKind::current(),
        &folder,
        progress,
    )
}

/// What [`apply`] did and what the caller must do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The installer is running. Close Zest now so it can replace the files;
    /// it starts the new version when it is done.
    InstallerStarted,
    /// The program files of this portable copy were replaced. Start
    /// `restart` and close; the old files are cleared away on the next start.
    Replaced {
        /// The new graphical executable.
        restart: PathBuf,
    },
}

/// Arguments the installer is started with: no questions, a progress window,
/// and no restart of Windows.
pub const INSTALLER_ARGS: [&str; 3] = ["/SILENT", "/SP-", "/NORESTART"];

/// Installs a verified [`Download`].
///
/// * Installed copy: starts the installer and returns
///   [`Applied::InstallerStarted`]. The caller closes Zest.
/// * Portable copy: replaces the executables beside the running one and
///   returns [`Applied::Replaced`]. Running files are renamed, not
///   overwritten, so this works while Zest is open; if any file cannot be
///   replaced, every file is put back as it was.
///
/// # Errors
///
/// [`UpdateError::BadPackage`] when the package does not contain what a
/// release package must contain, [`UpdateError::Io`] when files cannot be
/// replaced (nothing is left half-updated).
pub fn apply(download: &Download) -> Result<Applied, UpdateError> {
    apply_in(download, &paths::exe_dir())
}

fn apply_in(download: &Download, program_dir: &Path) -> Result<Applied, UpdateError> {
    match download.kind {
        CopyKind::Unmanaged => Err(UpdateError::Unmanaged),
        CopyKind::Installed => {
            swap::require_executable(&download.path)?;
            /// Windows' answer when a program insists on administrator rights.
            const ELEVATION_REQUIRED: i32 = 740;
            match std::process::Command::new(&download.path)
                .args(INSTALLER_ARGS)
                .spawn()
            {
                Ok(_) => {}
                // An installation for all users: let Windows ask for consent.
                Err(e) if e.raw_os_error() == Some(ELEVATION_REQUIRED) => {
                    zest_core::elevation::run_elevated(&download.path, &INSTALLER_ARGS, true)
                        .map_err(|e| match e {
                            zest_core::Error::Cancelled => UpdateError::Cancelled,
                            other => UpdateError::Io(format!(
                                "The installer could not be started: {other}"
                            )),
                        })?;
                }
                Err(e) => {
                    return Err(UpdateError::Io(format!(
                        "The installer could not be started: {e}"
                    )));
                }
            }
            log::info!("started the installer for version {}", download.version);
            Ok(Applied::InstallerStarted)
        }
        CopyKind::Portable => {
            let unpacked = download.path.with_extension("unpacked");
            let result = swap::unpack(&download.path, &unpacked)
                .and_then(|()| swap::replace_program_files(&unpacked, program_dir));
            // The staging folder is ours; clear it whatever happened.
            let _ = std::fs::remove_dir_all(&unpacked);
            result?;
            let _ = std::fs::remove_file(&download.path);
            log::info!(
                "replaced the program files with version {}",
                download.version
            );
            Ok(Applied::Replaced {
                restart: program_dir.join(brand::GUI_EXE),
            })
        }
    }
}

/// Clears away what an earlier update left behind: the replaced executables
/// (`*.old`) beside the running one and finished downloads. Call once at
/// start-up; it is quick and never fails.
pub fn cleanup_after_update() {
    swap::remove_replaced_files(&paths::exe_dir());
    let root = staging_root();
    if root.is_dir() {
        // Our own download folder in the temporary directory.
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Reduces Markdown release notes to readable plain text: headings lose
/// their marks, bullets become dots, links keep their text, emphasis marks
/// and code fences go.
fn plain_notes(markdown: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in markdown.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(trimmed.to_owned());
            continue;
        }
        let stripped = trimmed.trim_start();
        let indent = &trimmed[..trimmed.len() - stripped.len()];
        let body = if stripped.starts_with('#') {
            stripped.trim_start_matches('#').trim_start().to_owned()
        } else if let Some(item) = stripped
            .strip_prefix("- ")
            .or_else(|| stripped.strip_prefix("* "))
            .or_else(|| stripped.strip_prefix("+ "))
        {
            format!("{indent}\u{2022} {item}")
        } else {
            trimmed.to_owned()
        };
        out.push(strip_inline_marks(&body));
    }
    // Collapse runs of blank lines.
    let mut text = String::new();
    let mut blank = true;
    for line in out {
        if line.trim().is_empty() {
            if !blank {
                text.push('\n');
            }
            blank = true;
        } else {
            text.push_str(&line);
            text.push('\n');
            blank = false;
        }
    }
    text.trim_end().to_owned()
}

/// Removes `**bold**`, `__bold__`, `` `code` `` marks and turns
/// `[text](address)` into `text`.
fn strip_inline_marks(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '`' => {}
            '*' | '_' if chars.get(i + 1) == Some(&c) => i += 1,
            '[' => {
                // [text](address) -> text
                let close = chars[i..].iter().position(|&x| x == ']').map(|p| i + p);
                let linked = close.filter(|&close| chars.get(close + 1) == Some(&'('));
                let end = linked.and_then(|close| {
                    chars[close..]
                        .iter()
                        .position(|&x| x == ')')
                        .map(|p| close + p)
                });
                if let (Some(close), Some(end)) = (linked, end) {
                    out.extend(&chars[i + 1..close]);
                    i = end;
                } else {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// The user agent sent with update requests, for display in "About" texts.
pub fn user_agent() -> String {
    net::user_agent()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testserver::{Reply, TestServer};
    use sha2::{Digest, Sha256};

    fn endpoint(server: &TestServer) -> Endpoint {
        Endpoint {
            api_base: server.base.clone(),
            repo: "owner/zest".to_owned(),
            download_prefix: format!("{}/download/", server.base),
        }
    }

    fn release_json(server: &TestServer, tag: &str, assets: &[(&str, usize)]) -> String {
        let assets: Vec<serde_json::Value> = assets
            .iter()
            .map(|(name, size)| {
                serde_json::json!({
                    "name": name,
                    "size": size,
                    "browser_download_url": format!("{}/download/{tag}/{name}", server.base),
                })
            })
            .collect();
        serde_json::json!({
            "tag_name": tag,
            "name": format!("Zest {}", tag.trim_start_matches('v')),
            "body": "## What is new\n\n- Faster **scans**\n- See [the guide](https://example.invalid/guide)\n",
            "html_url": format!("https://github.com/owner/zest/releases/tag/{tag}"),
            "published_at": "2026-11-14T10:00:00Z",
            "draft": false,
            "prerelease": false,
            "assets": assets,
        })
        .to_string()
    }

    const LATEST: &str = "/repos/owner/zest/releases/latest";

    fn v(text: &str) -> Version {
        parse_version(text).unwrap()
    }

    #[test]
    fn versions_parse_leniently_and_order_like_semver() {
        assert_eq!(v("v1.2.3"), Version::new(1, 2, 3));
        assert_eq!(v("1.2.3"), Version::new(1, 2, 3));
        assert_eq!(v(" V0.9.0 "), Version::new(0, 9, 0));
        assert_eq!(v("1.2"), Version::new(1, 2, 0));
        assert_eq!(v("v2"), Version::new(2, 0, 0));
        assert_eq!(v("1.2-rc.1").to_string(), "1.2.0-rc.1");
        assert!(parse_version("").is_none());
        assert!(parse_version("latest").is_none());
        assert!(parse_version("1.2.3.4").is_none());
        assert!(parse_version("v").is_none());

        assert!(v("1.0.0") > v("0.9.9"));
        assert!(v("0.10.0") > v("0.9.0"), "numeric, not text, order");
        assert!(
            v("1.0.0") > v("1.0.0-rc.1"),
            "a release beats its candidates"
        );
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-beta.11") > v("1.0.0-beta.2"));
        assert!(v("1.0.0+build.5") >= v("1.0.0"));
        assert!(current_version() > Version::new(0, 0, 0));
    }

    #[test]
    fn offers_respect_the_skipped_version() {
        let current = v("1.0.0");
        assert!(should_offer(&v("1.1.0"), &current, None));
        assert!(!should_offer(&v("1.0.0"), &current, None));
        assert!(!should_offer(&v("0.9.0"), &current, None));
        assert!(!should_offer(&v("1.1.0"), &current, Some("1.1.0")));
        assert!(!should_offer(&v("1.1.0"), &current, Some("v1.1.0")));
        // Skipping one version does not hide the next.
        assert!(should_offer(&v("1.2.0"), &current, Some("1.1.0")));
        assert!(should_offer(&v("1.1.0"), &current, Some("nonsense")));
    }

    #[test]
    fn automatic_checks_run_once_a_day() {
        let now = 1_800_000_000;
        assert!(check_is_due(None, now));
        assert!(!check_is_due(Some(now - 60), now));
        assert!(!check_is_due(Some(now - CHECK_INTERVAL_SECS + 1), now));
        assert!(check_is_due(Some(now - CHECK_INTERVAL_SECS), now));
        assert!(check_is_due(Some(now + 5_000), now), "clock was set back");
    }

    #[test]
    fn a_newer_release_is_found_with_its_packages() {
        let server = TestServer::start();
        let endpoint = endpoint(&server);
        server.route(
            LATEST,
            Reply::ok(release_json(
                &server,
                "v1.4.0",
                &[
                    ("Zest-1.4.0-x64-setup.exe", 10),
                    ("Zest-1.4.0-x64-portable.zip", 20),
                    (CHECKSUM_FILE, 30),
                ],
            )),
        );
        let outcome = check_with(&Client::for_tests(), &endpoint, &v("1.3.9")).unwrap();
        let CheckOutcome::Available(release) = &outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(release.version, v("1.4.0"));
        assert_eq!(release.tag, "v1.4.0");
        assert_eq!(release.title, "Zest 1.4.0");
        assert_eq!(
            release.page_url,
            "https://github.com/owner/zest/releases/tag/v1.4.0"
        );
        assert_eq!(release.published.as_deref(), Some("2026-11-14T10:00:00Z"));
        assert_eq!(
            release.package_for(CopyKind::Installed).unwrap().name,
            "Zest-1.4.0-x64-setup.exe"
        );
        assert_eq!(
            release.package_for(CopyKind::Portable).unwrap().name,
            "Zest-1.4.0-x64-portable.zip"
        );
        assert!(release.package_for(CopyKind::Unmanaged).is_none());
        assert_eq!(release.checksums().unwrap().size, 30);
        assert_eq!(outcome.latest(), Some(&v("1.4.0")));
        assert_eq!(
            release.notes_plain(),
            "What is new\n\n\u{2022} Faster scans\n\u{2022} See the guide"
        );

        // The request asked the right address in the documented way.
        let seen = server.requests();
        assert_eq!(seen[0].path, LATEST);
        assert_eq!(
            seen[0].headers.get("accept").map(String::as_str),
            Some("application/vnd.github+json")
        );
        assert_eq!(seen[0].headers.get("user-agent"), Some(&user_agent()));
    }

    #[test]
    fn same_older_and_missing_releases_mean_up_to_date() {
        let server = TestServer::start();
        let endpoint = endpoint(&server);
        let client = Client::for_tests();

        // Nothing published yet: the feed answers 404.
        assert_eq!(
            check_with(&client, &endpoint, &v("1.0.0")).unwrap(),
            CheckOutcome::UpToDate { latest: None }
        );

        server.route(LATEST, Reply::ok(release_json(&server, "v1.0.0", &[])));
        assert_eq!(
            check_with(&client, &endpoint, &v("1.0.0")).unwrap(),
            CheckOutcome::UpToDate {
                latest: Some(v("1.0.0"))
            }
        );
        // Running something newer than what is published (a development build).
        assert_eq!(
            check_with(&client, &endpoint, &v("1.1.0-dev")).unwrap(),
            CheckOutcome::UpToDate {
                latest: Some(v("1.0.0"))
            }
        );
        // A release candidate is offered the final version.
        assert!(matches!(
            check_with(&client, &endpoint, &v("1.0.0-rc.1")).unwrap(),
            CheckOutcome::Available(_)
        ));

        // Drafts and pre-releases are never offered.
        let mut draft: serde_json::Value =
            serde_json::from_str(&release_json(&server, "v9.0.0", &[])).unwrap();
        draft["prerelease"] = serde_json::json!(true);
        server.route(LATEST, Reply::ok(draft.to_string()));
        assert_eq!(
            check_with(&client, &endpoint, &v("1.0.0")).unwrap(),
            CheckOutcome::UpToDate { latest: None }
        );
    }

    #[test]
    fn failures_are_told_apart() {
        let server = TestServer::start();
        let endpoint = endpoint(&server);
        let client = Client::for_tests();
        let current = v("1.0.0");

        server.route(
            LATEST,
            Reply::status(403)
                .header("X-RateLimit-Remaining", "0")
                .header("Retry-After", "1800"),
        );
        let limited = check_with(&client, &endpoint, &current).unwrap_err();
        assert_eq!(
            limited,
            UpdateError::RateLimited {
                retry_after_secs: Some(1800)
            }
        );
        assert!(limited.to_string().contains("30 minutes"), "{limited}");

        server.route(LATEST, Reply::status(429));
        assert!(matches!(
            check_with(&client, &endpoint, &current),
            Err(UpdateError::RateLimited {
                retry_after_secs: None
            })
        ));

        server.route(LATEST, Reply::status(500));
        assert_eq!(
            check_with(&client, &endpoint, &current).unwrap_err(),
            UpdateError::Server(500)
        );

        server.route(LATEST, Reply::ok("<html>not json</html>"));
        assert!(matches!(
            check_with(&client, &endpoint, &current),
            Err(UpdateError::Malformed(_))
        ));

        server.route(LATEST, Reply::ok(release_json(&server, "nightly", &[])));
        let bad_tag = check_with(&client, &endpoint, &current).unwrap_err();
        assert!(matches!(bad_tag, UpdateError::Malformed(ref m) if m.contains("nightly")));

        // Nobody listening at all.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = Endpoint {
            api_base: format!("http://{}", free.local_addr().unwrap()),
            ..endpoint.clone()
        };
        drop(free);
        assert!(matches!(
            check_with(&client, &dead, &current),
            Err(UpdateError::Offline(_))
        ));
    }

    #[test]
    fn files_from_unexpected_addresses_are_ignored() {
        let server = TestServer::start();
        let endpoint = endpoint(&server);
        let json = serde_json::json!({
            "tag_name": "v2.0.0",
            "html_url": "javascript:alert(1)",
            "assets": [
                { "name": "Zest-2.0.0-x64-setup.exe", "size": 1,
                  "browser_download_url": "https://elsewhere.invalid/Zest-2.0.0-x64-setup.exe" },
                { "name": "..\\Zest-2.0.0-x64-portable.zip", "size": 1,
                  "browser_download_url": format!("{}/download/v2.0.0/x.zip", server.base) },
                { "name": CHECKSUM_FILE, "size": 1,
                  "browser_download_url": format!("{}/download/v2.0.0/{CHECKSUM_FILE}", server.base) },
            ],
        })
        .to_string();
        let release = parse_release(&json, &endpoint).unwrap().unwrap();
        assert!(release.package_for(CopyKind::Installed).is_none());
        assert!(release.package_for(CopyKind::Portable).is_none());
        assert!(release.checksums().is_some());
        assert_eq!(release.title, "Zest 2.0.0");
        // A page address that is not a web address falls back to the release list.
        assert_eq!(
            release.page_url,
            "https://github.com/owner/zest/releases/latest"
        );
    }

    #[test]
    fn checksum_lists_are_read_in_common_formats() {
        let digest = "a".repeat(64);
        let upper = "B".repeat(64);
        let sums = format!(
            "\u{feff}{digest}  Zest-1.0.0-x64-setup.exe\r\n{upper} *Zest-1.0.0-x64-portable.zip\r\nnot a line\r\n1234  short.bin\r\n"
        );
        assert_eq!(
            find_checksum(&sums, "Zest-1.0.0-x64-setup.exe"),
            Some(digest.clone())
        );
        assert_eq!(
            find_checksum(&sums, "zest-1.0.0-x64-PORTABLE.zip"),
            Some("b".repeat(64))
        );
        assert_eq!(find_checksum(&sums, "short.bin"), None);
        assert_eq!(find_checksum(&sums, "other.exe"), None);
        assert_eq!(find_checksum("", "x"), None);
    }

    fn sha(bytes: &[u8]) -> String {
        net::hex(&Sha256::digest(bytes))
    }

    fn serve_release(
        server: &TestServer,
        tag: &str,
        package: (&str, &[u8]),
        sums: Option<String>,
    ) -> Release {
        let mut assets = vec![(package.0, package.1.len())];
        server.route(
            &format!("/download/{tag}/{}", package.0),
            Reply::ok(package.1.to_vec()),
        );
        if let Some(sums) = &sums {
            assets.push((CHECKSUM_FILE, sums.len()));
            server.route(
                &format!("/download/{tag}/{CHECKSUM_FILE}"),
                Reply::ok(sums.clone()),
            );
        }
        parse_release(&release_json(server, tag, &assets), &endpoint(server))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_package_is_kept_only_when_its_checksum_matches() {
        let server = TestServer::start();
        let client = Client::for_tests();
        let dir = tempfile::tempdir().unwrap();
        let body = b"MZ pretend installer".to_vec();
        let name = "Zest-1.4.0-x64-setup.exe";

        // Matching sums: kept.
        let release = serve_release(
            &server,
            "v1.4.0",
            (name, &body),
            Some(format!("{}  {name}\n", sha(&body))),
        );
        let progress = Progress::new();
        let download = download_with(
            &client,
            &release,
            CopyKind::Installed,
            dir.path(),
            &progress,
        )
        .unwrap();
        assert_eq!(download.path, dir.path().join(name));
        assert_eq!(std::fs::read(&download.path).unwrap(), body);
        assert_eq!(download.kind, CopyKind::Installed);
        assert_eq!(download.version, v("1.4.0"));
        assert_eq!(progress.done(), body.len() as u64);

        // Sums of something else: refused and deleted.
        std::fs::remove_file(&download.path).unwrap();
        let wrong = serve_release(
            &server,
            "v1.4.0",
            (name, &body),
            Some(format!("{}  {name}\n", sha(b"tampered"))),
        );
        let error = download_with(
            &client,
            &wrong,
            CopyKind::Installed,
            dir.path(),
            &Progress::new(),
        )
        .unwrap_err();
        assert!(
            matches!(&error, UpdateError::ChecksumMismatch { file, expected, actual }
                if file == name && *expected == sha(b"tampered") && *actual == sha(&body)),
            "{error:?}"
        );
        assert!(!dir.path().join(name).exists(), "unverified file was kept");

        // The list does not mention the package: refused before downloading.
        let unlisted = serve_release(
            &server,
            "v1.4.0",
            (name, &body),
            Some(format!("{}  something-else.exe\n", sha(&body))),
        );
        assert_eq!(
            download_with(
                &client,
                &unlisted,
                CopyKind::Installed,
                dir.path(),
                &Progress::new()
            )
            .unwrap_err(),
            UpdateError::NoChecksum(name.to_owned())
        );
        assert!(!dir.path().join(name).exists());

        // No list at all.
        let bare = serve_release(&server, "v1.4.0", (name, &body), None);
        assert_eq!(
            download_with(
                &client,
                &bare,
                CopyKind::Installed,
                dir.path(),
                &Progress::new()
            )
            .unwrap_err(),
            UpdateError::NoChecksum(name.to_owned())
        );

        // No package for this kind of copy, and copies that never self-update.
        assert_eq!(
            download_with(
                &client,
                &release,
                CopyKind::Portable,
                dir.path(),
                &Progress::new()
            )
            .unwrap_err(),
            UpdateError::NoPackage
        );
        assert_eq!(
            download_with(
                &client,
                &release,
                CopyKind::Unmanaged,
                dir.path(),
                &Progress::new()
            )
            .unwrap_err(),
            UpdateError::Unmanaged
        );

        // Cancelled before it starts.
        let cancelled = Progress::new();
        cancelled.cancel();
        assert_eq!(
            download_with(
                &client,
                &release,
                CopyKind::Installed,
                dir.path(),
                &cancelled
            )
            .unwrap_err(),
            UpdateError::Cancelled
        );
    }

    #[test]
    fn copies_are_recognised_by_what_sits_beside_them() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(CopyKind::of_dir(dir.path()), CopyKind::Unmanaged);
        std::fs::write(dir.path().join(paths::PORTABLE_MARKER), "").unwrap();
        assert_eq!(CopyKind::of_dir(dir.path()), CopyKind::Portable);
        // The installer's uninstaller wins: an installed copy is updated by
        // the installer even if a marker file was dropped beside it.
        std::fs::write(dir.path().join(UNINSTALLER), "MZ").unwrap();
        assert_eq!(CopyKind::of_dir(dir.path()), CopyKind::Installed);
    }

    #[test]
    fn unmanaged_copies_are_never_modified() {
        let dir = tempfile::tempdir().unwrap();
        let download = Download {
            path: dir.path().join("Zest-1.0.0-x64-setup.exe"),
            kind: CopyKind::Unmanaged,
            version: v("1.0.0"),
        };
        assert_eq!(
            apply_in(&download, dir.path()).unwrap_err(),
            UpdateError::Unmanaged
        );
        // An "installer" that is not a program is not started.
        std::fs::write(&download.path, "this is text").unwrap();
        let fake = Download {
            kind: CopyKind::Installed,
            ..download
        };
        assert!(matches!(
            apply_in(&fake, dir.path()),
            Err(UpdateError::BadPackage(_))
        ));
    }

    #[test]
    fn notes_are_readable_without_markdown() {
        let notes = "# Zest 1.2\r\n\r\n\r\n### Fixed\n* `zest-cli` no longer __hangs__\n  - nested [link](https://x.invalid) item\n\n```\nzest-cli update check\n```\nPlain [bracket] stays.";
        assert_eq!(
            plain_notes(notes),
            "Zest 1.2\n\nFixed\n\u{2022} zest-cli no longer hangs\n  \u{2022} nested link item\n\nzest-cli update check\nPlain [bracket] stays."
        );
        assert_eq!(plain_notes(""), "");
    }

    #[test]
    fn user_facing_messages_name_the_next_step() {
        assert!(
            UpdateError::Offline("x".into())
                .to_string()
                .contains("internet")
        );
        assert!(UpdateError::Unmanaged.to_string().contains("project page"));
        assert!(
            UpdateError::RateLimited {
                retry_after_secs: Some(30)
            }
            .to_string()
            .contains("in a minute")
        );
        assert!(
            UpdateError::RateLimited {
                retry_after_secs: None
            }
            .to_string()
            .contains("later")
        );
        assert_eq!(
            UpdateError::from(NetError::Status {
                status: 502,
                retry_after_secs: None
            }),
            UpdateError::Server(502)
        );
        assert_eq!(
            UpdateError::from(NetError::Cancelled),
            UpdateError::Cancelled
        );
    }
}
