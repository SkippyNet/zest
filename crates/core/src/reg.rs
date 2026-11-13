//! Registry paths, guarded deletion and `.reg` backups.
//!
//! Zest is a 64-bit program and always uses the native registry view.
//! 32-bit entries are addressed explicitly through `WOW6432Node`, which keeps
//! backups unambiguous: a `.reg` file restores exactly what was removed.

use crate::{Error, IoContext, Result, paths, process};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use winreg::enums::{
    HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_WRITE,
    RegType,
};
use winreg::{RegKey, RegValue};

/// A registry hive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RegRoot {
    /// `HKEY_LOCAL_MACHINE`
    Hklm,
    /// `HKEY_CURRENT_USER`
    Hkcu,
    /// `HKEY_CLASSES_ROOT`
    Hkcr,
    /// `HKEY_USERS`
    Hku,
}

impl RegRoot {
    /// Abbreviated name, e.g. `HKLM`.
    pub fn short(self) -> &'static str {
        match self {
            RegRoot::Hklm => "HKLM",
            RegRoot::Hkcu => "HKCU",
            RegRoot::Hkcr => "HKCR",
            RegRoot::Hku => "HKU",
        }
    }

    /// Full name, e.g. `HKEY_LOCAL_MACHINE`.
    pub fn long(self) -> &'static str {
        match self {
            RegRoot::Hklm => "HKEY_LOCAL_MACHINE",
            RegRoot::Hkcu => "HKEY_CURRENT_USER",
            RegRoot::Hkcr => "HKEY_CLASSES_ROOT",
            RegRoot::Hku => "HKEY_USERS",
        }
    }

    /// The predefined key for this hive.
    pub fn key(self) -> RegKey {
        RegKey::predef(match self {
            RegRoot::Hklm => HKEY_LOCAL_MACHINE,
            RegRoot::Hkcu => HKEY_CURRENT_USER,
            RegRoot::Hkcr => HKEY_CLASSES_ROOT,
            RegRoot::Hku => HKEY_USERS,
        })
    }

    /// Parses a short or long hive name (case-insensitive).
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "HKLM" | "HKEY_LOCAL_MACHINE" => Some(RegRoot::Hklm),
            "HKCU" | "HKEY_CURRENT_USER" => Some(RegRoot::Hkcu),
            "HKCR" | "HKEY_CLASSES_ROOT" => Some(RegRoot::Hkcr),
            "HKU" | "HKEY_USERS" => Some(RegRoot::Hku),
            _ => None,
        }
    }
}

/// A key location: hive plus sub-path (no leading or trailing backslash).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegPath {
    /// Hive.
    pub root: RegRoot,
    /// Path below the hive, e.g. `Software\Microsoft\Windows`.
    pub path: String,
}

impl RegPath {
    /// Creates a path, trimming stray backslashes.
    pub fn new(root: RegRoot, path: impl Into<String>) -> Self {
        let path: String = path.into();
        RegPath {
            root,
            path: path.trim_matches('\\').to_owned(),
        }
    }

    /// `HKLM\<path>`
    pub fn hklm(path: impl Into<String>) -> Self {
        Self::new(RegRoot::Hklm, path)
    }

    /// `HKCU\<path>`
    pub fn hkcu(path: impl Into<String>) -> Self {
        Self::new(RegRoot::Hkcu, path)
    }

    /// `HKCR\<path>`
    pub fn hkcr(path: impl Into<String>) -> Self {
        Self::new(RegRoot::Hkcr, path)
    }

