//! End-to-end tests of the file backup engine. Everything happens inside
//! temporary folders.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;
use zest_backup::{
    BackupOptions, BackupReport, EntryKind, Overwrite, Repository, RestoreOptions, RestoreTarget,
    Selection, SkipReason, Snapshot, VerifyScope, collect_garbage, create_snapshot,
    delete_snapshot, discard_damaged, prune, reference_counts, restore, stats, verify,
};
use zest_core::{Error, Progress};

/// Small chunks so multi-chunk files stay small.
const CHUNK: usize = 1024;

/// Bytes that look random but are the same on every run.
fn noise(seed: &str, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed.as_bytes());
    hasher.finalize_xof().fill(&mut out);
    out
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn set_modified(path: &Path, seconds_ago: u64) {
    let time = SystemTime::now() - Duration::from_secs(seconds_ago);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn attrib(flags: &str, path: &Path) {
    let status = std::process::Command::new("attrib.exe")
        .arg(flags)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "attrib {flags} failed");
}

/// Relative path -> contents (folders map to `None`).
fn tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn visit(dir: &Path, root: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.insert(rel, None);
                visit(&path, root, out);
            } else {
                out.insert(rel, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

struct Fixture {
    _tmp: TempDir,
    source: PathBuf,
    dest: PathBuf,
    out: PathBuf,
    repo: Repository,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical so comparisons with what the engine records line up.
        let base = tmp.path().to_path_buf();
        let source = base.join("Documents");
        let dest = base.join("BackupDrive");
        let out = base.join("Restored");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();
        let repo = Repository::open_or_create_with(&dest, CHUNK).unwrap();
        Fixture {
            _tmp: tmp,
            source,
            dest,
            out,
            repo,
        }
    }

    /// A small but varied set of files.
    fn populate(&self) {
        let s = &self.source;
        write(&s.join("notes.txt"), b"remember the milk\r\n");
        write(&s.join("empty.dat"), b"");
        write(&s.join("big.bin"), &noise("big", CHUNK * 5 + 300));
        write(&s.join(r"photos\2026\beach.jpg"), &noise("beach", 3000));
        write(
            &s.join(r"photos\2026\copy of beach.jpg"),
            &noise("beach", 3000),
        );
        write(
            &s.join("Übersicht – ελληνικά 日本語.txt"),
            "größe".as_bytes(),
        );
        write(&s.join(r"letters\draft.txt"), &b"Dear reader, ".repeat(400));
        fs::create_dir_all(s.join(r"empty folder\inner")).unwrap();
    }

    fn options(&self) -> BackupOptions {
        BackupOptions {
            sources: vec![self.source.clone()],
            ..BackupOptions::default()
        }
    }

    fn backup(&self) -> BackupReport {
        create_snapshot(&self.repo, &self.options(), &Progress::new()).unwrap()
    }

    fn snapshot(&self, id: &str) -> Snapshot {
        Snapshot::load(&self.repo, id).unwrap()
    }

    fn restore_all(&self, id: &str, overwrite: Overwrite) -> zest_backup::RestoreReport {
        restore(
            &self.repo,
            &self.snapshot(id),
            &RestoreOptions {
                target: RestoreTarget::Folder(self.out.clone()),
                overwrite,
                selection: Vec::new(),
            },
            &Progress::new(),
        )
        .unwrap()
    }
}

/// Snapshot identifiers have one-second resolution; the engine appends a
/// counter when two fall into the same second, so no waiting is needed.
#[test]
fn a_snapshot_restores_to_an_identical_tree() {
    let fx = Fixture::new();
    fx.populate();
    attrib("+h", &fx.source.join("notes.txt"));
    attrib("+r", &fx.source.join(r"letters\draft.txt"));
    set_modified(&fx.source.join("big.bin"), 86_400 * 400);

    let report = fx.backup();
    let info = &report.snapshot;
    assert_eq!(info.files, 7);
    assert_eq!(info.dirs, 5);
    assert_eq!(info.skipped, 0);
    assert_eq!(report.read_files, 7);
    assert!(info.stored_bytes > 0);
    assert!(
        info.new_bytes < info.bytes,
        "the copy of the photo is stored once: {} new of {}",
        info.new_bytes,
        info.bytes
    );

    let restored = fx.restore_all(&info.id, Overwrite::Skip);
    assert_eq!(restored.files, 7);
    assert!(restored.failed.is_empty(), "{:?}", restored.failed);
    assert!(restored.kept_existing.is_empty());

    let back = fx.out.join("Documents");
    assert_eq!(tree(&fx.source), tree(&back));

    // Dates and attributes come back too.
    let original = fs::metadata(fx.source.join("big.bin")).unwrap();
    let copy = fs::metadata(back.join("big.bin")).unwrap();
    assert_eq!(original.last_write_time(), copy.last_write_time());
    assert_eq!(original.creation_time(), copy.creation_time());
    let hidden = fs::metadata(back.join("notes.txt")).unwrap();
    assert_ne!(hidden.file_attributes() & 0x2, 0, "hidden survives");
    let read_only = fs::metadata(back.join(r"letters\draft.txt")).unwrap();
    assert!(read_only.permissions().readonly(), "read-only survives");

    // No temporary files are left anywhere.
    assert_eq!(fs::read_dir(fx.repo.root().join("tmp")).unwrap().count(), 0);
    assert!(
        tree(&back).keys().all(|name| !name.ends_with(".tmp")),
        "restore leaves no temporary files"
    );
    // So the temp dir can be removed.
    attrib("-r", &fx.source.join(r"letters\draft.txt"));
    attrib("-r", &back.join(r"letters\draft.txt"));
}

#[test]
fn unchanged_files_are_not_read_again() {
    let fx = Fixture::new();
    fx.populate();
    let first = fx.backup();

    let second = fx.backup();
    assert_ne!(first.snapshot.id, second.snapshot.id);
    assert_eq!(second.read_files, 0);
    assert_eq!(second.snapshot.reused_files, 7);
    assert_eq!(second.snapshot.new_bytes, 0);
    assert_eq!(second.snapshot.stored_bytes, 0);
    assert_eq!(second.snapshot.bytes, first.snapshot.bytes);

    // Change one file and add one.
    write(
        &fx.source.join("notes.txt"),
        b"remember the milk and the bread\r\n",
    );
    write(&fx.source.join("new.txt"), b"brand new");
    let third = fx.backup();
    assert_eq!(third.read_files, 2);
    assert_eq!(third.snapshot.reused_files, 6);
    assert_eq!(third.snapshot.files, 8);
    assert_eq!(
        third.snapshot.new_bytes,
        (b"remember the milk and the bread\r\n".len() + b"brand new".len()) as u64
    );

    // Every snapshot still restores its own version.
    let old = fx.restore_all(&first.snapshot.id, Overwrite::Skip);
    assert!(old.failed.is_empty());
    assert_eq!(
        fs::read(fx.out.join(r"Documents\notes.txt")).unwrap(),
        b"remember the milk\r\n"
    );
    assert!(!fx.out.join(r"Documents\new.txt").exists());
}

#[test]
fn only_the_changed_part_of_a_large_file_is_stored_again() {
    let fx = Fixture::new();
    let big = fx.source.join("disk.img");
    let mut data = noise("image", CHUNK * 8);
    write(&big, &data);
    let first = fx.backup();
    assert_eq!(first.snapshot.new_bytes, (CHUNK * 8) as u64);

    // Flip one byte in the fourth chunk.
    data[CHUNK * 3 + 17] ^= 0xFF;
    write(&big, &data);
    let second = fx.backup();
    assert_eq!(second.read_files, 1);
    assert_eq!(second.snapshot.new_bytes, CHUNK as u64, "one chunk is new");

    let counts = reference_counts(&fx.repo).unwrap();
    assert_eq!(counts.len(), 9, "eight chunks plus the changed one");
    assert_eq!(counts.values().filter(|n| **n == 2).count(), 7);

    fx.restore_all(&second.snapshot.id, Overwrite::Skip);
    assert_eq!(fs::read(fx.out.join(r"Documents\disk.img")).unwrap(), data);
}

#[test]
fn a_changed_file_with_an_unchanged_date_needs_a_full_reread() {
    let fx = Fixture::new();
    let file = fx.source.join("ledger.bin");
    write(&file, b"AAAA");
    let stamp = fs::metadata(&file).unwrap().modified().unwrap();
    fx.backup();

    write(&file, b"BBBB");
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    let quick = fx.backup();
    assert_eq!(
        quick.read_files, 0,
        "same size and date: taken as unchanged"
    );

    let thorough = create_snapshot(
        &fx.repo,
        &BackupOptions {
            reread_all: true,
            ..fx.options()
        },
        &Progress::new(),
    )
    .unwrap();
    assert_eq!(thorough.read_files, 1);
    fx.restore_all(&thorough.snapshot.id, Overwrite::Skip);
    assert_eq!(
        fs::read(fx.out.join(r"Documents\ledger.bin")).unwrap(),
        b"BBBB"
    );
}

#[test]
fn exclusions_leave_files_and_whole_folders_out() {
    let fx = Fixture::new();
    fx.populate();
    write(&fx.source.join("scratch.tmp"), b"x");
    write(&fx.source.join(r"project\node_modules\dep\index.js"), b"x");
    write(&fx.source.join(r"project\src\main.rs"), b"fn main() {}");
    write(&fx.source.join(r"project\target\debug\app.exe"), b"MZ");
    let options = BackupOptions {
        excludes: vec![
            "*.tmp".into(),
            "node_modules".into(),
            format!(r"{}\project\target", fx.source.display()),
        ],
        ..fx.options()
    };
    let report = create_snapshot(&fx.repo, &options, &Progress::new()).unwrap();
    let snapshot = fx.snapshot(&report.snapshot.id);
    let paths: Vec<&str> = snapshot
        .manifest()
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert!(paths.contains(&r"project\src\main.rs"));
    assert!(!paths.iter().any(|p| p.contains("node_modules")));
    assert!(!paths.iter().any(|p| p.contains("target")));
    assert!(!paths.iter().any(|p| p.ends_with(".tmp")));
    assert_eq!(report.snapshot.skipped, 0, "exclusions are not problems");
}

#[test]
fn links_are_recorded_as_skipped_and_never_followed() {
    let fx = Fixture::new();
    write(&fx.source.join("inside.txt"), b"inside");
    let outside = fx.dest.parent().unwrap().join("Elsewhere");
    write(&outside.join("secret.txt"), b"not part of the backup");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(fx.source.join("shortcut"))
        .arg(&outside)
        .output()
        .unwrap();
    assert!(status.status.success(), "mklink failed");

    let report = fx.backup();
    assert_eq!(report.snapshot.files, 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].reason, SkipReason::Link);
    assert!(report.skipped[0].path.ends_with("shortcut"));
    let snapshot = fx.snapshot(&report.snapshot.id);
    assert!(
        snapshot
            .manifest()
            .entries
            .iter()
            .all(|e| !e.path.contains("secret")),
        "nothing behind the link was backed up"
    );

    // A link as the source itself is refused outright.
    let refused = create_snapshot(
        &fx.repo,
        &BackupOptions {
            sources: vec![fx.source.join("shortcut")],
            ..BackupOptions::default()
        },
        &Progress::new(),
    );
    assert!(matches!(refused, Err(Error::Invalid(_))));
}

