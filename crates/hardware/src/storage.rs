//! Physical drives, their health and the volumes on them.
//!
//! Three sources are combined:
//!
//! * the Windows storage provider (`MSFT_PhysicalDisk`) for the drive list
//!   and Windows' own health verdict, readable by every user;
//! * the drive itself for wear, temperature and error counters: the NVMe
//!   health log and the storage temperature query, both of which Windows
//!   answers without administrator rights;
//! * `MSFT_StorageReliabilityCounter` for drives that are not NVMe, which
//!   Windows only hands to administrators.

use crate::model::{Bus, DriveCounters, Health, Media, PhysicalDrive, Volume};
use crate::system::clean;
use crate::wmi::{Record, Wmi, WmiError};
use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY;
use windows::core::PCWSTR;

/// `GetDriveTypeW`: removable media.
const DRIVE_REMOVABLE: u32 = 2;
/// `GetDriveTypeW`: fixed disk.
const DRIVE_FIXED: u32 = 3;

/// `STORAGE_PROPERTY_ID::StorageDeviceProtocolSpecificProperty`
const PROPERTY_PROTOCOL_SPECIFIC: u32 = 50;
/// `STORAGE_PROPERTY_ID::StorageDeviceTemperatureProperty`
const PROPERTY_TEMPERATURE: u32 = 52;
/// `STORAGE_PROTOCOL_TYPE::ProtocolTypeNvme`
const PROTOCOL_NVME: u32 = 3;
/// `STORAGE_PROTOCOL_NVME_DATA_TYPE::NVMeDataTypeLogPage`
const NVME_DATA_LOG_PAGE: u32 = 2;
/// NVMe log page 2: SMART / health information.
const NVME_LOG_HEALTH: u32 = 2;
/// Size of the NVMe health log.
const NVME_HEALTH_LOG_LEN: usize = 512;
/// Offset of `AdditionalParameters` in `STORAGE_PROPERTY_QUERY`.
const QUERY_HEADER_LEN: usize = 8;
/// Size of `STORAGE_PROTOCOL_SPECIFIC_DATA`.
const PROTOCOL_DATA_LEN: usize = 40;

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(buf: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        buf.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn get_u32(buf: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        buf.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// Reads a 128-bit little-endian counter, saturating at `u64::MAX`.
fn get_u128_as_u64(buf: &[u8], offset: usize) -> Option<u64> {
    let raw = u128::from_le_bytes(buf.get(offset..offset + 16)?.try_into().ok()?);
    Some(u64::try_from(raw).unwrap_or(u64::MAX))
}

/// A handle to a physical drive, opened for queries only: no read, no write
/// access is requested, which is why this works for every user.
struct DriveHandle(HANDLE);

impl DriveHandle {
    fn open(number: u32) -> Option<DriveHandle> {
        let path = wide(&format!(r"\\.\PhysicalDrive{number}"));
        // SAFETY: `path` is NUL terminated; no access rights are requested.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .ok()?;
        Some(DriveHandle(handle))
    }

    /// Sends a storage property query; `buffer` holds the request and
    /// receives the answer. Returns the number of bytes written.
    fn query_property(&self, buffer: &mut [u8]) -> Option<usize> {
        let mut returned = 0u32;
        // SAFETY: input and output are the same valid buffer of the stated
        // length; the control code only reads device properties.
        unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(buffer.as_ptr().cast()),
                buffer.len() as u32,
                Some(buffer.as_mut_ptr().cast()),
                buffer.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .ok()?;
        Some(returned as usize)
    }
}

impl Drop for DriveHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful CreateFileW.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Interprets an NVMe SMART / health information log page.
pub(crate) fn parse_nvme_health(log: &[u8]) -> Option<DriveCounters> {
    if log.len() < 192 {
        return None;
    }
    let kelvin = i32::from(get_u16(log, 1)?);
    // Data units are thousands of 512-byte blocks.
    let data_units =
        |offset: usize| get_u128_as_u64(log, offset).map(|units| units.saturating_mul(512_000));
    Some(DriveCounters {
        temperature_c: (kelvin > 0).then_some(kelvin - 273),
        wear_percent: Some(u32::from(log[5])),
        spare_percent: Some(u32::from(log[3])),
        spare_threshold_percent: Some(u32::from(log[4])),
        power_on_hours: get_u128_as_u64(log, 128),
        power_cycles: get_u128_as_u64(log, 112),
        unsafe_shutdowns: get_u128_as_u64(log, 144),
        media_errors: get_u128_as_u64(log, 160),
        read_errors: None,
        write_errors: None,
        bytes_read: data_units(32),
        bytes_written: data_units(48),
        critical_warning: Some(log[0]),
    })
}

/// Asks an NVMe drive for its health log.
fn nvme_health(drive: &DriveHandle) -> Option<DriveCounters> {
    let data_start = QUERY_HEADER_LEN + PROTOCOL_DATA_LEN;
    let mut buffer = vec![0u8; data_start + NVME_HEALTH_LOG_LEN];
    put_u32(&mut buffer, 0, PROPERTY_PROTOCOL_SPECIFIC);
    put_u32(&mut buffer, 4, 0); // PropertyStandardQuery
    let p = QUERY_HEADER_LEN;
    put_u32(&mut buffer, p, PROTOCOL_NVME);
    put_u32(&mut buffer, p + 4, NVME_DATA_LOG_PAGE);
    put_u32(&mut buffer, p + 8, NVME_LOG_HEALTH);
    put_u32(&mut buffer, p + 12, 0);
    put_u32(&mut buffer, p + 16, PROTOCOL_DATA_LEN as u32);
    put_u32(&mut buffer, p + 20, NVME_HEALTH_LOG_LEN as u32);
    drive.query_property(&mut buffer)?;
    // The answer is a STORAGE_PROTOCOL_DATA_DESCRIPTOR: version, size, then
    // the protocol data block, whose offset field locates the log. The log
    // must lie inside the buffer; the byte count the driver reports is not
    // relied on, because drivers differ in what they include in it.
    let offset = get_u32(&buffer, p + 16)? as usize;
    let length = get_u32(&buffer, p + 20)? as usize;
    let start = p + offset;
    if length < 192 || start + length > buffer.len() {
        return None;
    }
    let counters = parse_nvme_health(buffer.get(start..start + length)?)?;
    // A log of all zeroes means the drive answered with nothing.
    (!buffer[start..start + length].iter().all(|&b| b == 0)).then_some(counters)
}

/// Interprets a `STORAGE_TEMPERATURE_DATA_DESCRIPTOR`: the first sensor's
/// reading in degrees Celsius.
pub(crate) fn parse_temperature(answer: &[u8]) -> Option<i32> {
    let count = get_u16(answer, 12)?;
    if count == 0 {
        return None;
    }
    let celsius = i32::from(get_u16(answer, 24 + 2)? as i16);
    // Sensors that are not populated report zero or an out-of-range filler.
    (1..=150).contains(&celsius).then_some(celsius)
}

/// Asks any drive for its temperature.
fn temperature(drive: &DriveHandle) -> Option<i32> {
    let mut buffer = vec![0u8; 512];
    put_u32(&mut buffer, 0, PROPERTY_TEMPERATURE);
    put_u32(&mut buffer, 4, 0);
    let returned = drive.query_property(&mut buffer)?;
    parse_temperature(buffer.get(..returned.min(buffer.len()))?)
}

fn bus_of(code: Option<u32>) -> Bus {
    match code {
        Some(17) => Bus::Nvme,
        Some(3 | 11) => Bus::Sata,
        Some(7) => Bus::Usb,
        Some(10) => Bus::Sas,
        Some(8) => Bus::Raid,
        Some(12 | 13) => Bus::SdCard,
        Some(14..=16) => Bus::Virtual,
        Some(0) | None => Bus::Unknown,
        Some(_) => Bus::Other,
    }
}

fn health_of(code: Option<u32>) -> Health {
    match code {
        Some(0) => Health::Healthy,
        Some(1) => Health::Warning,
        Some(2) => Health::Unhealthy,
        _ => Health::Unknown,
    }
}

fn tidy_model(record: &Record) -> String {
    let raw = clean(record.text("Model"))
        .or_else(|| clean(record.text("FriendlyName")))
        .unwrap_or_else(|| "Drive".to_owned());
    let spaced = raw.replace('_', " ");
    spaced
        .strip_prefix("NVMe ")
        .unwrap_or(&spaced)
        .trim()
        .to_owned()
}

/// Builds the drive list from `MSFT_PhysicalDisk` records, without
/// counters or volumes.
pub(crate) fn drives_from(records: &[Record]) -> Vec<PhysicalDrive> {
    let mut drives: Vec<PhysicalDrive> = records
        .iter()
        .map(|record| {
            let bus = bus_of(record.u32("BusType"));
            let spindle = record
                .u32("SpindleSpeed")
                .filter(|&rpm| rpm > 0 && rpm != u32::MAX);
            let media = match record.u32("MediaType") {
                Some(3) => Media::Hdd,
                Some(4 | 5) => Media::Ssd,
                _ if bus == Bus::Nvme => Media::Ssd,
                _ if spindle.is_some() => Media::Hdd,
                _ => Media::Unknown,
            };
            PhysicalDrive {
                number: record.u32("DeviceId"),
                model: tidy_model(record),
                bus,
                media,
                size_bytes: record.u64("Size").unwrap_or(0),
                firmware: clean(record.text("FirmwareVersion")),
                spindle_rpm: spindle.filter(|_| media == Media::Hdd),
                health: health_of(record.u32("HealthStatus")),
                ..PhysicalDrive::default()
            }
        })
        .collect();
    drives.sort_by_key(|d| d.number.unwrap_or(u32::MAX));
    drives
}

/// Builds the drive list from the older `Win32_DiskDrive` class, for
/// systems without the storage provider.
pub(crate) fn drives_from_legacy(records: &[Record]) -> Vec<PhysicalDrive> {
    let mut drives: Vec<PhysicalDrive> = records
        .iter()
        .map(|record| {
            let interface = record.text("InterfaceType").unwrap_or_default();
            let model = clean(record.text("Model")).unwrap_or_else(|| "Drive".to_owned());
            let bus = if model.to_ascii_lowercase().contains("nvme") {
                Bus::Nvme
            } else if interface.eq_ignore_ascii_case("USB") {
                Bus::Usb
            } else if interface.eq_ignore_ascii_case("IDE") {
                Bus::Sata
            } else {
                Bus::Unknown
            };
            let health = match record.text("Status").as_deref() {
                Some("OK") => Health::Healthy,
                Some("Pred Fail" | "Degraded" | "Stressed") => Health::Warning,
                Some("Error" | "NonRecover" | "Lost Comm") => Health::Unhealthy,
                _ => Health::Unknown,
            };
            PhysicalDrive {
                number: record.u32("Index"),
                model,
                bus,
                media: if bus == Bus::Nvme {
                    Media::Ssd
                } else {
                    Media::Unknown
                },
                size_bytes: record.u64("Size").unwrap_or(0),
                firmware: clean(record.text("FirmwareRevision")),
                health,
                ..PhysicalDrive::default()
            }
        })
        .collect();
    drives.sort_by_key(|d| d.number.unwrap_or(u32::MAX));
    drives
}

/// Reads `MSFT_StorageReliabilityCounter` records into counters keyed by
/// the drive's device id.
pub(crate) fn reliability_from(records: &[Record]) -> HashMap<u32, DriveCounters> {
    records
        .iter()
        .filter_map(|record| {
            let counters = DriveCounters {
                temperature_c: record
                    .u32("Temperature")
                    .filter(|&c| (1..=150).contains(&c))
                    .map(|c| c as i32),
                wear_percent: record.u32("Wear"),
                power_on_hours: record.u64("PowerOnHours"),
                power_cycles: record.u64("StartStopCycleCount"),
                read_errors: record.u64("ReadErrorsUncorrected"),
                write_errors: record.u64("WriteErrorsUncorrected"),
                ..DriveCounters::default()
            };
            Some((record.u32("DeviceId")?, counters))
        })
        .collect()
}

/// Fills the gaps of `primary` with values from `secondary`.
fn merge_counters(primary: DriveCounters, secondary: &DriveCounters) -> DriveCounters {
    DriveCounters {
        temperature_c: primary.temperature_c.or(secondary.temperature_c),
        wear_percent: primary.wear_percent.or(secondary.wear_percent),
        spare_percent: primary.spare_percent.or(secondary.spare_percent),
        spare_threshold_percent: primary
            .spare_threshold_percent
            .or(secondary.spare_threshold_percent),
        power_on_hours: primary.power_on_hours.or(secondary.power_on_hours),
        power_cycles: primary.power_cycles.or(secondary.power_cycles),
        unsafe_shutdowns: primary.unsafe_shutdowns.or(secondary.unsafe_shutdowns),
        media_errors: primary.media_errors.or(secondary.media_errors),
        read_errors: primary.read_errors.or(secondary.read_errors),
        write_errors: primary.write_errors.or(secondary.write_errors),
        bytes_read: primary.bytes_read.or(secondary.bytes_read),
        bytes_written: primary.bytes_written.or(secondary.bytes_written),
        critical_warning: primary.critical_warning.or(secondary.critical_warning),
    }
}

/// Puts each lettered volume on the drive its partition belongs to and
/// counts the partitions without a letter. Returns the volumes no drive
/// claimed. `partitions` are `MSFT_Partition` records.
pub(crate) fn assign_volumes(
    drives: &mut [PhysicalDrive],
    partitions: &[Record],
    volumes: Vec<Volume>,
) -> Vec<Volume> {
    let mut disk_of_letter: HashMap<char, u32> = HashMap::new();
    for partition in partitions {
        let Some(disk) = partition.u32("DiskNumber") else {
            continue;
        };
        match partition.char("DriveLetter") {
            Some(letter) => {
                disk_of_letter.insert(letter.to_ascii_uppercase(), disk);
            }
            None => {
                if let Some(drive) = drives.iter_mut().find(|d| d.number == Some(disk)) {
                    drive.hidden_partitions += 1;
                }
            }
        }
    }
    let mut unclaimed = Vec::new();
    for volume in volumes {
        let drive = disk_of_letter
            .get(&volume.letter)
            .and_then(|disk| drives.iter_mut().find(|d| d.number == Some(*disk)));
        match drive {
            Some(drive) => drive.volumes.push(volume),
            None => unclaimed.push(volume),
        }
    }
    unclaimed
}

/// Lists the volumes with a drive letter on fixed and removable drives.
pub(crate) fn collect_volumes() -> Vec<Volume> {
    let system_letter = zest_core::paths::system_drive()
        .to_string_lossy()
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase());
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut volumes = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = wide(&format!(r"{letter}:\"));
        // SAFETY: `root` is NUL terminated.
        let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
        if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
            continue;
        }
        let (mut total, mut free) = (0u64, 0u64);
        // SAFETY: valid NUL terminated path and out parameters.
        let sized = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(root.as_ptr()),
                None,
                Some(&mut total),
                Some(&mut free),
            )
        };
        if sized.is_err() || total == 0 {
            continue; // no media, or a locked volume
        }
        let mut label = [0u16; 261];
        let mut file_system = [0u16; 261];
        // SAFETY: the buffers are valid for their stated lengths.
        let _ = unsafe {
            GetVolumeInformationW(
                PCWSTR(root.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut file_system),
            )
        };
        volumes.push(Volume {
            letter,
            label: from_wide_buf(&label),
            file_system: from_wide_buf(&file_system),
            total_bytes: total,
            free_bytes: free,
            is_system: Some(letter) == system_letter,
        });
    }
    volumes
}

