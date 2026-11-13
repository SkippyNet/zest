//! Private native plumbing: process enumeration through a single system
//! call, handle ownership, and the handful of `ntdll` entry points that have
//! no documented Win32 wrapper.

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;
use zest_core::{Error, Result};

// SAFETY: the signatures below match the long-stable native system calls
// exported by ntdll.dll on every supported Windows version. They are linked
// as a raw import, so no import library is required.
#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> i32;
    fn NtSuspendProcess(process: *mut c_void) -> i32;
    fn NtResumeProcess(process: *mut c_void) -> i32;
}

const SYSTEM_PROCESS_INFORMATION: u32 = 5;
const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

/// Thread state "Waiting" and wait reason "Suspended" as reported by the kernel.
const THREAD_STATE_WAITING: u32 = 5;
const WAIT_REASON_SUSPENDED: u32 = 5;

/// 100-nanosecond intervals between 1601-01-01 and the Unix epoch.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// Layout of one record returned for `SystemProcessInformation`.
#[repr(C)]
struct ProcessRecord {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

/// Layout of one thread record following a [`ProcessRecord`].
#[repr(C)]
struct ThreadRecord {
    kernel_time: i64,
    user_time: i64,
    create_time: i64,
    wait_time: u32,
    start_address: usize,
    client_id: [usize; 2],
    priority: i32,
    base_priority: i32,
    context_switches: u32,
    thread_state: u32,
    wait_reason: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<ProcessRecord>() == 256);
    assert!(size_of::<ThreadRecord>() == 80);
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorRecord {
    idle_time: i64,
    kernel_time: i64,
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
}

/// One process as reported by the kernel, without opening any handle.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RawProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub session_id: u32,
    pub threads: u32,
    pub handles: u32,
    /// Creation time in FILETIME ticks; together with `pid` this identifies
    /// a process unambiguously even after the pid is reused.
    pub create_time: u64,
    /// Kernel plus user time in 100 ns units.
    pub cpu_time: u64,
    pub working_set: u64,
    pub private_working_set: u64,
    pub private_bytes: u64,
    /// Bytes read plus bytes written since the process started.
    pub io_bytes: u64,
    /// True when every thread is in the suspended wait state.
    pub suspended: bool,
}

/// Reusable buffer for process enumeration.
#[derive(Default)]
pub(crate) struct ProcessLister {
    // u64 elements guarantee the 8-byte alignment the records require.
    buf: Vec<u64>,
}

impl ProcessLister {
    pub fn new() -> Self {
        ProcessLister {
            buf: vec![0u64; 64 * 1024],
        }
    }

    /// Lists every process on the system with one system call.
    pub fn list(&mut self) -> Result<Vec<RawProcess>> {
        if self.buf.is_empty() {
            self.buf = vec![0u64; 64 * 1024];
        }
        let mut returned = 0u32;
        let mut attempts = 0;
        loop {
            let bytes = (self.buf.len() * 8).min(u32::MAX as usize) as u32;
            // SAFETY: the buffer is valid for `bytes` bytes and suitably aligned.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION,
                    self.buf.as_mut_ptr().cast(),
                    bytes,
                    &mut returned,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH && attempts < 8 {
                attempts += 1;
                let wanted = (returned as usize).max(bytes as usize) + 256 * 1024;
                self.buf = vec![0u64; wanted.div_ceil(8)];
                continue;
            }
            if status < 0 {
                return Err(Error::Windows(format!(
                    "listing processes failed (status 0x{:08X})",
                    status as u32
                )));
            }
            break;
        }

        let base = self.buf.as_ptr() as usize;
        let total = (returned as usize).min(self.buf.len() * 8);
        let mut out = Vec::with_capacity(512);
        let mut offset = 0usize;
        while offset + size_of::<ProcessRecord>() <= total {
            // SAFETY: the record lies fully inside the buffer (checked above)
            // and the buffer is 8-byte aligned, as are all record offsets.
            let record = unsafe { &*((base + offset) as *const ProcessRecord) };
            let next = record.next_entry_offset as usize;
            let end = if next == 0 {
                total
            } else {
                (offset + next).min(total)
            };

            let name = {
                let ptr = record.image_name.buffer as usize;
                let len = record.image_name.length as usize;
                if ptr >= base && len > 0 && ptr + len <= base + total && ptr.is_multiple_of(2) {
                    // SAFETY: the string lies inside the buffer (checked above).
                    let units = unsafe { std::slice::from_raw_parts(ptr as *const u16, len / 2) };
                    String::from_utf16_lossy(units)
                } else {
                    String::new()
                }
            };

            let thread_count = record.number_of_threads as usize;
            let threads_start = offset + size_of::<ProcessRecord>();
            let mut suspended = thread_count > 0;
            for i in 0..thread_count {
                let at = threads_start + i * size_of::<ThreadRecord>();
                if at + size_of::<ThreadRecord>() > end {
                    suspended = false;
                    break;
                }
                // SAFETY: the thread record lies inside this process entry.
                let thread = unsafe { &*((base + at) as *const ThreadRecord) };
                if thread.thread_state != THREAD_STATE_WAITING
                    || thread.wait_reason != WAIT_REASON_SUSPENDED
                {
                    suspended = false;
                    break;
                }
            }

            let pid = record.unique_process_id as u32;
            out.push(RawProcess {
                pid,
                parent_pid: record.inherited_from_unique_process_id as u32,
                name: if pid == 0 && name.is_empty() {
                    "System Idle Process".to_owned()
                } else {
                    name
                },
                session_id: record.session_id,
                threads: record.number_of_threads,
                handles: record.handle_count,
                create_time: record.create_time.max(0) as u64,
                cpu_time: (record.kernel_time.max(0) as u64)
                    .saturating_add(record.user_time.max(0) as u64),
                working_set: record.working_set_size as u64,
                private_working_set: record.working_set_private_size.max(0) as u64,
                private_bytes: record.private_page_count as u64,
                io_bytes: (record.read_transfer_count.max(0) as u64)
                    .saturating_add(record.write_transfer_count.max(0) as u64),
                suspended,
            });

            if next == 0 {
                break;
            }
            offset += next;
        }
        Ok(out)
    }
}