    /// Parses `HKLM\Software\...` or `HKEY_LOCAL_MACHINE\Software\...`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (root, rest) = match text.split_once('\\') {
            Some((root, rest)) => (root, rest),
            None => (text, ""),
        };
        Some(Self::new(RegRoot::parse(root)?, rest))
    }

    /// The path of a direct sub-key.
    pub fn child(&self, name: &str) -> RegPath {
        if self.path.is_empty() {
            RegPath::new(self.root, name)
        } else {
            RegPath::new(
                self.root,
                format!("{}\\{}", self.path, name.trim_matches('\\')),
            )
        }
    }

    /// The parent key, or `None` for a hive root.
    pub fn parent(&self) -> Option<RegPath> {
        if self.path.is_empty() {
            return None;
        }
        Some(match self.path.rfind('\\') {
            Some(i) => RegPath::new(self.root, &self.path[..i]),
            None => RegPath::new(self.root, ""),
        })
    }

    /// Name of the last path component.
    pub fn leaf(&self) -> &str {
        self.path.rsplit('\\').next().unwrap_or("")
    }

    /// Number of path components below the hive.
    pub fn depth(&self) -> usize {
        self.path.split('\\').filter(|c| !c.is_empty()).count()
    }

    /// Opens the key read-only.
    pub fn open(&self) -> std::io::Result<RegKey> {
        self.root.key().open_subkey_with_flags(&self.path, KEY_READ)
    }

    /// Opens the key for reading and writing.
    pub fn open_write(&self) -> std::io::Result<RegKey> {
        self.root
            .key()
            .open_subkey_with_flags(&self.path, KEY_READ | KEY_WRITE)
    }

    /// True when the key exists and is readable.
    pub fn exists(&self) -> bool {
        self.open().is_ok()
    }

    /// Long form, e.g. `HKEY_LOCAL_MACHINE\SOFTWARE\Example`.
    pub fn full_name(&self) -> String {
        if self.path.is_empty() {
            self.root.long().to_owned()
        } else {
            format!("{}\\{}", self.root.long(), self.path)
        }
    }

    /// Reads a string value (`REG_SZ` / `REG_EXPAND_SZ`), unexpanded.
    pub fn read_string(&self, name: &str) -> Option<String> {
        self.open().ok()?.get_value::<String, _>(name).ok()
    }

    /// Reads a `REG_DWORD` value.
    pub fn read_u32(&self, name: &str) -> Option<u32> {
        self.open().ok()?.get_value::<u32, _>(name).ok()
    }
}

impl std::fmt::Display for RegPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(self.root.short())
        } else {
            write!(f, "{}\\{}", self.root.short(), self.path)
        }
    }
}

/// Keys that structure the registry itself. They may be cleaned *inside* but
/// never deleted, whatever a scanner believes.
const PROTECTED_KEYS: &[&str] = &[
    "software\\microsoft",
    "software\\microsoft\\windows",
    "software\\microsoft\\windows\\currentversion",
    "software\\microsoft\\windows\\currentversion\\run",
    "software\\microsoft\\windows\\currentversion\\runonce",
    "software\\microsoft\\windows\\currentversion\\uninstall",
    "software\\microsoft\\windows\\currentversion\\explorer",
    "software\\microsoft\\windows\\currentversion\\app paths",
    "software\\microsoft\\windows\\currentversion\\shareddlls",
    "software\\microsoft\\windows\\currentversion\\installer",
    "software\\microsoft\\windows\\currentversion\\policies",
    "software\\microsoft\\windows nt",
    "software\\microsoft\\windows nt\\currentversion",
    "software\\microsoft\\windows nt\\currentversion\\fonts",
    "software\\microsoft\\windows nt\\currentversion\\winlogon",
    "software\\classes",
    "software\\classes\\clsid",
    "software\\classes\\typelib",
    "software\\classes\\interface",
    "software\\classes\\appid",
    "software\\policies",
    "software\\wow6432node",
    "software\\wow6432node\\microsoft",
    "software\\wow6432node\\microsoft\\windows",
    "software\\wow6432node\\microsoft\\windows\\currentversion",
    "software\\wow6432node\\microsoft\\windows\\currentversion\\run",
    "software\\wow6432node\\microsoft\\windows\\currentversion\\uninstall",
    "software\\wow6432node\\classes",
    "software\\wow6432node\\classes\\clsid",
    "system\\currentcontrolset",
    "system\\currentcontrolset\\control",
    "system\\currentcontrolset\\services",
    "system\\currentcontrolset\\enum",
    "wow6432node\\clsid",
    "wow6432node\\typelib",
    "wow6432node\\interface",
];

