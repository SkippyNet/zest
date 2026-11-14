//! The repository on disk: where snapshots and the contents of their files
//! are kept.
//!
//! ```text
//! <destination>\ZestBackup\
//!     zest-backup.json        what this folder is and how it was set up
//!     objects\ab\abcdef...    one file per stored chunk, named by its hash
//!     snapshots\<id>.zmf      the full record of one snapshot (compressed)
//!     snapshots\<id>.json     its short summary, for fast listing
//!     tmp\                    files being written
//!     lock                    held open while something changes the repository
//! ```
//!
//! File contents are split into chunks, and every chunk is stored once under
//! the hash of its bytes. Two files (or two versions of one file) that share
//! a chunk share its stored copy. A snapshot exists from the moment its
//! record is renamed into `snapshots`; everything written before that is
//! invisible, so an interrupted run never leaves a half-made snapshot.

use crate::manifest::{MANIFEST_FORMAT, Manifest, ObjectId, SnapshotInfo};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zest_core::{Error, IoContext, Result};

/// Folder created inside the chosen destination.
pub const REPO_DIR_NAME: &str = "ZestBackup";
/// File that marks a folder as a repository.
pub const CONFIG_FILE: &str = "zest-backup.json";
/// Size of the pieces large files are stored in.
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

const REPO_FORMAT: u32 = 1;
const OBJECT_MAGIC: [u8; 3] = *b"ZO1";
const OBJECT_HEADER: usize = 8;
const CODEC_RAW: u8 = 0;
const CODEC_ZSTD: u8 = 1;
const MANIFEST_EXT: &str = "zmf";
const SUMMARY_EXT: &str = "json";
const ERROR_SHARING_VIOLATION: i32 = 32;

#[derive(Serialize, Deserialize)]
struct RepoConfig {
    format: u32,
    created: i64,
    chunk_size: u64,
}

/// A backup repository that has been opened.
#[derive(Clone, Debug)]
pub struct Repository {
    root: PathBuf,
    chunk_size: usize,
}

/// Keeps other writers out of the repository while it is alive.
pub struct RepoLock {
    _file: File,
}

