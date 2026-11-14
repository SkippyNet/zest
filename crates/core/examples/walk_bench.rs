//! Measures how fast a folder tree can be listed, comparing the ways Windows
//! offers to read a directory. Read-only: it only counts files and bytes.
//!
//! `cargo run --release -p zest-core --example walk_bench -- C:\ [threads...]`
//!
//! Variants:
//! * `std`   – `std::fs::read_dir`, one thread (what a naive walker does).
//! * `find`  – `FindFirstFileExW` with large fetch, one find handle per folder.
//! * `bulk`  – [`zest_core::fastdir`]: open the folder, read it in bulk.
//! * `rel`   – like `bulk`, but sub-folders are opened relative to their
//!   parent's handle, which stays open while its sub-tree is read.

use rayon::prelude::*;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, FILETIME, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, WIN32_FIND_DATAW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::core::PCWSTR;
use zest_core::fastdir;

#[derive(Default)]
struct Totals {
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    links: AtomicU64,
    failed: AtomicU64,
}

impl Totals {
    fn add(&self, files: u64, dirs: u64, bytes: u64, links: u64) {
        self.files.fetch_add(files, Ordering::Relaxed);
        self.dirs.fetch_add(dirs, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.links.fetch_add(links, Ordering::Relaxed);
    }
}

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;
const NAME_SURROGATE: u32 = 0x2000_0000;

// ── std ──────────────────────────────────────────────────────────────

fn walk_std(root: &Path, totals: &Totals) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            totals.failed.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let (mut files, mut dirs, mut bytes, mut links) = (0, 0, 0, 0);
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_type().is_symlink() {
                links += 1;
            } else if meta.is_dir() {
                dirs += 1;
                stack.push(entry.path());
            } else {
                files += 1;
                bytes += meta.len();
            }
        }
        totals.add(files, dirs, bytes, links);
    }
}

// ── find ─────────────────────────────────────────────────────────────

fn find_dir(dir: &Path, visit: &mut dyn FnMut(&WIN32_FIND_DATAW)) -> bool {
    let mut pattern: Vec<u16> = r"\\?\".encode_utf16().collect();
    pattern.extend(dir.as_os_str().encode_wide());
    if pattern.last() != Some(&(b'\\' as u16)) {
        pattern.push(b'\\' as u16);
    }
    pattern.push(b'*' as u16);
    pattern.push(0);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: the pattern is NUL terminated; `data` matches FindExInfoBasic.
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
    let Ok(handle) = handle else { return false };
    loop {
        visit(&data);
        // SAFETY: live find handle, valid output structure.
        match unsafe { FindNextFileW(handle, &mut data) } {
            Ok(()) => {}
            Err(e) => {
                // SAFETY: closed exactly once.
                unsafe {
                    let _ = FindClose(handle);
                }
                return e.code() == ERROR_NO_MORE_FILES.to_hresult();
            }
        }
    }
}

fn walk_find(dir: PathBuf, totals: &Totals) {
    let mut subdirs = Vec::new();
    let (mut files, mut dirs, mut bytes, mut links) = (0, 0, 0, 0);
    let ok = find_dir(&dir, &mut |d| {
        let len = d.cFileName.iter().position(|&c| c == 0).unwrap_or(260);
        let name = &d.cFileName[..len];
        if name == [46] || name == [46, 46] {
            return;
        }
        if d.dwFileAttributes & ATTR_REPARSE_POINT != 0 && d.dwReserved0 & NAME_SURROGATE != 0 {
            links += 1;
        } else if d.dwFileAttributes & ATTR_DIRECTORY != 0 {
            dirs += 1;
            subdirs.push(dir.join(String::from_utf16_lossy(name)));
        } else {
            files += 1;
            bytes += (u64::from(d.nFileSizeHigh) << 32) | u64::from(d.nFileSizeLow);
        }
    });
    if !ok {
        totals.failed.fetch_add(1, Ordering::Relaxed);
    }
    totals.add(files, dirs, bytes, links);
    subdirs.into_par_iter().for_each(|d| walk_find(d, totals));
}

