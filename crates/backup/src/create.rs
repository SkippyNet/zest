//! Taking a snapshot: find what is there, store what is new, write the record.

use crate::exclude::Excludes;
use crate::manifest::{
    Entry, EntryKind, MANIFEST_FORMAT, Manifest, ObjectId, RootKind, SkipReason, Skipped,
    SnapshotInfo, SourceRoot,
};
use crate::repo::Repository;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zest_core::{Error, Progress, Result, safety, walk};

const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
/// Attributes worth restoring: read-only, hidden, system.
pub(crate) const KEPT_ATTRIBUTES: u32 = 0x1 | 0x2 | 0x4;

/// What to back up and how.
#[derive(Clone, Debug)]
pub struct BackupOptions {
    /// Folders and files to back up.
    pub sources: Vec<PathBuf>,
    /// Patterns for what to leave out, see [`crate::exclude`].
    pub excludes: Vec<String>,
    /// Optional note stored with the snapshot.
    pub label: String,
    /// zstd level; 3 is fast and compresses well.
    pub compression_level: i32,
    /// Read every file again instead of trusting size and date to tell
    /// which ones changed. Slower; catches files altered without a new date.
    pub reread_all: bool,
}

impl Default for BackupOptions {
    fn default() -> Self {
        BackupOptions {
            sources: Vec::new(),
            excludes: Vec::new(),
            label: String::new(),
            compression_level: 3,
            reread_all: false,
        }
    }
}

/// Result of a finished backup run.
#[derive(Clone, Debug)]
pub struct BackupReport {
    /// The snapshot that was written.
    pub snapshot: SnapshotInfo,
    /// Files that were read because they were new or had changed.
    pub read_files: u64,
    /// What was left out and why.
    pub skipped: Vec<Skipped>,
    /// How long the run took.
    pub elapsed: Duration,
}

/// A source after validation.
struct PlannedRoot {
    root: SourceRoot,
    abs: PathBuf,
}

/// A folder or file found while scanning.
struct Planned {
    root: u32,
    rel: String,
    abs: PathBuf,
    kind: EntryKind,
    size: u64,
    modified: u64,
    created: u64,
    attributes: u32,
}

/// Canonical lower-case form of an existing path, for containment checks.
/// Resolves junctions and short names so two spellings of one folder agree.
fn comparable(path: &Path) -> Option<String> {
    let resolved = fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .ok()?;
    safety::normalize(&resolved)
}

/// True when `inner` is `outer` or lies below it.
fn is_within(inner: &str, outer: &str) -> bool {
    inner == outer
        || (inner.len() > outer.len()
            && inner.starts_with(outer)
            && (outer.ends_with('\\') || inner.as_bytes()[outer.len()] == b'\\'))
}

fn root_name(path: &Path) -> String {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        return name.to_owned();
    }
    // A drive root such as `D:\`.
    let text = path.to_string_lossy();
    match text.chars().next().filter(char::is_ascii_alphabetic) {
        Some(letter) => format!("Drive {}", letter.to_ascii_uppercase()),
        None => "Backup".to_owned(),
    }
}

/// Checks the sources and gives each a unique name inside the snapshot.
fn plan_roots(repo: &Repository, sources: &[PathBuf]) -> Result<Vec<PlannedRoot>> {
    if sources.is_empty() {
        return Err(Error::Invalid(
            "choose at least one folder to back up".into(),
        ));
    }
    let repo_root = comparable(repo.root()).ok_or_else(|| {
        Error::Invalid(format!(
            "{} cannot be used as a backup destination",
            repo.root().display()
        ))
    })?;

    // (comparable form, absolute path, kind)
    let mut checked: Vec<(String, PathBuf, RootKind)> = Vec::new();
    for source in sources {
        let abs = std::path::absolute(source)
            .map_err(|_| Error::Invalid(format!("{} is not a usable path", source.display())))?;
        let meta = match fs::symlink_metadata(&abs) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("{} does not exist", abs.display())));
            }
            Err(e) => {
                return Err(Error::Io {
                    context: format!("looking at {}", abs.display()),
                    source: e,
                });
            }
        };
        if walk::is_link(&meta) {
            return Err(Error::Invalid(format!(
                "{} is a link to another location. Choose the real folder instead.",
                abs.display()
            )));
        }
        let key = comparable(&abs)
            .ok_or_else(|| Error::Invalid(format!("{} is not a usable path", abs.display())))?;
        if is_within(&repo_root, &key) {
            return Err(Error::Invalid(format!(
                "The backup destination is inside {}. Choose a destination outside the folders being backed up.",
                abs.display()
            )));
        }
        if is_within(&key, &repo_root) {
            return Err(Error::Invalid(format!(
                "{} is inside the backup destination and cannot be backed up into it.",
                abs.display()
            )));
        }
        let kind = if meta.is_dir() {
            RootKind::Folder
        } else {
            RootKind::File
        };
        checked.push((key, abs, kind));
    }

    // A source inside another source is already covered by the outer one.
    let keys: Vec<String> = checked.iter().map(|(key, _, _)| key.clone()).collect();
    let mut roots: Vec<PlannedRoot> = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for (index, (key, abs, kind)) in checked.into_iter().enumerate() {
        let covered = keys.iter().enumerate().any(|(other, outer)| {
            other != index && is_within(&key, outer) && (key != *outer || other < index)
        });
        if covered {
            continue;
        }
        let base = root_name(&abs);
        let mut name = base.clone();
        let mut n = 2;
        while !used_names.insert(name.to_lowercase()) {
            name = format!("{base} ({n})");
            n += 1;
        }
        roots.push(PlannedRoot {
            root: SourceRoot {
                path: abs.to_string_lossy().into_owned(),
                name,
                kind,
            },
            abs,
        });
    }
    Ok(roots)
}