/// Per logical processor `(idle, total)` times in 100 ns units.
pub(crate) fn processor_times() -> Vec<(u64, u64)> {
    let mut records = vec![ProcessorRecord::default(); 512];
    let mut returned = 0u32;
    // SAFETY: the buffer is valid for the byte length passed.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            records.as_mut_ptr().cast(),
            (records.len() * size_of::<ProcessorRecord>()) as u32,
            &mut returned,
        )
    };
    if status < 0 {
        return Vec::new();
    }
    let count = returned as usize / size_of::<ProcessorRecord>();
    records
        .iter()
        .take(count)
        .map(|r| {
            // Kernel time includes idle time.
            let total = (r.kernel_time.max(0) as u64).saturating_add(r.user_time.max(0) as u64);
            (r.idle_time.max(0) as u64, total)
        })
        .collect()
}

/// System-wide `(idle, total)` processor time in 100 ns units.
pub(crate) fn system_times() -> Option<(u64, u64)> {
    use windows::Win32::System::Threading::GetSystemTimes;
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all three out pointers are valid FILETIME structures.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    Some((
        filetime_ticks(idle),
        filetime_ticks(kernel).saturating_add(filetime_ticks(user)),
    ))
}

pub(crate) fn filetime_ticks(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// Converts FILETIME ticks to a [`SystemTime`]; `None` for zero or pre-epoch values.
pub(crate) fn ticks_to_system_time(ticks: u64) -> Option<SystemTime> {
    let since_epoch = ticks.checked_sub(FILETIME_UNIX_EPOCH)?;
    if ticks == 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(since_epoch.saturating_mul(100)))
}

/// A process handle that closes itself.
pub(crate) struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle was returned by OpenProcess and is closed once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Why a process could not be opened for an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// The process no longer exists (or its pid now belongs to another process).
    Exited,
    /// The caller lacks the rights; usually means elevation is required.
    Denied,
    /// Anything else.
    Other(String),
}

const E_ACCESSDENIED: u32 = 0x8007_0005;
const E_INVALID_PARAMETER: u32 = 0x8007_0057;
const STILL_ACTIVE: u32 = 259;

/// Opens a process without checking its identity.
pub(crate) fn open_process(
    pid: u32,
    access: PROCESS_ACCESS_RIGHTS,
) -> std::result::Result<OwnedHandle, OpenError> {
    // SAFETY: plain FFI call; the returned handle is owned by `OwnedHandle`.
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => Ok(OwnedHandle(handle)),
        Err(e) => match e.code().0 as u32 {
            E_ACCESSDENIED => Err(OpenError::Denied),
            E_INVALID_PARAMETER => Err(OpenError::Exited),
            _ => Err(OpenError::Other(e.message())),
        },
    }
}

