//! Directory traversal that never follows junctions or symbolic links.
//!
//! Following a reparse point while deleting is how cleaners end up emptying
//! folders they were never pointed at. Everything here treats links as opaque
//! leaf entries.

use crate::{Progress, fastdir};
use rayon::prelude::*;
use std::fs::Metadata;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// True for any reparse point: links, but also cloud placeholders,
/// deduplicated or compressed files. Use [`is_link`] to decide whether an
/// entry redirects somewhere else.
pub fn is_reparse_point(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// True for junctions and symbolic links – entries that point at another
/// location and must never be followed while deleting or measuring.
pub fn is_link(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// True for a junction or a symbolic link whose target is a directory.
/// Such links are removed with `remove_dir`, which never touches the target.
pub fn is_link_to_dir(meta: &Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    meta.file_type().is_symlink_dir()
}

/// True for files whose contents live in the cloud (OneDrive "online-only"
/// and similar). Reading such a file triggers a download, so scanners that
/// hash or inspect contents should skip them.
pub fn is_cloud_placeholder(meta: &Metadata) -> bool {
    meta.file_attributes()
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

/// True when the read-only attribute is set.
pub fn is_read_only(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_READONLY != 0
}

/// True when the hidden attribute is set.
pub fn is_hidden(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

/// True when the system attribute is set.
pub fn is_system(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_SYSTEM != 0
}

/// What the visitor of [`walk_tree`] is looking at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A real directory, reported *before* its contents.
    Dir,
    /// A junction or symlink (to a file or a directory). Never descended into.
    Link,
}

/// Depth-first traversal of everything below `root` (the root itself is not
/// reported). Unreadable folders are skipped silently. Stops early when
/// `progress` is cancelled.
pub fn walk_tree(
    root: &Path,
    progress: Option<&Progress>,
    visit: &mut dyn FnMut(&Path, &Metadata, EntryKind),
) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if progress.is_some_and(|p| p.is_cancelled()) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // On Windows this metadata comes straight from the directory
            // listing and does not cost an extra system call.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            if is_link(&meta) {
                visit(&path, &meta, EntryKind::Link);
            } else if meta.is_dir() {
                visit(&path, &meta, EntryKind::Dir);
                stack.push(path);
            } else {
                visit(&path, &meta, EntryKind::File);
            }
        }
    }
}

/// Totals for a directory tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeSize {
    /// Sum of logical file sizes.
    pub bytes: u64,
    /// Number of files (links count as files of size zero).
    pub files: u64,
    /// Number of directories below the root.
    pub dirs: u64,
}

/// Measures everything below `root` without following links.
/// A `root` that is itself a file yields that file's size.
///
/// The tree is read in bulk and in parallel on the shared walk pool (see
/// [`crate::fastdir`]); the calling thread waits for the result.
pub fn tree_size(root: &Path, progress: Option<&Progress>) -> TreeSize {
    let mut size = TreeSize::default();
    match std::fs::symlink_metadata(root) {
        Ok(meta) if is_link(&meta) => {
            size.files = 1;
            return size;
        }
        Ok(meta) if meta.is_file() => {
            size.files = 1;
            size.bytes = meta.len();
            return size;
        }
        Ok(_) => {}
        Err(_) => return size,
    }
    let root = fastdir::absolute(root);
    fastdir::pool().install(|| size_below(&root, progress))
}

impl TreeSize {
    fn plus(self, other: TreeSize) -> TreeSize {
        TreeSize {
            bytes: self.bytes + other.bytes,
            files: self.files + other.files,
            dirs: self.dirs + other.dirs,
        }
    }
}

