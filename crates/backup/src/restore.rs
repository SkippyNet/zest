//! Looking inside a snapshot and getting files back out of it.

use crate::create::KEPT_ATTRIBUTES;
use crate::manifest::{Entry, EntryKind, Manifest, RootKind, SnapshotInfo};
use crate::repo::Repository;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::{self, File, FileTimes};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileTimesExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use zest_core::{Error, IoContext, Progress, Result};

/// A snapshot loaded for browsing and restoring.
pub struct Snapshot {
    manifest: Manifest,
    /// Entry indexes by (root, parent folder path).
    children: HashMap<(u32, String), Vec<usize>>,
    /// Size and file count below each folder, by (root, folder path).
    totals: HashMap<(u32, String), (u64, u64)>,
}

/// One row when browsing a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseEntry {
    /// Index of the source this belongs to.
    pub root: u32,
    /// Path below the source (empty for the source itself).
    pub path: String,
    /// Name to show.
    pub name: String,
    pub kind: EntryKind,
    /// Size of the file, or of everything inside the folder.
    pub size: u64,
    /// Number of files inside (one for a file).
    pub files: u64,
    /// Last write time as a Windows file time (zero when unknown).
    pub modified: u64,
}

/// Where restored files go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreTarget {
    /// Back to where they were backed up from.
    Original,
    /// Into this folder, below a folder named after each source.
    Folder(PathBuf),
}

/// What happens when a file to restore already exists and differs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overwrite {
    /// Leave the existing file alone and report it.
    #[default]
    Skip,
    /// Replace the existing file with the backed-up one.
    Replace,
    /// Keep the existing file and write the backed-up one beside it under a
    /// new name.
    KeepBoth,
}

/// Part of a snapshot chosen for restoring: a source, or a folder or file in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Index of the source.
    pub root: u32,
    /// Path below the source; empty for the whole source.
    pub path: String,
}

/// What to restore and how.
#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub target: RestoreTarget,
    pub overwrite: Overwrite,
    /// What to restore. Empty means the whole snapshot.
    pub selection: Vec<Selection>,
}

/// A file that could not be restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreFailure {
    /// Where the file was meant to go.
    pub path: String,
    /// Why it did not get there.
    pub reason: String,
}

/// Result of a restore.
#[derive(Clone, Debug, Default)]
pub struct RestoreReport {
    /// Files written.
    pub files: u64,
    /// Bytes written.
    pub bytes: u64,
    /// Files that were already there with the same size and date.
    pub unchanged: u64,
    /// Existing files that were left alone because they differ
    /// ([`Overwrite::Skip`]).
    pub kept_existing: Vec<String>,
    /// Files written under a new name beside an existing one
    /// ([`Overwrite::KeepBoth`]).
    pub renamed: Vec<String>,
    /// Files that could not be restored.
    pub failed: Vec<RestoreFailure>,
}

fn parent_of(path: &str) -> &str {
    path.rfind('\\').map_or("", |i| &path[..i])
}

fn name_of(path: &str) -> &str {
    path.rfind('\\').map_or(path, |i| &path[i + 1..])
}

impl Snapshot {
    /// Loads a snapshot from `repo`.
    pub fn load(repo: &Repository, id: &str) -> Result<Snapshot> {
        Ok(Snapshot::from_manifest(repo.load_manifest(id)?))
    }

    /// Indexes a manifest that has already been read and validated.
    pub fn from_manifest(manifest: Manifest) -> Snapshot {
        let mut children: HashMap<(u32, String), Vec<usize>> = HashMap::new();
        let mut totals: HashMap<(u32, String), (u64, u64)> = HashMap::new();
        for (index, entry) in manifest.entries.iter().enumerate() {
            if entry.path.is_empty() {
                // A single-file source: the file is the source itself.
                totals.insert((entry.root, String::new()), (entry.size, 1));
                continue;
            }
            children
                .entry((entry.root, parent_of(&entry.path).to_owned()))
                .or_default()
                .push(index);
            if entry.kind == EntryKind::File {
                let mut dir = parent_of(&entry.path);
                loop {
                    let total = totals.entry((entry.root, dir.to_owned())).or_default();
                    total.0 += entry.size;
                    total.1 += 1;
                    if dir.is_empty() {
                        break;
                    }
                    dir = parent_of(dir);
                }
            }
        }
        Snapshot {
            manifest,
            children,
            totals,
        }
    }

    /// The summary of the snapshot.
    pub fn info(&self) -> &SnapshotInfo {
        &self.manifest.info
    }

