//! Drive wiper: free-space wiping, whole-drive wiping and file shredding.
//!
//! All overwriting is delegated to `zest_core::delete`, so the pass
//! patterns are identical everywhere in the product.
//!
//! # What overwriting can and cannot do
//! On a classic hard disk an overwrite physically replaces the old data.
//! On solid-state drives, USB sticks and memory cards the controller remaps
//! blocks (wear levelling) and keeps spare areas the operating system cannot
//! address, so **no overwrite scheme can guarantee destruction on flash
//! storage**. Wiping still removes everything reachable through the file
//! system; for stronger guarantees use the drive's own secure-erase command
//! or full-disk encryption. Extra passes add no security on SSDs – they
//! only add wear – so one pass is the sensible choice there.
//!
//! Free-space wiping does not reach data stored inside the file system's
//! own records (very small files kept directly in NTFS file records, old
//! file names in directory indexes) or the slack space of existing files.

use crate::drives::{self, DriveInfo, DriveKind};
use crate::fastdir;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, SetFileAttributesW,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::FSCTL_SET_COMPRESSION;
use windows::core::PCWSTR;
use zest_core::delete::{
    DeleteOptions, DeleteReport, FastRng, WipeMethod, delete_tree, wipe_contents,
};
use zest_core::walk::tree_size;
use zest_core::{Error, IoContext, Progress, Result, paths, safety};

/// Prefix of the hidden working folder created on the volume being wiped.
pub const WIPE_FOLDER_PREFIX: &str = ".zest-wipe-";

/// Free space left untouched on the system drive while wiping, so Windows
/// and running programs keep working.
pub const SYSTEM_DRIVE_MARGIN: u64 = 512 * 1024 * 1024;

/// Size of each fill file. Small enough for FAT32 and for smooth progress.
const FILL_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// Allocations smaller than this are not worth another file.
const MIN_FILL_BYTES: u64 = 4096;

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Result of a free-space wipe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WipeReport {
    /// Bytes of free space that were overwritten.
    pub bytes_wiped: u64,
    /// Overwrite passes applied to every byte.
    pub passes: u32,
    /// Temporary files used (all removed again).
    pub files_used: u64,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
}

/// Result of wiping everything inside a folder or drive.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContentWipeReport {
    /// What was destroyed and what could not be removed.
    pub deleted: DeleteReport,
    /// Top-level entries that were left alone because they belong to the
    /// system (Recycle Bin, System Volume Information, ...).
    pub skipped_protected: Vec<PathBuf>,
}