#[test]
fn a_file_in_use_is_reported_and_the_rest_is_backed_up() {
    let fx = Fixture::new();
    fx.populate();
    let locked = fx.source.join("database.lock");
    write(&locked, b"held by another program");
    let _holder = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&locked)
        .unwrap();

    let report = fx.backup();
    assert_eq!(report.snapshot.files, 7, "everything else is there");
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].reason, SkipReason::Unreadable);
    assert!(report.skipped[0].path.ends_with("database.lock"));
    assert!(!report.skipped[0].detail.is_empty());
    let record = fx.snapshot(&report.snapshot.id);
    assert_eq!(record.manifest().skipped.len(), 1, "kept with the snapshot");
}

#[test]
fn online_only_files_are_not_downloaded() {
    let fx = Fixture::new();
    write(&fx.source.join("local.txt"), b"here");
    let cloud = fx.source.join("in the cloud.docx");
    write(&cloud, b"placeholder");
    // The offline attribute is what cloud providers set on files whose
    // contents are not on this PC.
    attrib("+o", &cloud);

    let report = fx.backup();
    assert_eq!(report.snapshot.files, 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].reason, SkipReason::CloudOnly);
}

#[test]
fn unsafe_combinations_of_source_and_destination_are_refused() {
    let fx = Fixture::new();
    fx.populate();
    let progress = Progress::new();

    // Destination inside the source.
    let inner_dest = fx.source.join("Backups");
    fs::create_dir_all(&inner_dest).unwrap();
    let inner_repo = Repository::open_or_create_with(&inner_dest, CHUNK).unwrap();
    let result = create_snapshot(&inner_repo, &fx.options(), &progress);
    assert!(
        matches!(&result, Err(Error::Invalid(m)) if m.contains("inside")),
        "{result:?}"
    );
    assert!(inner_repo.snapshot_ids().unwrap().is_empty());

    // The same through a differently spelled path.
    let upper = PathBuf::from(fx.source.to_string_lossy().to_uppercase());
    let result = create_snapshot(
        &inner_repo,
        &BackupOptions {
            sources: vec![upper.join(".").join("photos").join("..")],
            ..BackupOptions::default()
        },
        &progress,
    );
    assert!(matches!(result, Err(Error::Invalid(_))));

    // Source inside the destination.
    let result = create_snapshot(
        &fx.repo,
        &BackupOptions {
            sources: vec![fx.repo.root().join("objects")],
            ..BackupOptions::default()
        },
        &progress,
    );
    assert!(matches!(result, Err(Error::Invalid(_))));

    // Nothing chosen, and something that is not there.
    assert!(matches!(
        create_snapshot(&fx.repo, &BackupOptions::default(), &progress),
        Err(Error::Invalid(_))
    ));
    let result = create_snapshot(
        &fx.repo,
        &BackupOptions {
            sources: vec![fx.source.join("no such folder")],
            ..BackupOptions::default()
        },
        &progress,
    );
    assert!(matches!(result, Err(Error::NotFound(_))));
    assert!(fx.repo.snapshot_ids().unwrap().is_empty());
}