    /// The full record.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The backed-up sources, as the top level of the snapshot.
    pub fn roots(&self) -> Vec<BrowseEntry> {
        self.manifest
            .roots
            .iter()
            .enumerate()
            .map(|(index, root)| {
                let root_index = index as u32;
                let (size, files) = self
                    .totals
                    .get(&(root_index, String::new()))
                    .copied()
                    .unwrap_or_default();
                BrowseEntry {
                    root: root_index,
                    path: String::new(),
                    name: root.name.clone(),
                    kind: match root.kind {
                        RootKind::Folder => EntryKind::Dir,
                        RootKind::File => EntryKind::File,
                    },
                    size,
                    files,
                    modified: 0,
                }
            })
            .collect()
    }

    /// What is directly inside folder `dir` of source `root`: folders first,
    /// then files, each group by name.
    pub fn list(&self, root: u32, dir: &str) -> Vec<BrowseEntry> {
        let Some(indexes) = self.children.get(&(root, dir.to_owned())) else {
            return Vec::new();
        };
        let mut rows: Vec<BrowseEntry> = indexes
            .iter()
            .map(|&index| {
                let entry = &self.manifest.entries[index];
                let (size, files) = match entry.kind {
                    EntryKind::File => (entry.size, 1),
                    EntryKind::Dir => self
                        .totals
                        .get(&(root, entry.path.clone()))
                        .copied()
                        .unwrap_or_default(),
                };
                BrowseEntry {
                    root,
                    path: entry.path.clone(),
                    name: name_of(&entry.path).to_owned(),
                    kind: entry.kind,
                    size,
                    files,
                    modified: entry.modified,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            (a.kind == EntryKind::File, a.name.to_lowercase())
                .cmp(&(b.kind == EntryKind::File, b.name.to_lowercase()))
        });
        rows
    }

    /// Where an entry lands for a given target.
    fn destination(&self, entry: &Entry, target: &RestoreTarget) -> PathBuf {
        let root = &self.manifest.roots[entry.root as usize];
        let base = match target {
            RestoreTarget::Original => PathBuf::from(&root.path),
            RestoreTarget::Folder(folder) => folder.join(&root.name),
        };
        if entry.path.is_empty() {
            base
        } else {
            base.join(&entry.path)
        }
    }
}

fn is_selected(entry: &Entry, selection: &[Selection]) -> bool {
    selection.is_empty()
        || selection.iter().any(|s| {
            s.root == entry.root
                && (s.path.is_empty()
                    || entry.path == s.path
                    || (entry.path.len() > s.path.len()
                        && entry.path.starts_with(&s.path)
                        && entry.path.as_bytes()[s.path.len()] == b'\\'))
        })
}

/// Converts a Windows file time to a [`SystemTime`].
pub fn filetime_to_system(ticks: u64) -> Option<SystemTime> {
    // 100 ns ticks between 1601-01-01 and 1970-01-01.
    const EPOCH_DIFFERENCE: u64 = 116_444_736_000_000_000;
    let since_unix = ticks.checked_sub(EPOCH_DIFFERENCE)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
}

fn set_attributes(path: &Path, attributes: u32) {
    use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, SetFileAttributesW};
    use windows::core::PCWSTR;
    if attributes & KEPT_ATTRIBUTES == 0 {
        return;
    }
    let Ok(absolute) = std::path::absolute(path) else {
        return;
    };
    // The verbatim prefix lifts the 260 character limit for this call.
    let mut wide: Vec<u16> = Vec::new();
    let text = absolute.as_os_str();
    let already_verbatim = text.to_string_lossy().starts_with(r"\\");
    if !already_verbatim {
        wide.extend(r"\\?\".encode_utf16());
    }
    wide.extend(text.encode_wide());
    wide.push(0);
    // SAFETY: `wide` is a valid NUL-terminated string that outlives the call.
    let result = unsafe {
        SetFileAttributesW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(attributes & KEPT_ATTRIBUTES),
        )
    };
    if let Err(e) = result {
        log::debug!("could not set attributes of {}: {e}", path.display());
    }
}

