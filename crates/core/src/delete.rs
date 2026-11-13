//! File deletion: normal, to the Recycle Bin, or multi-pass secure overwrite.
//!
//! All functions run the [`crate::safety`] guard first and never follow
//! junctions or symbolic links.

use crate::safety::ensure_deletable;
use crate::walk::{is_link, is_link_to_dir, is_read_only};
use crate::{Error, IoContext, Progress, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// How thoroughly file contents are destroyed before the file is removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WipeMethod {
    /// Plain deletion (fastest; contents may be recoverable).
    #[default]
    None,
    /// One pass of random data.
    Simple,
    /// Three passes: zeros, ones, random (DoD 5220.22-M style).
    Dod3,
    /// Seven passes (DoD 5220.22-M ECE style).
    Advanced7,
    /// Thirty-five passes (Gutmann).
    Gutmann35,
}

#[derive(Clone, Copy)]
enum Pass {
    Random,
    Pattern([u8; 3]),
}

const fn byte(b: u8) -> Pass {
    Pass::Pattern([b, b, b])
}

impl WipeMethod {
    /// All methods in increasing order of thoroughness.
    pub const ALL: [WipeMethod; 5] = [
        WipeMethod::None,
        WipeMethod::Simple,
        WipeMethod::Dod3,
        WipeMethod::Advanced7,
        WipeMethod::Gutmann35,
    ];

    /// Number of overwrite passes.
    pub fn passes(self) -> u32 {
        self.plan().len() as u32
    }

    /// Short name for menus.
    pub fn label(self) -> &'static str {
        match self {
            WipeMethod::None => "Normal deletion (fastest)",
            WipeMethod::Simple => "Simple overwrite (1 pass)",
            WipeMethod::Dod3 => "Advanced overwrite (3 passes)",
            WipeMethod::Advanced7 => "Complex overwrite (7 passes)",
            WipeMethod::Gutmann35 => "Very complex overwrite (35 passes)",
        }
    }

    fn plan(self) -> Vec<Pass> {
        match self {
            WipeMethod::None => vec![],
            WipeMethod::Simple => vec![Pass::Random],
            WipeMethod::Dod3 => vec![byte(0x00), byte(0xFF), Pass::Random],
            WipeMethod::Advanced7 => vec![
                byte(0x00),
                byte(0xFF),
                Pass::Random,
                byte(0x96),
                byte(0x00),
                byte(0xFF),
                Pass::Random,
            ],
            WipeMethod::Gutmann35 => {
                let mut plan = vec![Pass::Random; 4];
                plan.extend([
                    byte(0x55),
                    byte(0xAA),
                    Pass::Pattern([0x92, 0x49, 0x24]),
                    Pass::Pattern([0x49, 0x24, 0x92]),
                    Pass::Pattern([0x24, 0x92, 0x49]),
                ]);
                plan.extend((0x0..=0xFu8).map(|n| byte(n * 0x11)));
                plan.extend([
                    Pass::Pattern([0x92, 0x49, 0x24]),
                    Pass::Pattern([0x49, 0x24, 0x92]),
                    Pass::Pattern([0x24, 0x92, 0x49]),
                    Pass::Pattern([0x6D, 0xB6, 0xDB]),
                    Pass::Pattern([0xB6, 0xDB, 0x6D]),
                    Pass::Pattern([0xDB, 0x6D, 0xB6]),
                ]);
                plan.extend([Pass::Random; 4]);
                plan
            }
        }
    }
}

/// Small, fast, non-cryptographic generator (xoshiro256++) used to produce
/// overwrite data. Seeded from the operating system via `RandomState`.
pub struct FastRng([u64; 4]);

impl Default for FastRng {
    fn default() -> Self {
        Self::new()
    }
}