#[test]
fn sources_get_unique_names_and_nested_ones_are_merged() {
    let fx = Fixture::new();
    let base = fx.source.parent().unwrap();
    let work = base.join(r"Work\Documents");
    write(&fx.source.join("home.txt"), b"home");
    write(&work.join("work.txt"), b"work");
    write(&fx.source.join(r"sub\deep.txt"), b"deep");
    let single = base.join("todo.txt");
    write(&single, b"one file");

    let report = create_snapshot(
        &fx.repo,
        &BackupOptions {
            sources: vec![
                fx.source.clone(),
                work.clone(),
                fx.source.join("sub"), // inside the first source
                fx.source.clone(),     // twice
                single.clone(),
            ],
            ..BackupOptions::default()
        },
        &Progress::new(),
    )
    .unwrap();
    assert_eq!(report.snapshot.sources.len(), 3);
    assert_eq!(report.snapshot.files, 4);

    let snapshot = fx.snapshot(&report.snapshot.id);
    let roots = snapshot.roots();
    let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Documents", "Documents (2)", "todo.txt"]);
    assert_eq!(roots[0].files, 2);
    assert_eq!(roots[2].kind, EntryKind::File);
    assert_eq!(roots[2].size, 8);

    // To a folder: one sub-folder per source, the single file by its name.
    let restored = fx.restore_all(&report.snapshot.id, Overwrite::Skip);
    assert!(restored.failed.is_empty());
    assert_eq!(
        fs::read(fx.out.join(r"Documents\home.txt")).unwrap(),
        b"home"
    );
    assert_eq!(
        fs::read(fx.out.join(r"Documents (2)\work.txt")).unwrap(),
        b"work"
    );
    assert_eq!(fs::read(fx.out.join("todo.txt")).unwrap(), b"one file");

    // Back to the original places.
    fs::remove_file(&single).unwrap();
    fs::remove_dir_all(&work).unwrap();
    let restored = restore(
        &fx.repo,
        &snapshot,
        &RestoreOptions {
            target: RestoreTarget::Original,
            overwrite: Overwrite::Skip,
            selection: Vec::new(),
        },
        &Progress::new(),
    )
    .unwrap();
    assert_eq!(restored.files, 2, "the two that were gone");
    assert_eq!(restored.unchanged, 2, "the two that were still there");
    assert_eq!(fs::read(&single).unwrap(), b"one file");
    assert_eq!(fs::read(work.join("work.txt")).unwrap(), b"work");
}