/// Reads the physical drives of this PC with their health, counters and
/// volumes. Returns the drives and the volumes that belong to none of them.
pub(crate) fn collect_drives(elevated: bool) -> (Vec<PhysicalDrive>, Vec<Volume>) {
    let volumes = collect_volumes();
    let storage = Wmi::connect(r"ROOT\Microsoft\Windows\Storage").ok();
    let modern = storage.as_ref().and_then(|wmi| {
        wmi.query(
            "MSFT_PhysicalDisk",
            &[
                "DeviceId",
                "FriendlyName",
                "Model",
                "MediaType",
                "BusType",
                "Size",
                "HealthStatus",
                "FirmwareVersion",
                "SpindleSpeed",
            ],
            None,
        )
        .ok()
        .filter(|records| !records.is_empty())
    });
    let mut drives = match &modern {
        Some(records) => drives_from(records),
        None => {
            let legacy = Wmi::connect(r"ROOT\CIMV2")
                .ok()
                .and_then(|wmi| {
                    wmi.query(
                        "Win32_DiskDrive",
                        &[
                            "Index",
                            "Model",
                            "InterfaceType",
                            "Size",
                            "Status",
                            "FirmwareRevision",
                        ],
                        None,
                    )
                    .ok()
                })
                .unwrap_or_default();
            drives_from_legacy(&legacy)
        }
    };

    // Counters Windows keeps for administrators.
    let mut reliability_denied = false;
    let reliability = match storage.as_ref().map(|wmi| {
        wmi.query(
            "MSFT_StorageReliabilityCounter",
            &[
                "DeviceId",
                "Temperature",
                "Wear",
                "PowerOnHours",
                "StartStopCycleCount",
                "ReadErrorsUncorrected",
                "WriteErrorsUncorrected",
            ],
            None,
        )
    }) {
        Some(Ok(records)) => {
            // Without rights the provider answers with nothing at all.
            reliability_denied = records.is_empty() && !elevated;
            reliability_from(&records)
        }
        Some(Err(WmiError::AccessDenied)) => {
            reliability_denied = true;
            HashMap::new()
        }
        _ => HashMap::new(),
    };

    for drive in &mut drives {
        let Some(number) = drive.number else {
            continue;
        };
        let mut counters = DriveCounters::default();
        if let Some(handle) = DriveHandle::open(number) {
            if drive.bus == Bus::Nvme
                && let Some(log) = nvme_health(&handle)
            {
                counters = log;
            }
            if counters.temperature_c.is_none() {
                counters.temperature_c = temperature(&handle);
            }
        }
        if let Some(extra) = reliability.get(&number) {
            counters = merge_counters(counters, extra);
        }
        // Wear and hours are what the administrator-only counters would add,
        // and only internal drives keep them: memory cards, USB sticks and
        // virtual disks have none for anyone.
        drive.counters_need_admin = reliability_denied
            && matches!(drive.bus, Bus::Nvme | Bus::Sata | Bus::Sas | Bus::Raid)
            && counters.wear_percent.is_none()
            && counters.power_on_hours.is_none();
        drive.counters = counters;
    }

    let partitions = storage
        .as_ref()
        .and_then(|wmi| {
            wmi.query("MSFT_Partition", &["DiskNumber", "DriveLetter"], None)
                .ok()
        })
        .unwrap_or_default();
    let unclaimed = assign_volumes(&mut drives, &partitions, volumes);
    (drives, unclaimed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi::Value;

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    fn nvme_log() -> Vec<u8> {
        let mut log = vec![0u8; NVME_HEALTH_LOG_LEN];
        log[0] = 0; // no critical warning
        log[1..3].copy_from_slice(&312u16.to_le_bytes()); // 39 °C
        log[3] = 100; // spare
        log[4] = 10; // spare threshold
        log[5] = 3; // percentage used
        log[32..48].copy_from_slice(&2_000_000u128.to_le_bytes()); // data units read
        log[48..64].copy_from_slice(&1_000_000u128.to_le_bytes()); // data units written
        log[112..128].copy_from_slice(&420u128.to_le_bytes()); // power cycles
        log[128..144].copy_from_slice(&1234u128.to_le_bytes()); // power-on hours
        log[144..160].copy_from_slice(&7u128.to_le_bytes()); // unsafe shutdowns
        log[160..176].copy_from_slice(&0u128.to_le_bytes()); // media errors
        log
    }

    #[test]
    fn nvme_health_log_is_decoded() {
        let counters = parse_nvme_health(&nvme_log()).unwrap();
        assert_eq!(counters.temperature_c, Some(39));
        assert_eq!(counters.wear_percent, Some(3));
        assert_eq!(counters.spare_percent, Some(100));
        assert_eq!(counters.spare_threshold_percent, Some(10));
        assert_eq!(counters.power_on_hours, Some(1234));
        assert_eq!(counters.power_cycles, Some(420));
        assert_eq!(counters.unsafe_shutdowns, Some(7));
        assert_eq!(counters.media_errors, Some(0));
        assert_eq!(counters.bytes_read, Some(2_000_000 * 512_000));
        assert_eq!(counters.bytes_written, Some(1_000_000 * 512_000));
        assert_eq!(counters.critical_warning, Some(0));
        assert_eq!(parse_nvme_health(&[0u8; 100]), None, "too short");

        // Counters beyond 64 bits saturate instead of wrapping.
        let mut huge = nvme_log();
        huge[128..144].copy_from_slice(&u128::MAX.to_le_bytes());
        assert_eq!(
            parse_nvme_health(&huge).unwrap().power_on_hours,
            Some(u64::MAX)
        );
        // A drive that reports no temperature.
        let mut cold = nvme_log();
        cold[1..3].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse_nvme_health(&cold).unwrap().temperature_c, None);
    }

    #[test]
    fn temperature_answer_is_decoded() {
        let mut answer = vec![0u8; 40];
        answer[12..14].copy_from_slice(&1u16.to_le_bytes()); // one sensor
        answer[26..28].copy_from_slice(&41i16.to_le_bytes());
        assert_eq!(parse_temperature(&answer), Some(41));
        answer[26..28].copy_from_slice(&0i16.to_le_bytes());
        assert_eq!(parse_temperature(&answer), None, "an unpopulated sensor");
        answer[26..28].copy_from_slice(&(-5i16).to_le_bytes());
        assert_eq!(parse_temperature(&answer), None);
        answer[12..14].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse_temperature(&answer), None, "no sensors");
        assert_eq!(parse_temperature(&[0u8; 8]), None, "truncated");
    }

    fn disk(id: u32, model: &str, media: i64, bus: i64, health: i64) -> Record {
        Record::from_pairs(&[
            ("DeviceId", text(&id.to_string())),
            ("FriendlyName", text(&format!("NVMe {model}"))),
            ("Model", text(model)),
            ("MediaType", Value::Int(media)),
            ("BusType", Value::Int(bus)),
            ("Size", text("1024209543168")),
            ("HealthStatus", Value::Int(health)),
            ("FirmwareVersion", text("V8MA000")),
            ("SpindleSpeed", Value::Int(0)),
        ])
    }

    #[test]
    fn drives_are_described_from_the_storage_provider() {
        let spinning = Record::from_pairs(&[
            ("DeviceId", text("1")),
            ("Model", text("Example HDD 4TB")),
            ("MediaType", Value::Int(3)),
            ("BusType", Value::Int(11)),
            ("Size", text("4000787030016")),
            ("HealthStatus", Value::Int(1)),
            ("SpindleSpeed", Value::Int(7200)),
        ]);
        let unspecified = Record::from_pairs(&[
            ("DeviceId", text("2")),
            ("FriendlyName", text("Example Flash Drive")),
            ("MediaType", Value::Int(0)),
            ("BusType", Value::Int(7)),
            ("Size", text("64000000000")),
            ("HealthStatus", Value::Int(0)),
            ("SpindleSpeed", Value::Int(4_294_967_295)),
        ]);
        let drives = drives_from(&[
            unspecified,
            spinning,
            disk(0, "Example_2500_ABC1T0", 4, 17, 0),
        ]);
        assert_eq!(drives.len(), 3);
        assert_eq!(drives[0].number, Some(0), "sorted by disk number");
        assert_eq!(drives[0].model, "Example 2500 ABC1T0");
        assert_eq!((drives[0].bus, drives[0].media), (Bus::Nvme, Media::Ssd));
        assert_eq!(drives[0].kind_label(), "NVMe SSD");
        assert_eq!(drives[0].health, Health::Healthy);
        assert_eq!(drives[0].firmware.as_deref(), Some("V8MA000"));
        assert_eq!(drives[0].size_bytes, 1_024_209_543_168);
        assert_eq!((drives[1].bus, drives[1].media), (Bus::Sata, Media::Hdd));
        assert_eq!(drives[1].spindle_rpm, Some(7200));
        assert_eq!(drives[1].health, Health::Warning);
        assert_eq!(drives[1].kind_label(), "SATA Hard disk");
        assert_eq!(drives[2].model, "Example Flash Drive");
        assert_eq!((drives[2].bus, drives[2].media), (Bus::Usb, Media::Unknown));
        assert_eq!(
            drives[2].spindle_rpm, None,
            "the all-ones filler is not a speed"
        );
        assert_eq!(drives[2].kind_label(), "USB drive");
    }

    #[test]
    fn legacy_drive_list() {
        let records = [
            Record::from_pairs(&[
                ("Index", Value::Int(0)),
                ("Model", text("Example NVMe 512GB")),
                ("InterfaceType", text("SCSI")),
                ("Size", text("512105932800")),
                ("Status", text("OK")),
                ("FirmwareRevision", text("1.0")),
            ]),
            Record::from_pairs(&[
                ("Index", Value::Int(1)),
                ("Model", text("Example Disk")),
                ("InterfaceType", text("IDE")),
                ("Status", text("Pred Fail")),
            ]),
        ];
        let drives = drives_from_legacy(&records);
        assert_eq!(drives[0].bus, Bus::Nvme);
        assert_eq!(drives[0].health, Health::Healthy);
        assert_eq!(drives[1].bus, Bus::Sata);
        assert_eq!(drives[1].health, Health::Warning);
        assert_eq!(drives[1].size_bytes, 0);
    }

    #[test]
    fn administrator_counters_fill_the_gaps() {
        let records = [Record::from_pairs(&[
            ("DeviceId", text("1")),
            ("Temperature", Value::Int(34)),
            ("Wear", Value::Int(12)),
            ("PowerOnHours", Value::Int(20_000)),
            ("ReadErrorsUncorrected", text("3")),
        ])];
        let map = reliability_from(&records);
        let extra = &map[&1];
        assert_eq!(extra.wear_percent, Some(12));
        assert_eq!(extra.read_errors, Some(3));

        let own = DriveCounters {
            temperature_c: Some(40),
            ..DriveCounters::default()
        };
        let merged = merge_counters(own, extra);
        assert_eq!(
            merged.temperature_c,
            Some(40),
            "the drive's own reading wins"
        );
        assert_eq!(merged.power_on_hours, Some(20_000));
        assert!(!merged.is_empty());
        assert!(DriveCounters::default().is_empty());
    }

    fn volume(letter: char) -> Volume {
        Volume {
            letter,
            label: String::new(),
            file_system: "NTFS".to_owned(),
            total_bytes: 100,
            free_bytes: 40,
            is_system: letter == 'C',
        }
    }

    #[test]
    fn volumes_land_on_their_drives() {
        let mut drives = drives_from(&[disk(0, "A", 4, 17, 0), disk(1, "B", 4, 11, 0)]);
        let partition = |disk: i64, letter: Option<char>| {
            let mut pairs = vec![("DiskNumber", Value::Int(disk))];
            // WMI reports a missing letter as character code zero.
            pairs.push(("DriveLetter", Value::Int(letter.map_or(0, |c| c as i64))));
            Record::from_pairs(&pairs)
        };
        let partitions = [
            partition(0, None),
            partition(0, None),
            partition(0, Some('C')),
            partition(1, Some('d')),
            partition(7, Some('X')),
        ];
        let unclaimed = assign_volumes(
            &mut drives,
            &partitions,
            vec![volume('C'), volume('D'), volume('X'), volume('Z')],
        );
        assert_eq!(drives[0].volumes.len(), 1);
        assert_eq!(drives[0].volumes[0].letter, 'C');
        assert_eq!(drives[0].hidden_partitions, 2);
        assert_eq!(drives[1].volumes[0].letter, 'D');
        let letters: Vec<char> = unclaimed.iter().map(|v| v.letter).collect();
        assert_eq!(
            letters,
            vec!['X', 'Z'],
            "volumes of unknown disks stay visible"
        );
        assert_eq!(volume('C').display_name(), "Local disk (C:)");
        assert!((volume('C').used_fraction() - 0.6).abs() < 1e-6);
        assert!((volume('C').free_fraction() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn this_pc_answers() {
        let volumes = collect_volumes();
        let system = volumes
            .iter()
            .find(|v| v.is_system)
            .expect("a system volume");
        assert!(system.total_bytes > 0 && system.free_bytes <= system.total_bytes);

        let (drives, unclaimed) = collect_drives(false);
        assert!(!drives.is_empty(), "every PC has a drive");
        let lettered = drives.iter().map(|d| d.volumes.len()).sum::<usize>() + unclaimed.len();
        assert_eq!(lettered, volumes.len(), "no volume is lost or duplicated");
        for drive in &drives {
            assert!(!drive.model.is_empty());
            if let Some(celsius) = drive.counters.temperature_c {
                assert!((1..=150).contains(&celsius), "{celsius}");
            }
        }
    }
}
