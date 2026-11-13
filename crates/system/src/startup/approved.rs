//! The `Explorer\StartupApproved` flags.
//!
//! Windows records whether a Run-key value or Startup-folder file may start
//! in a 12-byte binary value named after the entry: the low bit of the first
//! byte means "disabled", followed (when disabled) by the time it was turned
//! off. A missing value means "enabled". Task Manager and Settings read and
//! write exactly these values, so changes made here show up there.

use crate::util::{filetime_now, io_err};
use winreg::RegValue;
use winreg::enums::{KEY_READ, KEY_WRITE, RegType};
use zest_core::Result;
use zest_core::reg::{RegBackup, RegPath, RegRoot};

const APPROVED_BASE: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

/// The approval key for a hive and list name (`Run`, `Run32`, `StartupFolder`).
pub(crate) fn approved_key(root: RegRoot, list: &str) -> RegPath {
    RegPath::new(root, format!("{APPROVED_BASE}\\{list}"))
}

/// The per-user twin of a machine-wide approval key. A user may switch a
/// machine-wide entry off for themselves; that flag lives under the same
/// path in their own hive.
fn user_override(key: &RegPath) -> Option<RegPath> {
    (key.root == RegRoot::Hklm).then(|| RegPath::hkcu(key.path.clone()))
}

/// Reads one flag: `Some(true)` enabled, `Some(false)` disabled, `None` unset.
fn read_flag(key: &RegPath, name: &str) -> Option<bool> {
    let raw = key.open().ok()?.get_raw_value(name).ok()?;
    let first = raw.bytes.first()?;
    Some(first & 1 == 0)
}

/// Whether the entry `name` is allowed to start.
pub(crate) fn is_enabled(key: &RegPath, name: &str) -> bool {
    let primary = read_flag(key, name).unwrap_or(true);
    let overridden = user_override(key)
        .and_then(|k| read_flag(&k, name))
        .unwrap_or(true);
    primary && overridden
}

fn write_flag(key: &RegPath, name: &str, enabled: bool) -> Result<()> {
    let action = format!("change the startup state of \"{name}\"");
    let (handle, _) = key
        .root
        .key()
        .create_subkey_with_flags(&key.path, KEY_READ | KEY_WRITE)
        .map_err(|e| io_err(e, &action))?;
    let mut bytes = vec![0u8; 12];
    if enabled {
        bytes[0] = 0x02;
    } else {
        bytes[0] = 0x03;
        bytes[4..12].copy_from_slice(&filetime_now().to_le_bytes());
    }
    handle
        .set_raw_value(
            name,
            &RegValue {
                bytes: bytes.into(),
                vtype: RegType::REG_BINARY,
            },
        )
        .map_err(|e| io_err(e, &action))
}

/// Enables or disables the entry `name`.
pub(crate) fn set_enabled(key: &RegPath, name: &str, enabled: bool) -> Result<()> {
    write_flag(key, name, enabled)?;
    // Enabling must also lift a per-user "off" switch, otherwise the entry
    // would stay disabled for this user.
    if enabled
        && let Some(user_key) = user_override(key)
        && read_flag(&user_key, name) == Some(false)
    {
        write_flag(&user_key, name, true)?;
    }
    Ok(())
}

/// Removes the flag of a deleted entry (after exporting it to `backup`, so a
/// restored entry comes back in the state it was deleted in). Best effort:
/// a leftover flag is harmless.
pub(crate) fn forget(key: &RegPath, name: &str, backup: Option<&mut RegBackup>) {
    if let Err(e) = zest_core::reg::delete_value(key, name, backup) {
        log::debug!("startup flag of \"{name}\" left in place: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    #[test]
    fn missing_flag_means_enabled() {
        let scratch = Scratch::new("approved-missing");
        assert!(is_enabled(&scratch.0, "Anything"));
        // A key that does not exist at all behaves the same.
        assert!(is_enabled(&scratch.0.child("NoSuchKey"), "Anything"));
    }

    #[test]
    fn disable_and_enable_round_trip() {
        let scratch = Scratch::new("approved-toggle");
        let key = scratch.0.child("Run");

        set_enabled(&key, "My App", false).unwrap();
        assert!(!is_enabled(&key, "My App"));
        let raw = key.open().unwrap().get_raw_value("My App").unwrap();
        assert_eq!(raw.bytes.len(), 12);
        assert_eq!(raw.bytes[0], 0x03);
        assert!(raw.bytes[4..].iter().any(|&b| b != 0), "timestamp missing");

        set_enabled(&key, "My App", true).unwrap();
        assert!(is_enabled(&key, "My App"));
        let raw = key.open().unwrap().get_raw_value("My App").unwrap();
        assert_eq!(&raw.bytes[..], &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn understands_flags_written_by_windows() {
        let scratch = Scratch::new("approved-windows");
        let key = scratch.0.open_write().unwrap();
        for (name, first) in [("two", 2u8), ("three", 3), ("six", 6), ("seven", 7)] {
            let mut bytes = vec![0u8; 12];
            bytes[0] = first;
            key.set_raw_value(
                name,
                &RegValue {
                    bytes: bytes.into(),
                    vtype: RegType::REG_BINARY,
                },
            )
            .unwrap();
        }
        assert!(is_enabled(&scratch.0, "two"));
        assert!(!is_enabled(&scratch.0, "three"));
        assert!(is_enabled(&scratch.0, "six"));
        assert!(!is_enabled(&scratch.0, "seven"));
    }

    #[test]
    fn forgetting_backs_up_the_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::new("approved-forget");
        set_enabled(&scratch.0, "Gone", false).unwrap();
        let mut backup = RegBackup::at(tmp.path().join("flags.reg"));
        forget(&scratch.0, "Gone", Some(&mut backup));
        assert_eq!(backup.len(), 1);
        assert!(is_enabled(&scratch.0, "Gone"));
        assert!(backup.body().contains("\"Gone\"=hex:03"));
        // Forgetting something that was never flagged is a no-op.
        forget(&scratch.0, "Never", Some(&mut backup));
        assert_eq!(backup.len(), 1);
    }

    #[test]
    fn key_layout() {
        let key = approved_key(RegRoot::Hklm, "Run32");
        assert_eq!(
            key.to_string(),
            r"HKLM\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32"
        );
        assert_eq!(user_override(&key).unwrap().root, RegRoot::Hkcu);
        assert!(user_override(&approved_key(RegRoot::Hkcu, "Run")).is_none());
    }
}