/// Totals of one real folder: its own entries plus, in parallel, its
/// sub-folders. Links are counted and left alone.
fn size_below(dir: &Path, progress: Option<&Progress>) -> TreeSize {
    let mut size = TreeSize::default();
    if progress.is_some_and(|p| p.is_cancelled()) {
        return size;
    }
    let mut subdirs: Vec<PathBuf> = Vec::new();
    // An unreadable folder counts as empty, as it always has.
    let _ = fastdir::read_dir_raw(dir, &mut |entry| {
        if entry.is_link() {
            size.files += 1;
        } else if entry.is_dir() {
            size.dirs += 1;
            subdirs.push(entry.path_in(dir));
        } else {
            size.files += 1;
            size.bytes += entry.size;
        }
    });
    let below = if subdirs.len() < 2 {
        subdirs
            .iter()
            .map(|sub| size_below(sub, progress))
            .fold(TreeSize::default(), TreeSize::plus)
    } else {
        subdirs
            .par_iter()
            .map(|sub| size_below(sub, progress))
            .reduce(TreeSize::default, TreeSize::plus)
    };
    size.plus(below)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sizes_and_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("one.bin"), vec![0u8; 100]).unwrap();
        fs::write(root.join("a/two.bin"), vec![0u8; 250]).unwrap();
        fs::write(root.join("a/b/three.bin"), vec![0u8; 650]).unwrap();

        let size = tree_size(root, None);
        assert_eq!(
            size,
            TreeSize {
                bytes: 1000,
                files: 3,
                dirs: 2
            }
        );

        let mut dirs = 0;
        let mut files = 0;
        walk_tree(root, None, &mut |_, _, kind| match kind {
            EntryKind::Dir => dirs += 1,
            EntryKind::File => files += 1,
            EntryKind::Link => {}
        });
        assert_eq!((dirs, files), (2, 3));

        assert_eq!(tree_size(&root.join("one.bin"), None).bytes, 100);
        assert_eq!(tree_size(&root.join("missing"), None), TreeSize::default());
    }

    #[test]
    fn does_not_descend_into_junctions() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        let inside = tmp.path().join("inside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&inside).unwrap();
        fs::write(outside.join("precious.txt"), b"keep me").unwrap();

        // Junctions need no special privilege, unlike symlinks.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(inside.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink failed");

        let mut seen = Vec::new();
        walk_tree(&inside, None, &mut |p, _, kind| {
            seen.push((p.file_name().unwrap().to_string_lossy().into_owned(), kind));
        });
        assert_eq!(seen, vec![("link".to_owned(), EntryKind::Link)]);
        assert_eq!(tree_size(&inside, None).bytes, 0);
    }
}

#[cfg(test)]
mod fast_walk_tests {
    use super::*;
    use std::fs;

    fn junction(link: &Path, target: &Path) {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink failed");
    }

    /// The reference: the plain, one-entry-at-a-time walk.
    fn slow_size(root: &Path) -> TreeSize {
        let mut size = TreeSize::default();
        walk_tree(root, None, &mut |_, meta, kind| match kind {
            EntryKind::File => {
                size.files += 1;
                size.bytes += meta.len();
            }
            EntryKind::Link => size.files += 1,
            EntryKind::Dir => size.dirs += 1,
        });
        size
    }