#[test]
fn existing_files_are_only_replaced_when_asked() {
    let fx = Fixture::new();
    write(&fx.source.join("report.txt"), b"backed up version");
    write(&fx.source.join("same.txt"), b"never changes");
    write(&fx.source.join("locked.txt"), b"backed up read-only target");
    let report = fx.backup();
    let snapshot = fx.snapshot(&report.snapshot.id);

    // Since the backup: one file edited, one made read-only and edited.
    write(&fx.source.join("report.txt"), b"edited after the backup!");
    write(&fx.source.join("locked.txt"), b"edited and locked");
    attrib("+r", &fx.source.join("locked.txt"));
    let to_original = |overwrite| {
        restore(
            &fx.repo,
            &snapshot,
            &RestoreOptions {
                target: RestoreTarget::Original,
                overwrite,
                selection: Vec::new(),
            },
            &Progress::new(),
        )
        .unwrap()
    };

    let kept = to_original(Overwrite::Skip);
    assert_eq!(kept.files, 0);
    assert_eq!(kept.unchanged, 1);
    assert_eq!(kept.kept_existing.len(), 2);
    assert_eq!(
        fs::read(fx.source.join("report.txt")).unwrap(),
        b"edited after the backup!",
        "nothing is overwritten by default"
    );

    let both = to_original(Overwrite::KeepBoth);
    assert_eq!(both.files, 2);
    assert_eq!(both.renamed.len(), 2);
    assert_eq!(
        fs::read(fx.source.join("report.txt")).unwrap(),
        b"edited after the backup!"
    );
    let beside = fx
        .source
        .join(format!("report (restored {}).txt", report.snapshot.id));
    assert_eq!(fs::read(&beside).unwrap(), b"backed up version");

    let replaced = to_original(Overwrite::Replace);
    assert_eq!(replaced.files, 2);
    assert!(replaced.failed.is_empty(), "{:?}", replaced.failed);
    assert_eq!(
        fs::read(fx.source.join("report.txt")).unwrap(),
        b"backed up version"
    );
    assert_eq!(
        fs::read(fx.source.join("locked.txt")).unwrap(),
        b"backed up read-only target",
        "a read-only file in the way is replaced too"
    );

    // Restoring again finds everything as it should be.
    let again = to_original(Overwrite::Replace);
    assert_eq!(again.files, 0);
    assert_eq!(again.unchanged, 3);
}

