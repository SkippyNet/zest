//! Disk analyzer: where did the space go?
//!
//! One parallel pass over the selected folders produces totals, a breakdown
//! by file category and extension, the largest files and a pruned folder
//! tree suitable for a drill-down view or treemap.
//!
//! * Junctions and symbolic links are never followed (and not counted).
//! * Cloud placeholders are counted by their logical size but never opened;
//!   [`Analysis::cloud_bytes`] says how much of the total is cloud-only.
//! * Sizes are logical file sizes. A file with several hard links is counted
//!   once per link, like Windows Explorer does.
//!
//! # Progress
//! `done` = files seen, `bytes` = bytes seen, `total` stays zero
//! (indeterminate); the message is the folder currently being read.

use crate::categories::{ExtensionKey, FileCategory};
use crate::fastdir::{self, ATTR_SYSTEM, RawEntry};
use crate::pool::{FxHashMap, FxHashSet, Shards, pool};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};
use zest_core::{Error, Progress, Result};

/// Extension key that collects rarely seen extensions once a scan has met
/// more distinct extensions than it is willing to track individually.
pub const OVERFLOW_EXTENSION: &str = "*";

/// Distinct extensions tracked per worker before overflowing.
const MAX_TRACKED_EXTENSIONS: usize = 20_000;

/// Minimum interval between progress message updates.
const MESSAGE_INTERVAL_MS: u64 = 60;

/// What to scan and how much detail to keep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzeOptions {
    /// Folders or drive roots to analyze. Nested roots are merged.
    pub roots: Vec<PathBuf>,
    /// Folders to skip entirely.
    pub exclude: Vec<PathBuf>,
    /// Only count files of these categories (`None` = all files).
    pub categories: Option<Vec<FileCategory>>,
    /// How many of the largest files to report (zero disables the list).
    pub largest_files: usize,
    /// Files smaller than this never enter the largest-files list.
    pub min_large_file_size: u64,
    /// How many extensions to report, largest first.
    pub max_extensions: usize,
    /// Depth of the folder tree kept in the result (roots are depth 0;
    /// zero keeps only the root nodes with their totals).
    pub tree_max_depth: usize,
    /// Maximum sub-folders listed per folder; the rest is summarised in
    /// [`FolderNode::pruned_children`] / [`FolderNode::pruned_bytes`].
    pub tree_max_children: usize,
    /// Sub-folders and files smaller than this are not listed individually.
    pub tree_min_bytes: u64,
    /// Maximum number of large files listed directly inside each folder.
    pub tree_files_per_folder: usize,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        AnalyzeOptions {
            roots: Vec::new(),
            exclude: Vec::new(),
            categories: None,
            largest_files: 200,
            min_large_file_size: 0,
            max_extensions: 200,
            tree_max_depth: 64,
            tree_max_children: 48,
            tree_min_bytes: 1024 * 1024,
            tree_files_per_folder: 16,
        }
    }
}

impl AnalyzeOptions {
    /// Default options for the given roots.
    pub fn for_roots(roots: Vec<PathBuf>) -> Self {
        AnalyzeOptions {
            roots,
            ..Self::default()
        }
    }

    /// Default options for a single folder or drive.
    pub fn for_path(path: impl Into<PathBuf>) -> Self {
        Self::for_roots(vec![path.into()])
    }
}

/// One file in a result list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Full path.
    pub path: PathBuf,
    /// Logical size in bytes.
    pub size: u64,
    /// Last modification time, when available.
    pub modified: Option<SystemTime>,
    /// Category derived from the extension.
    pub category: FileCategory,
}

/// Space used by one file category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryStat {
    /// The category.
    pub category: FileCategory,
    /// Total bytes.
    pub bytes: u64,
    /// Number of files.
    pub files: u64,
}

/// Space used by one file extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionStat {
    /// Lower-case extension without the dot. Empty for files without an
    /// extension; [`OVERFLOW_EXTENSION`] for the overflow bucket.
    pub extension: String,
    /// Category the extension belongs to.
    pub category: FileCategory,
    /// Total bytes.
    pub bytes: u64,
    /// Number of files.
    pub files: u64,
}

