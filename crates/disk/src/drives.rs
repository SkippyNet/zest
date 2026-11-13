//! Discovery of drives and their properties.

use serde::{Deserialize, Serialize};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DEVICE_TRIM_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
    PropertyStandardQuery, STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY,
    StorageDeviceSeekPenaltyProperty, StorageDeviceTrimProperty,
};
use windows::core::PCWSTR;
use zest_core::{Error, Result, paths};

/// What kind of device a drive letter refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DriveKind {
    /// Internal disk (hard disk or solid-state drive).
    Fixed,
    /// USB stick, memory card or other removable media.
    Removable,
    /// Mapped network share.
    Network,
    /// CD / DVD / Blu-ray drive.
    Optical,
    /// RAM disk.
    RamDisk,
    /// Could not be determined.
    Unknown,
}

impl DriveKind {
    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Fixed => "Local disk",
            DriveKind::Removable => "Removable drive",
            DriveKind::Network => "Network drive",
            DriveKind::Optical => "Optical drive",
            DriveKind::RamDisk => "RAM disk",
            DriveKind::Unknown => "Unknown",
        }
    }

    fn from_raw(raw: u32) -> Self {
        match raw {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            6 => DriveKind::RamDisk,
            _ => DriveKind::Unknown,
        }
    }
}

/// A drive letter and what is known about its volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveInfo {
    /// Root path, e.g. `C:\`.
    pub root: PathBuf,
    /// Upper-case drive letter.
    pub letter: char,
    /// Volume label (may be empty).
    pub label: String,
    /// File system name, e.g. `NTFS`, `exFAT` (empty when not ready).
    pub file_system: String,
    /// Capacity in bytes (zero when not ready).
    pub total_bytes: u64,
    /// Free bytes available to the current user (zero when not ready).
    pub free_bytes: u64,
    /// Device kind.
    pub kind: DriveKind,
    /// True for the drive Windows is installed on.
    pub is_system: bool,
    /// `Some(true)` for solid-state storage, `Some(false)` for spinning
    /// disks, `None` when the device does not say (or the query is refused).
    pub is_ssd: Option<bool>,
    /// False when no media is present or the volume cannot be read
    /// (empty card reader, disconnected share, locked BitLocker volume).
    pub is_ready: bool,
}

impl DriveInfo {
    /// Bytes in use.
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    /// Used fraction in `0.0..=1.0` (zero when the capacity is unknown).
    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.used_bytes() as f64 / self.total_bytes as f64) as f32
        }
    }

    /// A short caption such as `Windows (C:)` or `Local disk (D:)`.
    pub fn display_name(&self) -> String {
        let name = if self.label.is_empty() {
            self.kind.label()
        } else {
            self.label.as_str()
        };
        format!("{name} ({}:)", self.letter)
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// The upper-case drive letter a path lives on, if it has one.
pub fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix(r"\\.\"))
        .unwrap_or(&text);
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some(bytes[0].to_ascii_uppercase() as char)
    } else {
        None
    }
}

/// True when `path` is exactly a drive root such as `D:\` or `D:`.
pub fn is_drive_root(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let bytes = text.as_bytes();
    matches!(bytes.len(), 2 | 3)
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'\\' || bytes[2] == b'/')
}

fn system_letter() -> Option<char> {
    drive_letter(&paths::system_drive())
}

fn query_kind(letter: char) -> DriveKind {
    let root = wide(&format!("{letter}:\\"));
    // SAFETY: `root` is a NUL terminated drive root string.
    DriveKind::from_raw(unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) })
}

/// Asks the storage device behind a drive letter whether it is solid-state.
/// Works without administrator rights because the volume is opened with no
/// access rights at all (metadata queries only).
fn query_is_ssd(letter: char) -> Option<bool> {
    let device = wide(&format!("\\\\.\\{letter}:"));
    // SAFETY: `device` is NUL terminated; the handle is closed below.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(device.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;

    let query = |property: STORAGE_PROPERTY_ID, out: *mut core::ffi::c_void, out_len: u32| {
        let request = STORAGE_PROPERTY_QUERY {
            PropertyId: property,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut returned = 0u32;
        // SAFETY: input and output buffers are valid for the sizes passed.
        unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some((&raw const request).cast()),
                size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(out),
                out_len,
                Some(&mut returned),
                None,
            )
        }
        .is_ok()
            && returned >= out_len
    };

    let mut seek = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    let mut result = None;
    if query(
        StorageDeviceSeekPenaltyProperty,
        (&raw mut seek).cast(),
        size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
    ) {
        result = Some(!seek.IncursSeekPenalty);
    } else {
        let mut trim = DEVICE_TRIM_DESCRIPTOR::default();
        if query(
            StorageDeviceTrimProperty,
            (&raw mut trim).cast(),
            size_of::<DEVICE_TRIM_DESCRIPTOR>() as u32,
        ) && trim.TrimEnabled
        {
            // TRIM support implies flash storage; its absence proves nothing.
            result = Some(true);
        }
    }
    // SAFETY: `handle` was returned by CreateFileW above.
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

