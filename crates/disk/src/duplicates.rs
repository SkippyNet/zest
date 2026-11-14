//! Duplicate file finder.
//!
//! # How files are compared
//! 1. All candidate files are indexed in parallel (links and cloud
//!    placeholders are skipped; nothing is opened yet).
//! 2. Files are grouped by the selected attributes (size, name, modified
//!    time). Only groups with at least two members continue.
//! 3. With content matching (the default) each remaining file is opened
//!    once: its identity (volume + file index) is read so several hard links
//!    to one file count as a single file, and the first and last 16 KiB are
//!    hashed. Files whose probe differs are dropped.
//! 4. What is left is hashed completely with BLAKE3. Only files with an
//!    identical full hash are reported.
//!
//! # Progress
//! While indexing: `done` = files seen, `total` = 0. While comparing:
//! `done`/`total` are bytes read / bytes to read (the total grows once when
//! full hashing starts) and `bytes` counts bytes read.

use crate::categories::ExtensionKey;
use crate::fastdir::{
    self, ATTR_HIDDEN, ATTR_READ_ONLY, ATTR_SYSTEM, RawEntry, filetime_to_system, path_key,
    system_to_filetime,
};
use crate::pool::{FxHashMap, FxHashSet, Shards, pool, walk_pool};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering as CmpOrdering;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
use zest_core::delete::{DeleteOptions, DeleteReport, delete_file, recycle};
use zest_core::{Error, Progress, Result, paths, walk};

/// Size of the blocks hashed from the start and the end of each candidate.
const PROBE_BLOCK: u64 = 16 * 1024;

/// Read buffer for full hashing.
const HASH_BUFFER: usize = 1024 * 1024;

/// Number of paths handed to the shell per Recycle Bin operation.
const RECYCLE_BATCH: usize = 48;

const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Default lower size limit for duplicate candidates (8 KiB).
///
/// Tiny files reclaim next to nothing, yet they are the ones that share
/// their size with thousands of unrelated files. Each of those has to be
/// opened to be compared, and the first time a file is opened the system's
/// real-time virus scanner inspects it – typically several milliseconds per
/// file – which makes scans of source trees and web caches crawl.
pub const DEFAULT_MIN_SIZE: u64 = 8 * 1024;

/// What to scan and what counts as a duplicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateOptions {
    /// Folders or drives to search. Nested roots are merged.
    pub roots: Vec<PathBuf>,
    /// Files must have identical contents (implies equal size). Default: on.
    pub match_content: bool,
    /// Files must have the same name (case-insensitive). Default: off.
    pub match_name: bool,
    /// Files must have the same size. Default: on.
    pub match_size: bool,
    /// Files must have the same modification time. Default: off.
    pub match_modified: bool,
    /// Ignore files smaller than this many bytes.
    /// Default: [`DEFAULT_MIN_SIZE`]; set to 1 to compare every file.
    pub min_size: u64,
    /// Ignore files larger than this many bytes. Default: no limit.
    pub max_size: Option<u64>,
    /// Ignore empty files. Default: on.
    pub ignore_zero_byte: bool,
    /// Ignore files and folders with the *system* attribute. Default: on.
    pub ignore_system: bool,
    /// Ignore files and folders with the *hidden* attribute. Default: on.
    pub ignore_hidden: bool,
    /// Ignore read-only files. Default: off.
    pub ignore_read_only: bool,
    /// Never descend into the Windows, Program Files and ProgramData
    /// folders, Recycle Bins or System Volume Information. Default: on.
    pub skip_system_folders: bool,
    /// Additional folders to skip.
    pub exclude: Vec<PathBuf>,
    /// Only consider these extensions (lower-case, without the dot).
    /// Empty means all files.
    pub extensions: Vec<String>,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        DuplicateOptions {
            roots: Vec::new(),
            match_content: true,
            match_name: false,
            match_size: true,
            match_modified: false,
            min_size: DEFAULT_MIN_SIZE,
            max_size: None,
            ignore_zero_byte: true,
            ignore_system: true,
            ignore_hidden: true,
            ignore_read_only: false,
            skip_system_folders: true,
            exclude: Vec::new(),
            extensions: Vec::new(),
        }
    }
}

impl DuplicateOptions {
    /// Default options for the given roots.
    pub fn for_roots(roots: Vec<PathBuf>) -> Self {
        DuplicateOptions {
            roots,
            ..Self::default()
        }
    }
}

/// One member of a duplicate group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DupFile {
    /// Full path.
    pub path: PathBuf,
    /// Size in bytes at scan time.
    pub size: u64,
    /// Modification time at scan time.
    pub modified: Option<SystemTime>,
}

/// A set of files that are duplicates of each other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// Size of the largest member. With size or content matching every
    /// member has exactly this size.
    pub size: u64,
    /// BLAKE3 hash of the contents (hex) when content matching was used.
    pub hash: Option<String>,
    /// The duplicate files, sorted by path. Always at least two.
    pub files: Vec<DupFile>,
}

impl DuplicateGroup {
    /// Bytes that would be freed by keeping only the largest member.
    pub fn wasted_bytes(&self) -> u64 {
        let total: u64 = self.files.iter().map(|f| f.size).sum();
        total.saturating_sub(self.size)
    }
}

/// The result of [`find_duplicates`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateReport {
    /// Duplicate groups, most wasted space first.
    pub groups: Vec<DuplicateGroup>,
    /// Files seen while indexing.
    pub files_scanned: u64,
    /// Files that passed the filters.
    pub candidates: u64,
    /// Bytes read for comparison.
    pub bytes_hashed: u64,
    /// Number of redundant files (group members minus one per group).
    pub duplicate_files: u64,
    /// Bytes that removing all redundant files would free.
    pub wasted_bytes: u64,
    /// Candidates that could not be opened or changed while being read.
    pub unreadable: u64,
    /// Extra hard links (or second paths) to a file already counted.
    pub hard_links_merged: u64,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
}