impl FastRng {
    /// Creates a generator with an unpredictable seed.
    pub fn new() -> Self {
        use std::hash::{BuildHasher, Hasher};
        let mut seed = [0u64; 4];
        for (i, slot) in seed.iter_mut().enumerate() {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_usize(i);
            hasher.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            *slot = hasher.finish() | 1;
        }
        FastRng(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Fills `buf` with random bytes.
    pub fn fill(&mut self, buf: &mut [u8]) {
        let mut chunks = buf.chunks_exact_mut(8);
        for chunk in &mut chunks {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        let rest = chunks.into_remainder();
        if !rest.is_empty() {
            let bytes = self.next_u64().to_le_bytes();
            rest.copy_from_slice(&bytes[..rest.len()]);
        }
    }

    /// A random lower-case alphanumeric string of `len` characters.
    pub fn name(&mut self, len: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        (0..len)
            .map(|_| ALPHABET[(self.next_u64() % ALPHABET.len() as u64) as usize] as char)
            .collect()
    }
}

/// Options that apply to one deletion request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeleteOptions {
    /// Overwrite contents before removal.
    pub wipe: WipeMethod,
    /// Move to the Recycle Bin instead of deleting (ignores `wipe`).
    pub to_recycle_bin: bool,
    /// Report what would be deleted without touching anything.
    pub dry_run: bool,
}

impl DeleteOptions {
    /// Plain, permanent deletion.
    pub fn permanent() -> Self {
        Self::default()
    }

    /// Recoverable deletion via the Recycle Bin.
    pub fn recycle() -> Self {
        DeleteOptions {
            to_recycle_bin: true,
            ..Self::default()
        }
    }

    /// Secure deletion with the given overwrite method.
    pub fn secure(wipe: WipeMethod) -> Self {
        DeleteOptions {
            wipe,
            ..Self::default()
        }
    }

    /// Returns a copy with `dry_run` set.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }
}

/// Maximum number of individual failures remembered in a [`DeleteReport`].
pub const MAX_RECORDED_FAILURES: usize = 200;

/// Outcome of a batch of deletions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeleteReport {
    /// Files removed (or that would be removed in a dry run).
    pub files: u64,
    /// Folders removed.
    pub dirs: u64,
    /// Bytes freed.
    pub bytes: u64,
    /// Number of entries that could not be removed.
    pub failed: u64,
    /// The first [`MAX_RECORDED_FAILURES`] failures, for display.
    pub failures: Vec<(PathBuf, String)>,
}

impl DeleteReport {
    /// Records a failure.
    pub fn fail(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.failed += 1;
        if self.failures.len() < MAX_RECORDED_FAILURES {
            self.failures.push((path.to_path_buf(), error.to_string()));
        }
    }

    /// Adds another report into this one.
    pub fn merge(&mut self, other: DeleteReport) {
        self.files += other.files;
        self.dirs += other.dirs;
        self.bytes += other.bytes;
        self.failed += other.failed;
        for failure in other.failures {
            if self.failures.len() >= MAX_RECORDED_FAILURES {
                break;
            }
            self.failures.push(failure);
        }
    }
}

const WIPE_CHUNK: usize = 1024 * 1024;

/// Overwrites the first `len` bytes of `file` according to `method`.
///
/// `progress`, when given, has its byte counter advanced and is checked for
/// cancellation between chunks.
pub fn wipe_contents(
    file: &mut File,
    len: u64,
    method: WipeMethod,
    progress: Option<&Progress>,
) -> Result<()> {
    let plan = method.plan();
    if plan.is_empty() || len == 0 {
        return Ok(());
    }
    let mut rng = FastRng::new();
    let mut buf = vec![0u8; WIPE_CHUNK.min(len as usize).max(1)];
    for pass in plan {
        if let Pass::Pattern(pattern) = pass {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = pattern[i % 3];
            }
        }
        file.seek(SeekFrom::Start(0))
            .ctx("rewinding file for overwrite")?;
        let mut remaining = len;
        while remaining > 0 {
            if let Some(p) = progress {
                p.check()?;
            }
            let n = remaining.min(buf.len() as u64) as usize;
            if matches!(pass, Pass::Random) {
                rng.fill(&mut buf[..n]);
            }
            file.write_all(&buf[..n]).ctx("overwriting file contents")?;
            remaining -= n as u64;
            if let Some(p) = progress {
                p.add_bytes(n as u64);
            }
        }
        // Force each pass to the device; otherwise the cache would collapse
        // all passes into the final one.
        file.sync_data().ctx("flushing overwrite pass")?;
    }
    Ok(())
}

fn clear_read_only(path: &Path, meta: &fs::Metadata) {
    if is_read_only(meta) {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(path, perms);
    }
}

/// Deletes one file (or one link) and returns the number of bytes freed.
///
/// * Missing files are treated as success (`Ok(0)`).
/// * Directories are rejected; use [`delete_tree`].
/// * Links are removed without touching their target.
pub fn delete_file(path: &Path, opts: &DeleteOptions) -> Result<u64> {
    delete_file_with_progress(path, opts, None)
}