/// A folder in the pruned result tree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderNode {
    /// Folder name (the full path for a root).
    pub name: String,
    /// Full path.
    pub path: PathBuf,
    /// Bytes in this folder and everything below it.
    pub bytes: u64,
    /// Files in this folder and everything below it.
    pub files: u64,
    /// Number of sub-folders at any depth.
    pub dirs: u64,
    /// Bytes of the files directly inside this folder.
    pub own_bytes: u64,
    /// Number of files directly inside this folder.
    pub own_files: u64,
    /// The largest sub-folders, biggest first (pruned, see [`AnalyzeOptions`]).
    pub children: Vec<FolderNode>,
    /// The largest files directly inside this folder, biggest first (pruned).
    pub top_files: Vec<FileEntry>,
    /// Sub-folders that exist but are not listed in `children`.
    pub pruned_children: u64,
    /// Combined size of the sub-folders not listed in `children`.
    pub pruned_bytes: u64,
    /// True when the folder could not be read (its totals are then zero).
    pub inaccessible: bool,
}

impl FolderNode {
    /// Bytes not attributable to a listed child folder or listed file:
    /// small files, pruned sub-folders and anything below the detail limits.
    pub fn unlisted_bytes(&self) -> u64 {
        let listed: u64 = self.children.iter().map(|c| c.bytes).sum::<u64>()
            + self.top_files.iter().map(|f| f.size).sum::<u64>();
        self.bytes.saturating_sub(listed)
    }

    /// Finds the node for `path` in this subtree, if it was kept.
    pub fn find(&self, path: &Path) -> Option<&FolderNode> {
        let wanted = fastdir::path_key(path);
        self.find_key(&wanted)
    }

    fn find_key(&self, wanted: &str) -> Option<&FolderNode> {
        let own = fastdir::path_key(&self.path);
        if own == wanted {
            return Some(self);
        }
        if !fastdir::key_is_under(wanted, &own) {
            return None;
        }
        self.children.iter().find_map(|c| c.find_key(wanted))
    }
}

/// The result of [`analyze`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Analysis {
    /// The roots that were scanned (after merging nested ones).
    pub roots: Vec<PathBuf>,
    /// Total bytes of all counted files.
    pub total_bytes: u64,
    /// Number of counted files.
    pub total_files: u64,
    /// Number of folders below the roots.
    pub total_dirs: u64,
    /// Junctions and symbolic links that were skipped.
    pub links_skipped: u64,
    /// Folders that could not be read (usually: access denied).
    pub inaccessible_dirs: u64,
    /// Bytes that exist only in the cloud (online-only placeholders).
    pub cloud_bytes: u64,
    /// Number of cloud-only files.
    pub cloud_files: u64,
    /// Every category, largest first.
    pub categories: Vec<CategoryStat>,
    /// The largest extensions, biggest first.
    pub extensions: Vec<ExtensionStat>,
    /// The largest files, biggest first.
    pub largest_files: Vec<FileEntry>,
    /// One pruned folder tree per root.
    pub tree: Vec<FolderNode>,
    /// Wall-clock duration of the scan in milliseconds.
    pub elapsed_ms: u64,
}

impl Analysis {
    /// The statistics of one category.
    pub fn category(&self, category: FileCategory) -> CategoryStat {
        self.categories
            .iter()
            .copied()
            .find(|c| c.category == category)
            .unwrap_or(CategoryStat {
                category,
                bytes: 0,
                files: 0,
            })
    }

    /// Finds the tree node for `path`, if it was kept.
    pub fn find_folder(&self, path: &Path) -> Option<&FolderNode> {
        self.tree.iter().find_map(|root| root.find(path))
    }
}

/// Keeps the `limit` largest entries seen so far with amortised O(1) pushes.
pub(crate) struct TopFiles {
    limit: usize,
    threshold: u64,
    items: Vec<FileEntry>,
}

