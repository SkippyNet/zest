//! Empty folder finder.
//!
//! A folder counts as empty when it contains no files and no links at any
//! depth – only (possibly nested) empty folders. Only the topmost folder of
//! an empty tree is reported.

use crate::fastdir::{self, ATTR_HIDDEN, ATTR_SYSTEM, path_key};
use crate::pool::{FxHashSet, pool};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zest_core::delete::{DeleteReport, recycle, remove_empty_dirs};
use zest_core::walk::{is_link, tree_size};
use zest_core::{Error, Progress, Result, safety};

/// Where to look for empty folders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyFolderOptions {
    /// Folders or drives to search. The roots themselves are never reported.
    pub roots: Vec<PathBuf>,
    /// Folders to skip entirely.
    pub exclude: Vec<PathBuf>,
    /// Also look inside (and report) hidden folders. Default: off, because
    /// hidden folders usually belong to programs that expect them to exist.
    pub include_hidden: bool,
    /// Never descend into the Windows, Program Files and ProgramData
    /// folders, Recycle Bins or System Volume Information. Default: on.
    pub skip_system_folders: bool,
}

impl Default for EmptyFolderOptions {
    fn default() -> Self {
        EmptyFolderOptions {
            roots: Vec::new(),
            exclude: Vec::new(),
            include_hidden: false,
            skip_system_folders: true,
        }
    }
}

impl EmptyFolderOptions {
    /// Default options for the given roots.
    pub fn for_roots(roots: Vec<PathBuf>) -> Self {
        EmptyFolderOptions {
            roots,
            ..Self::default()
        }
    }
}

struct Ctx<'a> {
    options: &'a EmptyFolderOptions,
    progress: &'a Progress,
    exclude: FxHashSet<String>,
}

/// Outcome of scanning one folder.
struct Scan {
    /// The folder holds nothing but empty folders.
    empty: bool,
    /// Topmost empty folders found below (only when `empty` is false).
    found: Vec<PathBuf>,
}

fn scan(ctx: &Ctx<'_>, dir: &Path) -> Scan {
    let not_empty = Scan {
        empty: false,
        found: Vec::new(),
    };
    if ctx.progress.is_cancelled() {
        return not_empty;
    }
    let mut has_content = false;
    // (path, may be reported/removed)
    let mut subdirs: Vec<(PathBuf, bool)> = Vec::new();
    let readable = fastdir::read_dir_raw(dir, &mut |entry| {
        if entry.is_link() || !entry.is_dir() {
            has_content = true;
            return;
        }
        let hidden = entry.attributes & ATTR_HIDDEN != 0;
        let system = entry.attributes & ATTR_SYSTEM != 0;
        let skipped = system || (hidden && !ctx.options.include_hidden);
        subdirs.push((entry.path_in(dir), !skipped));
    })
    .is_ok();
    if !readable {
        return not_empty;
    }
    ctx.progress.inc(1);

    let results: Vec<(PathBuf, Option<Scan>)> = subdirs
        .into_par_iter()
        .map(|(path, eligible)| {
            let excluded = !eligible
                || safety::is_protected(&path)
                || (!ctx.exclude.is_empty() && ctx.exclude.contains(&path_key(&path)));
            let result = if excluded {
                None
            } else {
                Some(scan(ctx, &path))
            };
            (path, result)
        })
        .collect();

    let all_children_empty = results
        .iter()
        .all(|(_, r)| r.as_ref().is_some_and(|s| s.empty));
    if !has_content && all_children_empty {
        return Scan {
            empty: true,
            found: Vec::new(),
        };
    }
    let mut found = Vec::new();
    for (path, result) in results {
        match result {
            Some(scan) if scan.empty => found.push(path),
            Some(scan) => found.extend(scan.found),
            None => {}
        }
    }
    Scan {
        empty: false,
        found,
    }
}

/// Finds empty folders below `options.roots`, sorted by path.
///
/// Folders guarded by the safety rules, folders with the *system* attribute
/// and (by default) hidden folders are neither reported nor searched.
///
/// # Progress
/// `done` = folders examined, `total` = 0 (indeterminate).
pub fn find_empty_folders(
    options: &EmptyFolderOptions,
    progress: &Progress,
) -> Result<Vec<PathBuf>> {
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
    let mut exclude: FxHashSet<String> = options
        .exclude
        .iter()
        .map(|p| path_key(&fastdir::absolute(p)))
        .collect();
    if options.skip_system_folders {
        exclude.extend(crate::duplicates::system_folder_keys());
    }
    let ctx = Ctx {
        options,
        progress,
        exclude,
    };
    progress.set_total(0);
    progress.set_done(0);
    progress.set_message("Looking for empty folders");
    let mut found: Vec<PathBuf> = pool().install(|| {
        roots
            .par_iter()
            .flat_map_iter(|root| scan(&ctx, root).found)
            .collect()
    });
    progress.check()?;
    found.sort();
    progress.set_message(String::new());
    Ok(found)
}