/// [`delete_file`] with byte-level progress for large secure wipes.
pub fn delete_file_with_progress(
    path: &Path,
    opts: &DeleteOptions,
    progress: Option<&Progress>,
) -> Result<u64> {
    ensure_deletable(path)?;
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).ctx(format!("inspecting {}", path.display())),
    };
    // `is_dir` is false for links, even links to directories.
    let link = is_link(&meta);
    if meta.is_dir() {
        return Err(Error::Invalid(format!("{} is a folder", path.display())));
    }
    let len = if link { 0 } else { meta.len() };
    if opts.dry_run {
        return Ok(len);
    }
    if opts.to_recycle_bin {
        recycle(&[path.to_path_buf()])?;
        return Ok(len);
    }

    clear_read_only(path, &meta);

    if link {
        // A link to a directory must be removed with remove_dir.
        let removed = if is_link_to_dir(&meta) {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        };
        removed.ctx(format!("removing link {}", path.display()))?;
        return Ok(0);
    }

    let mut target = path.to_path_buf();
    if opts.wipe != WipeMethod::None {
        let mut file = OpenOptions::new()
            .write(true)
            .open(path)
            .ctx(format!("opening {} for overwrite", path.display()))?;
        wipe_contents(&mut file, len, opts.wipe, progress)?;
        let _ = file.set_len(0);
        let _ = file.sync_all();
        drop(file);
        // Obscure the original name in the directory entry as well.
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            let scrambled = parent.join(FastRng::new().name(name.len().clamp(8, 32)));
            if !scrambled.exists() && fs::rename(path, &scrambled).is_ok() {
                target = scrambled;
            }
        }
    }
    fs::remove_file(&target).ctx(format!("deleting {}", path.display()))?;
    Ok(len)
}

/// Deletes everything below `root`, and `root` itself unless `keep_root`.
///
/// Failures never abort the batch; they are collected in `report`. Links are
/// removed without being followed. Honors cancellation through `progress`.
pub fn delete_tree(
    root: &Path,
    keep_root: bool,
    opts: &DeleteOptions,
    progress: Option<&Progress>,
    report: &mut DeleteReport,
) {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(_) => return,
    };

    // A file or a link: handle as a single entry.
    if !meta.is_dir() || is_link(&meta) {
        if keep_root {
            return;
        }
        match delete_file_with_progress(root, opts, progress) {
            Ok(bytes) => {
                report.files += 1;
                report.bytes += bytes;
            }
            Err(e) => report.fail(root, e),
        }
        return;
    }

    if !keep_root && let Err(e) = ensure_deletable(root) {
        report.fail(root, e);
        return;
    }

    // Recycling a whole folder is one shell operation.
    if opts.to_recycle_bin && !keep_root {
        let size = crate::walk::tree_size(root, progress);
        if opts.dry_run {
            report.files += size.files;
            report.dirs += size.dirs + 1;
            report.bytes += size.bytes;
            return;
        }
        match recycle(&[root.to_path_buf()]) {
            Ok(()) => {
                report.files += size.files;
                report.dirs += size.dirs + 1;
                report.bytes += size.bytes;
            }
            Err(e) => report.fail(root, e),
        }
        return;
    }

    delete_dir_recursive(root, opts, progress, report);

    if !keep_root && !opts.dry_run {
        match remove_dir_checked(root) {
            Ok(()) => report.dirs += 1,
            Err(e) => report.fail(root, e),
        }
    } else if !keep_root {
        report.dirs += 1;
    }
}

fn remove_dir_checked(dir: &Path) -> Result<()> {
    ensure_deletable(dir)?;
    if let Ok(meta) = fs::symlink_metadata(dir) {
        clear_read_only(dir, &meta);
    }
    fs::remove_dir(dir).ctx(format!("removing folder {}", dir.display()))
}