#[test]
fn a_folder_or_a_single_file_can_be_restored_on_its_own() {
    let fx = Fixture::new();
    fx.populate();
    let report = fx.backup();
    let snapshot = fx.snapshot(&report.snapshot.id);

    // Browsing: sources, then folders before files, with sizes rolled up.
    let top = snapshot.list(0, "");
    let names: Vec<&str> = top.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "empty folder",
            "letters",
            "photos",
            "big.bin",
            "empty.dat",
            "notes.txt",
            "Übersicht – ελληνικά 日本語.txt"
        ]
    );
    let photos = top.iter().find(|e| e.name == "photos").unwrap();
    assert_eq!((photos.size, photos.files), (6000, 2));
    assert_eq!(snapshot.list(0, r"photos\2026").len(), 2);
    assert!(snapshot.list(0, "no such folder").is_empty());
    assert_eq!(snapshot.roots()[0].size, report.snapshot.bytes);

    let pick = |path: &str| RestoreOptions {
        target: RestoreTarget::Folder(fx.out.clone()),
        overwrite: Overwrite::Skip,
        selection: vec![Selection {
            root: 0,
            path: path.into(),
        }],
    };
    let folder = restore(&fx.repo, &snapshot, &pick("photos"), &Progress::new()).unwrap();
    assert_eq!(folder.files, 2);
    let got = tree(&fx.out);
    assert!(got.contains_key(r"Documents\photos\2026\beach.jpg"));
    assert!(!got.contains_key(r"Documents\notes.txt"));

    let file = restore(&fx.repo, &snapshot, &pick("notes.txt"), &Progress::new()).unwrap();
    assert_eq!(file.files, 1);
    assert!(fx.out.join(r"Documents\notes.txt").is_file());
    assert!(!fx.out.join(r"Documents\big.bin").exists());

    let empty = restore(&fx.repo, &snapshot, &pick("empty folder"), &Progress::new()).unwrap();
    assert_eq!(empty.files, 0);
    assert!(fx.out.join(r"Documents\empty folder\inner").is_dir());

    let missing = restore(&fx.repo, &snapshot, &pick("nope"), &Progress::new());
    assert!(matches!(missing, Err(Error::NotFound(_))));
}

/// Finds the stored objects of one file by walking the repository.
fn object_files(repo: &Repository) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for prefix in fs::read_dir(repo.root().join("objects")).unwrap().flatten() {
        for object in fs::read_dir(prefix.path()).unwrap().flatten() {
            files.push(object.path());
        }
    }
    files.sort();
    files
}