impl TopFiles {
    pub fn new(limit: usize) -> Self {
        TopFiles {
            limit,
            threshold: 0,
            items: Vec::new(),
        }
    }

    /// Whether a file of `size` would currently make the list. Checking this
    /// first avoids building a path for files that would be discarded.
    #[inline]
    pub fn wants(&self, size: u64) -> bool {
        self.limit > 0 && (self.items.len() < self.limit || size > self.threshold)
    }

    pub fn push(&mut self, entry: FileEntry) {
        self.items.push(entry);
        if self.items.len() >= self.limit.saturating_mul(2).max(self.limit + 1) {
            self.compact();
        }
    }

    fn compact(&mut self) {
        self.items
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
        self.items.truncate(self.limit);
        if self.items.len() == self.limit {
            self.threshold = self.items.last().map_or(0, |e| e.size);
        }
    }

    pub fn extend(&mut self, other: TopFiles) {
        for entry in other.items {
            if self.wants(entry.size) {
                self.push(entry);
            }
        }
    }

    pub fn into_sorted(mut self) -> Vec<FileEntry> {
        self.compact();
        self.items
    }
}

#[derive(Clone, Copy)]
struct ExtAcc {
    bytes: u64,
    files: u64,
    category: FileCategory,
}

struct Shard {
    extensions: FxHashMap<String, ExtAcc>,
    categories: [(u64, u64); FileCategory::COUNT],
    largest: TopFiles,
    links: u64,
    inaccessible: u64,
    cloud_bytes: u64,
    cloud_files: u64,
}

impl Default for Shard {
    fn default() -> Self {
        Shard {
            extensions: FxHashMap::default(),
            categories: [(0, 0); FileCategory::COUNT],
            largest: TopFiles::new(0),
            links: 0,
            inaccessible: 0,
            cloud_bytes: 0,
            cloud_files: 0,
        }
    }
}

struct Ctx<'a> {
    progress: &'a Progress,
    shards: Shards<Shard>,
    exclude: FxHashSet<String>,
    category_mask: u32,
    largest_limit: usize,
    min_large: u64,
    tree_max_depth: usize,
    tree_max_children: usize,
    tree_min_bytes: u64,
    tree_files_per_folder: usize,
    started: Instant,
    last_message_ms: AtomicU64,
}

impl Ctx<'_> {
    fn report_folder(&self, dir: &Path) {
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.last_message_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= MESSAGE_INTERVAL_MS
            && self
                .last_message_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.progress.set_message(dir.display().to_string());
        }
    }
}

/// Books one file under its extension. Returns the file's category, or
/// `None` when the category filter excludes it (nothing is counted then).
#[inline]
fn account(slot: &mut ExtAcc, entry: &RawEntry<'_>, category_mask: u32) -> Option<FileCategory> {
    let mut category = slot.category;
    if category == FileCategory::Other && entry.attributes & ATTR_SYSTEM != 0 {
        category = FileCategory::System;
    }
    if category_mask & (1 << category.index()) == 0 {
        return None;
    }
    slot.bytes += entry.size;
    slot.files += 1;
    Some(category)
}