fn delete_dir_recursive(
    dir: &Path,
    opts: &DeleteOptions,
    progress: Option<&Progress>,
    report: &mut DeleteReport,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            report.fail(dir, e);
            return;
        }
    };
    for entry in entries.flatten() {
        if progress.is_some_and(|p| p.is_cancelled()) {
            return;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            report.fail(&path, "could not read attributes");
            continue;
        };
        if meta.is_dir() && !is_link(&meta) {
            delete_dir_recursive(&path, opts, progress, report);
            if opts.dry_run {
                report.dirs += 1;
            } else if opts.to_recycle_bin {
                // Contents were recycled individually; remove the now empty shell.
                if remove_dir_checked(&path).is_ok() {
                    report.dirs += 1;
                }
            } else {
                match remove_dir_checked(&path) {
                    Ok(()) => report.dirs += 1,
                    Err(e) => report.fail(&path, e),
                }
            }
        } else {
            match delete_file_with_progress(&path, opts, progress) {
                Ok(bytes) => {
                    report.files += 1;
                    report.bytes += bytes;
                    if let Some(p) = progress {
                        p.inc(1);
                    }
                }
                Err(Error::Cancelled) => return,
                Err(e) => report.fail(&path, e),
            }
        }
    }
}

/// Removes empty folders below `root` (bottom-up). Returns how many were removed.
/// `root` itself is kept.
pub fn remove_empty_dirs(root: &Path) -> u64 {
    fn visit(dir: &Path, is_root: bool, removed: &mut u64) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        let mut empty = true;
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                empty = false;
                continue;
            };
            if meta.is_dir() && !is_link(&meta) {
                if !visit(&entry.path(), false, removed) {
                    empty = false;
                }
            } else {
                empty = false;
            }
        }
        if empty && !is_root && remove_dir_checked(dir).is_ok() {
            *removed += 1;
            return true;
        }
        false
    }
    let mut removed = 0;
    visit(root, true, &mut removed);
    removed
}

/// Moves files or folders to the Recycle Bin (silently, without prompts).
pub fn recycle(paths: &[PathBuf]) -> Result<()> {
    use windows::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };
    use windows::core::PCWSTR;

    if paths.is_empty() {
        return Ok(());
    }
    // pFrom is a list of NUL-terminated paths ending with an extra NUL.
    let mut from: Vec<u16> = Vec::new();
    for path in paths {
        ensure_deletable(path)?;
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "{} is not absolute",
                path.display()
            )));
        }
        from.extend(path.as_os_str().encode_wide());
        from.push(0);
    }
    from.push(0);

    let flags = FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0;
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: flags as u16,
        ..Default::default()
    };
    // SAFETY: `op` is fully initialised and `from` outlives the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 {
        return Err(Error::Windows(format!(
            "moving to the Recycle Bin failed (code 0x{code:X})"
        )));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// Schedules `path` for deletion at the next restart (needs administrator