/// Removes folders reported by [`find_empty_folders`].
///
/// Each folder is re-checked immediately before removal: if it has gained a
/// file or link in the meantime it is left alone and recorded as a failure.
/// With `to_recycle_bin` the folder can be restored from the Recycle Bin.
///
/// # Progress
/// `total` = folders requested, `done` = folders processed.
pub fn delete_empty_folders(
    folders: &[PathBuf],
    to_recycle_bin: bool,
    progress: &Progress,
) -> DeleteReport {
    let mut report = DeleteReport::default();
    progress.set_done(0);
    progress.set_total(folders.len() as u64);
    for folder in folders {
        if progress.is_cancelled() {
            break;
        }
        progress.inc(1);
        let Ok(meta) = std::fs::symlink_metadata(folder) else {
            continue; // already gone
        };
        if !meta.is_dir() || is_link(&meta) {
            report.fail(folder, "not a folder");
            continue;
        }
        if let Err(e) = safety::ensure_deletable(folder) {
            report.fail(folder, e);
            continue;
        }
        let size = tree_size(folder, None);
        if size.files > 0 {
            report.fail(folder, "the folder is no longer empty");
            continue;
        }
        let outcome = if to_recycle_bin {
            recycle(std::slice::from_ref(folder))
                .map(|()| size.dirs + 1)
                .map_err(|e| e.to_string())
        } else {
            let nested = remove_empty_dirs(folder);
            // remove_dir refuses non-empty folders, so a file created in the
            // last instant still cannot be lost.
            std::fs::remove_dir(folder)
                .map(|()| nested + 1)
                .map_err(|e| e.to_string())
        };
        match outcome {
            Ok(dirs) => report.dirs += dirs,
            Err(e) => report.fail(folder, e),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn find(root: &Path) -> Vec<PathBuf> {
        find_empty_folders(
            &EmptyFolderOptions::for_roots(vec![root.to_path_buf()]),
            &Progress::new(),
        )
        .unwrap()
    }

    fn sample() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        fs::create_dir_all(r.join("empty-tree/a/b")).unwrap();
        fs::create_dir_all(r.join("empty-tree/c")).unwrap();
        fs::create_dir_all(r.join("full/sub-empty")).unwrap();
        fs::create_dir_all(r.join("full/sub-full")).unwrap();
        fs::write(r.join("full/sub-full/file.txt"), b"x").unwrap();
        fs::create_dir_all(r.join("lonely")).unwrap();
        tmp
    }

    #[test]
    fn reports_only_topmost_empty_folders() {
        let tmp = sample();
        let r = tmp.path();
        assert_eq!(
            find(r),
            vec![
                r.join("empty-tree"),
                r.join("full").join("sub-empty"),
                r.join("lonely")
            ]
        );
        // A root that is itself empty reports nothing.
        assert!(find(&r.join("lonely")).is_empty());
    }

    #[test]
    fn hidden_links_and_exclusions() {
        let tmp = sample();
        let r = tmp.path();
        let out = std::process::Command::new("attrib")
            .arg("+h")
            .arg(r.join("lonely"))
            .output()
            .unwrap();
        assert!(out.status.success());
        // A junction is content: its parent is not empty.
        fs::create_dir_all(r.join("has-link")).unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(r.join("has-link").join("link"))
            .arg(r.join("full"))
            .output()
            .unwrap();
        assert!(out.status.success());

        let found = find(r);
        assert!(
            !found.contains(&r.join("lonely")),
            "hidden folders are skipped by default"
        );
        assert!(!found.contains(&r.join("has-link")));
        assert!(found.contains(&r.join("empty-tree")));

        let options = EmptyFolderOptions {
            include_hidden: true,
            exclude: vec![r.join("empty-tree")],
            ..EmptyFolderOptions::for_roots(vec![r.to_path_buf()])
        };
        let found = find_empty_folders(&options, &Progress::new()).unwrap();
        assert!(found.contains(&r.join("lonely")));
        assert!(!found.contains(&r.join("empty-tree")));
    }

    #[test]
    fn deletion_rechecks_emptiness() {
        let tmp = sample();
        let r = tmp.path();
        let found = find(r);
        // One of the folders gains a file after the scan.
        fs::write(r.join("empty-tree/a/b/late.txt"), b"surprise").unwrap();
        let progress = Progress::new();
        let report = delete_empty_folders(&found, false, &progress);
        assert_eq!(report.failed, 1);
        assert_eq!(report.failures[0].0, r.join("empty-tree"));
        assert_eq!(report.dirs, 2);
        assert!(r.join("empty-tree/a/b/late.txt").exists());
        assert!(!r.join("lonely").exists());
        assert!(!r.join("full/sub-empty").exists());
        assert!(r.join("full/sub-full/file.txt").exists());
        assert_eq!(progress.fraction(), Some(1.0));

        // Nested empty folders are removed together and counted.
        fs::remove_file(r.join("empty-tree/a/b/late.txt")).unwrap();
        let report = delete_empty_folders(&[r.join("empty-tree")], false, &Progress::new());
        assert_eq!((report.dirs, report.failed), (4, 0));
        assert!(!r.join("empty-tree").exists());

        // Missing folders are ignored; plain files are refused.
        let report = delete_empty_folders(
            &[r.join("gone"), r.join("full/sub-full/file.txt")],
            false,
            &Progress::new(),
        );
        assert_eq!((report.dirs, report.failed), (0, 1));
        assert!(r.join("full/sub-full/file.txt").exists());
    }

    #[test]
    fn errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            find_empty_folders(&EmptyFolderOptions::default(), &Progress::new()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            find_empty_folders(
                &EmptyFolderOptions::for_roots(vec![tmp.path().join("missing")]),
                &Progress::new()
            ),
            Err(Error::NotFound(_))
        ));
        let progress = Progress::new();
        progress.cancel();
        assert!(matches!(
            find_empty_folders(
                &EmptyFolderOptions::for_roots(vec![tmp.path().to_path_buf()]),
                &progress
            ),
            Err(Error::Cancelled)
        ));
    }
}