/// What a check of one stored object found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectState {
    /// Present and intact; the uncompressed length is given.
    Intact(u64),
    /// The file is gone.
    Missing,
    /// The file is there but its contents no longer match its name.
    Damaged,
    /// The file is there but could not be read just now.
    Unreadable,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_name() -> String {
    format!(
        "{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// True for an identifier of the form this crate gives snapshots. Checked
/// before an identifier from outside is turned into a file name.
pub fn is_snapshot_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if bytes.len() < 15 || !digits(0..8) || bytes[8] != b'-' || !digits(9..15) {
        return false;
    }
    match bytes.get(15) {
        None => true,
        Some(b'-') => bytes.len() > 16 && digits(16..bytes.len()),
        Some(_) => false,
    }
}

/// Sort key that puts snapshots in the order they were made: by time, then
/// by the counter given to snapshots that share a second.
fn snapshot_order(id: &str) -> (String, u32) {
    let base = id.get(..15).unwrap_or(id).to_owned();
    let counter = id.get(16..).and_then(|n| n.parse().ok()).unwrap_or(1);
    (base, counter)
}

impl Repository {
    /// Where the repository for `destination` lives: the folder itself when
    /// it already is a repository, otherwise a `ZestBackup` folder inside it.
    pub fn resolve_root(destination: &Path) -> PathBuf {
        if destination.join(CONFIG_FILE).is_file() {
            destination.to_path_buf()
        } else {
            destination.join(REPO_DIR_NAME)
        }
    }

    /// True when `destination` already holds a repository.
    pub fn exists(destination: &Path) -> bool {
        Self::resolve_root(destination).join(CONFIG_FILE).is_file()
    }

    /// Opens the repository at `destination`.
    pub fn open(destination: &Path) -> Result<Repository> {
        let root = Self::resolve_root(destination);
        let file = root.join(CONFIG_FILE);
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!(
                    "no backups in {}",
                    destination.display()
                )));
            }
            Err(e) => return Err(e).ctx(format!("reading {}", file.display())),
        };
        let config: RepoConfig = serde_json::from_str(&text)
            .map_err(|e| Error::Invalid(format!("{} is damaged: {e}", file.display())))?;
        if config.format == 0 || config.format > REPO_FORMAT {
            return Err(Error::Invalid(format!(
                "the backups in {} were made by a newer version of this program",
                destination.display()
            )));
        }
        if config.chunk_size == 0 || config.chunk_size > 1 << 30 {
            return Err(Error::Invalid(format!("{} is damaged", file.display())));
        }
        Ok(Repository {
            root,
            chunk_size: config.chunk_size as usize,
        })
    }

    /// Opens the repository at `destination`, setting one up when there is
    /// none yet.
    pub fn open_or_create(destination: &Path) -> Result<Repository> {
        Self::open_or_create_with(destination, CHUNK_SIZE)
    }

    /// Like [`Repository::open_or_create`] with a chosen chunk size for a
    /// new repository. Small chunks let tests cover multi-chunk files without
    /// writing megabytes.
    #[doc(hidden)]
    pub fn open_or_create_with(destination: &Path, chunk_size: usize) -> Result<Repository> {
        if Self::exists(destination) {
            return Self::open(destination);
        }
        let root = Self::resolve_root(destination);
        let repo = Repository { root, chunk_size };
        for dir in [repo.objects_dir(), repo.snapshots_dir(), repo.tmp_dir()] {
            fs::create_dir_all(&dir).ctx(format!("creating {}", dir.display()))?;
        }
        let config = RepoConfig {
            format: REPO_FORMAT,
            created: chrono::Utc::now().timestamp(),
            chunk_size: chunk_size as u64,
        };
        let text = serde_json::to_string_pretty(&config)?;
        repo.write_atomic(&repo.root.join(CONFIG_FILE), text.as_bytes())?;
        Ok(repo)
    }

    /// The repository folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Size of the pieces large files are stored in.
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub(crate) fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    pub(crate) fn snapshots_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.objects_dir().join(&hex[..2]).join(&hex[2..])
    }

    fn manifest_path(&self, id: &str) -> PathBuf {
        self.snapshots_dir().join(format!("{id}.{MANIFEST_EXT}"))
    }

    fn summary_path(&self, id: &str) -> PathBuf {
        self.snapshots_dir().join(format!("{id}.{SUMMARY_EXT}"))
    }

    /// Takes the repository for writing. Fails when another backup, restore
    /// clean-up or other writer holds it; the lock goes away with its owner,
    /// even after a crash.
    pub fn lock(&self) -> Result<RepoLock> {
        let path = self.root.join("lock");
        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
        {
            Ok(file) => Ok(RepoLock { _file: file }),
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Err(Error::Other(
                "these backups are in use by another backup or clean-up that is still running"
                    .into(),
            )),
            Err(e) => Err(e).ctx(format!("locking {}", self.root.display())),
        }
    }

    /// Writes `bytes` to `target` so that the file is either complete or
    /// absent: a temporary file is written, flushed and renamed into place.
    fn write_atomic(&self, target: &Path, bytes: &[u8]) -> Result<()> {
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).ctx(format!("creating {}", tmp_dir.display()))?;
        let tmp = tmp_dir.join(temp_name());
        let write = || -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, target)
        };
        write().map_err(|e| {
            let _ = fs::remove_file(&tmp);
            Error::Io {
                context: format!("writing {}", target.display()),
                source: e,
            }
        })
    }

    // ── Objects ──────────────────────────────────────────────────────

    /// Stores one chunk under its hash. Returns the bytes added to the
    /// repository: zero when the chunk was already there.
    pub(crate) fn put_object(&self, id: &ObjectId, raw: &[u8], level: i32) -> Result<u64> {
        let path = self.object_path(id);
        if path.is_file() {
            return Ok(0);
        }
        let compressed = zstd::bulk::compress(raw, level).ctx("compressing")?;
        let (codec, payload) = if compressed.len() < raw.len() {
            (CODEC_ZSTD, compressed.as_slice())
        } else {
            (CODEC_RAW, raw)
        };
        let mut header = [0u8; OBJECT_HEADER];
        header[..3].copy_from_slice(&OBJECT_MAGIC);
        header[3] = codec;
        header[4..].copy_from_slice(&(raw.len() as u32).to_le_bytes());

        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).ctx(format!("creating {}", dir.display()))?;
        }
        let tmp = self.tmp_dir().join(temp_name());
        let write = || -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(&header)?;
            file.write_all(payload)?;
            drop(file);
            fs::rename(&tmp, &path)
        };
        match write() {
            Ok(()) => Ok((OBJECT_HEADER + payload.len()) as u64),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                // Another thread may have stored the same chunk meanwhile.
                if path.is_file() {
                    Ok(0)
                } else {
                    Err(e).ctx(format!("storing data in {}", self.root.display()))
                }
            }
        }
    }

    fn decode_object(id: &ObjectId, bytes: &[u8]) -> Option<Vec<u8>> {
        if bytes.len() < OBJECT_HEADER || bytes[..3] != OBJECT_MAGIC {
            return None;
        }
        let raw_len = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let payload = &bytes[OBJECT_HEADER..];
        let raw = match bytes[3] {
            CODEC_RAW => payload.to_vec(),
            CODEC_ZSTD => zstd::bulk::decompress(payload, raw_len).ok()?,
            _ => return None,
        };
        (raw.len() == raw_len && ObjectId::of(&raw) == *id).then_some(raw)
    }

    /// Reads one chunk back and checks it against its hash.
    pub(crate) fn read_object(&self, id: &ObjectId) -> Result<Vec<u8>> {
        let path = self.object_path(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!(
                    "stored data {} is missing from the backup",
                    &id.to_hex()[..12]
                )));
            }
            Err(e) => return Err(e).ctx(format!("reading {}", path.display())),
        };
        Self::decode_object(id, &bytes)
            .ok_or_else(|| Error::Invalid(format!("stored data {} is damaged", &id.to_hex()[..12])))
    }

    /// Checks one stored chunk without keeping its contents.
    pub(crate) fn object_state(&self, id: &ObjectId) -> ObjectState {
        match fs::read(self.object_path(id)) {
            Ok(bytes) => match Self::decode_object(id, &bytes) {
                Some(raw) => ObjectState::Intact(raw.len() as u64),
                None => ObjectState::Damaged,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ObjectState::Missing,
            Err(_) => ObjectState::Unreadable,
        }
    }

    /// Every stored object with its size on disk.
    pub(crate) fn list_objects(&self) -> Result<Vec<(ObjectId, u64)>> {
        let objects = self.objects_dir();
        let prefixes: Vec<PathBuf> = match fs::read_dir(&objects) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).ctx(format!("reading {}", objects.display())),
        };
        let lists: Vec<Vec<(ObjectId, u64)>> = prefixes
            .par_iter()
            .map(|dir| {
                let prefix = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let mut found = Vec::new();
                for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                    let name = entry.file_name();
                    let hex = format!("{prefix}{}", name.to_string_lossy());
                    if let Some(id) = ObjectId::from_hex(&hex) {
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        found.push((id, size));
                    }
                }
                found
            })
            .collect();
        Ok(lists.into_iter().flatten().collect())
    }

    /// The identifiers of every stored object.
    pub(crate) fn object_index(&self) -> Result<HashSet<ObjectId>> {
        Ok(self.list_objects()?.into_iter().map(|(id, _)| id).collect())
    }

    /// Deletes one stored object. Returns its size on disk.
    pub(crate) fn remove_object(&self, id: &ObjectId) -> Result<u64> {
        let path = self.object_path(id);
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        fs::remove_file(&path).ctx(format!("removing {}", path.display()))?;
        Ok(size)
    }

    /// Removes files left behind by interrupted writes. Call only while
    /// holding the lock. Returns the bytes freed.
    pub(crate) fn clear_tmp(&self) -> u64 {
        let mut freed = 0;
        for entry in fs::read_dir(self.tmp_dir()).into_iter().flatten().flatten() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if fs::remove_file(entry.path()).is_ok() {
                freed += size;
            }
        }
        freed
    }

    // ── Snapshots ────────────────────────────────────────────────────

    /// Identifiers of all snapshots, oldest first.
    pub fn snapshot_ids(&self) -> Result<Vec<String>> {
        let dir = self.snapshots_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).ctx(format!("reading {}", dir.display())),
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let is_manifest = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(MANIFEST_EXT));
                let id = path.file_stem()?.to_str()?.to_owned();
                (is_manifest && is_snapshot_id(&id)).then_some(id)
            })
            .collect();
        ids.sort_by_key(|id| snapshot_order(id));
        Ok(ids)
    }

    /// An identifier that no snapshot uses yet, based on the current time.
    pub(crate) fn new_snapshot_id(&self) -> (String, i64) {
        let now = chrono::Utc::now();
        let base = now.format("%Y%m%d-%H%M%S").to_string();
        let mut id = base.clone();
        let mut n = 2;
        while self.manifest_path(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        (id, now.timestamp())
    }

    /// Reads the full record of one snapshot.
    pub fn load_manifest(&self, id: &str) -> Result<Manifest> {
        if !is_snapshot_id(id) {
            return Err(Error::Invalid(format!("{id:?} is not a snapshot name")));
        }
        let path = self.manifest_path(id);
        let packed = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("snapshot {id}")));
            }
            Err(e) => return Err(e).ctx(format!("reading {}", path.display())),
        };
        let damaged = |why: &dyn std::fmt::Display| {
            Error::Invalid(format!("the record of snapshot {id} is damaged: {why}"))
        };
        let mut json = Vec::new();
        zstd::stream::read::Decoder::new(packed.as_slice())
            .and_then(|mut decoder| decoder.read_to_end(&mut json))
            .map_err(|e| damaged(&e))?;
        let manifest: Manifest = serde_json::from_slice(&json).map_err(|e| damaged(&e))?;
        if manifest.info.id != id {
            return Err(damaged(&"it belongs to another snapshot"));
        }
        manifest.validate()?;
        Ok(manifest)
    }

    /// Writes the record of a new snapshot. The snapshot exists once this
    /// returns; the summary beside it is only a convenience for listing.
    pub(crate) fn write_manifest(&self, manifest: &Manifest) -> Result<()> {
        debug_assert_eq!(manifest.format, MANIFEST_FORMAT);
        let json = serde_json::to_vec(manifest)?;
        let packed = zstd::bulk::compress(&json, 9).ctx("compressing the snapshot record")?;
        // `bulk::compress` output is a regular zstd frame the stream decoder reads.
        self.write_atomic(&self.manifest_path(&manifest.info.id), &packed)?;
        self.write_summary(&manifest.info);
        Ok(())
    }

    fn write_summary(&self, info: &SnapshotInfo) {
        let result = serde_json::to_vec_pretty(info)
            .map_err(Error::from)
            .and_then(|json| self.write_atomic(&self.summary_path(&info.id), &json));
        if let Err(e) = result {
            log::warn!("could not write the summary of snapshot {}: {e}", info.id);
        }
    }

    /// Summaries of all snapshots, newest first. A snapshot whose record
    /// cannot be read is listed by name with its label saying so.
    pub fn list_snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        let mut list: Vec<SnapshotInfo> = self
            .snapshot_ids()?
            .into_iter()
            .map(|id| self.snapshot_info(&id))
            .collect();
        list.reverse();
        Ok(list)
    }

    fn snapshot_info(&self, id: &str) -> SnapshotInfo {
        let summary = fs::read(self.summary_path(id))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SnapshotInfo>(&bytes).ok())
            .filter(|info| info.id == id);
        if let Some(info) = summary {
            return info;
        }
        match self.load_manifest(id) {
            Ok(manifest) => {
                self.write_summary(&manifest.info);
                manifest.info
            }
            Err(e) => {
                log::warn!("snapshot {id} cannot be read: {e}");
                SnapshotInfo {
                    id: id.to_owned(),
                    label: "Damaged: this snapshot's record cannot be read".to_owned(),
                    ..SnapshotInfo::default()
                }
            }
        }
    }

    /// The snapshots a new run compares against: for each of the `sources`
    /// the newest readable snapshot that contains it, newest first. Only the
    /// most recent few snapshots are looked at.
    pub(crate) fn base_manifests(&self, sources: &[&str]) -> Vec<Manifest> {
        const LOOK_BACK: usize = 8;
        let mut missing: HashSet<String> = sources.iter().map(|s| s.to_lowercase()).collect();
        let mut bases = Vec::new();
        let ids = self.snapshot_ids().unwrap_or_default();
        for id in ids.iter().rev().take(LOOK_BACK) {
            if missing.is_empty() {
                break;
            }
            let manifest = match self.load_manifest(id) {
                Ok(manifest) => manifest,
                Err(e) => {
                    log::warn!("ignoring snapshot {id} as a base for the next one: {e}");
                    continue;
                }
            };
            let before = missing.len();
            for root in &manifest.roots {
                missing.remove(&root.path.to_lowercase());
            }
            if missing.len() < before {
                bases.push(manifest);
            }
        }
        bases
    }

    /// The most recent snapshot that can be read, if any.
    #[cfg(test)]
    pub(crate) fn latest_manifest(&self) -> Option<Manifest> {
        let ids = self.snapshot_ids().ok()?;
        ids.iter()
            .rev()
            .find_map(|id| match self.load_manifest(id) {
                Ok(manifest) => Some(manifest),
                Err(e) => {
                    log::warn!("ignoring snapshot {id} as a base for the next one: {e}");
                    None
                }
            })
    }

    /// Removes the record and summary of a snapshot. Its stored data stays
    /// until the next clean-up.
    pub(crate) fn remove_snapshot_files(&self, id: &str) -> Result<()> {
        if !is_snapshot_id(id) {
            return Err(Error::Invalid(format!("{id:?} is not a snapshot name")));
        }
        let manifest = self.manifest_path(id);
        match fs::remove_file(&manifest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("snapshot {id}")));
            }
            Err(e) => return Err(e).ctx(format!("removing {}", manifest.display())),
        }
        let _ = fs::remove_file(self.summary_path(id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_ids_are_recognised() {
        assert!(is_snapshot_id("20261113-184502"));
        assert!(is_snapshot_id("20261113-184502-2"));
        assert!(is_snapshot_id("20261113-184502-17"));
        for bad in [
            "",
            "latest",
            "20261113-18450",
            "20261113_184502",
            "20261113-184502-",
            "20261113-184502-x",
            r"..\..\20261113-184502",
            "20261113-184502.zmf",
        ] {
            assert!(!is_snapshot_id(bad), "{bad:?}");
        }

        let mut ids = vec![
            "20261113-184503",
            "20261113-184502-10",
            "20261113-184502-2",
            "20261113-184502",
        ];
        ids.sort_by_key(|id| snapshot_order(id));
        assert_eq!(
            ids,
            [
                "20261113-184502",
                "20261113-184502-2",
                "20261113-184502-10",
                "20261113-184503"
            ]
        );
    }

    #[test]
    fn a_repository_is_created_inside_the_destination() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!Repository::exists(tmp.path()));
        assert!(matches!(
            Repository::open(tmp.path()),
            Err(Error::NotFound(_))
        ));
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        assert_eq!(repo.root(), tmp.path().join(REPO_DIR_NAME));
        assert_eq!(repo.chunk_size(), CHUNK_SIZE);
        assert!(Repository::exists(tmp.path()));
        // Pointing at the repository folder itself opens the same repository.
        let direct = Repository::open(&tmp.path().join(REPO_DIR_NAME)).unwrap();
        assert_eq!(direct.root(), repo.root());
        // A second "create" keeps the settings of the first.
        let again = Repository::open_or_create_with(tmp.path(), 123).unwrap();
        assert_eq!(again.chunk_size(), CHUNK_SIZE);
    }

    #[test]
    fn objects_round_trip_and_are_stored_once() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        let text = b"the same line over and over. ".repeat(500);
        let id = ObjectId::of(&text);
        let stored = repo.put_object(&id, &text, 3).unwrap();
        assert!(stored > 0 && stored < text.len() as u64, "text compresses");
        assert_eq!(repo.put_object(&id, &text, 3).unwrap(), 0, "stored once");
        assert_eq!(repo.read_object(&id).unwrap(), text);
        assert_eq!(
            repo.object_state(&id),
            ObjectState::Intact(text.len() as u64)
        );

        // Data that does not compress is kept as it is.
        let noise: Vec<u8> = (0..4096u32)
            .flat_map(|i| blake3::hash(&i.to_le_bytes()).as_bytes()[..4].to_vec())
            .collect();
        let noise_id = ObjectId::of(&noise);
        let stored = repo.put_object(&noise_id, &noise, 3).unwrap();
        assert_eq!(stored, (noise.len() + OBJECT_HEADER) as u64);
        assert_eq!(repo.read_object(&noise_id).unwrap(), noise);

        let empty_id = ObjectId::of(b"");
        repo.put_object(&empty_id, b"", 3).unwrap();
        assert_eq!(repo.read_object(&empty_id).unwrap(), b"");

        let mut listed = repo.object_index().unwrap().into_iter().collect::<Vec<_>>();
        listed.sort();
        let mut expected = vec![id, noise_id, empty_id];
        expected.sort();
        assert_eq!(listed, expected);
        assert_eq!(fs::read_dir(repo.tmp_dir()).unwrap().count(), 0);
    }

    #[test]
    fn damage_to_an_object_is_noticed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        let data = b"precious bytes ".repeat(100);
        let id = ObjectId::of(&data);
        repo.put_object(&id, &data, 3).unwrap();
        let path = repo.object_path(&id);

        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(repo.object_state(&id), ObjectState::Damaged);
        assert!(matches!(repo.read_object(&id), Err(Error::Invalid(_))));

        fs::write(&path, b"short").unwrap();
        assert_eq!(repo.object_state(&id), ObjectState::Damaged);

        fs::remove_file(&path).unwrap();
        assert_eq!(repo.object_state(&id), ObjectState::Missing);
        assert!(matches!(repo.read_object(&id), Err(Error::NotFound(_))));
    }

    #[test]
    fn only_one_writer_holds_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        let first = repo.lock().unwrap();
        assert!(repo.lock().is_err(), "a second writer is refused");
        drop(first);
        assert!(repo.lock().is_ok(), "the lock is free again");
    }

    #[test]
    fn a_newer_repository_format_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        fs::write(
            repo.root().join(CONFIG_FILE),
            r#"{"format": 99, "created": 0, "chunk_size": 4194304}"#,
        )
        .unwrap();
        assert!(matches!(
            Repository::open(tmp.path()),
            Err(Error::Invalid(_))
        ));
        fs::write(repo.root().join(CONFIG_FILE), "not json").unwrap();
        assert!(matches!(
            Repository::open(tmp.path()),
            Err(Error::Invalid(_))
        ));
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    use crate::manifest::{Entry, EntryKind, RootKind, SourceRoot};

    fn manifest(id: &str, path: &str) -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            info: SnapshotInfo {
                id: id.to_owned(),
                created: 1_800_000_000,
                files: 1,
                ..SnapshotInfo::default()
            },
            chunk_size: 1024,
            roots: vec![SourceRoot {
                path: r"C:\Data".into(),
                name: "Data".into(),
                kind: RootKind::Folder,
            }],
            entries: vec![Entry {
                root: 0,
                path: path.to_owned(),
                kind: EntryKind::File,
                size: 0,
                modified: 0,
                created: 0,
                attributes: 0,
                chunks: Vec::new(),
            }],
            skipped: Vec::new(),
        }
    }

    #[test]
    fn records_round_trip_and_list_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        let older = manifest("20260101-120000", "a.txt");
        let newer = manifest("20260102-120000", r"sub\b.txt");
        repo.write_manifest(&older).unwrap();
        repo.write_manifest(&newer).unwrap();

        assert_eq!(repo.load_manifest("20260101-120000").unwrap(), older);
        assert_eq!(
            repo.snapshot_ids().unwrap(),
            ["20260101-120000", "20260102-120000"]
        );
        let listed = repo.list_snapshots().unwrap();
        assert_eq!(listed[0], newer.info);
        assert_eq!(listed[1], older.info);
        assert_eq!(repo.latest_manifest().unwrap(), newer);
        assert!(matches!(
            repo.load_manifest("20250101-000000"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            repo.load_manifest(r"..\zest-backup"),
            Err(Error::Invalid(_))
        ));
        // Files that are not snapshot records are not listed.
        fs::write(repo.snapshots_dir().join("notes.zmf"), b"x").unwrap();
        fs::write(repo.snapshots_dir().join("20260103-120000.bak"), b"x").unwrap();
        assert_eq!(repo.snapshot_ids().unwrap().len(), 2);
    }

    #[test]
    fn a_record_that_would_write_outside_its_target_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        let tampered = manifest("20260101-120000", r"..\..\Windows\evil.dll");
        repo.write_manifest(&tampered).unwrap();
        assert!(matches!(
            repo.load_manifest("20260101-120000"),
            Err(Error::Invalid(_))
        ));
        assert!(repo.latest_manifest().is_none(), "never used as a base");
    }

    #[test]
    fn a_record_filed_under_the_wrong_name_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        repo.write_manifest(&manifest("20260101-120000", "a.txt"))
            .unwrap();
        fs::copy(
            repo.manifest_path("20260101-120000"),
            repo.manifest_path("20260505-120000"),
        )
        .unwrap();
        assert!(matches!(
            repo.load_manifest("20260505-120000"),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn new_identifiers_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Repository::open_or_create(tmp.path()).unwrap();
        let (first, created) = repo.new_snapshot_id();
        assert!(is_snapshot_id(&first));
        assert!(created > 1_700_000_000);
        repo.write_manifest(&manifest(&first, "a.txt")).unwrap();
        // Force a clash by claiming whatever comes next for the same second.
        let (second, _) = repo.new_snapshot_id();
        assert!(is_snapshot_id(&second));
        assert_ne!(first, second);
    }
}
