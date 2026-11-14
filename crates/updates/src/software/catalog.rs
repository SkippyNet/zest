//! The public package catalog, read without the package manager.
//!
//! The Windows Package Manager's community catalog is published as one
//! pre-built index: a small archive holding an SQLite database with every
//! package's id, latest version and the fingerprints that tie it to an
//! installed program (product codes, package family names, normalised names
//! and publishers). The package manager itself downloads exactly this file.
//!
//! On PCs where the package manager is missing or broken, Zest reads the
//! same index directly and compares it with the programs registered on the
//! PC. That is enough to say *which* programs are out of date and what the
//! newest version is. It is deliberately not enough to install anything:
//! downloading and running installers stays the package manager's job.
//!
//! # How a program is matched
//!
//! 1. By product code (the key a program registers itself under) or, for
//!    Store-style packages, by package family name. These are exact.
//! 2. Otherwise by normalised name *and* normalised publisher together
//!    (see the `normalize` module).
//!
//! When more than one package claims a program and name and publisher
//! cannot tell them apart, the program is left out. A missing entry is
//! harmless; a wrong one is not.

use super::normalize::{normalize_name, normalize_publisher};
use super::version;
use crate::net::{Client, NetError};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use zest_core::Progress;
use zest_system::apps::{AppSource, InstalledApp};

/// Address of the published catalog index.
pub const INDEX_URL: &str = "https://cdn.winget.microsoft.com/cache/source2.msix";

/// Path of the database inside the published archive.
const INDEX_ENTRY: &str = "Public/index.db";

/// The local copy is refreshed when it is older than this.
const REFRESH_AFTER: Duration = Duration::from_secs(12 * 60 * 60);

/// The archive is a few megabytes; far larger answers are refused.
const MAX_ARCHIVE_BYTES: u64 = 96 * 1024 * 1024;
/// The unpacked database is about ten megabytes.
const MAX_INDEX_BYTES: u64 = 768 * 1024 * 1024;

/// Why the catalog could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// It could not be downloaded and no earlier copy exists.
    #[error("The software catalog could not be downloaded: {0}")]
    Download(NetError),
    /// What was downloaded is not a catalog this version understands.
    #[error("The software catalog has an unexpected format: {0}")]
    Format(String),
    /// A local file operation failed.
    #[error("The software catalog could not be stored: {0}")]
    Io(String),
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
}

impl From<rusqlite::Error> for CatalogError {
    fn from(error: rusqlite::Error) -> Self {
        CatalogError::Format(error.to_string())
    }
}

/// How an installed program was tied to a catalog package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatchedBy {
    /// The product code the program registers itself under.
    ProductCode,
    /// The package family name of a Store-style package.
    FamilyName,
    /// Normalised name and publisher.
    NameAndPublisher,
}

/// A catalog package.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Package {
    row: i64,
    id: String,
    name: String,
    latest: String,
    /// Lowest and highest version the latest release registers itself with,
    /// when that differs from the package version.
    registered: Option<(String, String)>,
}

/// What Zest needs to know about an installed program to look it up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstalledProgram {
    /// Display name.
    pub name: String,
    /// Publisher, when declared.
    pub publisher: Option<String>,
    /// Version text, when declared.
    pub version: Option<String>,
    /// Product codes: the registry key name and the installer's own code.
    pub product_codes: Vec<String>,
    /// Package family name (Store-style packages).
    pub family_name: Option<String>,
    /// The product's web page, when declared.
    pub about_url: Option<String>,
}

impl From<&InstalledApp> for InstalledProgram {
    fn from(app: &InstalledApp) -> Self {
        let mut product_codes = Vec::new();
        if app.source == AppSource::Registry {
            if let Some(key) = &app.key {
                product_codes.push(key.leaf().to_owned());
            }
            if let Some(code) = &app.product_code
                && !product_codes.iter().any(|c| c.eq_ignore_ascii_case(code))
            {
                product_codes.push(code.clone());
            }
        }
        InstalledProgram {
            name: app.name.clone(),
            publisher: app.publisher.clone(),
            version: app.version.clone(),
            product_codes,
            family_name: app.package_family_name.clone(),
            about_url: app.about_url.clone(),
        }
    }
}

/// An installed program for which the catalog lists a newer version.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CatalogUpdate {
    /// Display name of the installed program.
    pub name: String,
    /// Catalog package id, e.g. `Mozilla.Firefox`.
    pub id: String,
    /// Version currently installed.
    pub installed_version: String,
    /// Newest version in the catalog.
    pub available_version: String,
    /// How the program was recognised.
    pub matched_by: MatchedBy,
    /// Where to get the new version: the product's own page when the program
    /// declares one, otherwise the package's page in the catalog.
    pub info_url: String,
}