fn clear_read_only(path: &Path) {
    if let Ok(meta) = fs::metadata(path) {
        let mut permissions = meta.permissions();
        if permissions.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

/// A name beside `dest` that is not taken: `name (restored <tag>).ext`.
fn free_name_beside(dest: &Path, tag: &str) -> PathBuf {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = dest
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem} (restored {tag}){extension}")
        } else {
            format!("{stem} (restored {tag} {n}){extension}")
        };
        let candidate = dest.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

static RESTORE_TEMP: AtomicU64 = AtomicU64::new(0);

/// What became of one file.
enum FileOutcome {
    Written(u64),
    Unchanged,
    KeptExisting(String),
    Renamed(String, u64),
    Failed(RestoreFailure),
}

struct Restorer<'a> {
    repo: &'a Repository,
    snapshot: &'a Snapshot,
    options: &'a RestoreOptions,
    progress: &'a Progress,
}

impl Restorer<'_> {
    /// Writes the contents of `entry` to `dest` through a temporary file in
    /// the same folder, so `dest` is never left half-written.
    fn write_file(&self, entry: &Entry, dest: &Path) -> Result<u64> {
        let dir = dest
            .parent()
            .ok_or_else(|| Error::Invalid(format!("{} has no folder", dest.display())))?;
        fs::create_dir_all(dir).ctx(format!("creating {}", dir.display()))?;
        let tmp = dir.join(format!(
            "zr-{}-{}.tmp",
            std::process::id(),
            RESTORE_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> Result<u64> {
            let mut file = File::create(&tmp).ctx(format!("writing into {}", dir.display()))?;
            let mut written = 0u64;
            for id in &entry.chunks {
                self.progress.check()?;
                let data = self.repo.read_object(id)?;
                file.write_all(&data)
                    .ctx(format!("writing {}", dest.display()))?;
                written += data.len() as u64;
                self.progress.inc(data.len() as u64);
                self.progress.add_bytes(data.len() as u64);
            }
            if written != entry.size {
                return Err(Error::Invalid(format!(
                    "the stored copy of {} is incomplete",
                    dest.display()
                )));
            }
            let mut times = FileTimes::new();
            if let Some(modified) = filetime_to_system(entry.modified) {
                times = times.set_modified(modified);
            }
            if let Some(created) = filetime_to_system(entry.created) {
                times = times.set_created(created);
            }
            // Dates are a nicety; a file system that refuses them still gets the file.
            let _ = file.set_times(times);
            drop(file);
            if dest.exists() {
                clear_read_only(dest);
            }
            fs::rename(&tmp, dest).ctx(format!("writing {}", dest.display()))?;
            set_attributes(dest, entry.attributes);
            Ok(written)
        };
        let result = write();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    fn restore_file(&self, entry: &Entry) -> Result<FileOutcome> {
        let dest = self.snapshot.destination(entry, &self.options.target);
        let shown = dest.to_string_lossy().into_owned();
        let fail = |reason: String| {
            self.progress.inc(entry.size);
            Ok(FileOutcome::Failed(RestoreFailure {
                path: shown.clone(),
                reason,
            }))
        };

        let mut write_to = dest.clone();
        let mut renamed = false;
        match fs::symlink_metadata(&dest) {
            Ok(existing) if existing.is_dir() => {
                return fail("a folder with this name is in the way".into());
            }
            Ok(existing) => {
                let same = !existing.file_type().is_symlink()
                    && existing.file_size() == entry.size
                    && existing.last_write_time() == entry.modified;
                if same {
                    self.progress.inc(entry.size);
                    return Ok(FileOutcome::Unchanged);
                }
                match self.options.overwrite {
                    Overwrite::Skip => {
                        self.progress.inc(entry.size);
                        return Ok(FileOutcome::KeptExisting(shown));
                    }
                    Overwrite::Replace => {}
                    Overwrite::KeepBoth => {
                        write_to = free_name_beside(&dest, &self.snapshot.info().id);
                        renamed = true;
                    }
                }
            }
            Err(_) => {}
        }

        match self.write_file(entry, &write_to) {
            Ok(bytes) if renamed => Ok(FileOutcome::Renamed(
                write_to.to_string_lossy().into_owned(),
                bytes,
            )),
            Ok(bytes) => Ok(FileOutcome::Written(bytes)),
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Err(e) => fail(e.to_string()),
        }
    }
}

/// Restores a snapshot, or the selected part of it.
///
/// Every chunk is checked against its hash as it is read, so damaged backup
/// data is reported for the file it belongs to instead of being written out.
/// An existing file that differs from the backed-up one is only touched when
/// [`Overwrite::Replace`] was asked for. `progress` counts bytes; cancelling
/// keeps the files restored so far.
pub fn restore(
    repo: &Repository,
    snapshot: &Snapshot,
    options: &RestoreOptions,
    progress: &Progress,
) -> Result<RestoreReport> {
    if let RestoreTarget::Folder(folder) = &options.target {
        fs::create_dir_all(folder).ctx(format!("creating {}", folder.display()))?;
    }
    let selected: Vec<&Entry> = snapshot
        .manifest
        .entries
        .iter()
        .filter(|entry| is_selected(entry, &options.selection))
        .collect();
    if selected.is_empty() && !options.selection.is_empty() {
        return Err(Error::NotFound(
            "the selected items are not in this snapshot".into(),
        ));
    }
    progress.set_total(
        selected
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.size)
            .sum(),
    );
    progress.set_message("Restoring files");

    let mut report = RestoreReport::default();

    // Folders first, so empty ones come back too.
    let mut restored_dirs: Vec<(&Entry, PathBuf)> = Vec::new();
    for entry in selected.iter().filter(|e| e.kind == EntryKind::Dir) {
        progress.check()?;
        let dest = snapshot.destination(entry, &options.target);
        match fs::create_dir_all(&dest) {
            Ok(()) => restored_dirs.push((entry, dest)),
            Err(e) => report.failed.push(RestoreFailure {
                path: dest.to_string_lossy().into_owned(),
                reason: e.to_string(),
            }),
        }
    }

    let restorer = Restorer {
        repo,
        snapshot,
        options,
        progress,
    };
    let outcomes: Vec<Result<FileOutcome>> = selected
        .par_iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|entry| restorer.restore_file(entry))
        .collect();
    let mut cancelled = false;
    for outcome in outcomes {
        match outcome {
            Ok(FileOutcome::Written(bytes)) => {
                report.files += 1;
                report.bytes += bytes;
            }
            Ok(FileOutcome::Unchanged) => report.unchanged += 1,
            Ok(FileOutcome::KeptExisting(path)) => report.kept_existing.push(path),
            Ok(FileOutcome::Renamed(path, bytes)) => {
                report.files += 1;
                report.bytes += bytes;
                report.renamed.push(path);
            }
            Ok(FileOutcome::Failed(failure)) => report.failed.push(failure),
            Err(Error::Cancelled) => cancelled = true,
            Err(e) => return Err(e),
        }
    }
    if cancelled {
        return Err(Error::Cancelled);
    }

    // Folder attributes last: a hidden or read-only folder is still writable above.
    for (entry, dest) in restored_dirs {
        set_attributes(&dest, entry.attributes & !0x1);
    }
    report.kept_existing.sort();
    report.renamed.sort();
    report.failed.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: u32, path: &str) -> Entry {
        Entry {
            root,
            path: path.into(),
            kind: EntryKind::File,
            size: 1,
            modified: 0,
            created: 0,
            attributes: 0,
            chunks: Vec::new(),
        }
    }

    #[test]
    fn selection_matches_whole_path_parts() {
        let pick = |path: &str| {
            vec![Selection {
                root: 0,
                path: path.into(),
            }]
        };
        assert!(is_selected(&entry(0, r"a\b.txt"), &[]));
        assert!(is_selected(&entry(0, r"a\b.txt"), &pick("")));
        assert!(is_selected(&entry(0, r"a\b.txt"), &pick("a")));
        assert!(is_selected(&entry(0, r"a\b.txt"), &pick(r"a\b.txt")));
        assert!(!is_selected(&entry(0, r"ab\b.txt"), &pick("a")));
        assert!(!is_selected(&entry(1, r"a\b.txt"), &pick("a")));
        assert!(!is_selected(&entry(0, "a"), &pick(r"a\b.txt")));
    }

    #[test]
    fn file_times_convert() {
        // 2000-01-01T00:00:00Z as a Windows file time.
        let ticks = 125_911_584_000_000_000u64;
        let time = filetime_to_system(ticks).unwrap();
        let unix = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(unix, 946_684_800);
        assert_eq!(
            filetime_to_system(0),
            None,
            "before 1970 is not representable here"
        );
    }

    #[test]
    fn names_beside_existing_files_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("report.docx");
        let first = free_name_beside(&dest, "20260101-000000");
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            "report (restored 20260101-000000).docx"
        );
        fs::write(&first, b"x").unwrap();
        let second = free_name_beside(&dest, "20260101-000000");
        assert_eq!(
            second.file_name().unwrap().to_str().unwrap(),
            "report (restored 20260101-000000 2).docx"
        );
        let bare = free_name_beside(&tmp.path().join("README"), "t");
        assert_eq!(
            bare.file_name().unwrap().to_str().unwrap(),
            "README (restored t)"
        );
    }
}