fn describe(letter: char, kind: DriveKind) -> DriveInfo {
    let root_text = format!("{letter}:\\");
    let root_wide = wide(&root_text);
    let mut label = [0u16; 261];
    let mut file_system = [0u16; 261];
    // SAFETY: the buffers are valid for their full length; root is NUL terminated.
    let is_ready = unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut file_system),
        )
    }
    .is_ok();

    let (mut free, mut total) = (0u64, 0u64);
    if is_ready {
        // SAFETY: out pointers reference live locals.
        let _ = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(root_wide.as_ptr()),
                Some(&mut free),
                Some(&mut total),
                None,
            )
        };
    }

    let is_ssd = match kind {
        DriveKind::Fixed | DriveKind::Removable if is_ready => query_is_ssd(letter),
        _ => None,
    };

    DriveInfo {
        root: PathBuf::from(root_text),
        letter,
        label: if is_ready {
            from_wide_buf(&label)
        } else {
            String::new()
        },
        file_system: if is_ready {
            from_wide_buf(&file_system)
        } else {
            String::new()
        },
        total_bytes: total,
        free_bytes: free,
        kind,
        is_system: system_letter() == Some(letter),
        is_ssd,
        is_ready,
    }
}

fn present_letters() -> Vec<char> {
    // SAFETY: no arguments; returns a bit mask of drive letters.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
        .collect()
}

/// Every drive letter currently present, in alphabetical order.
///
/// Querying a disconnected network share can block for several seconds, so
/// call this from a background thread (or use [`list_local_drives`]).
pub fn list_drives() -> Vec<DriveInfo> {
    present_letters()
        .into_iter()
        .map(|letter| describe(letter, query_kind(letter)))
        .collect()
}

/// Internal and removable drives only. Network, optical and RAM drives are
/// skipped without being probed, so this never waits on a network timeout.
pub fn list_local_drives() -> Vec<DriveInfo> {
    present_letters()
        .into_iter()
        .filter_map(|letter| match query_kind(letter) {
            kind @ (DriveKind::Fixed | DriveKind::Removable) => Some(describe(letter, kind)),
            _ => None,
        })
        .collect()
}

/// Information about the drive that holds `path`.
/// Returns `None` for paths without a drive letter (UNC shares, relative paths).
pub fn drive_info(path: &Path) -> Option<DriveInfo> {
    let letter = drive_letter(path)?;
    if !present_letters().contains(&letter) {
        return None;
    }
    Some(describe(letter, query_kind(letter)))
}

/// Free bytes available to the current user on the volume that holds `path`
/// (any existing folder on that volume).
pub fn free_bytes(path: &Path) -> Result<u64> {
    let dir = wide_path(path);
    let mut free = 0u64;
    // SAFETY: `dir` is NUL terminated and `free` is a live local.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(dir.as_ptr()), Some(&mut free), None, None) }.map_err(
        |e| {
            Error::Windows(format!(
                "reading free space of {}: {}",
                path.display(),
                e.message()
            ))
        },
    )?;
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_drive_is_listed_and_sane() {
        let drives = list_drives();
        let system: Vec<&DriveInfo> = drives.iter().filter(|d| d.is_system).collect();
        assert_eq!(system.len(), 1, "{drives:?}");
        let sys = system[0];
        assert!(sys.is_ready);
        assert_eq!(sys.kind, DriveKind::Fixed);
        assert!(sys.total_bytes > 0);
        assert!(sys.free_bytes <= sys.total_bytes);
        assert!(!sys.file_system.is_empty());
        assert_eq!(sys.root, paths::system_drive());
        assert!((0.0..=1.0).contains(&sys.used_fraction()));
        assert!(sys.display_name().ends_with(&format!("({}:)", sys.letter)));
        // Letters are unique and sorted.
        let letters: Vec<char> = drives.iter().map(|d| d.letter).collect();
        let mut sorted = letters.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(letters, sorted);
    }

    #[test]
    fn local_listing_is_a_subset() {
        let all = list_drives();
        for drive in list_local_drives() {
            assert!(matches!(
                drive.kind,
                DriveKind::Fixed | DriveKind::Removable
            ));
            assert!(all.iter().any(|d| d.letter == drive.letter));
        }
    }

    #[test]
    fn letters_and_roots() {
        assert_eq!(drive_letter(Path::new(r"c:\Users")), Some('C'));
        assert_eq!(drive_letter(Path::new(r"\\?\D:\x")), Some('D'));
        assert_eq!(drive_letter(Path::new(r"\\server\share")), None);
        assert_eq!(drive_letter(Path::new("relative")), None);
        assert!(is_drive_root(Path::new(r"D:\")));
        assert!(is_drive_root(Path::new("d:")));
        assert!(is_drive_root(Path::new(r"\\?\E:\")));
        assert!(!is_drive_root(Path::new(r"D:\folder")));
        assert!(!is_drive_root(Path::new(r"\\server\share")));
        assert!(!is_drive_root(Path::new("")));
    }

    #[test]
    fn info_and_free_space_for_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        let info = drive_info(tmp.path()).expect("temp dir is on a lettered drive");
        assert!(info.is_ready);
        let free = free_bytes(tmp.path()).unwrap();
        assert!(free > 0 && free <= info.total_bytes);
        assert!(drive_info(Path::new(r"\\server\share\x")).is_none());
        assert!(free_bytes(&tmp.path().join("missing-folder")).is_err());
    }

    #[test]
    fn serializes() {
        let drives = list_local_drives();
        let json = serde_json::to_string(&drives).unwrap();
        let back: Vec<DriveInfo> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, drives);
    }
}