/// Result of [`wipe_entire_drive`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DriveWipeReport {
    /// Outcome of destroying the drive's files.
    pub contents: ContentWipeReport,
    /// Whether the current user's Recycle Bin on the drive was emptied
    /// before the free-space wipe (so recycled files are overwritten too).
    pub recycle_bin_emptied: bool,
    /// Outcome of the free-space wipe that followed.
    pub free_space: WipeReport,
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn require_method(method: WipeMethod) -> Result<()> {
    if method == WipeMethod::None {
        Err(Error::Invalid(
            "choose an overwrite method for wiping".to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// The working folder; removes itself (and its fill files) when dropped, so
/// cancellation, errors and panics never leave the disk full.
struct FillFolder {
    path: PathBuf,
}

impl FillFolder {
    fn create(root: &Path) -> Result<Self> {
        let mut rng = FastRng::new();
        let path = root.join(format!("{WIPE_FOLDER_PREFIX}{}", rng.name(10)));
        std::fs::create_dir(&path).ctx(format!("creating work folder on {}", root.display()))?;
        let folder = FillFolder { path };
        let name = wide(&folder.path);
        // SAFETY: `name` is NUL terminated. Failure only affects visibility.
        unsafe {
            let _ = SetFileAttributesW(
                PCWSTR(name.as_ptr()),
                FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED,
            );
        }
        folder.disable_compression();
        Ok(folder)
    }

    /// On a compressed volume new files inherit compression, and repetitive
    /// overwrite patterns would compress to almost nothing instead of
    /// occupying the free space. Switch it off for the work folder.
    fn disable_compression(&self) {
        let Ok(dir) = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&self.path)
        else {
            return;
        };
        let format_none: u16 = 0;
        let mut returned = 0u32;
        // SAFETY: the handle is valid for the call and the input buffer is a
        // live u16 as FSCTL_SET_COMPRESSION expects.
        unsafe {
            let _ = DeviceIoControl(
                HANDLE(dir.as_raw_handle()),
                FSCTL_SET_COMPRESSION,
                Some((&raw const format_none).cast()),
                size_of::<u16>() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            );
        }
    }

    fn file(&self, number: u64) -> PathBuf {
        self.path.join(format!("fill-{number:06}.bin"))
    }
}

impl Drop for FillFolder {
    fn drop(&mut self) {
        let mut report = DeleteReport::default();
        delete_tree(
            &self.path,
            false,
            &DeleteOptions::permanent(),
            None,
            &mut report,
        );
        if report.failed > 0 {
            log::warn!(
                "could not remove wipe work folder {}: {:?}",
                self.path.display(),
                report.failures.first()
            );
        }
    }
}

/// Removes work folders left behind by an interrupted wipe (power loss,
/// crash) directly inside `root`. Returns the number of bytes released.
///
/// Only folders whose name starts with [`WIPE_FOLDER_PREFIX`] are touched.
pub fn cleanup_stale_wipe_folders(root: &Path) -> u64 {
    let mut released = 0;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_ours = name.to_string_lossy().starts_with(WIPE_FOLDER_PREFIX);
        let is_real_dir = entry
            .metadata()
            .is_ok_and(|m| m.is_dir() && !zest_core::walk::is_link(&m));
        if is_ours && is_real_dir {
            let mut report = DeleteReport::default();
            delete_tree(
                &entry.path(),
                false,
                &DeleteOptions::permanent(),
                None,
                &mut report,
            );
            released += report.bytes;
        }
    }
    released
}

fn margin_for(root: &Path) -> u64 {
    let on_system_drive =
        drives::drive_letter(root) == drives::drive_letter(&paths::system_drive());
    if on_system_drive {
        SYSTEM_DRIVE_MARGIN
    } else {
        0
    }
}

/// Reserves `len` bytes for a new fill file, halving the request when the
/// volume turns out to be fuller than expected.
fn allocate(path: &Path, mut len: u64) -> Result<Option<(File, u64)>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .ctx(format!("creating {}", path.display()))?;
    loop {
        if file.set_len(len).is_ok() {
            return Ok(Some((file, len)));
        }
        len /= 2;
        if len < MIN_FILL_BYTES {
            break;
        }
    }
    drop(file);
    let _ = std::fs::remove_file(path);
    Ok(None)
}

/// Overwrites the free space of the volume that holds `root`.
///
/// `root` is normally a drive root (`D:\`) but may be any existing folder
/// on the volume. Free space is claimed with temporary files inside a hidden
/// work folder under `root`, every byte of them is overwritten with all
/// passes of `method`, and the files are removed again – also on
/// cancellation or error.
///
/// * `limit_bytes` caps how much free space is wiped (`None` = all of it).
/// * On the system drive [`SYSTEM_DRIVE_MARGIN`] bytes are always left free.
/// * `WipeMethod::None` is rejected with [`Error::Invalid`].
///
/// Does not need administrator rights as long as `root` is writable.
///
/// # Progress
/// `total` = bytes to write (free space x passes), `done` advances after
/// each temporary file, `bytes` advances continuously.
pub fn wipe_free_space(
    root: &Path,
    method: WipeMethod,
    limit_bytes: Option<u64>,
    progress: &Progress,
) -> Result<WipeReport> {
    require_method(method)?;
    let started = Instant::now();
    let root = fastdir::absolute(root);
    if !root.is_dir() {
        return Err(Error::NotFound(root.display().to_string()));
    }
    progress.check()?;
    cleanup_stale_wipe_folders(&root);

    let passes = u64::from(method.passes());
    let margin = margin_for(&root);
    let limit = limit_bytes.unwrap_or(u64::MAX);
    let initially_free = drives::free_bytes(&root)?.saturating_sub(margin);
    progress.set_done(0);
    progress.set_total(initially_free.min(limit).saturating_mul(passes));
    progress.set_message(format!("Wiping free space on {}", root.display()));

    let folder = FillFolder::create(&root)?;
    let mut report = WipeReport {
        passes: method.passes(),
        ..WipeReport::default()
    };
    loop {
        progress.check()?;
        let available = drives::free_bytes(&folder.path)?.saturating_sub(margin);
        let remaining = limit - report.bytes_wiped;
        let want = available.min(remaining).min(FILL_FILE_BYTES);
        // Stop when nothing is left, or when the volume (not the limit) is
        // what makes the next file tiny.
        if want == 0 || (want < MIN_FILL_BYTES && want < remaining) {
            break;
        }
        let Some((mut file, len)) = allocate(&folder.file(report.files_used), want)? else {
            break;
        };
        report.files_used += 1;
        wipe_contents(&mut file, len, method, Some(progress))?;
        drop(file);
        report.bytes_wiped += len;
        progress.inc(len.saturating_mul(passes));
        if report.bytes_wiped >= limit {
            break;
        }
    }
    drop(folder);
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    progress.set_message(String::new());
    Ok(report)
}

/// Securely destroys files and folders chosen by the user.
///
/// Every file is overwritten according to `method`
/// (`WipeMethod::None` simply deletes) and then removed; folders are
/// processed recursively without following links. Failures are collected in
/// the returned report, cancellation stops early.
///
/// # Progress
/// `total` = number of files, `done` = files destroyed, `bytes` = bytes
/// overwritten.
pub fn shred(paths: &[PathBuf], method: WipeMethod, progress: &Progress) -> DeleteReport {
    let mut report = DeleteReport::default();
    progress.set_done(0);
    progress.set_message("Counting files");
    let total: u64 = paths
        .iter()
        .map(|p| tree_size(p, Some(progress)).files)
        .sum();
    progress.set_total(total);
    progress.set_message("Shredding files");
    let options = DeleteOptions::secure(method);
    for path in paths {
        if progress.is_cancelled() {
            break;
        }
        let is_single = std::fs::symlink_metadata(path)
            .map(|m| !m.is_dir() || zest_core::walk::is_link(&m))
            .unwrap_or(false);
        delete_tree(path, false, &options, Some(progress), &mut report);
        if is_single {
            // delete_tree only advances the counter for files inside folders.
            progress.inc(1);
        }
    }
    progress.set_message(String::new());
    report
}

/// Locations that must never be inside a folder whose contents get wiped.
fn critical_locations() -> Vec<PathBuf> {
    let user = paths::UserDirs::current();
    let mut list = vec![
        paths::windows_dir(),
        paths::program_files(),
        paths::program_files_x86(),
        paths::program_data(),
        paths::users_dir(),
        user.profile,
        paths::data_dir(),
    ];
    if let Ok(exe) = std::env::current_exe() {
        list.push(exe);
    }
    list
}

fn holds_critical_data(folder: &Path) -> Option<PathBuf> {
    let folder_key = fastdir::path_key(folder);
    critical_locations()
        .into_iter()
        .find(|critical| fastdir::key_is_under(&fastdir::path_key(critical), &folder_key))
}

/// Securely destroys **everything inside** `folder` (the folder itself is
/// kept). This is the core of [`wipe_entire_drive`], usable on any folder.
///
/// Refuses with [`Error::Protected`] when `folder` is, or contains, the
/// Windows folder, a program folder, the users folder, the current user's
/// profile, Zest's data folder or the running program. Entries guarded by
/// the global safety rules (Recycle Bin, System Volume Information, ...)
/// are skipped and listed in the report. Links are removed, never followed.
///
/// # Progress
/// `total` = number of files, `done` = files destroyed.
pub fn wipe_folder_contents(
    folder: &Path,
    method: WipeMethod,
    progress: &Progress,
) -> Result<ContentWipeReport> {
    require_method(method)?;
    let folder = fastdir::absolute(folder);
    if !folder.is_dir() {
        return Err(Error::NotFound(folder.display().to_string()));
    }
    // Check the path as written and the location it really resolves to, so
    // a junction cannot smuggle a critical folder past the test.
    let resolved = std::fs::canonicalize(&folder).ctx(format!("resolving {}", folder.display()))?;
    if holds_critical_data(&folder).is_some() || holds_critical_data(&resolved).is_some() {
        return Err(Error::Protected(folder));
    }
    progress.check()?;

    let mut report = ContentWipeReport::default();
    let mut targets: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&folder)
        .ctx(format!("reading {}", folder.display()))?
        .flatten()
    {
        let path = entry.path();
        let ours = entry
            .file_name()
            .to_string_lossy()
            .starts_with(WIPE_FOLDER_PREFIX);
        if safety::is_protected(&path) && !ours {
            report.skipped_protected.push(path);
        } else {
            targets.push(path);
        }
    }
    report.skipped_protected.sort();

    progress.set_done(0);
    progress.set_message("Counting files");
    let total: u64 = targets
        .iter()
        .map(|p| tree_size(p, Some(progress)).files)
        .sum();
    progress.set_total(total);
    progress.set_message(format!("Erasing {}", folder.display()));

    let options = DeleteOptions::secure(method);
    for target in &targets {
        progress.check()?;
        let is_single = std::fs::symlink_metadata(target)
            .map(|m| !m.is_dir() || zest_core::walk::is_link(&m))
            .unwrap_or(false);
        delete_tree(target, false, &options, Some(progress), &mut report.deleted);
        if is_single {
            progress.inc(1);
        }
    }
    progress.check()?;
    Ok(report)
}