/// Reads one folder: accounts its files and returns the names of its real
/// sub-folders. Kept separate from the recursion so the large find-data
/// buffer is off the stack before descending.
#[inline(never)]
fn read_folder(ctx: &Ctx<'_>, node: &mut FolderNode, depth: usize) -> Vec<OsString> {
    let mut subdirs: Vec<OsString> = Vec::new();
    let listed_depth = depth <= ctx.tree_max_depth;
    let mut top = TopFiles::new(if listed_depth {
        ctx.tree_files_per_folder
    } else {
        0
    });
    let dir = node.path.clone();
    let mut own_bytes = 0u64;
    let mut own_files = 0u64;

    let mut guard = ctx.shards.local();
    let shard = &mut *guard;
    if shard.largest.limit != ctx.largest_limit {
        shard.largest = TopFiles::new(ctx.largest_limit);
    }

    let mut visit = |entry: &RawEntry<'_>| {
        if entry.is_link() {
            shard.links += 1;
            return;
        }
        if entry.is_dir() {
            subdirs.push(entry.os_name());
            return;
        }

        let key = ExtensionKey::from_name(entry.name);
        let ext = key.as_str();
        let accounted = if let Some(slot) = shard.extensions.get_mut(ext) {
            account(slot, entry, ctx.category_mask)
        } else {
            let (name, category) = if shard.extensions.len() >= MAX_TRACKED_EXTENSIONS {
                (OVERFLOW_EXTENSION, FileCategory::Other)
            } else {
                (ext, FileCategory::of_extension(ext))
            };
            let slot = shard.extensions.entry(name.to_owned()).or_insert(ExtAcc {
                bytes: 0,
                files: 0,
                category,
            });
            account(slot, entry, ctx.category_mask)
        };
        let Some(category) = accounted else {
            return;
        };

        let size = entry.size;
        let cat = &mut shard.categories[category.index()];
        cat.0 += size;
        cat.1 += 1;
        own_bytes += size;
        own_files += 1;
        if entry.is_cloud_placeholder() {
            shard.cloud_bytes += size;
            shard.cloud_files += 1;
        }

        let wanted_globally = size >= ctx.min_large && shard.largest.wants(size);
        let wanted_locally = size >= ctx.tree_min_bytes && top.wants(size);
        if wanted_globally || wanted_locally {
            let file = FileEntry {
                path: entry.path_in(&dir),
                size,
                modified: fastdir::filetime_to_system(entry.modified),
                category,
            };
            match (wanted_globally, wanted_locally) {
                (true, true) => {
                    shard.largest.push(file.clone());
                    top.push(file);
                }
                (true, false) => shard.largest.push(file),
                _ => top.push(file),
            }
        }
    };

    if fastdir::read_dir_raw(&dir, &mut visit).is_err() {
        node.inaccessible = true;
        shard.inaccessible += 1;
    }
    drop(guard);

    node.own_bytes = own_bytes;
    node.own_files = own_files;
    node.top_files = top.into_sorted();
    subdirs
}

fn scan_dir(ctx: &Ctx<'_>, path: PathBuf, name: String, depth: usize) -> FolderNode {
    let mut node = FolderNode {
        name,
        path,
        ..FolderNode::default()
    };
    if ctx.progress.is_cancelled() {
        return node;
    }

    let subdirs = read_folder(ctx, &mut node, depth);
    ctx.progress.inc(node.own_files);
    ctx.progress.add_bytes(node.own_bytes);
    ctx.report_folder(&node.path);

    let parent = node.path.clone();
    let descend = |os_name: OsString| -> Option<FolderNode> {
        let child_path = parent.join(&os_name);
        if !ctx.exclude.is_empty() && ctx.exclude.contains(&fastdir::path_key(&child_path)) {
            return None;
        }
        Some(scan_dir(
            ctx,
            child_path,
            os_name.to_string_lossy().into_owned(),
            depth + 1,
        ))
    };
    let mut children: Vec<FolderNode> = if subdirs.len() < 2 {
        subdirs.into_iter().filter_map(descend).collect()
    } else {
        subdirs.into_par_iter().filter_map(descend).collect()
    };

    node.bytes = node.own_bytes;
    node.files = node.own_files;
    for child in &children {
        node.bytes += child.bytes;
        node.files += child.files;
        node.dirs += child.dirs + 1;
    }

    // Prune: keep only the children worth showing, so the tree stays small
    // no matter how many folders the disk holds.
    children.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let keep = if depth < ctx.tree_max_depth {
        children
            .iter()
            .take(ctx.tree_max_children)
            .take_while(|c| c.bytes >= ctx.tree_min_bytes)
            .count()
    } else {
        0
    };
    for dropped in &children[keep..] {
        node.pruned_children += 1;
        node.pruned_bytes += dropped.bytes;
    }
    children.truncate(keep);
    children.shrink_to_fit();
    node.children = children;
    node
}