/// Returns true when the key itself must never be deleted.
///
/// Hive roots and every first-level key are always protected. For
/// `HKEY_USERS` the per-user prefix (`<SID>\`) is ignored before matching.
pub fn is_protected_key(path: &RegPath) -> bool {
    let mut rel = path.path.to_lowercase();
    if path.root == RegRoot::Hku {
        match rel.split_once('\\') {
            Some((_sid, rest)) => rel = rest.to_owned(),
            None => return true,
        }
    }
    if rel.is_empty() || !rel.contains('\\') {
        return true;
    }
    PROTECTED_KEYS.contains(&rel.as_str())
}

fn map_access(e: std::io::Error, what: String) -> Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        Error::NeedsElevation(what)
    } else {
        Error::Io {
            context: what,
            source: e,
        }
    }
}

/// Deletes a key and everything below it, exporting it to `backup` first.
///
/// Deleting a key that does not exist is not an error.
pub fn delete_key(path: &RegPath, backup: Option<&mut RegBackup>) -> Result<()> {
    if is_protected_key(path) {
        return Err(Error::Other(format!(
            "refused to delete protected registry key {path}"
        )));
    }
    if !path.exists() {
        return Ok(());
    }
    if let Some(backup) = backup {
        backup.add_key(path)?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::Other(format!("{path} has no parent key")))?;
    let parent_key = parent
        .open_write()
        .map_err(|e| map_access(e, format!("delete registry key {path}")))?;
    parent_key
        .delete_subkey_all(path.leaf())
        .map_err(|e| map_access(e, format!("delete registry key {path}")))
}

/// Deletes one value, exporting it to `backup` first.
///
/// Deleting a value that does not exist is not an error.
pub fn delete_value(path: &RegPath, name: &str, backup: Option<&mut RegBackup>) -> Result<()> {
    let key = match path.open_write() {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(map_access(e, format!("modify registry key {path}"))),
    };
    if key.get_raw_value(name).is_err() {
        return Ok(());
    }
    if let Some(backup) = backup {
        backup.add_value(path, name)?;
    }
    key.delete_value(name)
        .map_err(|e| map_access(e, format!("delete registry value {path}\\{name}")))
}

fn escape_reg_string(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
}

fn decode_sz(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    while units.last() == Some(&0) {
        units.pop();
    }
    if units.contains(&0) {
        return None;
    }
    String::from_utf16(&units).ok()
}

fn write_hex(bytes: &[u8], out: &mut String) {
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{b:02x}");
    }
}

fn write_value(name: &str, value: &RegValue, out: &mut String) {
    if name.is_empty() {
        out.push('@');
    } else {
        out.push('"');
        escape_reg_string(name, out);
        out.push('"');
    }
    out.push('=');
    let bytes: &[u8] = &value.bytes[..];
    let type_code = value.vtype.clone() as u32;
    match value.vtype {
        RegType::REG_SZ => match decode_sz(bytes) {
            // Line breaks inside a quoted string do not survive an import
            // reliably, so such values are written in hex form instead.
            Some(text) if !text.contains(['\r', '\n']) => {
                out.push('"');
                escape_reg_string(&text, out);
                out.push('"');
            }
            _ => {
                out.push_str("hex(1):");
                write_hex(bytes, out);
            }
        },
        RegType::REG_DWORD if bytes.len() == 4 => {
            let v = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let _ = write!(out, "dword:{v:08x}");
        }
        RegType::REG_BINARY => {
            out.push_str("hex:");
            write_hex(bytes, out);
        }
        _ => {
            let _ = write!(out, "hex({type_code:x}):");
            write_hex(bytes, out);
        }
    }
    out.push_str("\r\n");
}