    #[test]
    fn agrees_with_the_plain_walk_on_a_wide_and_deep_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut expected_bytes = 0u64;
        for a in 0..12 {
            for b in 0..6 {
                let dir = root.join(format!("a{a}")).join(format!("b{b}")).join("c");
                fs::create_dir_all(&dir).unwrap();
                for f in 0..5 {
                    let len = (a * 37 + b * 11 + f) as usize;
                    fs::write(dir.join(format!("f{f}.bin")), vec![0u8; len]).unwrap();
                    expected_bytes += len as u64;
                }
            }
        }
        fs::create_dir(root.join("empty")).unwrap();
        let fast = tree_size(root, None);
        assert_eq!(fast, slow_size(root));
        assert_eq!(fast.bytes, expected_bytes);
        assert_eq!(fast.files, 12 * 6 * 5);
        assert_eq!(fast.dirs, 12 + 12 * 6 * 2 + 1);
    }

    #[test]
    fn a_junction_deep_inside_is_counted_but_never_entered() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(outside.join("more")).unwrap();
        fs::write(outside.join("precious.bin"), vec![0u8; 5000]).unwrap();
        fs::write(outside.join("more").join("also.bin"), vec![0u8; 7000]).unwrap();

        let inside = tmp.path().join("inside");
        let nested = inside.join("x").join("y");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("own.bin"), vec![0u8; 10]).unwrap();
        // Two siblings so the parallel branch is taken as well.
        fs::create_dir_all(inside.join("x").join("z")).unwrap();
        junction(&nested.join("link"), &outside);

        let size = tree_size(&inside, None);
        assert_eq!(size.bytes, 10, "nothing behind the junction is measured");
        assert_eq!(size.files, 2, "the file and the link itself");
        assert_eq!(size.dirs, 3);
        assert_eq!(size, slow_size(&inside));

        // A junction as the root is one link, not a folder to measure.
        let root_link = tmp.path().join("root-link");
        junction(&root_link, &outside);
        assert_eq!(
            tree_size(&root_link, None),
            TreeSize {
                bytes: 0,
                files: 1,
                dirs: 0
            }
        );
    }

    #[test]
    fn paths_beyond_260_characters_and_unicode_names_are_measured() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        for i in 0..10 {
            deep.push(format!("ein-ziemlich-langer-ordnername-{i}-日本語-🍊"));
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("größe.dat"), vec![0u8; 4321]).unwrap();
        assert!(deep.as_os_str().len() > 260);
        let size = tree_size(tmp.path(), None);
        assert_eq!(size.bytes, 4321);
        assert_eq!(size.files, 1);
        assert_eq!(size.dirs, 10);
        assert_eq!(size, slow_size(tmp.path()));
    }

    #[test]
    fn a_folder_that_cannot_be_read_counts_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("visible.bin"), vec![0u8; 100]).unwrap();
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden.bin"), vec![0u8; 900]).unwrap();
        let user = std::env::var("USERNAME").unwrap_or_default();
        let deny = std::process::Command::new("icacls")
            .arg(&locked)
            .args(["/deny", &format!("{user}:(RD)")])
            .output();
        let denied = deny.is_ok_and(|o| o.status.success()) && fs::read_dir(&locked).is_err();

        let size = tree_size(tmp.path(), None);

        // Always give the permission back so the temp folder can be removed.
        let _ = std::process::Command::new("icacls")
            .arg(&locked)
            .args(["/remove:d", &user])
            .output();
        if !denied {
            // Could not take the permission away (unusual account set-up):
            // nothing to check here.
            return;
        }
        assert_eq!(size.bytes, 100, "the unreadable folder adds nothing");
        assert_eq!(size.files, 1);
        assert_eq!(size.dirs, 1, "it is still counted as a folder");
    }

    #[test]
    fn a_cancelled_walk_stops_and_reports_less() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..20 {
            let dir = tmp.path().join(format!("d{i}"));
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("f.bin"), vec![0u8; 10]).unwrap();
        }
        let progress = Progress::new();
        progress.cancel();
        assert_eq!(tree_size(tmp.path(), Some(&progress)), TreeSize::default());
        assert_eq!(tree_size(tmp.path(), None).bytes, 200);
    }

    #[test]
    fn works_when_called_from_inside_the_walk_pool_and_from_plain_threads() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..8 {
            let dir = tmp.path().join(format!("d{i}"));
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("f.bin"), vec![0u8; 25]).unwrap();
        }
        let root = tmp.path().to_path_buf();
        // Nested use: scanners call tree_size from their own pool jobs.
        let inside: Vec<u64> = fastdir::pool().install(|| {
            (0..16)
                .into_par_iter()
                .map(|_| tree_size(&root, None).bytes)
                .collect()
        });
        assert!(inside.iter().all(|&bytes| bytes == 200));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || tree_size(&root, None).bytes)
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 200);
        }
    }
}