// ── bulk ─────────────────────────────────────────────────────────────

fn walk_bulk(dir: PathBuf, totals: &Totals) {
    let mut subdirs = Vec::new();
    let (mut files, mut dirs, mut bytes, mut links) = (0, 0, 0, 0);
    let result = fastdir::read_dir_raw(&dir, &mut |e| {
        if e.is_link() {
            links += 1;
        } else if e.is_dir() {
            dirs += 1;
            subdirs.push(e.path_in(&dir));
        } else {
            files += 1;
            bytes += e.size;
        }
    });
    if result.is_err() {
        totals.failed.fetch_add(1, Ordering::Relaxed);
    }
    totals.add(files, dirs, bytes, links);
    subdirs.into_par_iter().for_each(|d| walk_bulk(d, totals));
}

// ── rel ──────────────────────────────────────────────────────────────

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_qos: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: isize,
    information: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtOpenFile(
        handle: *mut HANDLE,
        access: u32,
        attributes: *const ObjectAttributes,
        io_status: *mut IoStatusBlock,
        share: u32,
        options: u32,
    ) -> i32;
    fn NtQueryDirectoryFile(
        handle: HANDLE,
        event: HANDLE,
        apc_routine: *const c_void,
        apc_context: *const c_void,
        io_status: *mut IoStatusBlock,
        info: *mut c_void,
        length: u32,
        class: u32,
        return_single: u8,
        file_name: *const UnicodeString,
        restart: u8,
    ) -> i32;
    fn NtClose(handle: HANDLE) -> i32;
}

struct NtDir(HANDLE);
// SAFETY: a directory handle may be used from any thread.
unsafe impl Send for NtDir {}
// SAFETY: relative opens through a shared handle are thread safe.
unsafe impl Sync for NtDir {}
impl Drop for NtDir {
    fn drop(&mut self) {
        // SAFETY: closed exactly once.
        unsafe {
            NtClose(self.0);
        }
    }
}

fn nt_open(parent: Option<&NtDir>, name: &[u16]) -> Option<NtDir> {
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const SHARE_ALL: u32 = 0x7;
    const FILE_DIRECTORY_FILE: u32 = 0x1;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_OPEN_FOR_BACKUP_INTENT: u32 = 0x4000;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    let unicode = UnicodeString {
        length: (name.len() * 2) as u16,
        maximum_length: (name.len() * 2) as u16,
        buffer: name.as_ptr(),
    };
    let attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.map_or(HANDLE::default(), |p| p.0),
        object_name: &unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null(),
        security_qos: std::ptr::null(),
    };
    let mut handle = HANDLE::default();
    let mut io = IoStatusBlock {
        status: 0,
        information: 0,
    };
    // SAFETY: all pointers refer to live locals for the duration of the call.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
            &attributes,
            &mut io,
            SHARE_ALL,
            FILE_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_FOR_BACKUP_INTENT
                | FILE_OPEN_REPARSE_POINT,
        )
    };
    (status >= 0).then_some(NtDir(handle))
}