const MAX_EXPORT_DEPTH: usize = 128;

fn export_key_inner(path: &RegPath, key: &RegKey, depth: usize, out: &mut String) {
    let _ = write!(out, "[{}]\r\n", path.full_name());
    for (name, value) in key.enum_values().flatten() {
        write_value(&name, &value, out);
    }
    out.push_str("\r\n");
    if depth >= MAX_EXPORT_DEPTH {
        return;
    }
    for sub in key.enum_keys().flatten() {
        let child = path.child(&sub);
        if let Ok(child_key) = key.open_subkey_with_flags(&sub, KEY_READ) {
            export_key_inner(&child, &child_key, depth + 1, out);
        }
    }
}

/// Appends a `.reg` representation of `path` (recursively) to `out`.
pub fn export_key(path: &RegPath, out: &mut String) -> Result<()> {
    let key = path.open().ctx(format!("reading registry key {path}"))?;
    export_key_inner(path, &key, 0, out);
    Ok(())
}

/// Appends a `.reg` representation of a single value to `out`.
pub fn export_value(path: &RegPath, name: &str, out: &mut String) -> Result<()> {
    let key = path.open().ctx(format!("reading registry key {path}"))?;
    let value = key
        .get_raw_value(name)
        .ctx(format!("reading registry value {path}\\{name}"))?;
    let _ = write!(out, "[{}]\r\n", path.full_name());
    write_value(name, &value, out);
    out.push_str("\r\n");
    Ok(())
}

/// Folder that holds registry backups.
pub fn registry_backups_dir() -> PathBuf {
    paths::backups_dir().join("registry")
}

/// Collects exported keys and values and writes them as one `.reg` file that
/// can be restored with [`import_reg_file`] (or by double-clicking it).
#[derive(Debug)]
pub struct RegBackup {
    file: PathBuf,
    body: String,
    entries: usize,
}

impl RegBackup {
    /// A backup that will be saved under [`registry_backups_dir`] with a
    /// timestamped name containing `label`.
    pub fn new(label: &str) -> Self {
        let safe: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let mut file = registry_backups_dir().join(format!("{stamp}-{safe}.reg"));
        let mut n = 1;
        while file.exists() {
            n += 1;
            file = registry_backups_dir().join(format!("{stamp}-{safe}-{n}.reg"));
        }
        Self::at(file)
    }

    /// A backup saved to an explicit file.
    pub fn at(file: PathBuf) -> Self {
        RegBackup {
            file,
            body: String::new(),
            entries: 0,
        }
    }

    /// Adds a whole key (recursively).
    pub fn add_key(&mut self, key: &RegPath) -> Result<()> {
        export_key(key, &mut self.body)?;
        self.entries += 1;
        Ok(())
    }

    /// Adds a single value.
    pub fn add_value(&mut self, key: &RegPath, name: &str) -> Result<()> {
        export_value(key, name, &mut self.body)?;
        self.entries += 1;
        Ok(())
    }

    /// Number of keys/values captured.
    pub fn len(&self) -> usize {
        self.entries
    }

    /// True when nothing has been captured.
    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    /// Where the backup is (or will be) stored.
    pub fn path(&self) -> &Path {
        &self.file
    }

    /// The `.reg` text captured so far (without the file header).
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Writes the backup to disk. Returns `None` when there was nothing to save.
    pub fn save(&self) -> Result<Option<PathBuf>> {
        if self.is_empty() {
            return Ok(None);
        }
        if let Some(dir) = self.file.parent() {
            paths::ensure_dir(dir)?;
        }
        let text = format!("Windows Registry Editor Version 5.00\r\n\r\n{}", self.body);
        // Version 5.00 files are UTF-16 LE with a byte order mark.
        let mut bytes = Vec::with_capacity(2 + text.len() * 2);
        bytes.extend_from_slice(&[0xFF, 0xFE]);
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&self.file, bytes)
            .ctx(format!("writing registry backup {}", self.file.display()))?;
        Ok(Some(self.file.clone()))
    }
}