fn planned(root: u32, rel: String, abs: PathBuf, kind: EntryKind, meta: &Metadata) -> Planned {
    Planned {
        root,
        rel,
        abs,
        kind,
        size: if kind == EntryKind::File {
            meta.file_size()
        } else {
            0
        },
        modified: meta.last_write_time(),
        created: meta.creation_time(),
        attributes: meta.file_attributes() & KEPT_ATTRIBUTES,
    }
}

fn skip(path: &Path, reason: SkipReason, detail: impl Into<String>) -> Skipped {
    Skipped {
        path: path.to_string_lossy().into_owned(),
        reason,
        detail: detail.into(),
    }
}

/// Lists everything below one source without following links.
fn scan_root(
    index: u32,
    root: &PlannedRoot,
    excludes: &Excludes,
    progress: &Progress,
    found: &mut Vec<Planned>,
    skipped: &mut Vec<Skipped>,
) -> Result<()> {
    if root.root.kind == RootKind::File {
        match fs::symlink_metadata(&root.abs) {
            Ok(meta) if walk::is_cloud_placeholder(&meta) => {
                skipped.push(skip(&root.abs, SkipReason::CloudOnly, ""));
            }
            Ok(meta) => found.push(planned(
                index,
                String::new(),
                root.abs.clone(),
                EntryKind::File,
                &meta,
            )),
            Err(e) => skipped.push(skip(&root.abs, SkipReason::Unreadable, e.to_string())),
        }
        return Ok(());
    }

    let mut stack: Vec<(PathBuf, String)> = vec![(root.abs.clone(), String::new())];
    while let Some((dir, rel_dir)) = stack.pop() {
        progress.check()?;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                skipped.push(skip(&dir, SkipReason::Unreadable, e.to_string()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    skipped.push(skip(&dir, SkipReason::Unreadable, e.to_string()));
                    break;
                }
            };
            let path = entry.path();
            let os_name = entry.file_name();
            let Some(name) = os_name.to_str() else {
                skipped.push(skip(&path, SkipReason::BadName, ""));
                continue;
            };
            if excludes.matches(&path, name) {
                continue;
            }
            // On Windows this comes with the directory listing: no extra call.
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(e) => {
                    skipped.push(skip(&path, SkipReason::Unreadable, e.to_string()));
                    continue;
                }
            };
            let rel = if rel_dir.is_empty() {
                name.to_owned()
            } else {
                format!("{rel_dir}\\{name}")
            };
            if walk::is_link(&meta) {
                skipped.push(skip(&path, SkipReason::Link, ""));
            } else if meta.is_dir() {
                found.push(planned(
                    index,
                    rel.clone(),
                    path.clone(),
                    EntryKind::Dir,
                    &meta,
                ));
                stack.push((path, rel));
            } else if walk::is_cloud_placeholder(&meta) {
                skipped.push(skip(&path, SkipReason::CloudOnly, ""));
            } else {
                found.push(planned(index, rel, path, EntryKind::File, &meta));
            }
        }
    }
    Ok(())
}