/// Opens a process and verifies that it is still the process that was
/// started at `start_ticks` (guards against pid reuse) and still running.
///
/// `access` must include a query right so the identity can be checked.
pub(crate) fn open_verified(
    pid: u32,
    start_ticks: u64,
    access: PROCESS_ACCESS_RIGHTS,
) -> std::result::Result<OwnedHandle, OpenError> {
    if pid == 0 || pid == 4 {
        return Err(OpenError::Denied);
    }
    let handle = open_process(pid, access)?;
    match process_create_time(&handle) {
        Some(created) if created == start_ticks => {}
        Some(_) => return Err(OpenError::Exited),
        None => {
            return Err(OpenError::Other(
                "could not verify the process identity".to_owned(),
            ));
        }
    }
    let mut code = 0u32;
    // SAFETY: valid handle and out pointer.
    if unsafe { GetExitCodeProcess(handle.0, &mut code) }.is_ok() && code != STILL_ACTIVE {
        return Err(OpenError::Exited);
    }
    Ok(handle)
}

/// Creation time of a process in FILETIME ticks.
pub(crate) fn process_create_time(handle: &OwnedHandle) -> Option<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: valid handle and four valid out pointers.
    unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some(filetime_ticks(created))
}

/// Full Win32 path of a process image.
pub(crate) fn image_path(handle: &OwnedHandle) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    loop {
        let mut len = buf.len() as u32;
        // SAFETY: `buf` is valid for `len` UTF-16 units.
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        };
        match result {
            Ok(()) => {
                let text = String::from_utf16_lossy(&buf[..len as usize]);
                return (!text.is_empty()).then(|| PathBuf::from(text));
            }
            // ERROR_INSUFFICIENT_BUFFER: retry with a larger buffer.
            Err(e) if e.code().0 as u32 == 0x8007_007A && buf.len() < 32_768 => {
                buf = vec![0u16; buf.len() * 4];
            }
            Err(_) => return None,
        }
    }
}

fn nt_result(status: i32, what: &str) -> Result<()> {
    if status >= 0 {
        Ok(())
    } else if status as u32 == 0xC000_0022 {
        Err(Error::NeedsElevation(what.to_owned()))
    } else {
        Err(Error::Windows(format!(
            "could not {what} (status 0x{:08X})",
            status as u32
        )))
    }
}

/// Freezes every thread of the process. Needs `PROCESS_SUSPEND_RESUME`.
pub(crate) fn suspend_process(handle: &OwnedHandle) -> Result<()> {
    // SAFETY: the handle is valid for the duration of the call.
    nt_result(
        unsafe { NtSuspendProcess(handle.0.0) },
        "suspend the process",
    )
}

/// Reverses one [`suspend_process`]. Needs `PROCESS_SUSPEND_RESUME`.
pub(crate) fn resume_process(handle: &OwnedHandle) -> Result<()> {
    // SAFETY: the handle is valid for the duration of the call.
    nt_result(unsafe { NtResumeProcess(handle.0.0) }, "resume the process")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_this_process_with_sane_numbers() {
        let mut lister = ProcessLister::new();
        let all = lister.list().unwrap();
        let me = all
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("own process must be listed");
        assert!(me.name.to_lowercase().ends_with(".exe"), "{}", me.name);
        assert!(me.threads >= 1);
        assert!(me.working_set > 0);
        assert!(me.create_time > FILETIME_UNIX_EPOCH);
        assert!(!me.suspended);
        assert!(all.iter().any(|p| p.pid == 4 && p.name == "System"));
        assert!(all.len() > 20);
        // Calling again reuses the buffer and still works.
        assert!(
            lister
                .list()
                .unwrap()
                .iter()
                .any(|p| p.pid == std::process::id())
        );
    }

    #[test]
    fn identity_check_rejects_wrong_start_time() {
        use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
        let me = std::process::id();
        let handle = open_process(me, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        let created = process_create_time(&handle).unwrap();
        assert!(open_verified(me, created, PROCESS_QUERY_LIMITED_INFORMATION).is_ok());
        assert!(matches!(
            open_verified(me, created + 1, PROCESS_QUERY_LIMITED_INFORMATION),
            Err(OpenError::Exited)
        ));
        assert!(image_path(&handle).is_some_and(|p| p.is_file()));
        assert!(matches!(
            open_process(0x7FFF_FFF0, PROCESS_QUERY_LIMITED_INFORMATION),
            Err(OpenError::Exited)
        ));
    }

    #[test]
    fn time_helpers() {
        assert_eq!(ticks_to_system_time(0), None);
        assert_eq!(
            ticks_to_system_time(FILETIME_UNIX_EPOCH),
            Some(SystemTime::UNIX_EPOCH)
        );
        let (idle, total) = system_times().unwrap();
        assert!(total >= idle && total > 0);
        let cores = processor_times();
        assert!(!cores.is_empty());
        assert!(cores.iter().all(|(idle, total)| total >= idle));
    }
}