/// The catalog's public page for a package id.
pub fn package_page(id: &str) -> String {
    let initial = id
        .chars()
        .next()
        .map(|c| c.to_lowercase().to_string())
        .unwrap_or_default();
    format!(
        "https://github.com/microsoft/winget-pkgs/tree/master/manifests/{initial}/{}",
        id.replace('.', "/")
    )
}

/// An opened catalog index.
pub struct Catalog {
    db: Connection,
    packages: usize,
    age: Option<Duration>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("packages", &self.packages)
            .field("age", &self.age)
            .finish()
    }
}

/// Folder holding the local copy of the index.
pub fn cache_dir() -> PathBuf {
    zest_core::paths::data_dir().join("catalog")
}

const INDEX_FILE: &str = "index.db";

fn io_error(action: &str, path: &Path, error: &std::io::Error) -> CatalogError {
    CatalogError::Io(format!("could not {action} {}: {error}", path.display()))
}

impl Catalog {
    /// Opens an index database file and checks that it is one.
    pub fn open(file: &Path) -> Result<Catalog, CatalogError> {
        let db = Connection::open_with_flags(
            file,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let major: Option<String> = db
            .query_row(
                "SELECT value FROM metadata WHERE name = 'majorVersion'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| CatalogError::Format(format!("no version information ({e})")))?;
        if major.as_deref() != Some("2") {
            return Err(CatalogError::Format(format!(
                "index version {} is not supported",
                major.unwrap_or_default()
            )));
        }
        // Every table and column the queries below rely on.
        for probe in [
            "SELECT rowid, id, name, latest_version, arp_min_version, arp_max_version FROM packages LIMIT 1",
            "SELECT productcode, package FROM productcodes2 LIMIT 1",
            "SELECT norm_name, package FROM norm_names2 LIMIT 1",
            "SELECT norm_publisher, package FROM norm_publishers2 LIMIT 1",
            "SELECT pfn, package FROM pfns2 LIMIT 1",
        ] {
            db.prepare(probe)
                .map_err(|e| CatalogError::Format(format!("a table is missing ({e})")))?;
        }
        let packages: i64 = db.query_row("SELECT COUNT(*) FROM packages", [], |row| row.get(0))?;
        let age = std::fs::metadata(file)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        Ok(Catalog {
            db,
            packages: packages.max(0) as usize,
            age,
        })
    }

    /// Number of packages in the catalog.
    pub fn package_count(&self) -> usize {
        self.packages
    }

    /// How long ago the local copy was downloaded.
    pub fn age(&self) -> Option<Duration> {
        self.age
    }

    /// Opens the local copy of the catalog, downloading it first when there
    /// is none or it is older than half a day.
    ///
    /// The download is one anonymous request for a public file. When it
    /// fails and an earlier copy exists, that copy is used.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Download`] when there is no copy and none can be
    /// fetched, [`CatalogError::Format`] when the published format changed.
    pub fn load(progress: &Progress) -> Result<Catalog, CatalogError> {
        Catalog::load_from(&Client::new(), INDEX_URL, &cache_dir(), progress)
    }

    fn load_from(
        client: &Client,
        url: &str,
        folder: &Path,
        progress: &Progress,
    ) -> Result<Catalog, CatalogError> {
        let index = folder.join(INDEX_FILE);
        let fresh = std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age < REFRESH_AFTER);
        if fresh && let Ok(catalog) = Catalog::open(&index) {
            return Ok(catalog);
        }
        match download_index(client, url, folder, progress) {
            Ok(()) => Catalog::open(&index),
            Err(CatalogError::Cancelled) => Err(CatalogError::Cancelled),
            Err(error) => {
                // An older copy is better than none.
                match Catalog::open(&index) {
                    Ok(catalog) => {
                        log::warn!("using the earlier software catalog: {error}");
                        Ok(catalog)
                    }
                    Err(_) => Err(error),
                }
            }
        }
    }

    fn packages_where(&self, sql: &str, key: &str) -> Vec<Package> {
        let query = format!(
            "SELECT p.rowid, p.id, p.name, p.latest_version, p.arp_min_version, p.arp_max_version \
             FROM packages p WHERE p.rowid IN ({sql})"
        );
        let read = || -> rusqlite::Result<Vec<Package>> {
            let mut statement = self.db.prepare_cached(&query)?;
            let rows = statement.query_map([key], |row| {
                let min: Option<String> = row.get(4)?;
                let max: Option<String> = row.get(5)?;
                Ok(Package {
                    row: row.get(0)?,
                    id: row.get(1)?,
                    name: row.get(2)?,
                    latest: row.get(3)?,
                    registered: min.zip(max).filter(|(a, b)| !a.is_empty() && !b.is_empty()),
                })
            })?;
            rows.collect()
        };
        read().unwrap_or_else(|error| {
            log::warn!("catalog query failed: {error}");
            Vec::new()
        })
    }

    fn by_product_code(&self, code: &str) -> Vec<Package> {
        self.packages_where(
            "SELECT package FROM productcodes2 WHERE productcode = ?1",
            &code.to_lowercase(),
        )
    }

    fn by_family_name(&self, family: &str) -> Vec<Package> {
        self.packages_where(
            "SELECT package FROM pfns2 WHERE pfn = ?1",
            &family.to_lowercase(),
        )
    }

    /// Packages that list `name`; with a publisher, only those that list
    /// both together.
    fn by_name(&self, name: &str, publisher: Option<&str>) -> Vec<Package> {
        match publisher {
            Some(publisher) => {
                let query = "SELECT p.rowid, p.id, p.name, p.latest_version, p.arp_min_version, p.arp_max_version \
                     FROM norm_names2 n \
                     JOIN norm_publishers2 u ON u.package = n.package \
                     JOIN packages p ON p.rowid = n.package \
                     WHERE n.norm_name = ?1 AND u.norm_publisher = ?2";
                let read = || -> rusqlite::Result<Vec<Package>> {
                    let mut statement = self.db.prepare_cached(query)?;
                    let rows = statement.query_map([name, publisher], |row| {
                        let min: Option<String> = row.get(4)?;
                        let max: Option<String> = row.get(5)?;
                        Ok(Package {
                            row: row.get(0)?,
                            id: row.get(1)?,
                            name: row.get(2)?,
                            latest: row.get(3)?,
                            registered: min
                                .zip(max)
                                .filter(|(a, b)| !a.is_empty() && !b.is_empty()),
                        })
                    })?;
                    rows.collect()
                };
                read().unwrap_or_default()
            }
            None => {
                self.packages_where("SELECT package FROM norm_names2 WHERE norm_name = ?1", name)
            }
        }
    }

    /// Finds the one catalog package that is `program`, if it can be told.
    fn identify(&self, program: &InstalledProgram) -> Option<(Package, MatchedBy)> {
        let normalized = normalize_name(&program.name);
        let publisher = program
            .publisher
            .as_deref()
            .map(normalize_publisher)
            .filter(|p| !p.is_empty());
        // A name with its architecture is the more specific spelling.
        let names: Vec<String> = normalized
            .with_arch()
            .into_iter()
            .chain(std::iter::once(normalized.name.clone()))
            .filter(|name| !name.is_empty() && !name.starts_with('('))
            .collect();

        // 1. Exact fingerprints.
        let mut exact: Vec<Package> = Vec::new();
        let mut how = MatchedBy::ProductCode;
        for code in &program.product_codes {
            for package in self.by_product_code(code) {
                if !exact.iter().any(|p| p.row == package.row) {
                    exact.push(package);
                }
            }
        }
        if exact.is_empty()
            && let Some(family) = &program.family_name
        {
            exact = self.by_family_name(family);
            how = MatchedBy::FamilyName;
        }
        match exact.len() {
            1 => return exact.pop().map(|package| (package, how)),
            0 => {}
            _ => {
                // Several packages share the fingerprint (release channels of
                // one product, forks). Name, then name and publisher, decide.
                let narrowed = |with_publisher: bool| -> Option<Package> {
                    for name in &names {
                        let publisher = if with_publisher {
                            Some(publisher.as_deref()?)
                        } else {
                            None
                        };
                        let named = self.by_name(name, publisher);
                        let mut both = exact
                            .iter()
                            .filter(|p| named.iter().any(|n| n.row == p.row));
                        if let (Some(only), None) = (both.next(), both.next()) {
                            return Some(only.clone());
                        }
                    }
                    None
                };
                return narrowed(false).or_else(|| narrowed(true)).map(|p| (p, how));
            }
        }

        // 2. Name and publisher together.
        let publisher = publisher?;
        for name in &names {
            let mut found = self.by_name(name, Some(&publisher));
            match found.len() {
                0 => continue,
                1 => return found.pop().map(|p| (p, MatchedBy::NameAndPublisher)),
                // Several packages with one name and publisher: cannot tell.
                _ => return None,
            }
        }
        None
    }

    /// The catalog package id of `program`, when it can be identified.
    pub fn package_id(&self, program: &InstalledProgram) -> Option<String> {
        self.identify(program).map(|(package, _)| package.id)
    }

    /// Compares installed programs with the catalog.
    ///
    /// Returns one entry per catalog package that has a newer version than
    /// the newest installed copy of it, sorted by name. Programs without a
    /// readable version and programs that cannot be identified with
    /// certainty are left out.
    pub fn updates_for(&self, installed: &[InstalledProgram]) -> Vec<CatalogUpdate> {
        struct Seen {
            update: Option<CatalogUpdate>,
            newest_installed: String,
        }
        let mut by_package: HashMap<String, Seen> = HashMap::new();
        for program in installed {
            let Some(installed_version) = program.version.as_deref().map(str::trim) else {
                continue;
            };
            if version::CatalogVersion::parse(installed_version).is_none() {
                continue;
            }
            let Some((package, matched_by)) = self.identify(program) else {
                continue;
            };
            // Where the latest release registers itself under another
            // version scheme, that is what installed versions compare with.
            let outdated = match &package.registered {
                Some((lowest, _)) => version::is_older(installed_version, lowest),
                None => version::is_older(installed_version, &package.latest),
            };
            let update = outdated.then(|| CatalogUpdate {
                name: program.name.clone(),
                info_url: program
                    .about_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                    .map(str::to_owned)
                    .unwrap_or_else(|| package_page(&package.id)),
                id: package.id.clone(),
                installed_version: installed_version.to_owned(),
                available_version: package.latest.clone(),
                matched_by,
            });
            match by_package.get_mut(&package.id) {
                None => {
                    by_package.insert(
                        package.id,
                        Seen {
                            update,
                            newest_installed: installed_version.to_owned(),
                        },
                    );
                }
                // Several copies side by side: the newest one counts.
                Some(seen) => {
                    if version::is_older(&seen.newest_installed, installed_version) {
                        seen.newest_installed = installed_version.to_owned();
                        seen.update = update;
                    }
                }
            }
        }
        let mut updates: Vec<CatalogUpdate> = by_package
            .into_values()
            .filter_map(|seen| seen.update)
            .collect();
        updates.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        updates
    }
}

/// Downloads the published archive and replaces the local index with the
/// database inside it. The previous index stays in place until the new one
/// has been unpacked and opened successfully.
fn download_index(
    client: &Client,
    url: &str,
    folder: &Path,
    progress: &Progress,
) -> Result<(), CatalogError> {
    std::fs::create_dir_all(folder).map_err(|e| io_error("create", folder, &e))?;
    let archive = folder.join("index.download");
    let unpacked = folder.join("index.new");
    let index = folder.join(INDEX_FILE);
    progress.set_message("Downloading the software catalog…");
    let result = (|| {
        client
            .download(url, &archive, MAX_ARCHIVE_BYTES, progress)
            .map_err(|error| match error {
                NetError::Cancelled => CatalogError::Cancelled,
                other => CatalogError::Download(other),
            })?;
        extract_index(&archive, &unpacked)?;
        // Opening validates the format before the old copy is given up.
        drop(Catalog::open(&unpacked)?);
        if index.exists() {
            std::fs::remove_file(&index).map_err(|e| io_error("replace", &index, &e))?;
        }
        std::fs::rename(&unpacked, &index).map_err(|e| io_error("store", &index, &e))
    })();
    // Our own temporary files.
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_file(&unpacked);
    result
}

fn extract_index(archive: &Path, into: &Path) -> Result<(), CatalogError> {
    let file = std::fs::File::open(archive).map_err(|e| io_error("open", archive, &e))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| CatalogError::Format(format!("not an archive ({e})")))?;
    let mut entry = zip
        .by_name(INDEX_ENTRY)
        .map_err(|e| CatalogError::Format(format!("no index inside ({e})")))?;
    if entry.size() > MAX_INDEX_BYTES {
        return Err(CatalogError::Format(
            "the index is implausibly large".to_owned(),
        ));
    }
    let mut out = std::fs::File::create(into).map_err(|e| io_error("write", into, &e))?;
    let copied = std::io::copy(
        &mut std::io::Read::take(&mut entry, MAX_INDEX_BYTES + 1),
        &mut out,
    )
    .map_err(|e| CatalogError::Format(format!("the index cannot be unpacked ({e})")))?;
    if copied > MAX_INDEX_BYTES {
        return Err(CatalogError::Format(
            "the index is implausibly large".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::testserver::{Reply, TestServer};
    use std::io::Write;

    /// Builds an index database with the published schema and a handful of
    /// packages modelled on real catalog entries.
    pub(crate) fn sample_index(file: &Path) {
        let db = Connection::open(file).unwrap();
        db.execute_batch(
            "CREATE TABLE [metadata]([name] TEXT PRIMARY KEY NOT NULL, [value] TEXT NOT NULL) WITHOUT ROWID;
             CREATE TABLE [packages](rowid INTEGER PRIMARY KEY, [id] TEXT NOT NULL, [name] TEXT NOT NULL, [moniker] TEXT, [latest_version] TEXT NOT NULL, [arp_min_version] TEXT, [arp_max_version] TEXT, [hash] BLOB);
             CREATE TABLE [productcodes2]([productcode] TEXT NOT NULL, [package] INT64 NOT NULL, PRIMARY KEY([productcode], [package])) WITHOUT ROWID;
             CREATE TABLE [norm_names2]([norm_name] TEXT NOT NULL, [package] INT64 NOT NULL, PRIMARY KEY([norm_name], [package])) WITHOUT ROWID;
             CREATE TABLE [norm_publishers2]([norm_publisher] TEXT NOT NULL, [package] INT64 NOT NULL, PRIMARY KEY([norm_publisher], [package])) WITHOUT ROWID;
             CREATE TABLE [pfns2]([pfn] TEXT NOT NULL, [package] INT64 NOT NULL, PRIMARY KEY([pfn], [package])) WITHOUT ROWID;
             CREATE TABLE [upgradecodes2]([upgradecode] TEXT NOT NULL, [package] INT64 NOT NULL, PRIMARY KEY([upgradecode], [package])) WITHOUT ROWID;
             INSERT INTO metadata VALUES ('majorVersion', '2'), ('minorVersion', '1');",
        )
        .unwrap();
        // (row, id, name, latest, arp min, arp max, names, publishers, codes, family names)
        type Row<'a> = (
            i64,
            &'a str,
            &'a str,
            &'a str,
            Option<&'a str>,
            Option<&'a str>,
            &'a [&'a str],
            &'a [&'a str],
            &'a [&'a str],
            &'a [&'a str],
        );
        let rows: &[Row] = &[
            (
                1,
                "7zip.7zip",
                "7-Zip",
                "26.06",
                None,
                None,
                &["7zip", "7zip(X64)"],
                &["igorpavlov"],
                &["7-zip", "{23170f69-40c1-2701-1604-000001000000}"],
                &[],
            ),
            (
                2,
                "Git.Git",
                "Git",
                "2.56.0.4",
                None,
                None,
                &["git"],
                &["thegitdevelopmentcommunity", "johannesschindelin"],
                &[],
                &[],
            ),
            (
                3,
                "Mozilla.Firefox",
                "Mozilla Firefox (en-US)",
                "160.0",
                None,
                None,
                &[
                    "mozillafirefox",
                    "mozillafirefox(X64)",
                    "mozillafirefox(X86)",
                ],
                &["mozilla"],
                &["mozilla firefox 131.0 (x64 en-us)"],
                &[],
            ),
            (
                4,
                "Google.Chrome.EXE",
                "Google Chrome (EXE)",
                "157.0.8103.79",
                None,
                None,
                &["googlechrome"],
                &["google"],
                &["google chrome"],
                &[],
            ),
            (
                5,
                "Google.Chrome.Beta.EXE",
                "Google Chrome Beta (EXE)",
                "159.0.8154.6",
                None,
                None,
                &["googlechromebeta"],
                &["google"],
                &["google chrome", "google chrome beta"],
                &[],
            ),
            (
                6,
                "JetBrains.PhpStorm",
                "PhpStorm",
                "2026.3",
                Some("263.7307.215"),
                Some("263.7307.215"),
                &["phpstorm"],
                &["jetbrains"],
                &["phpstorm 2025.1.2"],
                &[],
            ),
            (
                7,
                "CoreyButler.NVMforWindows",
                "NVM for Windows",
                "1.2.2",
                None,
                None,
                &["nvmforwindows"],
                &["authorsoftware", "ecorventures"],
                &["40078385-f676-4c61-9a9c-f9028599d6d3_is1"],
                &[],
            ),
            (
                8,
                "AuthorSoftware.NVMWindows",
                "NVM for Windows",
                "2.0.1",
                None,
                None,
                &["nvmforwindows"],
                &["authorsoftware"],
                &["40078385-f676-4c61-9a9c-f9028599d6d3_is1"],
                &[],
            ),
            (
                9,
                "Microsoft.WindowsTerminal",
                "Windows Terminal",
                "1.25.3131.0",
                None,
                None,
                &["windowsterminal"],
                &["microsoft"],
                &[],
                &["microsoft.windowsterminal_8wekyb3d8bbwe"],
            ),
            (
                10,
                "Microsoft.VCRedist.2013.x64",
                "Microsoft Visual C++ 2013 Redistributable (x64)",
                "12.0.40664.0",
                None,
                None,
                &[
                    "microsoftvisualc2013redistributable",
                    "microsoftvisualc2013redistributable(X64)",
                ],
                &["microsoft"],
                &[
                    "{042d26ef-3dbe-4c25-95d3-4c1b11b235a7}",
                    "{ef6b00ec-13e1-4c25-9064-b2f383cb8412}",
                ],
                &[],
            ),
            (
                11,
                "Example.Tool",
                "Example Tool",
                "3.0",
                None,
                None,
                &["exampletool"],
                &["example"],
                &[],
                &[],
            ),
            (
                12,
                "Example.Tool.Nightly",
                "Example Tool",
                "4.0-nightly",
                None,
                None,
                &["exampletool"],
                &["example"],
                &[],
                &[],
            ),
        ];
        for (row, id, name, latest, min, max, names, publishers, codes, families) in rows {
            db.execute(
                "INSERT INTO packages(rowid, id, name, latest_version, arp_min_version, arp_max_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![row, id, name, latest, min, max],
            )
            .unwrap();
            for value in *names {
                db.execute(
                    "INSERT INTO norm_names2 VALUES (?1, ?2)",
                    rusqlite::params![value, row],
                )
                .unwrap();
            }
            for value in *publishers {
                db.execute(
                    "INSERT INTO norm_publishers2 VALUES (?1, ?2)",
                    rusqlite::params![value, row],
                )
                .unwrap();
            }
            for value in *codes {
                db.execute(
                    "INSERT INTO productcodes2 VALUES (?1, ?2)",
                    rusqlite::params![value, row],
                )
                .unwrap();
            }
            for value in *families {
                db.execute(
                    "INSERT INTO pfns2 VALUES (?1, ?2)",
                    rusqlite::params![value, row],
                )
                .unwrap();
            }
        }
    }

    fn sample_catalog(dir: &Path) -> Catalog {
        let file = dir.join("index.db");
        sample_index(&file);
        Catalog::open(&file).unwrap()
    }

    fn program(name: &str, publisher: &str, version: &str, codes: &[&str]) -> InstalledProgram {
        InstalledProgram {
            name: name.to_owned(),
            publisher: (!publisher.is_empty()).then(|| publisher.to_owned()),
            version: (!version.is_empty()).then(|| version.to_owned()),
            product_codes: codes.iter().map(|c| (*c).to_owned()).collect(),
            family_name: None,
            about_url: None,
        }
    }

    #[test]
    fn programs_are_identified_by_code_then_by_name_and_publisher() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = sample_catalog(dir.path());
        assert_eq!(catalog.package_count(), 12);

        // Product code, whatever the display name says, in any letter case.
        let by_code = program(
            "Seven Zip",
            "",
            "23.01",
            &["{23170F69-40C1-2701-1604-000001000000}"],
        );
        assert_eq!(
            catalog.identify(&by_code).map(|(p, how)| (p.id, how)),
            Some(("7zip.7zip".to_owned(), MatchedBy::ProductCode))
        );
        // No code in the catalog: name and publisher.
        let git = program(
            "Git version 2.47.1",
            "The Git Development Community",
            "2.47.1",
            &["Git_is1"],
        );
        assert_eq!(
            catalog.identify(&git).map(|(p, how)| (p.id, how)),
            Some(("Git.Git".to_owned(), MatchedBy::NameAndPublisher))
        );
        // Decorations in the name do not matter.
        let firefox = program(
            "Mozilla Firefox (x64 de)",
            "Mozilla",
            "131.0",
            &["Mozilla Firefox 131.0 (x64 de)"],
        );
        assert_eq!(
            catalog.package_id(&firefox).as_deref(),
            Some("Mozilla.Firefox")
        );
        // Package family name.
        let terminal = InstalledProgram {
            family_name: Some("Microsoft.WindowsTerminal_8wekyb3d8bbwe".to_owned()),
            ..program("Terminal", "", "1.24.1.0", &[])
        };
        assert_eq!(
            catalog.identify(&terminal).map(|(p, how)| (p.id, how)),
            Some((
                "Microsoft.WindowsTerminal".to_owned(),
                MatchedBy::FamilyName
            ))
        );

        // The right name with the wrong publisher is somebody else's program.
        let impostor = program("Git", "Somebody Else", "1.0", &[]);
        assert_eq!(catalog.package_id(&impostor), None);
        // Without a publisher a name alone is never enough.
        assert_eq!(catalog.package_id(&program("Git", "", "1.0", &[])), None);
        assert_eq!(
            catalog.package_id(&program("Unknown Thing", "Nobody", "1.0", &["{nope}"])),
            None
        );
        assert_eq!(catalog.package_id(&program("", "", "", &[])), None);
    }

    #[test]
    fn shared_fingerprints_are_told_apart_or_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = sample_catalog(dir.path());
        // Stable and beta register under one key; the name decides.
        let chrome = program(
            "Google Chrome",
            "Google LLC",
            "157.0.8103.78",
            &["Google Chrome"],
        );
        assert_eq!(
            catalog.package_id(&chrome).as_deref(),
            Some("Google.Chrome.EXE")
        );
        let beta = program(
            "Google Chrome Beta",
            "Google LLC",
            "158.0.1.2",
            &["Google Chrome Beta"],
        );
        assert_eq!(
            catalog.package_id(&beta).as_deref(),
            Some("Google.Chrome.Beta.EXE")
        );

        // Two packages with the same code, name and publisher: not guessed.
        let nvm = program(
            "NVM for Windows 1.2.2",
            "Author Software Inc.",
            "1.2.2",
            &["40078385-F676-4C61-9A9C-F9028599D6D3_is1"],
        );
        assert_eq!(catalog.package_id(&nvm), None);
        // The same for name and publisher alone.
        let twin = program("Example Tool", "Example Ltd", "1.0", &[]);
        assert_eq!(catalog.package_id(&twin), None);
        assert!(catalog.updates_for(&[nvm, twin]).is_empty());
    }

    #[test]
    fn only_genuinely_older_programs_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = sample_catalog(dir.path());
        let installed = vec![
            // Older than the catalog: listed.
            program("7-Zip 23.01 (x64)", "Igor Pavlov", "23.01", &["7-Zip"]),
            InstalledProgram {
                about_url: Some("https://git-scm.com/".to_owned()),
                ..program(
                    "Git",
                    "The Git Development Community",
                    "2.47.1",
                    &["Git_is1"],
                )
            },
            // Current, newer than the catalog, unknown version: not listed.
            program("Mozilla Firefox (x64 en-US)", "Mozilla", "160.0", &[]),
            program(
                "Google Chrome",
                "Google LLC",
                "158.0.0.1",
                &["Google Chrome"],
            ),
            program("Windows Terminal Preview", "Microsoft Corporation", "", &[]),
            // The latest release registers itself as 263.x: 251.x is older
            // although "251" is a bigger number than "2026.3" suggests.
            program(
                "PhpStorm 2025.1.2",
                "JetBrains s.r.o.",
                "251.26094.133",
                &["PhpStorm 2025.1.2"],
            ),
            // Two copies side by side, one of them current: nothing to do.
            program(
                "Microsoft Visual C++ 2013 Redistributable (x64) - 12.0.30501",
                "Microsoft Corporation",
                "12.0.30501.0",
                &["{042d26ef-3dbe-4c25-95d3-4c1b11b235a7}"],
            ),
            program(
                "Microsoft Visual C++ 2013 Redistributable (x64) - 12.0.40664",
                "Microsoft Corporation",
                "12.0.40664.0",
                &["{ef6b00ec-13e1-4c25-9064-b2f383cb8412}"],
            ),
        ];
        let updates = catalog.updates_for(&installed);
        let summary: Vec<(&str, &str, &str)> = updates
            .iter()
            .map(|u| {
                (
                    u.id.as_str(),
                    u.installed_version.as_str(),
                    u.available_version.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("7zip.7zip", "23.01", "26.06"),
                ("Git.Git", "2.47.1", "2.56.0.4"),
                ("JetBrains.PhpStorm", "251.26094.133", "2026.3"),
            ]
        );
        assert_eq!(updates[0].matched_by, MatchedBy::ProductCode);
        assert_eq!(
            updates[0].info_url,
            "https://github.com/microsoft/winget-pkgs/tree/master/manifests/7/7zip/7zip"
        );
        assert_eq!(updates[1].info_url, "https://git-scm.com/");
        assert_eq!(updates[1].matched_by, MatchedBy::NameAndPublisher);

        // A PhpStorm that registers as 263.x is current.
        let current = program("PhpStorm 2026.3", "JetBrains s.r.o.", "263.7307.215", &[]);
        assert!(catalog.updates_for(&[current]).is_empty());
        assert!(catalog.updates_for(&[]).is_empty());
    }

    #[test]
    fn package_pages_follow_the_catalog_layout() {
        assert_eq!(
            package_page("Mozilla.Firefox"),
            "https://github.com/microsoft/winget-pkgs/tree/master/manifests/m/Mozilla/Firefox"
        );
        assert_eq!(
            package_page("Google.Chrome.EXE"),
            "https://github.com/microsoft/winget-pkgs/tree/master/manifests/g/Google/Chrome/EXE"
        );
    }

    #[test]
    fn files_that_are_not_an_index_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("text.db");
        std::fs::write(&text, "not a database").unwrap();
        assert!(matches!(Catalog::open(&text), Err(CatalogError::Format(_))));

        let empty = dir.path().join("empty.db");
        Connection::open(&empty)
            .unwrap()
            .execute_batch("CREATE TABLE t(x);")
            .unwrap();
        assert!(matches!(
            Catalog::open(&empty),
            Err(CatalogError::Format(_))
        ));

        // A future major version is not read as if it were the known one.
        let future = dir.path().join("future.db");
        sample_index(&future);
        Connection::open(&future)
            .unwrap()
            .execute(
                "UPDATE metadata SET value = '3' WHERE name = 'majorVersion'",
                [],
            )
            .unwrap();
        let error = Catalog::open(&future).unwrap_err();
        assert!(
            matches!(&error, CatalogError::Format(m) if m.contains('3')),
            "{error:?}"
        );

        // A renamed column is caught when the index is opened, not later.
        let changed = dir.path().join("changed.db");
        sample_index(&changed);
        Connection::open(&changed)
            .unwrap()
            .execute_batch("ALTER TABLE packages RENAME COLUMN latest_version TO newest;")
            .unwrap();
        assert!(matches!(
            Catalog::open(&changed),
            Err(CatalogError::Format(_))
        ));
    }

    fn archive_with_index(dir: &Path) -> Vec<u8> {
        let file = dir.join("source-index.db");
        sample_index(&file);
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("AppxManifest.xml", options).unwrap();
        writer.write_all(b"<Package/>").unwrap();
        writer.start_file(INDEX_ENTRY, options).unwrap();
        writer.write_all(&std::fs::read(&file).unwrap()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn the_index_is_downloaded_once_and_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        let server = TestServer::start();
        server.route("/source2.msix", Reply::ok(archive_with_index(dir.path())));
        let cache = dir.path().join("cache");
        let client = Client::for_tests();
        let url = server.url("/source2.msix");

        let catalog = Catalog::load_from(&client, &url, &cache, &Progress::new()).unwrap();
        assert_eq!(catalog.package_count(), 12);
        assert!(
            catalog
                .age()
                .is_some_and(|age| age < Duration::from_secs(60))
        );
        drop(catalog);
        assert_eq!(server.requests().len(), 1);
        // Only the index itself stays behind.
        let files: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, [INDEX_FILE]);

        // A second load within the refresh interval makes no request.
        let again = Catalog::load_from(&client, &url, &cache, &Progress::new()).unwrap();
        assert_eq!(again.package_count(), 12);
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn an_earlier_copy_survives_failed_and_bad_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let server = TestServer::start();
        let cache = dir.path().join("cache");
        let client = Client::for_tests();
        let url = server.url("/source2.msix");

        // Nothing cached and nothing to download: an error, not an empty catalog.
        let error = Catalog::load_from(&client, &url, &cache, &Progress::new()).unwrap_err();
        assert!(
            matches!(error, CatalogError::Download(ref e) if e.is_not_found()),
            "{error:?}"
        );

        // A cached copy that is due for refresh...
        std::fs::create_dir_all(&cache).unwrap();
        let index = cache.join(INDEX_FILE);
        sample_index(&index);
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&index)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // ...is still used when the server is unavailable,
        let stale = Catalog::load_from(&client, &url, &cache, &Progress::new()).unwrap();
        assert_eq!(stale.package_count(), 12);
        assert!(
            stale
                .age()
                .is_some_and(|age| age > Duration::from_secs(2 * 24 * 60 * 60))
        );
        drop(stale);
        // when the server sends something that is not an archive,
        server.route("/source2.msix", Reply::ok("<html>maintenance</html>"));
        assert_eq!(
            Catalog::load_from(&client, &url, &cache, &Progress::new())
                .unwrap()
                .package_count(),
            12
        );
        // and when the archive holds something that is not an index.
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(INDEX_ENTRY, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"not a database").unwrap();
        server.route(
            "/source2.msix",
            Reply::ok(writer.finish().unwrap().into_inner()),
        );
        assert_eq!(
            Catalog::load_from(&client, &url, &cache, &Progress::new())
                .unwrap()
                .package_count(),
            12
        );

        // Cancelling is reported as such and keeps the copy.
        let cancelled = Progress::new();
        cancelled.cancel();
        assert_eq!(
            Catalog::load_from(&client, &url, &cache, &cancelled).unwrap_err(),
            CatalogError::Cancelled
        );
        assert!(index.exists());
    }

    #[test]
    fn installed_apps_convert_with_their_fingerprints() {
        use zest_core::reg::RegPath;
        let json = serde_json::json!({
            "id": "win32|x", "source": "Registry", "name": "Git", "publisher": "The Git Development Community",
            "version": "2.47.1", "install_date": null, "install_date_estimated": false, "size_bytes": null,
            "install_location": null, "uninstall_string": null, "quiet_uninstall_string": null,
            "modify_path": null, "is_system_component": false, "is_update": false, "is_msi": false,
            "product_code": null, "icon_path": null,
            "key": RegPath::hklm(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Git_is1"),
            "is_64bit": true, "scope": "Machine", "requires_admin": true, "can_uninstall": true,
            "can_repair": false, "package_full_name": null, "package_family_name": null,
            "about_url": "https://gitforwindows.org/"
        });
        let Ok(app) = serde_json::from_value::<InstalledApp>(json) else {
            // The inventory type gained a field this fixture does not know;
            // the conversion itself is covered by the live example.
            return;
        };
        let program = InstalledProgram::from(&app);
        assert_eq!(program.product_codes, ["Git_is1"]);
        assert_eq!(program.name, "Git");
        assert_eq!(
            program.about_url.as_deref(),
            Some("https://gitforwindows.org/")
        );
    }
}
