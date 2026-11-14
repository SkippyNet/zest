//! Fast directory enumeration shared by every scanner.
//!
//! `std::fs::read_dir` allocates a path and a name for every entry and hands
//! out one entry per call. When a scan touches millions of files that
//! dominates the CPU profile, so the scanners read directories in bulk
//! instead: the directory is opened once and the file system fills a large
//! buffer with as many entries as fit (a whole folder in one or two system
//! calls). Names are borrowed straight from that buffer as UTF-16 slices,
//! and size, attributes and timestamps arrive with the listing, so no file
//! is ever opened or queried on its own.
//!
//! Nothing here follows links. An entry that is a junction or a symbolic
//! link is reported with [`RawEntry::is_link`] set and it is the caller's
//! duty not to descend into it; every walker in this crate honours that.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL,
    ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FileFullDirectoryInfo, FindClose,
    FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
    GetFileInformationByHandleEx, OPEN_EXISTING, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

/// `FILE_ATTRIBUTE_READONLY`
pub const ATTR_READ_ONLY: u32 = 0x1;
/// `FILE_ATTRIBUTE_HIDDEN`
pub const ATTR_HIDDEN: u32 = 0x2;
/// `FILE_ATTRIBUTE_SYSTEM`
pub const ATTR_SYSTEM: u32 = 0x4;
/// `FILE_ATTRIBUTE_DIRECTORY`
pub const ATTR_DIRECTORY: u32 = 0x10;
/// `FILE_ATTRIBUTE_REPARSE_POINT`
pub const ATTR_REPARSE_POINT: u32 = 0x400;
/// `FILE_ATTRIBUTE_OFFLINE`
pub const ATTR_OFFLINE: u32 = 0x1000;
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`
pub const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// Bit that marks a reparse tag as a "name surrogate": the entry stands in
/// for another location (junction, symbolic link, mount point).
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

/// Offset between the Windows epoch (1601) and the Unix epoch, in 100 ns ticks.
const FILETIME_UNIX_OFFSET: u64 = 116_444_736_000_000_000;

/// Size of the listing buffer. 64 KiB holds several hundred entries, which
/// covers almost every folder in a single system call; larger folders simply
/// take a few more.
const BUFFER_BYTES: usize = 64 * 1024;

/// One directory entry, borrowed from the enumeration buffer.
#[derive(Clone, Copy, Debug)]
pub struct RawEntry<'a> {
    /// File name as UTF-16 (no terminator).
    pub name: &'a [u16],
    /// `FILE_ATTRIBUTE_*` bits.
    pub attributes: u32,
    /// Logical size in bytes (zero for directories).
    pub size: u64,
    /// Space reserved on disk in bytes. Smaller than `size` for compressed,
    /// sparse and cloud-only files; rounded up to whole clusters otherwise.
    /// On the rare volume that cannot be read in bulk this equals `size`.
    pub allocated: u64,
    /// Last write time in FILETIME ticks (100 ns since 1601).
    pub modified: u64,
    /// Reparse tag (only meaningful when the reparse attribute is set).
    pub reparse_tag: u32,
}

impl RawEntry<'_> {
    /// True for junctions and symbolic links: entries that point at another
    /// location and must never be followed. This is the same test the
    /// standard library applies for `FileType::is_symlink`, and therefore
    /// agrees with [`crate::walk::is_link`].
    #[inline]
    pub fn is_link(&self) -> bool {
        self.attributes & ATTR_REPARSE_POINT != 0
            && self.reparse_tag & REPARSE_TAG_NAME_SURROGATE != 0
    }

    /// True for directories (including links to directories; test
    /// [`RawEntry::is_link`] first).
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    /// True when the contents live in the cloud and reading would download them.
    #[inline]
    pub fn is_cloud_placeholder(&self) -> bool {
        self.attributes & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
    }

    /// Last write time, when the file system recorded one.
    pub fn modified_time(&self) -> Option<SystemTime> {
        filetime_to_system(self.modified)
    }

    /// The file name as an owned OS string (lossless).
    pub fn os_name(&self) -> OsString {
        OsString::from_wide(self.name)
    }

    /// Full path of this entry inside `dir`.
    pub fn path_in(&self, dir: &Path) -> PathBuf {
        dir.join(self.os_name())
    }
}

/// Converts FILETIME ticks to a [`SystemTime`]. Returns `None` for times
/// before 1970 or zero.
pub fn filetime_to_system(ticks: u64) -> Option<SystemTime> {
    let since_unix = ticks.checked_sub(FILETIME_UNIX_OFFSET)?;
    if ticks == 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::new(
        since_unix / 10_000_000,
        ((since_unix % 10_000_000) * 100) as u32,
    ))
}

/// Converts a [`SystemTime`] to FILETIME ticks (zero when out of range).
pub fn system_to_filetime(time: SystemTime) -> u64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            FILETIME_UNIX_OFFSET + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100
        }
        Err(_) => 0,
    }
}

/// Builds the NUL-terminated verbatim (`\\?\...`) form of a directory path.
///
/// The verbatim prefix lifts the 260 character limit, but Windows then takes
/// the path literally: it must be absolute, use backslashes and hold no
/// `.`/`..` components ([`is_literal`] checks that, [`absolute`] repairs it).
fn verbatim_path(dir: &Path) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    let raw: Vec<u16> = dir.as_os_str().encode_wide().collect();
    let verbatim: &[u16] = &[BACKSLASH, BACKSLASH, b'?' as u16, BACKSLASH];
    let mut out: Vec<u16> = Vec::with_capacity(raw.len() + 12);
    if raw.starts_with(verbatim) {
        out.extend_from_slice(&raw);
    } else if raw.starts_with(&[BACKSLASH, BACKSLASH]) {
        // \\server\share\dir -> \\?\UNC\server\share\dir
        out.extend_from_slice(verbatim);
        out.extend("UNC\\".encode_utf16());
        out.extend_from_slice(&raw[2..]);
    } else {
        out.extend_from_slice(verbatim);
        out.extend_from_slice(&raw);
    }
    // A drive root needs its trailing backslash (`\\?\C:` is the drive's
    // current directory object, not its root); everything else works with it.
    if out.last() != Some(&BACKSLASH) {
        out.push(BACKSLASH);
    }
    out.push(0);
    out
}

struct DirHandle(HANDLE);

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn io_error(e: &windows::core::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// The listing buffer: 64 KiB, aligned for the 8-byte fields of the entries.
struct Buffer(Box<[u64]>);

impl Buffer {
    fn new() -> Buffer {
        Buffer(vec![0u64; BUFFER_BYTES / 8].into_boxed_slice())
    }
}

thread_local! {
    /// One listing buffer per thread, reused for every directory it reads.
    static BUFFER: RefCell<Buffer> = RefCell::new(Buffer::new());
}

// Layout of FILE_FULL_DIR_INFO, read field by field so no reference to a
// packed or partially initialised structure is ever formed.
const OFFSET_NEXT: usize = 0;
const OFFSET_LAST_WRITE: usize = 24;
const OFFSET_END_OF_FILE: usize = 40;
const OFFSET_ALLOCATION: usize = 48;
const OFFSET_ATTRIBUTES: usize = 56;
const OFFSET_NAME_LENGTH: usize = 60;
const OFFSET_EA_SIZE: usize = 64;
const OFFSET_NAME: usize = 68;

#[inline]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[inline]
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | (u64::from(u32_at(bytes, at + 4)) << 32)
}

/// Walks the entries the file system wrote into `words[..filled]`.
fn visit_buffer(words: &[u64], filled: usize, visit: &mut dyn FnMut(&RawEntry<'_>)) {
    const DOT: u16 = b'.' as u16;
    let filled = filled.min(words.len() * 8);
    // SAFETY: plain bytes of an initialised u64 slice, within its bounds.
    let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), filled) };
    let mut at = 0usize;
    while at + OFFSET_NAME <= bytes.len() {
        let record = &bytes[at..];
        let next = u32_at(record, OFFSET_NEXT) as usize;
        let name_bytes = u32_at(record, OFFSET_NAME_LENGTH) as usize;
        if OFFSET_NAME + name_bytes > record.len() {
            // A truncated record: the file system never produces one, but a
            // malformed buffer must not send us out of bounds.
            return;
        }
        // SAFETY: every record starts on an 8-byte boundary of a u64 slice
        // and the name follows at offset 68, which is 2-byte aligned; the
        // length was checked against the filled part of the buffer above.
        let name = unsafe {
            std::slice::from_raw_parts(
                record.as_ptr().add(OFFSET_NAME).cast::<u16>(),
                name_bytes / 2,
            )
        };
        if name != [DOT] && name != [DOT, DOT] {
            let attributes = u32_at(record, OFFSET_ATTRIBUTES);
            visit(&RawEntry {
                name,
                attributes,
                size: u64_at(record, OFFSET_END_OF_FILE),
                allocated: u64_at(record, OFFSET_ALLOCATION),
                modified: u64_at(record, OFFSET_LAST_WRITE),
                // For reparse points the file system reports the tag in
                // place of the extended-attribute size.
                reparse_tag: if attributes & ATTR_REPARSE_POINT != 0 {
                    u32_at(record, OFFSET_EA_SIZE)
                } else {
                    0
                },
            });
        }
        // Records are 8-byte aligned in practice; all this code needs is an
        // even offset, for the UTF-16 name.
        if next == 0 || !next.is_multiple_of(2) {
            return;
        }
        at += next;
    }
}

/// Reads a folder in bulk: one open, then as many entries per call as fit
/// the buffer.
fn read_bulk(
    dir: &Path,
    buffer: &mut Buffer,
    visit: &mut dyn FnMut(&RawEntry<'_>),
) -> std::io::Result<()> {
    let path = verbatim_path(dir);
    // SAFETY: `path` is NUL terminated and outlives the call. Only the right
    // to list is requested, the same right a `dir` command needs; backup
    // semantics is what allows opening a directory at all. Sharing
    // everything means the scan never blocks another program.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|e| io_error(&e))?;
    let handle = DirHandle(handle);

    loop {
        // SAFETY: the buffer is BUFFER_BYTES long, writable and 8-byte
        // aligned as the information class requires.
        let result = unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileFullDirectoryInfo,
                buffer.0.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
            )
        };
        match result {
            // The call does not say how much it wrote; the records chain to
            // each other and the last one has a zero link, so the walk ends
            // on its own. Passing the full length is therefore correct.
            Ok(()) => visit_buffer(&buffer.0, BUFFER_BYTES, visit),
            Err(e)
                if e.code() == ERROR_NO_MORE_FILES.to_hresult()
                    || e.code() == ERROR_FILE_NOT_FOUND.to_hresult() =>
            {
                return Ok(());
            }
            Err(e) => return Err(io_error(&e)),
        }
    }
}

struct FindGuard(HANDLE);

impl Drop for FindGuard {
    fn drop(&mut self) {
        // SAFETY: the handle came from FindFirstFileExW and is closed once.
        unsafe {
            let _ = FindClose(self.0);
        }
    }
}

/// Reads a folder through the classic find functions. Every file system
/// Windows can mount supports them, so this is what [`read_dir_raw`] falls
/// back to when a volume refuses the bulk query.
fn read_find(dir: &Path, visit: &mut dyn FnMut(&RawEntry<'_>)) -> std::io::Result<()> {
    const DOT: u16 = b'.' as u16;
    let mut pattern = verbatim_path(dir);
    // `verbatim_path` ends with a backslash and the terminator.
    pattern.pop();
    pattern.push(b'*' as u16);
    pattern.push(0);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL terminated and outlives the call; `data` is a
    // correctly sized WIN32_FIND_DATAW as required by FindExInfoBasic.
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (&raw mut data).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match handle {
        Ok(handle) => FindGuard(handle),
        Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(()),
        Err(e) => return Err(io_error(&e)),
    };

    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        if name != [DOT] && name != [DOT, DOT] {
            let size = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
            visit(&RawEntry {
                name,
                attributes: data.dwFileAttributes,
                size,
                allocated: size,
                modified: (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32)
                    | u64::from(data.ftLastWriteTime.dwLowDateTime),
                reparse_tag: if data.dwFileAttributes & ATTR_REPARSE_POINT != 0 {
                    data.dwReserved0
                } else {
                    0
                },
            });
        }
        // SAFETY: `handle` is a live find handle and `data` is valid for writes.
        match unsafe { FindNextFileW(handle.0, &mut data) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(()),
            Err(e) => return Err(io_error(&e)),
        }
    }
}

/// True for the errors a volume answers with when it does not implement the
/// bulk query (as opposed to "access denied" or "not found").
fn bulk_unsupported(error: &std::io::Error) -> bool {
    let unsupported = [
        ERROR_INVALID_FUNCTION,
        ERROR_NOT_SUPPORTED,
        ERROR_INVALID_PARAMETER,
        ERROR_INVALID_LEVEL,
    ];
    error
        .raw_os_error()
        .is_some_and(|code| unsupported.iter().any(|e| e.0 as i32 == code))
}

/// Bulk first; the classic way when the volume cannot do bulk.
fn read_with(
    dir: &Path,
    buffer: &mut Buffer,
    visit: &mut dyn FnMut(&RawEntry<'_>),
) -> std::io::Result<()> {
    let mut visited = false;
    let result = read_bulk(dir, buffer, &mut |entry| {
        visited = true;
        visit(entry);
    });
    match result {
        // Only when nothing was reported yet, or entries would repeat.
        Err(error) if !visited && bulk_unsupported(&error) => read_find(dir, visit),
        other => other,
    }
}

/// Calls `visit` for every entry of `dir` except `.` and `..`.
///
/// Returns an error when the directory cannot be opened (access denied,
/// vanished, not a directory, ...). An empty directory is not an error.
///
/// When `dir` itself is a link it is followed, exactly as opening it in
/// Explorer would; links *inside* it are only reported. Entries arrive in
/// the order the file system keeps them (alphabetical on NTFS).
pub fn read_dir_raw(dir: &Path, visit: &mut dyn FnMut(&RawEntry<'_>)) -> std::io::Result<()> {
    if !is_literal(dir) {
        // Relative, or with `..` or forward slashes: Windows would take the
        // verbatim form of such a path at face value. Clean it up first.
        return read_dir_raw(&absolute(dir), visit);
    }
    BUFFER.with(|cell| match cell.try_borrow_mut() {
        Ok(mut buffer) => read_with(dir, &mut buffer, visit),
        // The visitor of an outer listing on this thread is reading another
        // directory: give the inner listing a buffer of its own.
        Err(_) => read_with(dir, &mut Buffer::new(), visit),
    })
}

/// Makes a path absolute and lexically clean (`.`/`..` resolved, forward
/// slashes converted) without touching the file system.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// True when `path` can be handed to Windows in verbatim form as it is:
/// absolute, backslashes only, no `.` or `..` components. Scanners build
/// their paths by joining names onto such a root, so this is the usual case
/// and the check never allocates.
fn is_literal(path: &Path) -> bool {
    const BACKSLASH: u16 = b'\\' as u16;
    const SLASH: u16 = b'/' as u16;
    const DOT: u16 = b'.' as u16;
    if !path.is_absolute() {
        return false;
    }
    let (mut length, mut dots_only) = (0usize, true);
    for unit in path.as_os_str().encode_wide().chain([BACKSLASH]) {
        match unit {
            SLASH => return false,
            BACKSLASH => {
                if dots_only && (length == 1 || length == 2) {
                    return false;
                }
                (length, dots_only) = (0, true);
            }
            other => {
                length += 1;
                dots_only &= other == DOT;
            }
        }
    }
    true
}

/// How many threads walk directories at once.
///
/// Listing folders is limited by the file system, not by the processor:
/// every folder that is opened passes through the file-system filter stack
/// (antivirus, cloud sync, ...), and that stack stops scaling long before a
/// modern CPU runs out of cores. Measured on a 16-thread laptop with 295,000
/// folders, a walk took 6.7 s with 2 threads, 3.1 s with 6, 2.9 s with 8 and
/// 4.0 s with 16, while burning twice the processor time of the 8-thread
/// run. `ZEST_SCAN_THREADS` overrides the choice for experiments.
pub fn walk_threads() -> usize {
    const MOST_USEFUL: usize = 8;
    std::env::var("ZEST_SCAN_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|threads| (1..=64).contains(threads))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(4, |n| n.get())
                .clamp(2, MOST_USEFUL)
        })
}

/// Recursive walks nest one stack frame per folder level and may pick up
/// further work while waiting, so the workers get a generous stack.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The thread pool every directory walk runs on.
///
/// One shared pool keeps the number of folders being read at any moment at
/// [`walk_threads`], however many scans overlap.
pub fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(walk_threads())
            .stack_size(WORKER_STACK_BYTES)
            .thread_name(|i| format!("zest-walk-{i}"))
            .build()
            .unwrap_or_else(|_| {
                // Falling back to a minimal pool keeps scans working even if
                // large stacks cannot be reserved.
                rayon::ThreadPoolBuilder::new()
                    .num_threads(2)
                    .build()
                    .expect("a two-thread pool can always be created")
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn junction(link: &Path, target: &Path) {
        // Junctions need no special privilege, unlike symbolic links.
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink failed");
    }

    #[test]
    fn enumerates_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), vec![1u8; 1234]).unwrap();
        fs::write(tmp.path().join("b.bin"), b"").unwrap();

        let mut seen = Vec::new();
        read_dir_raw(tmp.path(), &mut |e| {
            seen.push((
                e.os_name().to_string_lossy().into_owned(),
                e.is_dir(),
                e.size,
                e.is_link(),
            ));
        })
        .unwrap();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("a.txt".to_owned(), false, 1234, false),
                ("b.bin".to_owned(), false, 0, false),
                ("sub".to_owned(), true, 0, false),
            ]
        );
    }

    #[test]
    fn empty_missing_and_non_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let mut count = 0;
        read_dir_raw(tmp.path(), &mut |_| count += 1).unwrap();
        assert_eq!(count, 0);
        assert!(read_dir_raw(&tmp.path().join("nope"), &mut |_| {}).is_err());
        // A file is not a directory: an error, never a listing.
        let file = tmp.path().join("plain.txt");
        fs::write(&file, b"x").unwrap();
        let mut listed = 0;
        assert!(read_dir_raw(&file, &mut |_| listed += 1).is_err());
        assert_eq!(listed, 0);
    }

    #[test]
    fn junctions_are_reported_as_links_and_not_entered() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside.txt"), b"data").unwrap();
        let listed = tmp.path().join("listed");
        fs::create_dir(&listed).unwrap();
        junction(&listed.join("link"), &target);

        let mut entries = Vec::new();
        read_dir_raw(&listed, &mut |e| {
            entries.push((e.os_name(), e.is_link(), e.is_dir()));
        })
        .unwrap();
        // The junction shows up as one link entry; nothing of its target does.
        assert_eq!(entries, vec![(OsString::from("link"), true, true)]);
    }

    #[test]
    fn agrees_with_the_standard_library_about_links() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        junction(&tmp.path().join("link"), &target);
        fs::write(tmp.path().join("file.txt"), b"x").unwrap();

        let mut ours = Vec::new();
        read_dir_raw(tmp.path(), &mut |e| {
            ours.push((e.os_name(), e.is_link(), e.is_dir()));
        })
        .unwrap();
        ours.sort();
        let mut theirs: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                (
                    entry.file_name(),
                    crate::walk::is_link(&meta),
                    meta.is_dir() || crate::walk::is_link_to_dir(&meta),
                )
            })
            .collect();
        theirs.sort();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn long_paths_are_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        for _ in 0..12 {
            deep.push("a-rather-long-directory-name-to-exceed-max-path");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("leaf.dat"), b"12345").unwrap();
        assert!(deep.as_os_str().len() > 300);
        let mut found = None;
        read_dir_raw(&deep, &mut |e| found = Some((e.os_name(), e.size))).unwrap();
        assert_eq!(found, Some((OsString::from("leaf.dat"), 5)));
    }

    #[test]
    fn unicode_names_survive_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let names = [
            "über größe.txt",
            "日本語のファイル.dat",
            "emoji 🍊 zest.bin",
        ];
        for name in names {
            fs::write(tmp.path().join(name), b"1").unwrap();
        }
        let mut seen = Vec::new();
        read_dir_raw(tmp.path(), &mut |e| seen.push(e.os_name())).unwrap();
        seen.sort();
        let mut expected: Vec<OsString> = names.iter().map(OsString::from).collect();
        expected.sort();
        assert_eq!(seen, expected);
    }

    #[test]
    fn folders_larger_than_one_buffer_are_read_completely() {
        let tmp = tempfile::tempdir().unwrap();
        // Long names so that 64 KiB cannot hold them all.
        let count = 900;
        for i in 0..count {
            let name = format!("{i:04}-{}", "n".repeat(120));
            fs::write(tmp.path().join(name), [0u8; 3]).unwrap();
        }
        let mut seen = std::collections::HashSet::new();
        let mut bytes = 0;
        read_dir_raw(tmp.path(), &mut |e| {
            assert!(seen.insert(e.os_name()), "an entry was reported twice");
            bytes += e.size;
        })
        .unwrap();
        assert_eq!(seen.len(), count);
        assert_eq!(bytes, count as u64 * 3);
    }

    #[test]
    fn a_listing_may_be_started_from_inside_another() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("a")).unwrap();
        fs::create_dir(tmp.path().join("b")).unwrap();
        fs::write(tmp.path().join("a").join("one"), b"1").unwrap();
        fs::write(tmp.path().join("b").join("two"), b"22").unwrap();
        let mut inner = Vec::new();
        read_dir_raw(tmp.path(), &mut |outer| {
            let dir = outer.path_in(tmp.path());
            read_dir_raw(&dir, &mut |e| inner.push((e.os_name(), e.size))).unwrap();
        })
        .unwrap();
        inner.sort();
        assert_eq!(
            inner,
            vec![(OsString::from("one"), 1), (OsString::from("two"), 2)]
        );
    }

    #[test]
    fn timestamps_and_sizes_match_the_standard_library() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("t.txt");
        fs::write(&file, vec![7u8; 5000]).unwrap();
        let std_meta = fs::metadata(&file).unwrap();
        let std_time = std_meta.modified().unwrap();
        let mut raw = None;
        read_dir_raw(tmp.path(), &mut |e| {
            raw = Some((e.modified, e.size, e.allocated, e.modified_time()))
        })
        .unwrap();
        let (modified, size, allocated, time) = raw.unwrap();
        assert_eq!(filetime_to_system(modified), Some(std_time));
        assert_eq!(time, Some(std_time));
        assert_eq!(system_to_filetime(std_time), modified);
        assert_eq!(size, std_meta.len());
        assert!(allocated >= size || allocated == 0, "allocated {allocated}");
        assert_eq!(filetime_to_system(0), None);
    }

    #[test]
    fn the_classic_fallback_reports_exactly_what_the_bulk_read_does() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let dir = tmp.path().join("dir");
        fs::create_dir(&dir).unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        junction(&dir.join("link"), &target);
        for i in 0..300 {
            fs::write(dir.join(format!("file-{i:03}-größe.bin")), vec![0u8; i]).unwrap();
        }
        let collect = |bulk: bool| {
            let mut seen = Vec::new();
            let mut push = |e: &RawEntry<'_>| {
                seen.push((
                    e.os_name(),
                    e.attributes,
                    e.size,
                    e.modified,
                    e.is_link(),
                    e.is_dir(),
                ));
            };
            if bulk {
                read_bulk(&dir, &mut Buffer::new(), &mut push).unwrap();
            } else {
                read_find(&dir, &mut push).unwrap();
            }
            seen.sort();
            seen
        };
        let bulk = collect(true);
        assert_eq!(bulk.len(), 302);
        assert_eq!(bulk, collect(false));
        // Both refuse a path that is not a folder.
        let file = dir.join("file-001-größe.bin");
        assert!(read_find(&file, &mut |_| {}).is_err());
        assert!(read_bulk(&file, &mut Buffer::new(), &mut |_| {}).is_err());
    }

    #[test]
    fn relative_and_untidy_paths_are_cleaned_up_first() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a").join("b")).unwrap();
        fs::write(tmp.path().join("a").join("here.txt"), b"x").unwrap();
        let names = |dir: &Path| {
            let mut names = Vec::new();
            read_dir_raw(dir, &mut |e| names.push(e.os_name())).unwrap();
            names.sort();
            names
        };
        let expected = vec![OsString::from("b"), OsString::from("here.txt")];
        // `..` must be resolved, not looked up as a folder named "..".
        assert_eq!(names(&tmp.path().join("a").join("b").join("..")), expected);
        assert_eq!(names(&tmp.path().join("a").join(".")), expected);
        let forward = PathBuf::from(format!("{}/a", tmp.path().display()));
        assert_eq!(names(&forward), expected);

        assert!(is_literal(Path::new(r"C:\Users\someone")));
        assert!(is_literal(Path::new(r"C:\")));
        assert!(is_literal(Path::new(r"\\?\C:\very\long")));
        assert!(is_literal(Path::new(r"\\server\share\dir")));
        assert!(is_literal(Path::new(r"C:\dir\...\file.with.dots\.hidden")));
        assert!(!is_literal(Path::new(r"C:\Users\..\Windows")));
        assert!(!is_literal(Path::new(r"C:\Users\.\x")));
        assert!(!is_literal(Path::new(r"C:\Users\x\..")));
        assert!(!is_literal(Path::new("C:/Users")));
        assert!(!is_literal(Path::new(r"relative\dir")));
        assert!(!is_literal(Path::new(r"\rooted-but-no-drive")));
    }

    #[test]
    fn the_walk_pool_is_bounded() {
        let threads = pool().current_num_threads();
        assert!((1..=64).contains(&threads));
        if std::env::var("ZEST_SCAN_THREADS").is_err() {
            assert!((2..=8).contains(&threads), "{threads} threads");
        }
    }

    #[test]
    fn drive_roots_can_be_listed() {
        let windows = crate::paths::windows_dir();
        let root = windows
            .ancestors()
            .last()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(r"C:\"));
        let mut names = Vec::new();
        read_dir_raw(&root, &mut |e| {
            names.push(e.os_name().to_string_lossy().to_lowercase())
        })
        .unwrap();
        let windows_name = windows
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        assert!(names.contains(&windows_name), "{names:?}");
    }
}