struct Candidate {
    dir: u32,
    name: Box<OsStr>,
    size: u64,
    modified: u64,
}

#[derive(Default)]
struct CollectShard {
    dirs: Vec<PathBuf>,
    candidates: Vec<Candidate>,
}

struct Collector<'a> {
    options: &'a DuplicateOptions,
    progress: &'a Progress,
    shards: Shards<CollectShard>,
    exclude: FxHashSet<String>,
    extensions: FxHashSet<String>,
    files_seen: AtomicU64,
}

impl Collector<'_> {
    fn accepts(&self, entry: &RawEntry<'_>) -> bool {
        let o = self.options;
        if entry.size == 0 && o.ignore_zero_byte {
            return false;
        }
        if entry.size < o.min_size || o.max_size.is_some_and(|max| entry.size > max) {
            return false;
        }
        if entry.is_cloud_placeholder() {
            return false;
        }
        let a = entry.attributes;
        if (o.ignore_hidden && a & ATTR_HIDDEN != 0)
            || (o.ignore_system && a & ATTR_SYSTEM != 0)
            || (o.ignore_read_only && a & ATTR_READ_ONLY != 0)
        {
            return false;
        }
        self.extensions.is_empty()
            || self
                .extensions
                .contains(ExtensionKey::from_name(entry.name).as_str())
    }

    fn enters(&self, entry: &RawEntry<'_>) -> bool {
        let o = self.options;
        if (o.ignore_hidden && entry.attributes & ATTR_HIDDEN != 0)
            || (o.ignore_system && entry.attributes & ATTR_SYSTEM != 0)
        {
            return false;
        }
        if o.skip_system_folders && matches!(entry.name.len(), 12 | 25) {
            let name = String::from_utf16_lossy(entry.name).to_lowercase();
            if name == "$recycle.bin" || name == "system volume information" {
                return false;
            }
        }
        true
    }
}

fn collect_dir<'s>(scope: &rayon::Scope<'s>, collector: &'s Collector<'s>, dir: PathBuf) {
    if collector.progress.is_cancelled() {
        return;
    }
    let mut subdirs: Vec<PathBuf> = Vec::new();
    let mut found: Vec<(Box<OsStr>, u64, u64)> = Vec::new();
    let mut seen = 0u64;
    let _ = fastdir::read_dir_raw(&dir, &mut |entry| {
        if entry.is_link() {
            return;
        }
        if entry.is_dir() {
            if collector.enters(entry) {
                subdirs.push(entry.path_in(&dir));
            }
            return;
        }
        seen += 1;
        if collector.accepts(entry) {
            found.push((
                entry.os_name().into_boxed_os_str(),
                entry.size,
                entry.modified,
            ));
        }
    });
    collector.files_seen.fetch_add(seen, Ordering::Relaxed);
    collector.progress.inc(seen);

    for child in subdirs {
        if !collector.exclude.is_empty() && collector.exclude.contains(&path_key(&child)) {
            continue;
        }
        scope.spawn(move |s| collect_dir(s, collector, child));
    }

    if !found.is_empty() {
        let mut shard = collector.shards.local();
        let index = shard.dirs.len() as u32;
        shard.dirs.push(dir);
        shard
            .candidates
            .extend(found.into_iter().map(|(name, size, modified)| Candidate {
                dir: index,
                name,
                size,
                modified,
            }));
    }
}

/// Identity of a file on disk: all hard links to a file share it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FileId {
    volume: u32,
    index: u64,
}

/// Reads identity and current size from an open handle.
fn identity(file: &File) -> Option<(FileId, u64)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is owned by `file` and valid for the call; `info`
    // is a correctly sized output structure.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some((
        FileId {
            volume: info.dwVolumeSerialNumber,
            index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        },
        (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
    ))
}

fn open_for_read(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

/// Opens a file without any access rights – enough to query its identity
/// even when another program holds it open exclusively.
fn open_for_query(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

struct Probe {
    id: FileId,
    digest: [u8; 32],
}

struct Hashing<'a> {
    progress: &'a Progress,
    bytes: AtomicU64,
    unreadable: AtomicU64,
}

impl Hashing<'_> {
    fn count(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
        self.progress.inc(n);
        self.progress.add_bytes(n);
    }

    fn fail<T>(&self) -> Option<T> {
        self.unreadable.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Reads identity plus a digest of the first and last block. For files
    /// of at most two blocks the digest covers the whole file and equals
    /// the full BLAKE3 hash.
    fn probe(&self, path: &Path, size: u64) -> Option<Probe> {
        if self.progress.is_cancelled() {
            return None;
        }
        let Ok(mut file) = open_for_read(path) else {
            return self.fail();
        };
        let Some((id, actual)) = identity(&file) else {
            return self.fail();
        };
        if actual != size {
            return self.fail();
        }
        let mut hasher = blake3::Hasher::new();
        let covered = probe_bytes(size);
        let mut buf = vec![0u8; covered.min(2 * PROBE_BLOCK) as usize];
        let ok = if size <= 2 * PROBE_BLOCK {
            file.read_exact(&mut buf).is_ok()
        } else {
            let (head, tail) = buf.split_at_mut(PROBE_BLOCK as usize);
            file.read_exact(head).is_ok()
                && file.seek(SeekFrom::Start(size - PROBE_BLOCK)).is_ok()
                && file.read_exact(tail).is_ok()
        };
        if !ok {
            return self.fail();
        }
        hasher.update(&buf);
        self.count(covered);
        Some(Probe {
            id,
            digest: *hasher.finalize().as_bytes(),
        })
    }

    /// Hashes the complete file.
    fn full(&self, path: &Path, size: u64) -> Option<[u8; 32]> {
        let Ok(mut file) = open_for_read(path) else {
            return self.fail();
        };
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; (size.min(HASH_BUFFER as u64) as usize).max(1)];
        let mut read_total = 0u64;
        loop {
            if self.progress.is_cancelled() {
                return None;
            }
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                    read_total += n as u64;
                    self.count(n as u64);
                    if read_total > size {
                        return self.fail();
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return self.fail(),
            }
        }
        if read_total != size {
            return self.fail();
        }
        Some(*hasher.finalize().as_bytes())
    }
}