/// rights). Useful for files locked by running programs.
pub fn delete_on_reboot(path: &Path) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
    use windows::core::PCWSTR;

    ensure_deletable(path)?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL terminated and outlives the call.
    unsafe {
        MoveFileExW(
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    }
    .map_err(|e| {
        if e.code().0 as u32 == 0x8007_0005 {
            Error::NeedsElevation("schedule deletion at restart".to_owned())
        } else {
            e.into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("zest-delete-test-")
            .tempdir()
            .unwrap()
    }

    #[test]
    fn pass_counts() {
        assert_eq!(WipeMethod::None.passes(), 0);
        assert_eq!(WipeMethod::Simple.passes(), 1);
        assert_eq!(WipeMethod::Dod3.passes(), 3);
        assert_eq!(WipeMethod::Advanced7.passes(), 7);
        assert_eq!(WipeMethod::Gutmann35.passes(), 35);
    }

    #[test]
    fn rng_is_not_constant() {
        let mut rng = FastRng::new();
        let mut a = [0u8; 37];
        let mut b = [0u8; 37];
        rng.fill(&mut a);
        rng.fill(&mut b);
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 37]);
        assert_eq!(rng.name(12).len(), 12);
    }

    #[test]
    fn deletes_plain_and_read_only_files() {
        let tmp = sandbox();
        let file = tmp.path().join("a.txt");
        fs::write(&file, b"hello").unwrap();
        let mut perms = fs::metadata(&file).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&file, perms).unwrap();

        assert_eq!(delete_file(&file, &DeleteOptions::permanent()).unwrap(), 5);
        assert!(!file.exists());
        // Deleting again is not an error.
        assert_eq!(delete_file(&file, &DeleteOptions::permanent()).unwrap(), 0);
    }

    #[test]
    fn dry_run_touches_nothing() {
        let tmp = sandbox();
        let file = tmp.path().join("a.txt");
        fs::write(&file, b"hello world").unwrap();
        let opts = DeleteOptions::permanent().with_dry_run(true);
        assert_eq!(delete_file(&file, &opts).unwrap(), 11);
        assert!(file.exists());

        let mut report = DeleteReport::default();
        delete_tree(tmp.path(), true, &opts, None, &mut report);
        assert_eq!((report.files, report.bytes, report.failed), (1, 11, 0));
        assert!(file.exists());
    }

    #[test]
    fn wipe_overwrites_every_byte() {
        let tmp = sandbox();
        let path = tmp.path().join("secret.bin");
        let original = vec![0xABu8; 3 * 1024 + 7];
        fs::write(&path, &original).unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // Dod3 ends with a random pass; none of the content should survive.
        wipe_contents(&mut file, original.len() as u64, WipeMethod::Dod3, None).unwrap();
        drop(file);
        let after = fs::read(&path).unwrap();
        assert_eq!(after.len(), original.len());
        let unchanged = after.iter().filter(|&&b| b == 0xAB).count();
        assert!(unchanged < 64, "{unchanged} bytes survived the overwrite");
    }

    #[test]
    fn secure_delete_removes_file() {
        let tmp = sandbox();
        for method in WipeMethod::ALL {
            let path = tmp.path().join("secret.txt");
            fs::write(&path, b"top secret contents").unwrap();
            let freed = delete_file(&path, &DeleteOptions::secure(method)).unwrap();
            assert_eq!(freed, 19);
            assert!(!path.exists());
            assert_eq!(
                fs::read_dir(tmp.path()).unwrap().count(),
                0,
                "{method:?} left debris"
            );
        }
    }

    #[test]
    fn tree_deletion_with_and_without_root() {
        let tmp = sandbox();
        let root = tmp.path().join("cache");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("1.tmp"), vec![1u8; 10]).unwrap();
        fs::write(root.join("a/2.tmp"), vec![1u8; 20]).unwrap();
        fs::write(root.join("a/b/3.tmp"), vec![1u8; 30]).unwrap();

        let mut report = DeleteReport::default();
        delete_tree(&root, true, &DeleteOptions::permanent(), None, &mut report);
        assert_eq!(
            (report.files, report.dirs, report.bytes, report.failed),
            (3, 2, 60, 0)
        );
        assert!(root.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

        fs::write(root.join("again.tmp"), b"x").unwrap();
        let mut report = DeleteReport::default();
        delete_tree(&root, false, &DeleteOptions::permanent(), None, &mut report);
        assert_eq!((report.files, report.dirs), (1, 1));
        assert!(!root.exists());
    }

    #[test]
    fn junction_is_unlinked_not_followed() {
        let tmp = sandbox();
        let outside = tmp.path().join("outside");
        let cache = tmp.path().join("cache");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(outside.join("precious.txt"), b"keep me").unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(cache.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());

        let mut report = DeleteReport::default();
        delete_tree(&cache, true, &DeleteOptions::permanent(), None, &mut report);
        assert_eq!(report.failed, 0, "{:?}", report.failures);
        assert!(!cache.join("link").exists());
        assert_eq!(fs::read(outside.join("precious.txt")).unwrap(), b"keep me");
    }

    #[test]
    fn protected_paths_are_refused() {
        let err = delete_file(
            &crate::paths::system32_dir().join("kernel32.dll"),
            &DeleteOptions::permanent().with_dry_run(true),
        );
        assert!(matches!(err, Err(Error::Protected(_))));
        let mut report = DeleteReport::default();
        delete_tree(
            &crate::paths::windows_dir(),
            false,
            &DeleteOptions::permanent().with_dry_run(true),
            None,
            &mut report,
        );
        assert_eq!(report.failed, 1);
        assert_eq!(report.files, 0);
    }

    #[test]
    fn empty_dirs_are_pruned() {
        let tmp = sandbox();
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        fs::create_dir_all(tmp.path().join("d")).unwrap();
        fs::write(tmp.path().join("d/keep.txt"), b"x").unwrap();
        assert_eq!(remove_empty_dirs(tmp.path()), 3);
        assert!(!tmp.path().join("a").exists());
        assert!(tmp.path().join("d/keep.txt").exists());
    }
}