fn root_name(root: &Path) -> String {
    root.display().to_string()
}

/// Analyzes the folders in `options.roots`.
///
/// Returns [`Error::Cancelled`] when `progress` is cancelled,
/// [`Error::Invalid`] when no root was given and [`Error::NotFound`] when a
/// root is not an existing folder.
pub fn analyze(options: &AnalyzeOptions, progress: &Progress) -> Result<Analysis> {
    let started = Instant::now();
    let roots = fastdir::normalize_roots(&options.roots);
    if roots.is_empty() {
        return Err(Error::Invalid(
            "no folder was selected to analyze".to_owned(),
        ));
    }
    for root in &roots {
        if !root.is_dir() {
            return Err(Error::NotFound(root.display().to_string()));
        }
    }

    let category_mask = match &options.categories {
        None => u32::MAX,
        Some(list) => list.iter().fold(0u32, |mask, c| mask | (1 << c.index())),
    };
    let ctx = Ctx {
        progress,
        shards: Shards::new(),
        exclude: options
            .exclude
            .iter()
            .map(|p| fastdir::path_key(&fastdir::absolute(p)))
            .collect(),
        category_mask,
        largest_limit: options.largest_files,
        min_large: options.min_large_file_size,
        tree_max_depth: options.tree_max_depth,
        tree_max_children: options.tree_max_children,
        tree_min_bytes: options.tree_min_bytes,
        tree_files_per_folder: options.tree_files_per_folder,
        started,
        last_message_ms: AtomicU64::new(0),
    };

    progress.set_total(0);
    let tree: Vec<FolderNode> = pool().install(|| {
        roots
            .par_iter()
            .map(|root| scan_dir(&ctx, root.clone(), root_name(root), 0))
            .collect()
    });
    progress.check()?;

    let mut analysis = Analysis {
        roots,
        ..Analysis::default()
    };
    let mut extensions: FxHashMap<String, ExtAcc> = FxHashMap::default();
    let mut categories = [(0u64, 0u64); FileCategory::COUNT];
    let mut largest = TopFiles::new(options.largest_files);
    for shard in ctx.shards.into_inner() {
        analysis.links_skipped += shard.links;
        analysis.inaccessible_dirs += shard.inaccessible;
        analysis.cloud_bytes += shard.cloud_bytes;
        analysis.cloud_files += shard.cloud_files;
        for (total, part) in categories.iter_mut().zip(shard.categories) {
            total.0 += part.0;
            total.1 += part.1;
        }
        for (ext, acc) in shard.extensions {
            let slot = extensions.entry(ext).or_insert(ExtAcc {
                bytes: 0,
                files: 0,
                category: acc.category,
            });
            slot.bytes += acc.bytes;
            slot.files += acc.files;
        }
        largest.extend(shard.largest);
    }

    for root in &tree {
        analysis.total_bytes += root.bytes;
        analysis.total_files += root.files;
        analysis.total_dirs += root.dirs;
    }
    analysis.categories = FileCategory::ALL
        .iter()
        .map(|&category| CategoryStat {
            category,
            bytes: categories[category.index()].0,
            files: categories[category.index()].1,
        })
        .collect();
    analysis
        .categories
        .sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.category.cmp(&b.category)));

    let mut ext_stats: Vec<ExtensionStat> = extensions
        .into_iter()
        .filter(|(_, acc)| acc.files > 0)
        .map(|(extension, acc)| ExtensionStat {
            extension,
            category: acc.category,
            bytes: acc.bytes,
            files: acc.files,
        })
        .collect();
    ext_stats.sort_unstable_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| b.files.cmp(&a.files))
            .then_with(|| a.extension.cmp(&b.extension))
    });
    ext_stats.truncate(options.max_extensions);
    analysis.extensions = ext_stats;
    analysis.largest_files = largest.into_sorted();
    analysis.tree = tree;
    analysis.elapsed_ms = started.elapsed().as_millis() as u64;
    progress.set_message(String::new());
    Ok(analysis)
}