/// Bytes the probe stage reads from a file of `size` bytes.
fn probe_bytes(size: u64) -> u64 {
    size.min(2 * PROBE_BLOCK)
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

struct Index {
    dirs: Vec<PathBuf>,
    candidates: Vec<Candidate>,
}

impl Index {
    fn path(&self, candidate: u32) -> PathBuf {
        let c = &self.candidates[candidate as usize];
        self.dirs[c.dir as usize].join(&*c.name)
    }

    fn size(&self, candidate: u32) -> u64 {
        self.candidates[candidate as usize].size
    }

    fn dup_file(&self, candidate: u32) -> DupFile {
        let c = &self.candidates[candidate as usize];
        DupFile {
            path: self.path(candidate),
            size: c.size,
            modified: filetime_to_system(c.modified),
        }
    }

    fn group(&self, members: &[u32], digest: Option<&[u8; 32]>) -> DuplicateGroup {
        let mut files: Vec<DupFile> = members.iter().map(|&m| self.dup_file(m)).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        DuplicateGroup {
            size: files.iter().map(|f| f.size).max().unwrap_or(0),
            hash: digest.map(hex),
            files,
        }
    }
}

pub(crate) fn system_folder_keys() -> Vec<String> {
    [
        paths::windows_dir(),
        paths::program_files(),
        paths::program_files_x86(),
        paths::program_data(),
    ]
    .iter()
    .map(|p| path_key(p))
    .collect()
}

/// Splits `0..len` into runs of equal elements according to `same`.
fn runs(len: usize, same: impl Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=len {
        if i == len || !same(i - 1, i) {
            out.push((start, i));
            start = i;
        }
    }
    out
}

/// Searches `options.roots` for duplicate files.
///
/// Returns [`Error::Cancelled`] when `progress` is cancelled,
/// [`Error::Invalid`] when no root or no matching criterion was given and
/// [`Error::NotFound`] when a root is not an existing folder.
pub fn find_duplicates(options: &DuplicateOptions, progress: &Progress) -> Result<DuplicateReport> {
    let started = Instant::now();
    if !(options.match_content
        || options.match_name
        || options.match_size
        || options.match_modified)
    {
        return Err(Error::Invalid(
            "choose at least one way to match duplicates".to_owned(),
        ));
    }
    let roots = fastdir::normalize_roots(&options.roots);
    if roots.is_empty() {
        return Err(Error::Invalid(
            "no folder was selected to search".to_owned(),
        ));
    }
    for root in &roots {
        if !root.is_dir() {
            return Err(Error::NotFound(root.display().to_string()));
        }
    }

    // ---- Stage 1: index candidates -------------------------------------
    progress.set_total(0);
    progress.set_done(0);
    progress.set_message("Indexing files");
    let mut exclude: FxHashSet<String> = options
        .exclude
        .iter()
        .map(|p| path_key(&fastdir::absolute(p)))
        .collect();
    if options.skip_system_folders {
        exclude.extend(system_folder_keys());
    }
    let collector = Collector {
        options,
        progress,
        shards: Shards::new(),
        exclude,
        extensions: options
            .extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect(),
        files_seen: AtomicU64::new(0),
    };
    // Listing folders runs on the small walk pool; the hashing further down
    // uses every core.
    walk_pool().install(|| {
        rayon::scope(|scope| {
            for root in &roots {
                let root = root.clone();
                let collector = &collector;
                scope.spawn(move |s| collect_dir(s, collector, root));
            }
        });
    });
    progress.check()?;

    let files_scanned = collector.files_seen.load(Ordering::Relaxed);
    let mut index = Index {
        dirs: Vec::new(),
        candidates: Vec::new(),
    };
    for shard in collector.shards.into_inner() {
        let base = index.dirs.len() as u32;
        index.dirs.extend(shard.dirs);
        index
            .candidates
            .extend(shard.candidates.into_iter().map(|mut c| {
                c.dir += base;
                c
            }));
    }
    let mut report = DuplicateReport {
        files_scanned,
        candidates: index.candidates.len() as u64,
        ..DuplicateReport::default()
    };

    // ---- Stage 2: group by the cheap attributes -------------------------
    let use_size = options.match_size || options.match_content;
    let lower_names: Vec<Box<str>> = if options.match_name {
        index
            .candidates
            .par_iter()
            .map(|c| c.name.to_string_lossy().to_lowercase().into_boxed_str())
            .collect()
    } else {
        Vec::new()
    };
    let compare = |a: u32, b: u32| -> CmpOrdering {
        let (ca, cb) = (&index.candidates[a as usize], &index.candidates[b as usize]);
        let mut ord = CmpOrdering::Equal;
        if use_size {
            // Largest first, so the most valuable groups are compared first.
            ord = cb.size.cmp(&ca.size);
        }
        if ord == CmpOrdering::Equal && options.match_modified {
            ord = ca.modified.cmp(&cb.modified);
        }
        if ord == CmpOrdering::Equal && options.match_name {
            ord = lower_names[a as usize].cmp(&lower_names[b as usize]);
        }
        ord
    };
    let mut order: Vec<u32> = (0..index.candidates.len() as u32).collect();
    pool().install(|| order.par_sort_unstable_by(|&a, &b| compare(a, b)));
    let mut groups: Vec<Vec<u32>> = runs(order.len(), |i, j| {
        compare(order[i], order[j]) == CmpOrdering::Equal
    })
    .into_iter()
    .filter(|(start, end)| end - start >= 2)
    .map(|(start, end)| order[start..end].to_vec())
    .collect();
    drop(order);
    drop(lower_names);
    progress.check()?;

    let hashing = Hashing {
        progress,
        bytes: AtomicU64::new(0),
        unreadable: AtomicU64::new(0),
    };
    let mut hard_links = 0u64;
    let mut result: Vec<DuplicateGroup> = Vec::new();

    if options.match_content {
        // Empty files are trivially identical; nothing to read.
        groups.retain(|members| {
            if index.size(members[0]) == 0 {
                result.push(index.group(members, Some(blake3::hash(b"").as_bytes())));
                false
            } else {
                true
            }
        });

        // ---- Stage 3: identity + first/last block ------------------------
        progress.set_message("Comparing files");
        progress.set_done(0);
        progress.set_total(
            groups
                .iter()
                .flatten()
                .map(|&m| probe_bytes(index.size(m)))
                .sum(),
        );
        let probes: Vec<Vec<Option<Probe>>> = pool().install(|| {
            groups
                .par_iter()
                .map(|members| {
                    members
                        .par_iter()
                        .map(|&m| hashing.probe(&index.path(m), index.size(m)))
                        .collect()
                })
                .collect()
        });
        progress.check()?;

        let mut pending: Vec<Vec<u32>> = Vec::new();
        for (members, probes) in groups.iter().zip(probes) {
            let size = index.size(members[0]);
            let mut seen: FxHashSet<FileId> = FxHashSet::default();
            let mut by_digest: FxHashMap<[u8; 32], Vec<u32>> = FxHashMap::default();
            for (&member, probe) in members.iter().zip(probes) {
                let Some(probe) = probe else { continue };
                if !seen.insert(probe.id) {
                    hard_links += 1;
                    continue;
                }
                by_digest.entry(probe.digest).or_default().push(member);
            }
            for (digest, same) in by_digest {
                if same.len() < 2 {
                    continue;
                }
                if size <= 2 * PROBE_BLOCK {
                    // The probe already covered the whole file.
                    result.push(index.group(&same, Some(&digest)));
                } else {
                    pending.push(same);
                }
            }
        }

        // ---- Stage 4: full hash ------------------------------------------
        progress.set_message("Verifying matches");
        progress.add_total(pending.iter().flatten().map(|&m| index.size(m)).sum());
        let hashes: Vec<Vec<Option<[u8; 32]>>> = pool().install(|| {
            pending
                .par_iter()
                .map(|members| {
                    members
                        .par_iter()
                        .map(|&m| hashing.full(&index.path(m), index.size(m)))
                        .collect()
                })
                .collect()
        });
        progress.check()?;
        for (members, hashes) in pending.iter().zip(hashes) {
            let mut by_digest: FxHashMap<[u8; 32], Vec<u32>> = FxHashMap::default();
            for (&member, digest) in members.iter().zip(hashes) {
                if let Some(digest) = digest {
                    by_digest.entry(digest).or_default().push(member);
                }
            }
            for (digest, same) in by_digest {
                if same.len() >= 2 {
                    result.push(index.group(&same, Some(&digest)));
                }
            }
        }
    } else {
        // Attribute matching only: still make sure two paths to the same
        // file (hard links) are not presented as duplicates of each other.
        progress.set_message("Checking file identities");
        progress.set_done(0);
        progress.set_total(groups.iter().map(|g| g.len() as u64).sum());
        let ids: Vec<Vec<Option<FileId>>> = pool().install(|| {
            groups
                .par_iter()
                .map(|members| {
                    members
                        .iter()
                        .map(|&m| {
                            progress.inc(1);
                            if progress.is_cancelled() {
                                return None;
                            }
                            let id = open_for_query(&index.path(m))
                                .ok()
                                .and_then(|f| identity(&f))
                                .map(|(id, _)| id);
                            if id.is_none() {
                                hashing.unreadable.fetch_add(1, Ordering::Relaxed);
                            }
                            id
                        })
                        .collect()
                })
                .collect()
        });
        progress.check()?;
        for (members, ids) in groups.iter().zip(ids) {
            let mut seen: FxHashSet<FileId> = FxHashSet::default();
            let mut unique: Vec<u32> = Vec::new();
            for (&member, id) in members.iter().zip(ids) {
                let Some(id) = id else { continue };
                if seen.insert(id) {
                    unique.push(member);
                } else {
                    hard_links += 1;
                }
            }
            if unique.len() >= 2 {
                result.push(index.group(&unique, None));
            }
        }
    }

    result.sort_by(|a, b| {
        b.wasted_bytes()
            .cmp(&a.wasted_bytes())
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });
    report.duplicate_files = result.iter().map(|g| g.files.len() as u64 - 1).sum();
    report.wasted_bytes = result.iter().map(DuplicateGroup::wasted_bytes).sum();
    report.bytes_hashed = hashing.bytes.load(Ordering::Relaxed);
    report.unreadable = hashing.unreadable.load(Ordering::Relaxed);
    report.hard_links_merged = hard_links;
    report.groups = result;
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    progress.set_message(String::new());
    Ok(report)
}

/// Which member of each duplicate group to keep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeepStrategy {
    /// Keep the most recently modified file.
    Newest,
    /// Keep the least recently modified file.
    Oldest,
    /// Keep the file with the shortest path.
    ShortestPath,
    /// Keep the file with the longest path.
    LongestPath,
    /// Keep every copy inside this folder and remove copies elsewhere.
    /// Groups without a copy inside the folder are left untouched.
    InFolder(PathBuf),
}