fn walk_rel(dir: &NtDir, totals: &Totals) {
    const FILE_FULL_DIRECTORY_INFORMATION: u32 = 2;
    let mut buffer = vec![0u64; 8192];
    let mut subdirs: Vec<Vec<u16>> = Vec::new();
    let (mut files, mut dirs, mut bytes, mut links) = (0, 0, 0, 0);
    loop {
        let mut io = IoStatusBlock {
            status: 0,
            information: 0,
        };
        // SAFETY: the buffer is writable, aligned and as long as stated.
        let status = unsafe {
            NtQueryDirectoryFile(
                dir.0,
                HANDLE::default(),
                std::ptr::null(),
                std::ptr::null(),
                &mut io,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                FILE_FULL_DIRECTORY_INFORMATION,
                0,
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            break;
        }
        let bytes_filled = io.information;
        // SAFETY: plain bytes of the initialised buffer.
        let raw = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), bytes_filled) };
        let mut at = 0usize;
        loop {
            let rec = &raw[at..];
            let u32_at =
                |o: usize| u32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]]);
            let next = u32_at(0) as usize;
            let size = u64::from(u32_at(40)) | (u64::from(u32_at(44)) << 32);
            let attrs = u32_at(56);
            let name_len = u32_at(60) as usize / 2;
            let tag = u32_at(64);
            // SAFETY: the name lies inside the filled part of the buffer.
            let name =
                unsafe { std::slice::from_raw_parts(rec.as_ptr().add(68).cast::<u16>(), name_len) };
            if name != [46] && name != [46, 46] {
                if attrs & ATTR_REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0 {
                    links += 1;
                } else if attrs & ATTR_DIRECTORY != 0 {
                    dirs += 1;
                    subdirs.push(name.to_vec());
                } else {
                    files += 1;
                    bytes += size;
                }
            }
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    drop(buffer);
    totals.add(files, dirs, bytes, links);
    subdirs
        .into_par_iter()
        .for_each(|name| match nt_open(Some(dir), &name) {
            Some(child) => walk_rel(&child, totals),
            None => {
                totals.failed.fetch_add(1, Ordering::Relaxed);
            }
        });
}

// ── harness ──────────────────────────────────────────────────────────

fn cpu_times() -> (f64, f64) {
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: valid output pointers, pseudo handle of this process.
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let ms = |t: FILETIME| {
        ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 10_000.0
    };
    (ms(u), ms(k))
}

fn measure(label: &str, threads: usize, run: impl FnOnce(&Totals) + Send) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .stack_size(16 * 1024 * 1024)
        .build()
        .expect("thread pool");
    let totals = Totals::default();
    let (user0, kernel0) = cpu_times();
    let started = Instant::now();
    pool.install(|| run(&totals));
    let wall = started.elapsed().as_secs_f64() * 1000.0;
    let (user1, kernel1) = cpu_times();
    println!(
        "{label:<5} {threads:>2} threads: {wall:>7.0} ms wall {:>7.0} ms user {:>7.0} ms kernel | {} files {} dirs {} links {:.1} GB, {} unreadable",
        user1 - user0,
        kernel1 - kernel0,
        totals.files.load(Ordering::Relaxed),
        totals.dirs.load(Ordering::Relaxed),
        totals.links.load(Ordering::Relaxed),
        totals.bytes.load(Ordering::Relaxed) as f64 / (1u64 << 30) as f64,
        totals.failed.load(Ordering::Relaxed),
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().unwrap_or_else(|| r"C:\".to_owned()));
    let root = fastdir::absolute(&root);
    let mut thread_counts: Vec<usize> = args.filter_map(|a| a.parse().ok()).collect();
    if thread_counts.is_empty() {
        thread_counts = vec![std::thread::available_parallelism().map_or(4, |n| n.get())];
    }
    let variants = std::env::var("WALK_BENCH").unwrap_or_else(|_| "find,bulk,rel".to_owned());
    println!("root: {}", root.display());
    for &threads in &thread_counts {
        for variant in variants.split(',') {
            match variant {
                "std" => measure("std", 1, |t| walk_std(&root, t)),
                "find" => measure("find", threads, |t| walk_find(root.clone(), t)),
                "bulk" => measure("bulk", threads, |t| walk_bulk(root.clone(), t)),
                "rel" => {
                    let mut nt_path: Vec<u16> = r"\??\".encode_utf16().collect();
                    nt_path.extend(root.as_os_str().encode_wide());
                    measure("rel", threads, |t| match nt_open(None, &nt_path) {
                        Some(dir) => walk_rel(&dir, t),
                        None => println!("could not open the root"),
                    });
                }
                other => println!("unknown variant {other}"),
            }
        }
    }
}