/// Fills `buffer` from `file`, stopping early only at the end of the file.
fn read_full(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Counters shared by the worker threads of one run.
#[derive(Default)]
struct Counters {
    new_bytes: AtomicU64,
    stored_bytes: AtomicU64,
    read_files: AtomicU64,
    reused_files: AtomicU64,
}

/// What happened to one planned item.
enum Outcome {
    Entry(Entry),
    Skipped(Skipped),
}

struct Run<'a> {
    repo: &'a Repository,
    level: i32,
    known: RwLock<HashSet<ObjectId>>,
    counters: Counters,
    progress: &'a Progress,
}

impl Run<'_> {
    fn has_object(&self, id: &ObjectId) -> bool {
        self.known
            .read()
            .map(|set| set.contains(id))
            .unwrap_or(false)
    }

    /// Marks an object as taken care of. Returns true for the one caller
    /// that has to store it; everyone else finds it claimed. This keeps two
    /// threads that meet the same chunk at the same moment from both storing
    /// (and both counting) it.
    fn claim(&self, id: &ObjectId) -> bool {
        if self.has_object(id) {
            return false;
        }
        self.known
            .write()
            .map(|mut set| set.insert(*id))
            .unwrap_or(true)
    }

    /// Reads a file chunk by chunk and stores the chunks that are new.
    /// Returns the chunk list and the number of bytes actually read.
    fn store_file(&self, path: &Path, size_hint: u64) -> Result<(Vec<ObjectId>, u64)> {
        let io = |source: std::io::Error| Error::Io {
            context: format!("reading {}", path.display()),
            source,
        };
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
            .open(path)
            .map_err(io)?;
        let chunk_size = self.repo.chunk_size();
        let mut buffer = vec![0u8; chunk_size.min(size_hint.max(1) as usize)];
        let mut chunks = Vec::new();
        let mut total = 0u64;
        loop {
            self.progress.check()?;
            let read = read_full(&mut file, &mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            let data = &buffer[..read];
            let id = ObjectId::of(data);
            if self.claim(&id) {
                let stored = self.repo.put_object(&id, data, self.level)?;
                if stored > 0 {
                    self.counters
                        .new_bytes
                        .fetch_add(read as u64, Ordering::Relaxed);
                    self.counters
                        .stored_bytes
                        .fetch_add(stored, Ordering::Relaxed);
                }
            }
            chunks.push(id);
            total += read as u64;
            self.progress.inc(read as u64);
            self.progress.add_bytes(read as u64);
            if read < buffer.len() {
                break;
            }
            // The file turned out larger than the listing said: grow to full chunks.
            if buffer.len() < chunk_size {
                buffer = vec![0u8; chunk_size];
            }
        }
        Ok((chunks, total))
    }

    fn process(&self, item: &Planned, previous: Option<&Entry>) -> Result<Outcome> {
        let mut entry = Entry {
            root: item.root,
            path: item.rel.clone(),
            kind: item.kind,
            size: item.size,
            modified: item.modified,
            created: item.created,
            attributes: item.attributes,
            chunks: Vec::new(),
        };
        if item.kind == EntryKind::Dir {
            return Ok(Outcome::Entry(entry));
        }
        self.progress.check()?;

        // Unchanged since the last snapshot, and its data is still stored?
        if let Some(old) = previous
            && old.kind == EntryKind::File
            && old.size == item.size
            && old.modified == item.modified
            && old.chunks.iter().all(|id| self.has_object(id))
        {
            entry.chunks = old.chunks.clone();
            self.counters.reused_files.fetch_add(1, Ordering::Relaxed);
            self.progress.inc(item.size);
            return Ok(Outcome::Entry(entry));
        }

        match self.store_file(&item.abs, item.size) {
            Ok((chunks, read)) => {
                entry.chunks = chunks;
                // A file that changed while it was read is recorded as read.
                entry.size = read;
                self.counters.read_files.fetch_add(1, Ordering::Relaxed);
                Ok(Outcome::Entry(entry))
            }
            Err(Error::Io { source, .. }) => {
                // In use or not permitted: note it and carry on.
                self.progress.inc(item.size);
                Ok(Outcome::Skipped(skip(
                    &item.abs,
                    SkipReason::Unreadable,
                    source.to_string(),
                )))
            }
            Err(other) => Err(other),
        }
    }
}

fn previous_key(root_path: &str, rel: &str) -> String {
    format!("{}\u{0}{}", root_path.to_lowercase(), rel.to_lowercase())
}