/// Applies `strategy` to every group and returns the paths to remove.
///
/// At least one file of every group is always kept. Ties are broken by path
/// order, so the result is deterministic.
pub fn select_for_removal(groups: &[DuplicateGroup], strategy: &KeepStrategy) -> Vec<PathBuf> {
    let mut remove = Vec::new();
    for group in groups {
        if group.files.len() < 2 {
            continue;
        }
        if let KeepStrategy::InFolder(folder) = strategy {
            let folder_key = path_key(&fastdir::absolute(folder));
            let inside: Vec<bool> = group
                .files
                .iter()
                .map(|f| fastdir::key_is_under(&path_key(&f.path), &folder_key))
                .collect();
            if inside.iter().any(|&i| i) {
                remove.extend(
                    group
                        .files
                        .iter()
                        .zip(&inside)
                        .filter(|&(_, &keep)| !keep)
                        .map(|(f, _)| f.path.clone()),
                );
            }
            continue;
        }
        let path_len = |f: &DupFile| f.path.as_os_str().len();
        let better = |candidate: &DupFile, best: &DupFile| -> bool {
            match strategy {
                // An unknown time never wins.
                KeepStrategy::Newest => match (candidate.modified, best.modified) {
                    (Some(c), Some(b)) => c > b,
                    (Some(_), None) => true,
                    _ => false,
                },
                KeepStrategy::Oldest => match (candidate.modified, best.modified) {
                    (Some(c), Some(b)) => c < b,
                    (Some(_), None) => true,
                    _ => false,
                },
                KeepStrategy::ShortestPath => path_len(candidate) < path_len(best),
                KeepStrategy::LongestPath => path_len(candidate) > path_len(best),
                KeepStrategy::InFolder(_) => false,
            }
        };
        let mut keep = 0;
        for (i, file) in group.files.iter().enumerate().skip(1) {
            if better(file, &group.files[keep]) {
                keep = i;
            }
        }
        remove.extend(
            group
                .files
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != keep)
                .map(|(_, f)| f.path.clone()),
        );
    }
    remove
}