/// Restores a `.reg` file through the system's `reg.exe`.
///
/// Restoring machine-wide keys requires administrator rights.
pub fn import_reg_file(file: &Path) -> Result<()> {
    if !file.is_file() {
        return Err(Error::NotFound(file.display().to_string()));
    }
    process::run_checked(
        process::system_tool("reg.exe"),
        [std::ffi::OsStr::new("import"), file.as_os_str()],
    )
    .map(|_| ())
}

/// A saved registry backup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupInfo {
    /// Location of the `.reg` file.
    pub path: PathBuf,
    /// File name without extension.
    pub label: String,
    /// Creation time.
    pub created: SystemTime,
    /// Size in bytes.
    pub size: u64,
}

/// Lists saved registry backups, newest first.
pub fn list_backups() -> Vec<BackupInfo> {
    let mut backups: Vec<BackupInfo> = std::fs::read_dir(registry_backups_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("reg"))
            {
                return None;
            }
            let meta = entry.metadata().ok()?;
            Some(BackupInfo {
                label: path.file_stem()?.to_string_lossy().into_owned(),
                created: meta.modified().or_else(|_| meta.created()).ok()?,
                size: meta.len(),
                path,
            })
        })
        .collect();
    backups.sort_by(|a, b| b.created.cmp(&a.created));
    backups
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch key under HKCU that is removed again when dropped.
    struct Scratch(RegPath);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = RegPath::hkcu(format!("Software\\ZestTests\\{tag}-{}", std::process::id()));
            RegRoot::Hkcu.key().create_subkey(&path.path).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = RegRoot::Hkcu.key().delete_subkey_all(&self.0.path);
            // Remove the shared parent when no other test is using it.
            let _ = RegRoot::Hkcu.key().delete_subkey("Software\\ZestTests");
        }
    }

    #[test]
    fn path_manipulation() {
        let p = RegPath::parse(r"HKEY_LOCAL_MACHINE\SOFTWARE\Example\").unwrap();
        assert_eq!(p, RegPath::hklm(r"SOFTWARE\Example"));
        assert_eq!(p.to_string(), r"HKLM\SOFTWARE\Example");
        assert_eq!(p.full_name(), r"HKEY_LOCAL_MACHINE\SOFTWARE\Example");
        assert_eq!(p.leaf(), "Example");
        assert_eq!(p.depth(), 2);
        assert_eq!(p.child("Sub").path, r"SOFTWARE\Example\Sub");
        assert_eq!(p.parent().unwrap().path, "SOFTWARE");
        assert_eq!(p.parent().unwrap().parent().unwrap().path, "");
        assert!(p.parent().unwrap().parent().unwrap().parent().is_none());
        assert!(RegPath::parse(r"HKXX\Nope").is_none());
        assert_eq!(RegPath::parse("hkcu").unwrap(), RegPath::hkcu(""));
    }

    #[test]
    fn structural_keys_are_protected() {
        assert!(is_protected_key(&RegPath::hklm("")));
        assert!(is_protected_key(&RegPath::hklm("SOFTWARE")));
        assert!(is_protected_key(&RegPath::hkcu("Software")));
        assert!(is_protected_key(&RegPath::hklm(r"SOFTWARE\Microsoft")));
        assert!(is_protected_key(&RegPath::hklm(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
        )));
        assert!(is_protected_key(&RegPath::hkcr("CLSID")));
        assert!(is_protected_key(&RegPath::new(RegRoot::Hku, "S-1-5-21-1")));
        assert!(is_protected_key(&RegPath::new(
            RegRoot::Hku,
            r"S-1-5-21-1\Software"
        )));
        assert!(!is_protected_key(&RegPath::hkcu(r"Software\SomeVendor")));
        assert!(!is_protected_key(&RegPath::hkcr(
            r"CLSID\{00000000-0000-0000-0000-000000000000}"
        )));
        assert!(delete_key(&RegPath::hklm("SOFTWARE"), None).is_err());
    }

    #[test]
    fn export_formats_every_value_type() {
        let scratch = Scratch::new("export");
        let key = scratch.0.open_write().unwrap();
        key.set_value("", &"default value").unwrap();
        key.set_value("Text", &r#"C:\Path\"quoted""#).unwrap();
        key.set_value("Number", &0x2Au32).unwrap();
        key.set_value("Big", &0x0102030405060708u64).unwrap();
        key.set_raw_value(
            "Blob",
            &RegValue {
                bytes: vec![0xDE, 0xAD, 0xBE, 0xEF].into(),
                vtype: RegType::REG_BINARY,
            },
        )
        .unwrap();
        key.create_subkey("Child")
            .unwrap()
            .0
            .set_value("Inner", &"x")
            .unwrap();

        let mut out = String::new();
        export_key(&scratch.0, &mut out).unwrap();
        assert!(
            out.contains(&format!("[{}]\r\n", scratch.0.full_name())),
            "{out}"
        );
        assert!(out.contains("@=\"default value\"\r\n"), "{out}");
        assert!(out.contains(r#""Text"="C:\\Path\\\"quoted\"""#), "{out}");
        assert!(out.contains("\"Number\"=dword:0000002a\r\n"), "{out}");
        assert!(
            out.contains("\"Big\"=hex(b):08,07,06,05,04,03,02,01\r\n"),
            "{out}"
        );
        assert!(out.contains("\"Blob\"=hex:de,ad,be,ef\r\n"), "{out}");
        assert!(
            out.contains(&format!(
                "[{}\\Child]\r\n\"Inner\"=\"x\"",
                scratch.0.full_name()
            )),
            "{out}"
        );
    }

    #[test]
    fn backup_delete_and_restore_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = Scratch::new("roundtrip");
        let victim = scratch.0.child("Victim");
        let (key, _) = RegRoot::Hkcu.key().create_subkey(&victim.path).unwrap();
        key.set_value("Keep", &"important").unwrap();
        key.set_value("Count", &7u32).unwrap();
        key.create_subkey("Deep")
            .unwrap()
            .0
            .set_value("Leaf", &"value")
            .unwrap();
        drop(key);

        let mut backup = RegBackup::at(tmp.path().join("backup.reg"));
        delete_value(&victim, "Count", Some(&mut backup)).unwrap();
        delete_key(&victim, Some(&mut backup)).unwrap();
        assert_eq!(backup.len(), 2);
        assert!(!victim.exists());
        // Deleting what is already gone is fine and records nothing.
        delete_key(&victim, Some(&mut backup)).unwrap();
        delete_value(&victim, "Count", Some(&mut backup)).unwrap();
        assert_eq!(backup.len(), 2);

        let saved = backup.save().unwrap().expect("backup should be written");
        let raw = std::fs::read(&saved).unwrap();
        assert_eq!(&raw[..2], &[0xFF, 0xFE], "missing UTF-16 byte order mark");

        import_reg_file(&saved).unwrap();
        assert_eq!(victim.read_string("Keep").as_deref(), Some("important"));
        assert_eq!(victim.read_u32("Count"), Some(7));
        assert_eq!(
            victim.child("Deep").read_string("Leaf").as_deref(),
            Some("value")
        );
    }

    #[test]
    fn empty_backup_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let backup = RegBackup::at(tmp.path().join("none.reg"));
        assert!(backup.is_empty());
        assert_eq!(backup.save().unwrap(), None);
        assert!(!tmp.path().join("none.reg").exists());
    }
}
