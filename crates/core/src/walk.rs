//! Directory traversal that never follows junctions or symbolic links.
//!
//! Following a reparse point while deleting is how cleaners end up emptying
//! folders they were never pointed at. Everything here treats links as opaque
//! leaf entries.

use crate::Progress;
use std::fs::Metadata;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

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
    walk_tree(root, progress, &mut |_, meta, kind| match kind {
        EntryKind::File => {
            size.files += 1;
            size.bytes += meta.len();
        }
        EntryKind::Link => size.files += 1,
        EntryKind::Dir => size.dirs += 1,
    });
    size
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