/// The outcome of [`delete_duplicates`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DuplicateDeleteReport {
    /// What was deleted (or would be, in a dry run) and what failed.
    pub deleted: DeleteReport,
    /// Files that were requested for removal but kept because they were the
    /// last remaining verified copy of their group.
    pub kept_last_copy: Vec<PathBuf>,
    /// Files skipped because they changed (size or modification time) since
    /// the scan and may no longer be duplicates.
    pub changed: Vec<PathBuf>,
    /// Requested paths that are not members of any given group. They are
    /// never touched.
    pub not_duplicates: Vec<PathBuf>,
}

/// True when `file` still looks exactly like it did when it was scanned.
fn unchanged(file: &DupFile) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(&file.path) else {
        return false;
    };
    if !meta.is_file() || walk::is_link(&meta) || meta.len() != file.size {
        return false;
    }
    match (file.modified, meta.modified()) {
        (Some(recorded), Ok(current)) => {
            system_to_filetime(recorded) == system_to_filetime(current)
        }
        _ => true,
    }
}

/// Removes the selected duplicates.
///
/// `groups` must be groups from [`find_duplicates`] and `remove` the paths
/// chosen from them (for example via [`select_for_removal`]). Deletion goes
/// through `zest_core::delete`, so `options` decides between Recycle Bin
/// (recommended: [`DeleteOptions::recycle`]), permanent or secure deletion,
/// and supports dry runs.
///
/// Safety rules enforced here, whatever the caller selected:
/// * A path that is not a member of one of the groups is never touched.
/// * A file that changed since the scan is skipped.
/// * **At least one verified, unchanged copy of every group survives.** If
///   every copy was selected, the first one is kept.
///
/// # Progress
/// `total` = files to delete, `done` = files processed.
pub fn delete_duplicates(
    groups: &[DuplicateGroup],
    remove: &[PathBuf],
    options: &DeleteOptions,
    progress: &Progress,
) -> Result<DuplicateDeleteReport> {
    let mut report = DuplicateDeleteReport::default();
    let requested: FxHashMap<String, &PathBuf> = remove.iter().map(|p| (path_key(p), p)).collect();
    let mut matched: FxHashSet<String> = FxHashSet::default();
    let mut queue: Vec<&DupFile> = Vec::new();

    for group in groups {
        let mut selected: Vec<&DupFile> = Vec::new();
        let mut others: Vec<&DupFile> = Vec::new();
        for file in &group.files {
            let key = path_key(&file.path);
            if requested.contains_key(&key) {
                matched.insert(key);
                selected.push(file);
            } else {
                others.push(file);
            }
        }
        if selected.is_empty() {
            continue;
        }
        let mut has_survivor = others.iter().any(|f| unchanged(f));
        for file in selected {
            if !unchanged(file) {
                if file.path.exists() {
                    report.changed.push(file.path.clone());
                }
                continue;
            }
            if !has_survivor {
                report.kept_last_copy.push(file.path.clone());
                has_survivor = true;
                continue;
            }
            queue.push(file);
        }
    }
    for (key, path) in &requested {
        if !matched.contains(key) {
            report.not_duplicates.push((*path).clone());
        }
    }
    report.not_duplicates.sort();

    progress.set_done(0);
    progress.set_total(queue.len() as u64);
    progress.set_message("Removing duplicates");

    let delete_one =
        |file: &DupFile, report: &mut DuplicateDeleteReport| match delete_file(&file.path, options)
        {
            Ok(bytes) => {
                report.deleted.files += 1;
                report.deleted.bytes += bytes;
            }
            Err(e) => report.deleted.fail(&file.path, e),
        };

    if options.to_recycle_bin && !options.dry_run {
        // One shell operation per batch is far faster than one per file.
        for batch in queue.chunks(RECYCLE_BATCH) {
            progress.check()?;
            let paths: Vec<PathBuf> = batch.iter().map(|f| f.path.clone()).collect();
            if recycle(&paths).is_ok() {
                report.deleted.files += batch.len() as u64;
                report.deleted.bytes += batch.iter().map(|f| f.size).sum::<u64>();
            } else {
                // Retry one by one to find out which file is the problem.
                for file in batch {
                    if file.path.exists() {
                        delete_one(file, &mut report);
                    } else {
                        report.deleted.files += 1;
                        report.deleted.bytes += file.size;
                    }
                }
            }
            progress.inc(batch.len() as u64);
        }
    } else {
        for file in queue {
            progress.check()?;
            delete_one(file, &mut report);
            progress.inc(1);
        }
    }
    progress.set_message(String::new());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, data: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, data).unwrap();
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    /// Default options, but without the lower size limit, so the tiny
    /// files used throughout these tests are compared.
    fn all_sizes(roots: Vec<PathBuf>) -> DuplicateOptions {
        DuplicateOptions {
            min_size: 1,
            ..DuplicateOptions::for_roots(roots)
        }
    }

    #[test]
    fn default_options_skip_tiny_files() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("tiny1.txt"), b"same tiny text");
        write(&tmp.path().join("tiny2.txt"), b"same tiny text");
        write(
            &tmp.path().join("big1.bin"),
            &pattern(DEFAULT_MIN_SIZE as usize, 4),
        );
        write(
            &tmp.path().join("big2.bin"),
            &pattern(DEFAULT_MIN_SIZE as usize, 4),
        );
        let defaults = DuplicateOptions::for_roots(vec![tmp.path().to_path_buf()]);
        assert_eq!(defaults.min_size, DEFAULT_MIN_SIZE);
        let report = find_duplicates(&defaults, &Progress::new()).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(names(&report.groups[0]), vec!["big1.bin", "big2.bin"]);
        assert_eq!((report.files_scanned, report.candidates), (4, 2));
        assert_eq!(scan(tmp.path()).groups.len(), 2);
    }

    fn scan(root: &Path) -> DuplicateReport {
        find_duplicates(&all_sizes(vec![root.to_path_buf()]), &Progress::new()).unwrap()
    }

    fn names(group: &DuplicateGroup) -> Vec<String> {
        group
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn set_hidden(path: &Path) {
        let out = std::process::Command::new("attrib")
            .arg("+h")
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn finds_identical_content_only() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let big = pattern(200_000, 1);
        write(&r.join("a/big1.bin"), &big);
        write(&r.join("b/big2.bin"), &big);
        write(&r.join("c/deep/big3.bin"), &big);
        // Same size, different content.
        write(&r.join("a/other.bin"), &pattern(200_000, 2));
        // Small duplicates (fully covered by the probe).
        write(&r.join("a/s1.txt"), b"hello world");
        write(&r.join("b/s2.txt"), b"hello world");
        // Unique.
        write(&r.join("unique.txt"), b"nothing like me");

        let report = scan(r);
        assert_eq!(report.groups.len(), 2, "{:#?}", report.groups);
        let g0 = &report.groups[0];
        assert_eq!(g0.size, 200_000);
        assert_eq!(names(g0), vec!["big1.bin", "big2.bin", "big3.bin"]);
        assert_eq!(g0.wasted_bytes(), 400_000);
        assert_eq!(
            g0.hash.as_deref(),
            Some(blake3::hash(&big).to_hex().as_str())
        );
        let g1 = &report.groups[1];
        assert_eq!(names(g1), vec!["s1.txt", "s2.txt"]);
        assert_eq!(
            g1.hash.as_deref(),
            Some(blake3::hash(b"hello world").to_hex().as_str())
        );
        assert_eq!(report.duplicate_files, 3);
        assert_eq!(report.wasted_bytes, 400_000 + 11);
        assert_eq!(report.files_scanned, 7);
        assert_eq!(report.candidates, 7);
        assert_eq!(report.unreadable, 0);
        assert!(report.bytes_hashed >= 3 * 200_000);
    }

    #[test]
    fn same_head_and_tail_but_different_middle_is_not_a_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let a = pattern(300_000, 5);
        let mut b = a.clone();
        b[150_000] ^= 0xFF;
        write(&tmp.path().join("a.bin"), &a);
        write(&tmp.path().join("b.bin"), &b);
        let report = scan(tmp.path());
        assert!(report.groups.is_empty(), "{:#?}", report.groups);
        // Both were fully hashed because their probes matched.
        assert!(report.bytes_hashed >= 600_000);
    }

    #[test]
    fn hard_links_are_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let data = pattern(50_000, 9);
        write(&tmp.path().join("original.bin"), &data);
        fs::hard_link(tmp.path().join("original.bin"), tmp.path().join("link.bin")).unwrap();
        let report = scan(tmp.path());
        assert!(
            report.groups.is_empty(),
            "hard links must not count as duplicates"
        );
        assert_eq!(report.hard_links_merged, 1);

        // A real copy next to the two links: one group of two files.
        write(&tmp.path().join("copy.bin"), &data);
        let report = scan(tmp.path());
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].files.len(), 2);
        assert_eq!(report.wasted_bytes, 50_000);
        assert!(names(&report.groups[0]).contains(&"copy.bin".to_owned()));

        // Attribute-only matching must not be fooled either.
        fs::remove_file(tmp.path().join("copy.bin")).unwrap();
        let options = DuplicateOptions {
            match_content: false,
            ..all_sizes(vec![tmp.path().to_path_buf()])
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert!(report.groups.is_empty());
        assert_eq!(report.hard_links_merged, 1);
    }

    #[test]
    fn attribute_matching_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(&r.join("x/Report.txt"), b"aaaa");
        write(&r.join("y/report.TXT"), b"bbbbbbbb");
        write(&r.join("y/same-size.txt"), b"cccc");

        // By name only: the two reports, despite different sizes.
        let options = DuplicateOptions {
            match_content: false,
            match_size: false,
            match_name: true,
            ..all_sizes(vec![r.to_path_buf()])
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(names(&report.groups[0]), vec!["Report.txt", "report.TXT"]);
        assert_eq!(report.groups[0].size, 8);
        assert_eq!(report.groups[0].hash, None);
        assert_eq!(report.groups[0].wasted_bytes(), 4);

        // By size only: the two four-byte files.
        let options = DuplicateOptions {
            match_content: false,
            ..all_sizes(vec![r.to_path_buf()])
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            names(&report.groups[0]),
            vec!["Report.txt", "same-size.txt"]
        );

        // By name and size: nothing.
        let options = DuplicateOptions {
            match_content: false,
            match_name: true,
            ..all_sizes(vec![r.to_path_buf()])
        };
        assert!(
            find_duplicates(&options, &Progress::new())
                .unwrap()
                .groups
                .is_empty()
        );

        // Content plus name: identical content under different names is ignored.
        write(&r.join("x/one.dat"), b"identical");
        write(&r.join("y/two.dat"), b"identical");
        let options = DuplicateOptions {
            match_name: true,
            ..all_sizes(vec![r.to_path_buf()])
        };
        assert!(
            find_duplicates(&options, &Progress::new())
                .unwrap()
                .groups
                .is_empty()
        );

        // No criterion at all is rejected.
        let options = DuplicateOptions {
            match_content: false,
            match_size: false,
            ..all_sizes(vec![r.to_path_buf()])
        };
        assert!(matches!(
            find_duplicates(&options, &Progress::new()),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn filters() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(&r.join("e1"), b"");
        write(&r.join("e2"), b"");
        write(&r.join("small1.txt"), b"abc");
        write(&r.join("small2.txt"), b"abc");
        write(&r.join("pic1.jpg"), &pattern(5000, 3));
        write(&r.join("pic2.JPG"), &pattern(5000, 3));
        write(&r.join("hidden-dir/pic3.jpg"), &pattern(5000, 3));
        set_hidden(&r.join("hidden-dir"));
        write(&r.join("skipme/pic4.jpg"), &pattern(5000, 3));

        let base = all_sizes(vec![r.to_path_buf()]);

        // Defaults: empty files ignored, hidden folder not entered.
        let report = find_duplicates(&base, &Progress::new()).unwrap();
        assert_eq!(report.groups.len(), 2);
        assert_eq!(
            names(&report.groups[0]),
            vec!["pic1.jpg", "pic2.JPG", "pic4.jpg"]
        );

        // Hidden items included on request.
        let options = DuplicateOptions {
            ignore_hidden: false,
            ..base.clone()
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert_eq!(report.groups[0].files.len(), 4);

        // Exclusion list.
        let options = DuplicateOptions {
            exclude: vec![r.join("SkipMe")],
            ..base.clone()
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert_eq!(names(&report.groups[0]), vec!["pic1.jpg", "pic2.JPG"]);

        // Size window.
        let options = DuplicateOptions {
            min_size: 1000,
            ..base.clone()
        };
        assert_eq!(
            find_duplicates(&options, &Progress::new())
                .unwrap()
                .groups
                .len(),
            1
        );
        let options = DuplicateOptions {
            max_size: Some(100),
            ..base.clone()
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(names(&report.groups[0]), vec!["small1.txt", "small2.txt"]);

        // Extension filter (case-insensitive, dot optional).
        let options = DuplicateOptions {
            extensions: vec![".TXT".into()],
            ..base.clone()
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.candidates, 2);

        // Empty files on request.
        let options = DuplicateOptions {
            ignore_zero_byte: false,
            min_size: 0,
            ..base.clone()
        };
        let report = find_duplicates(&options, &Progress::new()).unwrap();
        let empty = report.groups.iter().find(|g| g.size == 0).unwrap();
        assert_eq!(names(empty), vec!["e1", "e2"]);
        assert_eq!(empty.wasted_bytes(), 0);
    }

    #[test]
    fn junctions_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let scanned = tmp.path().join("scanned");
        let outside = tmp.path().join("outside");
        write(&scanned.join("a.bin"), &pattern(4000, 1));
        write(&outside.join("b.bin"), &pattern(4000, 1));
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(scanned.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(scan(&scanned).groups.is_empty());
    }

    #[test]
    fn system_locations_are_skipped_by_default() {
        let keys = system_folder_keys();
        assert!(keys.contains(&path_key(&paths::windows_dir())));
        // Scanning the Windows folder itself as an explicit root is allowed,
        // but descending into it from the drive root is not.
        let collector_options = DuplicateOptions::default();
        assert!(collector_options.skip_system_folders);
    }

    fn two_groups(root: &Path) -> DuplicateReport {
        write(&root.join("keep/a.bin"), &pattern(3000, 1));
        write(&root.join("dup/a-copy.bin"), &pattern(3000, 1));
        write(&root.join("dup/nested/a-copy-2.bin"), &pattern(3000, 1));
        write(&root.join("keep/b.bin"), &pattern(900, 2));
        write(&root.join("dup/b-copy.bin"), &pattern(900, 2));
        let report = scan(root);
        assert_eq!(report.groups.len(), 2);
        report
    }

    #[test]
    fn selection_strategies() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let mut report = two_groups(r);

        let shortest = select_for_removal(&report.groups, &KeepStrategy::ShortestPath);
        assert_eq!(shortest.len(), 3);
        assert!(!shortest.contains(&r.join("keep").join("a.bin")));
        assert!(!shortest.contains(&r.join("keep").join("b.bin")));

        let longest = select_for_removal(&report.groups, &KeepStrategy::LongestPath);
        assert!(!longest.contains(&r.join("dup").join("nested").join("a-copy-2.bin")));
        assert_eq!(longest.len(), 3);

        let in_folder = select_for_removal(&report.groups, &KeepStrategy::InFolder(r.join("KEEP")));
        assert_eq!(in_folder.len(), 3);
        assert!(in_folder.iter().all(|p| p.starts_with(r.join("dup"))));

        // A preferred folder that holds no copy leaves groups untouched.
        let none = select_for_removal(&report.groups, &KeepStrategy::InFolder(r.join("elsewhere")));
        assert!(none.is_empty());
        // A folder holding every copy removes nothing either.
        let all_inside =
            select_for_removal(&report.groups, &KeepStrategy::InFolder(r.to_path_buf()));
        assert!(all_inside.is_empty());

        // Newest / oldest use the recorded times.
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (i, file) in report.groups[0].files.iter_mut().enumerate() {
            file.modified = Some(base + std::time::Duration::from_secs(i as u64 * 60));
        }
        let first = report.groups[0].files[0].path.clone();
        let last = report.groups[0].files[2].path.clone();
        let newest = select_for_removal(&report.groups[..1], &KeepStrategy::Newest);
        assert_eq!(newest.len(), 2);
        assert!(!newest.contains(&last));
        let oldest = select_for_removal(&report.groups[..1], &KeepStrategy::Oldest);
        assert!(!oldest.contains(&first));
        assert_eq!(oldest.len(), 2);

        // Every strategy leaves at least one file per group.
        for strategy in [
            KeepStrategy::Newest,
            KeepStrategy::Oldest,
            KeepStrategy::ShortestPath,
            KeepStrategy::LongestPath,
        ] {
            let remove = select_for_removal(&report.groups, &strategy);
            for group in &report.groups {
                let kept = group
                    .files
                    .iter()
                    .filter(|f| !remove.contains(&f.path))
                    .count();
                assert_eq!(kept, 1, "{strategy:?}");
            }
        }
    }

    #[test]
    fn deletion_removes_only_selected_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let report = two_groups(r);
        let remove = select_for_removal(&report.groups, &KeepStrategy::InFolder(r.join("keep")));

        // Dry run first: nothing changes.
        let dry = delete_duplicates(
            &report.groups,
            &remove,
            &DeleteOptions::permanent().with_dry_run(true),
            &Progress::new(),
        )
        .unwrap();
        assert_eq!(
            (dry.deleted.files, dry.deleted.bytes),
            (3, 3000 + 3000 + 900)
        );
        assert!(r.join("dup/a-copy.bin").exists());

        let progress = Progress::new();
        let done = delete_duplicates(
            &report.groups,
            &remove,
            &DeleteOptions::permanent(),
            &progress,
        )
        .unwrap();
        assert_eq!((done.deleted.files, done.deleted.failed), (3, 0));
        assert_eq!(done.deleted.bytes, 6900);
        assert_eq!(progress.fraction(), Some(1.0));
        assert!(!r.join("dup/a-copy.bin").exists());
        assert!(!r.join("dup/nested/a-copy-2.bin").exists());
        assert!(!r.join("dup/b-copy.bin").exists());
        assert!(r.join("keep/a.bin").exists());
        assert!(r.join("keep/b.bin").exists());
        assert!(done.kept_last_copy.is_empty() && done.changed.is_empty());
    }

    #[test]
    fn deletion_never_removes_every_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let report = two_groups(r);
        // Ask for *everything* to be removed.
        let everything: Vec<PathBuf> = report
            .groups
            .iter()
            .flat_map(|g| g.files.iter().map(|f| f.path.clone()))
            .collect();
        let done = delete_duplicates(
            &report.groups,
            &everything,
            &DeleteOptions::permanent(),
            &Progress::new(),
        )
        .unwrap();
        assert_eq!(done.kept_last_copy.len(), 2);
        assert_eq!(done.deleted.files, 3);
        for group in &report.groups {
            let alive = group.files.iter().filter(|f| f.path.exists()).count();
            assert_eq!(alive, 1, "exactly one copy of each group must survive");
        }
    }

    #[test]
    fn deletion_respects_stale_survivors_changed_files_and_strangers() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let report = two_groups(r);
        let group_a = report.groups.iter().find(|g| g.size == 3000).unwrap();
        let group_b = report.groups.iter().find(|g| g.size == 900).unwrap();

        // Group A: the copy we intend to keep is edited after the scan, so
        // it is no longer a verified survivor.
        write(&r.join("keep/a.bin"), b"edited after the scan");
        // Group B: the file selected for removal is edited after the scan.
        write(&r.join("dup/b-copy.bin"), b"also edited, different length!");
        // A stranger that is not part of any group.
        write(&r.join("stranger.txt"), b"do not touch");

        let remove = vec![
            r.join("dup/a-copy.bin"),
            r.join("dup/nested/a-copy-2.bin"),
            r.join("dup/b-copy.bin"),
            r.join("stranger.txt"),
        ];
        let done = delete_duplicates(
            &report.groups,
            &remove,
            &DeleteOptions::permanent(),
            &Progress::new(),
        )
        .unwrap();

        // A: one of the two selected copies must have been preserved.
        assert_eq!(done.kept_last_copy.len(), 1);
        let alive_a = group_a
            .files
            .iter()
            .filter(|f| f.path != r.join("keep").join("a.bin") && f.path.exists())
            .count();
        assert_eq!(alive_a, 1);
        // B: the changed file is skipped and reported.
        assert_eq!(done.changed, vec![r.join("dup").join("b-copy.bin")]);
        assert!(group_b.files.iter().all(|f| f.path.exists()));
        // The stranger is refused.
        assert_eq!(done.not_duplicates, vec![r.join("stranger.txt")]);
        assert!(r.join("stranger.txt").exists());
        assert_eq!(done.deleted.files, 1);
    }

    #[test]
    fn errors_and_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            find_duplicates(&DuplicateOptions::default(), &Progress::new()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            find_duplicates(
                &all_sizes(vec![tmp.path().join("missing")]),
                &Progress::new()
            ),
            Err(Error::NotFound(_))
        ));
        write(&tmp.path().join("a"), b"x");
        let progress = Progress::new();
        progress.cancel();
        assert!(matches!(
            find_duplicates(&all_sizes(vec![tmp.path().to_path_buf()]), &progress),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn report_serializes() {
        let tmp = tempfile::tempdir().unwrap();
        let report = two_groups(tmp.path());
        let json = serde_json::to_string(&report).unwrap();
        let back: DuplicateReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.groups, report.groups);
    }
}