/// Checks whether [`wipe_entire_drive`] would accept `root`, without
/// changing anything. Front ends call this before asking for confirmation.
///
/// A drive is refused when
/// * `root` is not a drive root such as `E:\`,
/// * the drive is not an internal or removable disk (network, optical and
///   RAM drives are refused) or holds no readable volume,
/// * it is the system drive, or
/// * it holds the running program, the current user's profile, Zest's
///   data folder or any other critical location.
pub fn check_drive_wipe_allowed(root: &Path) -> Result<DriveInfo> {
    if !drives::is_drive_root(root) {
        return Err(Error::Invalid(format!(
            "{} is not a drive (expected something like E:\\)",
            root.display()
        )));
    }
    let info = drives::drive_info(root)
        .ok_or_else(|| Error::NotFound(format!("drive {}", root.display())))?;
    if !matches!(info.kind, DriveKind::Fixed | DriveKind::Removable) {
        return Err(Error::Invalid(format!(
            "{} is a {} and cannot be wiped",
            info.root.display(),
            info.kind.label().to_lowercase()
        )));
    }
    if !info.is_ready {
        return Err(Error::NotFound(format!(
            "no readable volume in drive {}",
            info.root.display()
        )));
    }
    if info.is_system {
        return Err(Error::Protected(info.root));
    }
    if let Some(critical) = critical_locations()
        .into_iter()
        .find(|p| drives::drive_letter(p) == Some(info.letter))
    {
        log::warn!(
            "refusing to wipe {}: it holds {}",
            info.root.display(),
            critical.display()
        );
        return Err(Error::Protected(info.root));
    }
    Ok(info)
}