/// Takes a snapshot of `options.sources` into `repo`.
///
/// Files that have the size and date they had in the latest snapshot are not
/// read again. Everything else is read, split into chunks, and each chunk the
/// repository does not hold yet is compressed and stored. Links are never
/// followed, online-only files are not downloaded, and a file that cannot be
/// read is listed in the report instead of stopping the run.
///
/// `progress` counts bytes. Cancelling leaves the repository as it was: no
/// snapshot is recorded, and what was stored so far is reused by the next
/// run or removed by the next clean-up.
pub fn create_snapshot(
    repo: &Repository,
    options: &BackupOptions,
    progress: &Progress,
) -> Result<BackupReport> {
    let started = Instant::now();
    let roots = plan_roots(repo, &options.sources)?;
    let _lock = repo.lock()?;

    progress.set_message("Looking for files");
    let excludes = Excludes::new(&options.excludes);
    let mut items: Vec<Planned> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        scan_root(
            index as u32,
            root,
            &excludes,
            progress,
            &mut items,
            &mut skipped,
        )?;
    }
    progress.set_total(items.iter().map(|i| i.size).sum());

    // The newest earlier snapshot of each source tells which files changed.
    let bases = if options.reread_all {
        Vec::new()
    } else {
        let wanted: Vec<&str> = roots.iter().map(|r| r.root.path.as_str()).collect();
        repo.base_manifests(&wanted)
    };
    let mut previous_entries: HashMap<String, &Entry> = HashMap::new();
    for manifest in &bases {
        for entry in &manifest.entries {
            if let Some(root) = manifest.roots.get(entry.root as usize) {
                // Newer snapshots come first and win.
                previous_entries
                    .entry(previous_key(&root.path, &entry.path))
                    .or_insert(entry);
            }
        }
    }

    progress.set_message("Copying what is new");
    let run = Run {
        repo,
        level: options.compression_level,
        known: RwLock::new(repo.object_index()?),
        counters: Counters::default(),
        progress,
    };
    let outcomes: Vec<Result<Outcome>> = items
        .par_iter()
        .map(|item| {
            let key = previous_key(&roots[item.root as usize].root.path, &item.rel);
            run.process(item, previous_entries.get(&key).copied())
        })
        .collect();

    let mut entries = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome? {
            Outcome::Entry(entry) => entries.push(entry),
            Outcome::Skipped(item) => skipped.push(item),
        }
    }
    progress.check()?;
    entries.sort_by(|a, b| (a.root, &a.path).cmp(&(b.root, &b.path)));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    progress.set_message("Finishing");
    let (id, created) = repo.new_snapshot_id();
    let files = entries.iter().filter(|e| e.kind == EntryKind::File);
    let info = SnapshotInfo {
        id,
        created,
        label: options.label.trim().to_owned(),
        sources: roots.iter().map(|r| r.root.path.clone()).collect(),
        files: files.clone().count() as u64,
        dirs: entries.iter().filter(|e| e.kind == EntryKind::Dir).count() as u64,
        bytes: files.map(|e| e.size).sum(),
        new_bytes: run.counters.new_bytes.load(Ordering::Relaxed),
        stored_bytes: run.counters.stored_bytes.load(Ordering::Relaxed),
        reused_files: run.counters.reused_files.load(Ordering::Relaxed),
        skipped: skipped.len() as u64,
    };
    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        info: info.clone(),
        chunk_size: repo.chunk_size() as u64,
        roots: roots.into_iter().map(|r| r.root).collect(),
        entries,
        skipped: skipped.clone(),
    };
    repo.write_manifest(&manifest)?;
    log::info!(
        "snapshot {}: {} files, {} bytes, {} new bytes, {} skipped",
        info.id,
        info.files,
        info.bytes,
        info.new_bytes,
        info.skipped
    );
    Ok(BackupReport {
        snapshot: info,
        read_files: run.counters.read_files.load(Ordering::Relaxed),
        skipped,
        elapsed: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_compares_whole_path_parts() {
        assert!(is_within(r"c:\data\sub", r"c:\data"));
        assert!(is_within(r"c:\data", r"c:\data"));
        assert!(!is_within(r"c:\database", r"c:\data"));
        assert!(!is_within(r"c:\data", r"c:\data\sub"));
        assert!(is_within(r"c:\data", r"c:\"));
        assert!(!is_within(r"d:\data", r"c:\"));
    }

    #[test]
    fn roots_are_named_after_their_last_part() {
        assert_eq!(root_name(Path::new(r"C:\Users\me\Documents")), "Documents");
        assert_eq!(root_name(Path::new(r"d:\")), "Drive D");
        assert_eq!(root_name(Path::new(r"C:\notes.txt")), "notes.txt");
    }
}