#[test]
fn verification_finds_damaged_and_missing_data() {
    let fx = Fixture::new();
    write(&fx.source.join("a.bin"), &noise("a", CHUNK * 2));
    write(&fx.source.join("b.bin"), &noise("b", 500));
    write(&fx.source.join("c.txt"), b"small and fine");
    let report = fx.backup();

    let clean = verify(&fx.repo, &VerifyScope::All, &Progress::new()).unwrap();
    assert!(clean.is_ok());
    assert_eq!(clean.snapshots, 1);
    assert_eq!(clean.objects, 4);
    assert_eq!(clean.bytes, (CHUNK * 2 + 500 + 14) as u64);

    // Damage one object and delete another.
    let snapshot = fx.snapshot(&report.snapshot.id);
    let chunk_of = |name: &str| {
        let entry = snapshot
            .manifest()
            .entries
            .iter()
            .find(|e| e.path == name)
            .unwrap();
        let hex = entry.chunks[0].to_hex();
        fx.repo
            .root()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
    };
    let damaged = chunk_of("a.bin");
    let mut bytes = fs::read(&damaged).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    fs::write(&damaged, bytes).unwrap();
    fs::remove_file(chunk_of("b.bin")).unwrap();

    let scope = VerifyScope::Snapshot(report.snapshot.id.clone());
    let found = verify(&fx.repo, &scope, &Progress::new()).unwrap();
    assert!(!found.is_ok());
    assert_eq!((found.damaged, found.missing), (1, 1));
    assert_eq!(found.damaged_file_count, 2);
    let mut names: Vec<&str> = found
        .damaged_files
        .iter()
        .map(|f| f.path.rsplit('\\').next().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["a.bin", "b.bin"]);
    assert!(
        found.damaged_files[0]
            .path
            .starts_with(&*fx.source.to_string_lossy())
    );

    // Restoring writes the intact file and reports the others, without
    // leaving damaged output behind.
    let restored = fx.restore_all(&report.snapshot.id, Overwrite::Skip);
    assert_eq!(restored.files, 1);
    assert_eq!(restored.failed.len(), 2);
    assert_eq!(
        tree(&fx.out.join("Documents")).keys().collect::<Vec<_>>(),
        ["c.txt"]
    );

    // Damaged data is discarded, and the next backup stores both files again
    // from the copies that are still on the PC.
    assert_eq!(found.damaged_objects.len(), 1);
    assert_eq!(discard_damaged(&fx.repo, &found).unwrap(), 1);
    assert_eq!(
        discard_damaged(&fx.repo, &found).unwrap(),
        0,
        "already gone"
    );
    let healed = fx.backup();
    assert_eq!(
        healed.read_files, 2,
        "the two affected files are read again"
    );
    let after = verify(
        &fx.repo,
        &VerifyScope::Snapshot(healed.snapshot.id),
        &Progress::new(),
    )
    .unwrap();
    assert!(after.is_ok());
    assert!(
        verify(&fx.repo, &VerifyScope::All, &Progress::new())
            .unwrap()
            .is_ok()
    );

    assert!(matches!(
        verify(
            &fx.repo,
            &VerifyScope::Snapshot("20000101-000000".into()),
            &Progress::new()
        ),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn removing_a_snapshot_frees_only_what_nothing_else_uses() {
    let fx = Fixture::new();
    write(&fx.source.join("shared.bin"), &noise("shared", CHUNK * 3));
    write(&fx.source.join("old.bin"), &noise("old", CHUNK * 2));
    let first = fx.backup();

    fs::remove_file(fx.source.join("old.bin")).unwrap();
    write(&fx.source.join("new.bin"), &noise("new", CHUNK));
    let second = fx.backup();
    assert_eq!(object_files(&fx.repo).len(), 6);

    let before = stats(&fx.repo).unwrap();
    assert_eq!(before.snapshots, 2);
    assert_eq!(before.objects, 6);
    assert_eq!(before.latest_bytes, (CHUNK * 4) as u64);
    assert_eq!(before.logical_bytes, (CHUNK * 9) as u64);
    assert!(before.savings_ratio().unwrap() > 1.4);
    assert_eq!(before.newest, Some(second.snapshot.created));

    let freed = delete_snapshot(&fx.repo, &first.snapshot.id, &Progress::new()).unwrap();
    assert_eq!(freed.objects_removed, 2, "only old.bin's chunks go");
    assert!(freed.bytes_freed >= (CHUNK * 2) as u64);
    assert_eq!(object_files(&fx.repo).len(), 4);
    assert_eq!(fx.repo.list_snapshots().unwrap().len(), 1);

    // What is left is complete.
    assert!(
        verify(&fx.repo, &VerifyScope::All, &Progress::new())
            .unwrap()
            .is_ok()
    );
    let restored = fx.restore_all(&second.snapshot.id, Overwrite::Skip);
    assert_eq!(restored.files, 2);
    assert_eq!(tree(&fx.source), tree(&fx.out.join("Documents")));

    assert!(matches!(
        delete_snapshot(&fx.repo, &first.snapshot.id, &Progress::new()),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        delete_snapshot(&fx.repo, r"..\..\zest-backup", &Progress::new()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn pruning_keeps_the_newest_snapshots() {
    let fx = Fixture::new();
    let mut ids = Vec::new();
    for round in 0..5 {
        write(
            &fx.source.join("journal.txt"),
            format!("entry {round}").repeat(50).as_bytes(),
        );
        ids.push(fx.backup().snapshot.id);
    }
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "identifiers sort in the order they were made");

    assert_eq!(
        prune(&fx.repo, 0, &Progress::new()).unwrap().removed.len(),
        0
    );
    assert_eq!(
        prune(&fx.repo, 9, &Progress::new()).unwrap().removed.len(),
        0
    );

    let report = prune(&fx.repo, 2, &Progress::new()).unwrap();
    assert_eq!(report.removed, ids[..3]);
    assert_eq!(report.gc.objects_removed, 3);
    let left: Vec<String> = fx
        .repo
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(left, [ids[4].clone(), ids[3].clone()], "newest first");
    assert!(
        verify(&fx.repo, &VerifyScope::All, &Progress::new())
            .unwrap()
            .is_ok()
    );
}

#[test]
fn clean_up_removes_leftovers_but_never_guesses() {
    let fx = Fixture::new();
    fx.populate();
    let report = fx.backup();
    let objects_before = object_files(&fx.repo).len();

    // Leftovers of an interrupted run: a stray temporary file, and stored
    // data that no snapshot refers to (made by a cancelled backup).
    fs::write(fx.repo.root().join(r"tmp\123-4.tmp"), b"half written").unwrap();
    let progress = Progress::new();
    write(&fx.source.join("huge.bin"), &noise("huge", CHUNK * 40));
    progress.cancel();
    let cancelled = create_snapshot(&fx.repo, &fx.options(), &progress);
    assert!(matches!(cancelled, Err(Error::Cancelled)));
    assert_eq!(
        fx.repo.snapshot_ids().unwrap().len(),
        1,
        "no snapshot was recorded"
    );

    // An uncancelled run that dies before the record is written leaves data.
    let orphan = fx.repo.root().join(r"objects\zz");
    fs::create_dir_all(&orphan).unwrap();
    let stray_name = "0".repeat(62);
    fs::write(orphan.join(&stray_name), b"not an object name").unwrap();
    let real_orphan = fx.repo.root().join(r"objects\ab").join("c".repeat(62));
    write(&real_orphan, b"orphaned data");

    let gc = collect_garbage(&fx.repo, &Progress::new()).unwrap();
    assert_eq!(gc.objects_removed, 1, "the unreferenced object");
    assert!(!real_orphan.exists());
    assert!(
        orphan.join(&stray_name).exists(),
        "unknown files are left alone"
    );
    assert_eq!(fs::read_dir(fx.repo.root().join("tmp")).unwrap().count(), 0);
    assert_eq!(object_files(&fx.repo).len(), objects_before + 1);

    // With an unreadable snapshot record nothing may be removed.
    let record = fx
        .repo
        .root()
        .join("snapshots")
        .join(format!("{}.zmf", report.snapshot.id));
    let good = fs::read(&record).unwrap();
    fs::write(&record, b"garbage").unwrap();
    write(&real_orphan, b"orphaned data");
    let refused = collect_garbage(&fx.repo, &Progress::new());
    assert!(refused.is_err());
    assert!(real_orphan.exists(), "nothing was removed");
    assert_eq!(object_files(&fx.repo).len(), objects_before + 2);

    // The damaged snapshot is still listed, flagged, and verification names it.
    fs::remove_file(record.with_extension("json")).unwrap();
    let listed = fx.repo.list_snapshots().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].label.starts_with("Damaged"));
    let check = verify(&fx.repo, &VerifyScope::All, &Progress::new()).unwrap();
    assert_eq!(check.unreadable_snapshots.len(), 1);
    assert!(!check.is_ok());

    // Put the record back: everything works again, and the summary is rebuilt.
    fs::write(&record, good).unwrap();
    let listed = fx.repo.list_snapshots().unwrap();
    assert_eq!(listed[0], report.snapshot);
    assert!(record.with_extension("json").is_file());
    assert_eq!(
        collect_garbage(&fx.repo, &Progress::new())
            .unwrap()
            .objects_removed,
        1
    );
}

#[test]
fn a_cancelled_run_changes_nothing_and_the_next_one_succeeds() {
    let fx = Fixture::new();
    fx.populate();
    let progress = Progress::new();
    progress.cancel();
    assert!(matches!(
        create_snapshot(&fx.repo, &fx.options(), &progress),
        Err(Error::Cancelled)
    ));
    assert!(fx.repo.snapshot_ids().unwrap().is_empty());
    assert!(fx.repo.lock().is_ok(), "the lock was released");

    let report = fx.backup();
    assert_eq!(report.snapshot.files, 7);

    // A cancelled restore reports it and leaves no temporary files.
    let progress = Progress::new();
    progress.cancel();
    let result = restore(
        &fx.repo,
        &fx.snapshot(&report.snapshot.id),
        &RestoreOptions {
            target: RestoreTarget::Folder(fx.out.clone()),
            overwrite: Overwrite::Skip,
            selection: Vec::new(),
        },
        &progress,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    if fx.out.exists() {
        assert!(tree(&fx.out).keys().all(|name| !name.ends_with(".tmp")));
    }
}

#[test]
fn only_one_backup_runs_at_a_time() {
    let fx = Fixture::new();
    fx.populate();
    let held = fx.repo.lock().unwrap();
    let result = create_snapshot(&fx.repo, &fx.options(), &Progress::new());
    assert!(matches!(&result, Err(Error::Other(m)) if m.contains("in use")));
    assert!(collect_garbage(&fx.repo, &Progress::new()).is_err());
    drop(held);
    assert_eq!(fx.backup().snapshot.files, 7);
}

#[test]
fn very_long_paths_survive_the_round_trip() {
    let fx = Fixture::new();
    let mut deep = fx.source.clone();
    for level in 0..12 {
        deep = deep.join(format!("level {level:02} with a fairly long folder name"));
    }
    let file = deep.join("at the bottom – ünïcödé.txt");
    assert!(
        file.as_os_str().len() > 400,
        "longer than the classic limit"
    );
    write(&file, b"deep down");

    let report = fx.backup();
    assert_eq!(report.snapshot.files, 1);
    assert_eq!(report.snapshot.dirs, 12);
    assert_eq!(report.snapshot.skipped, 0);

    let restored = fx.restore_all(&report.snapshot.id, Overwrite::Skip);
    assert!(restored.failed.is_empty(), "{:?}", restored.failed);
    let rel = file.strip_prefix(&fx.source).unwrap();
    assert_eq!(
        fs::read(fx.out.join("Documents").join(rel)).unwrap(),
        b"deep down"
    );
}

#[test]
fn progress_counts_bytes() {
    let fx = Fixture::new();
    fx.populate();
    let progress = Progress::new();
    let report = create_snapshot(&fx.repo, &fx.options(), &progress).unwrap();
    assert_eq!(progress.total(), report.snapshot.bytes);
    assert_eq!(progress.done(), report.snapshot.bytes);
    assert_eq!(progress.fraction(), Some(1.0));

    let progress = Progress::new();
    restore(
        &fx.repo,
        &fx.snapshot(&report.snapshot.id),
        &RestoreOptions {
            target: RestoreTarget::Folder(fx.out.clone()),
            overwrite: Overwrite::Skip,
            selection: Vec::new(),
        },
        &progress,
    )
    .unwrap();
    assert_eq!(progress.done(), report.snapshot.bytes);
}

#[test]
fn an_existing_repository_is_found_again_from_the_destination() {
    let fx = Fixture::new();
    fx.populate();
    let report = fx.backup();

    assert!(Repository::exists(&fx.dest));
    let reopened = Repository::open(&fx.dest).unwrap();
    assert_eq!(
        reopened.chunk_size(),
        CHUNK,
        "settings come from the repository"
    );
    let listed = reopened.list_snapshots().unwrap();
    assert_eq!(listed, vec![report.snapshot.clone()]);
    assert_eq!(
        listed[0].sources,
        vec![fx.source.to_string_lossy().into_owned()]
    );
    assert!(listed[0].created > 1_700_000_000);
}

#[test]
fn each_source_is_compared_with_its_own_last_snapshot() {
    let fx = Fixture::new();
    let other = fx.source.parent().unwrap().join("Pictures");
    write(&fx.source.join("a.txt"), b"documents");
    write(&other.join("b.jpg"), &noise("b", 2000));
    let only = |path: &Path| BackupOptions {
        sources: vec![path.to_path_buf()],
        ..BackupOptions::default()
    };
    let run =
        |options: &BackupOptions| create_snapshot(&fx.repo, options, &Progress::new()).unwrap();

    assert_eq!(run(&only(&fx.source)).read_files, 1);
    assert_eq!(run(&only(&other)).read_files, 1);
    // The latest snapshot holds only Pictures; Documents is still recognised
    // as unchanged from the snapshot before it.
    let third = run(&only(&fx.source));
    assert_eq!(third.read_files, 0);
    assert_eq!(third.snapshot.reused_files, 1);

    let both = run(&BackupOptions {
        sources: vec![fx.source.clone(), other.clone()],
        ..BackupOptions::default()
    });
    assert_eq!(both.read_files, 0);
    assert_eq!(both.snapshot.reused_files, 2);
}