/// The NUL-terminated `X:\` string naming one drive's Recycle Bin, or `None`
/// when `root` is not a drive root.
///
/// The shell treats a missing root as "every drive", so this must never
/// produce an empty string.
fn recycle_bin_root(root: &Path) -> Option<Vec<u16>> {
    if !drives::is_drive_root(root) {
        return None;
    }
    let letter = drives::drive_letter(root)?;
    Some(
        format!("{letter}:\\")
            .encode_utf16()
            .chain(Some(0))
            .collect(),
    )
}

/// Empties the current user's Recycle Bin on a single drive. Returns false
/// when `root` is not a drive root or the shell reports a failure (which it
/// also does for a bin that is already empty).
fn empty_recycle_bin(root: &Path) -> bool {
    use windows::Win32::UI::Shell::{
        SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW,
    };
    let Some(drive) = recycle_bin_root(root) else {
        return false;
    };
    debug_assert_eq!(drive.len(), 4);
    // SAFETY: `drive` is a NUL terminated "X:\" string that outlives the call.
    unsafe {
        SHEmptyRecycleBinW(
            None,
            PCWSTR(drive.as_ptr()),
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        )
    }
    .is_ok()
}

/// Destroys **all data** on a non-system drive: every file is securely
/// deleted, the drive's Recycle Bin is emptied, then the free space is wiped
/// (which also overwrites whatever the Recycle Bin held).
///
/// The drive must pass [`check_drive_wipe_allowed`]. This cannot be undone;
/// front ends must obtain explicit confirmation first.
///
/// May need administrator rights for files the current user cannot delete;
/// those end up in `contents.deleted.failures`. Other users' Recycle Bins
/// and the volume's restore data (`System Volume Information`) are outside
/// the reach of a normal user and are left as they are.
///
/// # Progress
/// Runs in two phases, each restarting `done`/`total`: first files
/// destroyed, then bytes of free space written. The message names the phase.
pub fn wipe_entire_drive(
    root: &Path,
    method: WipeMethod,
    progress: &Progress,
) -> Result<DriveWipeReport> {
    require_method(method)?;
    let info = check_drive_wipe_allowed(root)?;
    let contents = wipe_folder_contents(&info.root, method, progress)?;
    let recycle_bin_emptied = empty_recycle_bin(&info.root);
    let free_space = wipe_free_space(&info.root, method, None, progress)?;
    Ok(DriveWipeReport {
        contents,
        recycle_bin_emptied,
        free_space,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MB: u64 = 1024 * 1024;

    fn sandbox() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("zest-wipe-test-")
            .tempdir()
            .unwrap()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn free_space_wipe_respects_limit_and_cleans_up() {
        let tmp = sandbox();
        fs::write(tmp.path().join("bystander.txt"), b"untouched").unwrap();
        let progress = Progress::new();
        let report =
            wipe_free_space(tmp.path(), WipeMethod::Dod3, Some(4 * MB), &progress).unwrap();
        assert_eq!(report.bytes_wiped, 4 * MB);
        assert_eq!(report.passes, 3);
        assert_eq!(report.files_used, 1);
        assert_eq!(progress.done(), 12 * MB);
        assert_eq!(progress.total(), 12 * MB);
        assert_eq!(progress.bytes(), 12 * MB);
        // Nothing but the bystander remains.
        assert_eq!(entries(tmp.path()), vec!["bystander.txt"]);
        assert_eq!(
            fs::read(tmp.path().join("bystander.txt")).unwrap(),
            b"untouched"
        );
    }

    #[test]
    fn free_space_wipe_tiny_limit() {
        let tmp = sandbox();
        let report =
            wipe_free_space(tmp.path(), WipeMethod::Simple, Some(1000), &Progress::new()).unwrap();
        assert_eq!(report.bytes_wiped, 1000);
        assert!(entries(tmp.path()).is_empty());
        let report =
            wipe_free_space(tmp.path(), WipeMethod::Simple, Some(0), &Progress::new()).unwrap();
        assert_eq!((report.bytes_wiped, report.files_used), (0, 0));
        assert!(entries(tmp.path()).is_empty());
    }

    #[test]
    fn free_space_wipe_rejects_bad_input() {
        let tmp = sandbox();
        assert!(matches!(
            wipe_free_space(tmp.path(), WipeMethod::None, Some(MB), &Progress::new()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            wipe_free_space(
                &tmp.path().join("missing"),
                WipeMethod::Simple,
                Some(MB),
                &Progress::new()
            ),
            Err(Error::NotFound(_))
        ));
        assert!(entries(tmp.path()).is_empty());
    }

    #[test]
    fn cancellation_leaves_nothing_behind() {
        let tmp = sandbox();
        // Cancel from another thread while the wipe is writing.
        let progress = Progress::new();
        let canceller = progress.clone();
        let handle = std::thread::spawn(move || {
            while canceller.bytes() == 0 {
                std::thread::yield_now();
            }
            canceller.cancel();
        });
        let result = wipe_free_space(tmp.path(), WipeMethod::Gutmann35, Some(8 * MB), &progress);
        handle.join().unwrap();
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert!(
            entries(tmp.path()).is_empty(),
            "work folder must be removed"
        );

        // Already cancelled before starting: nothing is created at all.
        let result = wipe_free_space(tmp.path(), WipeMethod::Simple, Some(MB), &progress);
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(entries(tmp.path()).is_empty());
    }

    #[test]
    fn stale_work_folders_are_removed() {
        let tmp = sandbox();
        let stale = tmp.path().join(format!("{WIPE_FOLDER_PREFIX}leftover"));
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("fill-000000.bin"), vec![0u8; 5000]).unwrap();
        fs::create_dir(tmp.path().join("ordinary")).unwrap();
        fs::write(tmp.path().join("ordinary").join("keep.txt"), b"x").unwrap();
        // A plain file with the prefix is not a work folder.
        fs::write(tmp.path().join(format!("{WIPE_FOLDER_PREFIX}file")), b"y").unwrap();

        assert_eq!(cleanup_stale_wipe_folders(tmp.path()), 5000);
        assert!(!stale.exists());
        assert!(tmp.path().join("ordinary").join("keep.txt").exists());
        assert!(
            tmp.path()
                .join(format!("{WIPE_FOLDER_PREFIX}file"))
                .exists()
        );
        assert_eq!(cleanup_stale_wipe_folders(&tmp.path().join("missing")), 0);
    }

    #[test]
    fn shred_files_and_folders() {
        let tmp = sandbox();
        let single = tmp.path().join("secret.txt");
        let folder = tmp.path().join("folder");
        fs::write(&single, b"top secret").unwrap();
        fs::create_dir_all(folder.join("sub")).unwrap();
        fs::write(folder.join("a.bin"), vec![1u8; 2000]).unwrap();
        fs::write(folder.join("sub").join("b.bin"), vec![2u8; 3000]).unwrap();
        fs::write(tmp.path().join("bystander.txt"), b"stay").unwrap();

        let progress = Progress::new();
        let report = shred(
            &[single.clone(), folder.clone(), tmp.path().join("missing")],
            WipeMethod::Dod3,
            &progress,
        );
        assert_eq!(
            (report.files, report.dirs, report.failed),
            (3, 2, 0),
            "{:?}",
            report.failures
        );
        assert_eq!(report.bytes, 10 + 2000 + 3000);
        assert_eq!(progress.fraction(), Some(1.0));
        assert_eq!(progress.bytes(), 3 * (10 + 2000 + 3000));
        assert_eq!(entries(tmp.path()), vec!["bystander.txt"]);
    }

    #[test]
    fn shred_refuses_protected_paths() {
        let report = shred(
            &[
                paths::system32_dir().join("kernel32.dll"),
                paths::windows_dir(),
            ],
            WipeMethod::Simple,
            &{
                // Counting the Windows folder would take a while; cancel
                // straight away. Shredding must still not touch anything.
                let p = Progress::new();
                p.cancel();
                p
            },
        );
        assert_eq!(report.files, 0);
        assert!(paths::system32_dir().join("kernel32.dll").exists());
    }

    #[test]
    fn folder_contents_are_wiped_but_folder_survives() {
        let tmp = sandbox();
        let root = tmp.path().join("volume");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("top.txt"), b"12345").unwrap();
        fs::write(root.join("a/one.bin"), vec![1u8; 100]).unwrap();
        fs::write(root.join("a/b/two.bin"), vec![2u8; 200]).unwrap();
        // A junction pointing outside must be unlinked, not followed.
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("precious.txt"), b"keep me").unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("link"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(out.status.success());

        let progress = Progress::new();
        let report = wipe_folder_contents(&root, WipeMethod::Simple, &progress).unwrap();
        assert_eq!(report.deleted.failed, 0, "{:?}", report.deleted.failures);
        assert_eq!(report.deleted.bytes, 305);
        assert_eq!(report.deleted.files, 4); // three files + the junction
        assert!(report.skipped_protected.is_empty());
        assert!(root.is_dir());
        assert!(entries(&root).is_empty());
        assert_eq!(fs::read(outside.join("precious.txt")).unwrap(), b"keep me");
        assert_eq!(progress.fraction(), Some(1.0));
    }

    #[test]
    fn folder_wipe_refuses_critical_locations() {
        let user = paths::UserDirs::current();
        for folder in [
            paths::system_drive(),
            paths::windows_dir(),
            paths::users_dir(),
            user.profile.clone(),
            paths::program_files(),
        ] {
            // A cancelled progress is a second line of defence for this test:
            // even if a check were wrong, nothing would be deleted.
            let progress = Progress::new();
            progress.cancel();
            let result = wipe_folder_contents(&folder, WipeMethod::Simple, &progress);
            assert!(
                matches!(result, Err(Error::Protected(_))),
                "{} must be refused, got {result:?}",
                folder.display()
            );
        }
        let tmp = sandbox();
        assert!(matches!(
            wipe_folder_contents(tmp.path(), WipeMethod::None, &Progress::new()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            wipe_folder_contents(
                &tmp.path().join("missing"),
                WipeMethod::Simple,
                &Progress::new()
            ),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn drive_wipe_refusals() {
        // The system drive is always refused.
        assert!(matches!(
            check_drive_wipe_allowed(&paths::system_drive()),
            Err(Error::Protected(_))
        ));
        // Not a drive root.
        assert!(matches!(
            check_drive_wipe_allowed(&paths::windows_dir()),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            check_drive_wipe_allowed(Path::new(r"\\server\share")),
            Err(Error::Invalid(_))
        ));
        // A drive letter that does not exist.
        let present: Vec<char> = drives::list_drives().iter().map(|d| d.letter).collect();
        if let Some(free_letter) = ('D'..='Z').rev().find(|l| !present.contains(l)) {
            assert!(matches!(
                check_drive_wipe_allowed(Path::new(&format!("{free_letter}:\\"))),
                Err(Error::NotFound(_))
            ));
        }
        // Every drive that holds a critical location is refused; this never
        // wipes anything because only the read-only check is called.
        for drive in drives::list_drives() {
            let holds_critical = critical_locations()
                .iter()
                .any(|p| drives::drive_letter(p) == Some(drive.letter));
            if holds_critical || drive.is_system {
                assert!(check_drive_wipe_allowed(&drive.root).is_err());
            }
        }
        // The public entry point applies the same checks before doing anything.
        let progress = Progress::new();
        progress.cancel();
        assert!(matches!(
            wipe_entire_drive(&paths::system_drive(), WipeMethod::Simple, &progress),
            Err(Error::Protected(_))
        ));
        assert!(matches!(
            wipe_entire_drive(&paths::system_drive(), WipeMethod::None, &progress),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn recycle_bin_root_always_names_exactly_one_drive() {
        let expect = |letter: char| -> Vec<u16> {
            format!("{letter}:\\")
                .encode_utf16()
                .chain(Some(0))
                .collect()
        };
        assert_eq!(recycle_bin_root(Path::new(r"E:\")), Some(expect('E')));
        assert_eq!(recycle_bin_root(Path::new("f:")), Some(expect('F')));
        // Anything that is not a drive root yields nothing, so the shell is
        // never called with an empty ("all drives") root.
        assert_eq!(recycle_bin_root(Path::new("")), None);
        assert_eq!(recycle_bin_root(Path::new(r"E:\folder")), None);
        assert_eq!(recycle_bin_root(Path::new(r"\\server\share")), None);
        assert_eq!(recycle_bin_root(Path::new("relative")), None);
    }

    #[test]
    fn margin_only_applies_to_the_system_drive() {
        assert_eq!(margin_for(&paths::system_drive()), SYSTEM_DRIVE_MARGIN);
        assert_eq!(margin_for(&paths::windows_dir()), SYSTEM_DRIVE_MARGIN);
        let present: Vec<char> = drives::list_drives().iter().map(|d| d.letter).collect();
        if let Some(other) = ('D'..='Z').find(|l| !present.contains(l)) {
            assert_eq!(margin_for(Path::new(&format!("{other}:\\"))), 0);
        }
    }
}