/// Finds the largest files below `roots`: at most `limit` files of at least
/// `min_size` bytes, biggest first.
///
/// This is [`analyze`] with everything except the file list switched off.
pub fn find_large_files(
    roots: &[PathBuf],
    min_size: u64,
    limit: usize,
    progress: &Progress,
) -> Result<Vec<FileEntry>> {
    let options = AnalyzeOptions {
        roots: roots.to_vec(),
        largest_files: limit,
        min_large_file_size: min_size,
        max_extensions: 0,
        tree_max_depth: 0,
        tree_files_per_folder: 0,
        ..AnalyzeOptions::default()
    };
    Ok(analyze(&options, progress)?.largest_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, len: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![7u8; len]).unwrap();
    }

    /// root/
    ///   photo.jpg (3000)   notes.txt (500)   noext (10)
    ///   media/ song.mp3 (2000)  clip.mp4 (9000)
    ///   media/deep/ archive.zip (4000)
    ///   code/ main.rs (100)  lib.dll (700)
    ///   empty/
    fn sample() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(&r.join("photo.jpg"), 3000);
        write(&r.join("notes.txt"), 500);
        write(&r.join("noext"), 10);
        write(&r.join("media/song.mp3"), 2000);
        write(&r.join("media/clip.mp4"), 9000);
        write(&r.join("media/deep/archive.zip"), 4000);
        write(&r.join("code/main.rs"), 100);
        write(&r.join("code/lib.dll"), 700);
        fs::create_dir_all(r.join("empty")).unwrap();
        tmp
    }

    fn detailed(root: &Path) -> AnalyzeOptions {
        AnalyzeOptions {
            tree_min_bytes: 0,
            ..AnalyzeOptions::for_path(root)
        }
    }

    #[test]
    fn totals_categories_and_extensions() {
        let tmp = sample();
        let a = analyze(&detailed(tmp.path()), &Progress::new()).unwrap();
        assert_eq!(a.total_files, 8);
        assert_eq!(
            a.total_bytes,
            3000 + 500 + 10 + 2000 + 9000 + 4000 + 100 + 700
        );
        assert_eq!(a.total_dirs, 4);
        assert_eq!(a.links_skipped, 0);
        assert_eq!(a.inaccessible_dirs, 0);

        assert_eq!(a.category(FileCategory::Pictures).bytes, 3000);
        assert_eq!(a.category(FileCategory::Music).bytes, 2000);
        assert_eq!(a.category(FileCategory::Video).bytes, 9000);
        assert_eq!(a.category(FileCategory::Documents).bytes, 500);
        assert_eq!(a.category(FileCategory::Archives).bytes, 4000);
        assert_eq!(a.category(FileCategory::Code).bytes, 100);
        assert_eq!(a.category(FileCategory::System).bytes, 700);
        assert_eq!(
            a.category(FileCategory::Other),
            CategoryStat {
                category: FileCategory::Other,
                bytes: 10,
                files: 1
            }
        );
        assert_eq!(a.categories.len(), FileCategory::COUNT);
        assert_eq!(a.categories[0].category, FileCategory::Video);
        assert_eq!(
            a.categories.iter().map(|c| c.bytes).sum::<u64>(),
            a.total_bytes
        );
        assert_eq!(
            a.categories.iter().map(|c| c.files).sum::<u64>(),
            a.total_files
        );

        assert_eq!(a.extensions[0].extension, "mp4");
        assert_eq!(a.extensions[0].category, FileCategory::Video);
        let no_ext = a
            .extensions
            .iter()
            .find(|e| e.extension.is_empty())
            .unwrap();
        assert_eq!((no_ext.bytes, no_ext.files), (10, 1));
        assert_eq!(
            a.extensions.iter().map(|e| e.bytes).sum::<u64>(),
            a.total_bytes
        );
    }

    #[test]
    fn largest_files_are_sorted_and_limited() {
        let tmp = sample();
        let mut options = detailed(tmp.path());
        options.largest_files = 3;
        let a = analyze(&options, &Progress::new()).unwrap();
        let sizes: Vec<u64> = a.largest_files.iter().map(|f| f.size).collect();
        assert_eq!(sizes, vec![9000, 4000, 3000]);
        assert_eq!(
            a.largest_files[0].path,
            tmp.path().join("media").join("clip.mp4")
        );
        assert_eq!(a.largest_files[0].category, FileCategory::Video);
        assert!(a.largest_files[0].modified.is_some());

        let big =
            find_large_files(&[tmp.path().to_path_buf()], 2500, 10, &Progress::new()).unwrap();
        assert_eq!(
            big.iter().map(|f| f.size).collect::<Vec<_>>(),
            vec![9000, 4000, 3000]
        );
        let none =
            find_large_files(&[tmp.path().to_path_buf()], 1 << 30, 10, &Progress::new()).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn tree_structure() {
        let tmp = sample();
        let a = analyze(&detailed(tmp.path()), &Progress::new()).unwrap();
        assert_eq!(a.tree.len(), 1);
        let root = &a.tree[0];
        assert_eq!(root.bytes, a.total_bytes);
        assert_eq!(root.own_files, 3);
        assert_eq!(root.own_bytes, 3510);
        assert_eq!(root.dirs, 4);
        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["media", "code", "empty"]);

        let media = a.find_folder(&tmp.path().join("media")).unwrap();
        assert_eq!((media.bytes, media.files, media.dirs), (15_000, 3, 1));
        assert_eq!(media.own_bytes, 11_000);
        assert_eq!(media.top_files[0].size, 9000);
        assert_eq!(media.children[0].name, "deep");
        assert_eq!(media.children[0].bytes, 4000);
        assert_eq!(media.unlisted_bytes(), 0);

        let deep = a
            .find_folder(&tmp.path().join("media").join("deep"))
            .unwrap();
        assert_eq!(deep.files, 1);
        assert!(a.find_folder(&tmp.path().join("nope")).is_none());
        assert!(a.find_folder(Path::new(r"Q:\elsewhere")).is_none());
    }

    #[test]
    fn tree_pruning_keeps_totals() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10usize {
            write(&tmp.path().join(format!("d{i}/f.bin")), (i + 1) * 1000);
            write(&tmp.path().join(format!("d{i}/sub/g.bin")), 10);
        }
        let options = AnalyzeOptions {
            tree_max_children: 3,
            tree_min_bytes: 0,
            tree_max_depth: 1,
            ..AnalyzeOptions::for_path(tmp.path())
        };
        let a = analyze(&options, &Progress::new()).unwrap();
        let root = &a.tree[0];
        assert_eq!(root.children.len(), 3);
        assert_eq!(root.children[0].name, "d9");
        assert_eq!(root.pruned_children, 7);
        assert_eq!(
            root.pruned_bytes + root.children.iter().map(|c| c.bytes).sum::<u64>(),
            root.bytes
        );
        assert_eq!(root.bytes, 55_000 + 100);
        assert_eq!(root.dirs, 20);
        // Depth limit: children of depth-1 nodes are summarised, not listed.
        let d9 = &root.children[0];
        assert!(d9.children.is_empty());
        assert_eq!(d9.pruned_children, 1);
        assert_eq!(d9.bytes, 10_010);
        assert_eq!(d9.unlisted_bytes(), 10);

        // A size floor hides small folders and files.
        let options = AnalyzeOptions {
            tree_min_bytes: 5000,
            ..AnalyzeOptions::for_path(tmp.path())
        };
        let a = analyze(&options, &Progress::new()).unwrap();
        assert_eq!(a.tree[0].children.len(), 6);
        assert!(a.tree[0].children.iter().all(|c| c.bytes >= 5000));
        assert_eq!(a.tree[0].pruned_children, 4);

        // Depth zero keeps only the root totals.
        let options = AnalyzeOptions {
            tree_max_depth: 0,
            ..AnalyzeOptions::for_path(tmp.path())
        };
        let a = analyze(&options, &Progress::new()).unwrap();
        assert!(a.tree[0].children.is_empty());
        assert_eq!(a.tree[0].pruned_children, 10);
        assert_eq!(a.tree[0].bytes, 55_100);
    }

    #[test]
    fn category_filter_and_exclusions() {
        let tmp = sample();
        let mut options = detailed(tmp.path());
        options.categories = Some(vec![FileCategory::Video, FileCategory::Music]);
        let a = analyze(&options, &Progress::new()).unwrap();
        assert_eq!((a.total_files, a.total_bytes), (2, 11_000));
        assert_eq!(a.extensions.len(), 2);
        assert_eq!(a.tree[0].own_files, 0);

        let mut options = detailed(tmp.path());
        options.exclude = vec![tmp.path().join("MEDIA")];
        let a = analyze(&options, &Progress::new()).unwrap();
        assert_eq!(a.total_bytes, 3000 + 500 + 10 + 100 + 700);
        assert!(a.find_folder(&tmp.path().join("media")).is_none());
        assert_eq!(a.total_dirs, 2);
    }

    #[test]
    fn junctions_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let scanned = tmp.path().join("scanned");
        let outside = tmp.path().join("outside");
        write(&scanned.join("a.bin"), 100);
        write(&outside.join("big.bin"), 5000);
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(scanned.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());
        let a = analyze(&detailed(&scanned), &Progress::new()).unwrap();
        assert_eq!((a.total_bytes, a.total_files, a.links_skipped), (100, 1, 1));
        assert_eq!(a.total_dirs, 0);
    }

    #[test]
    fn multiple_and_nested_roots() {
        let tmp = sample();
        let options = AnalyzeOptions::for_roots(vec![
            tmp.path().join("media"),
            tmp.path().join("code"),
            tmp.path().join("media").join("deep"),
        ]);
        let a = analyze(&options, &Progress::new()).unwrap();
        assert_eq!(a.roots.len(), 2);
        assert_eq!(a.tree.len(), 2);
        assert_eq!(a.total_bytes, 15_000 + 800);
    }

    #[test]
    fn errors_and_cancellation() {
        let tmp = sample();
        assert!(matches!(
            analyze(&AnalyzeOptions::default(), &Progress::new()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            analyze(
                &AnalyzeOptions::for_path(tmp.path().join("missing")),
                &Progress::new()
            ),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            analyze(
                &AnalyzeOptions::for_path(tmp.path().join("photo.jpg")),
                &Progress::new()
            ),
            Err(Error::NotFound(_))
        ));
        let progress = Progress::new();
        progress.cancel();
        assert!(matches!(
            analyze(&AnalyzeOptions::for_path(tmp.path()), &progress),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn progress_counts_files_and_bytes() {
        let tmp = sample();
        let progress = Progress::new();
        let a = analyze(&detailed(tmp.path()), &progress).unwrap();
        assert_eq!(progress.done(), a.total_files);
        assert_eq!(progress.bytes(), a.total_bytes);
        assert_eq!(progress.fraction(), None);
    }

    #[test]
    fn result_serializes() {
        let tmp = sample();
        let a = analyze(&detailed(tmp.path()), &Progress::new()).unwrap();
        let json = serde_json::to_string(&a).unwrap();
        let back: Analysis = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn top_files_keeps_the_largest() {
        let mut top = TopFiles::new(3);
        for size in [5u64, 1, 9, 3, 7, 2, 8, 4, 6, 10] {
            if top.wants(size) {
                top.push(FileEntry {
                    path: PathBuf::from(format!("f{size}")),
                    size,
                    modified: None,
                    category: FileCategory::Other,
                });
            }
        }
        let sizes: Vec<u64> = top.into_sorted().iter().map(|f| f.size).collect();
        assert_eq!(sizes, vec![10, 9, 8]);
        assert!(!TopFiles::new(0).wants(u64::MAX));
    }
}
