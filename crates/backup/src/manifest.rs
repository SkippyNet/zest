//! What a snapshot records: the list of backed-up files and folders and the
//! stored objects that hold their contents.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zest_core::{Error, Result};

/// Version of the manifest format written by this build.
pub const MANIFEST_FORMAT: u32 = 1;

/// Identifier of a stored object: the BLAKE3 hash of its uncompressed bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    /// Hashes `data`.
    pub fn of(data: &[u8]) -> ObjectId {
        ObjectId(*blake3::hash(data).as_bytes())
    }

    /// Lower-case hexadecimal form, 64 characters.
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(DIGITS[(byte >> 4) as usize] as char);
            out.push(DIGITS[(byte & 0xF) as usize] as char);
        }
        out
    }

    /// Parses the form written by [`ObjectId::to_hex`].
    pub fn from_hex(text: &str) -> Option<ObjectId> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
        }
        Some(ObjectId(out))
    }
}

impl std::fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjectId({})", &self.to_hex()[..12])
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        ObjectId::from_hex(&text).ok_or_else(|| serde::de::Error::custom("not an object id"))
    }
}

/// Whether a backed-up source was a folder or a single file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootKind {
    Folder,
    File,
}

/// One of the places a snapshot was taken from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRoot {
    /// Full path the source had when it was backed up.
    pub path: String,
    /// Short unique name of the source inside the snapshot, e.g. `Documents`.
    pub name: String,
    /// Folder or single file.
    pub kind: RootKind,
}

/// Whether an entry is a folder or a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntryKind {
    #[serde(rename = "d")]
    Dir,
    #[serde(rename = "f")]
    File,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// One folder or file of a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Index into [`Manifest::roots`].
    #[serde(rename = "r")]
    pub root: u32,
    /// Path below the root, parts joined with `\`. Empty for a file root.
    #[serde(rename = "p")]
    pub path: String,
    #[serde(rename = "k")]
    pub kind: EntryKind,
    /// Size in bytes (files only).
    #[serde(rename = "s", default, skip_serializing_if = "is_zero")]
    pub size: u64,
    /// Last write time as a Windows file time (100 ns ticks since 1601).
    #[serde(rename = "m", default, skip_serializing_if = "is_zero")]
    pub modified: u64,
    /// Creation time as a Windows file time.
    #[serde(rename = "c", default, skip_serializing_if = "is_zero")]
    pub created: u64,
    /// Windows file attributes worth keeping (read-only, hidden, system).
    #[serde(rename = "a", default, skip_serializing_if = "is_zero_u32")]
    pub attributes: u32,
    /// The stored objects that make up the file, in order.
    #[serde(rename = "h", default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<ObjectId>,
}

/// Why something was left out of a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    /// A junction or symbolic link. Links are never followed.
    Link,
    /// The contents live in the cloud; reading would download them.
    CloudOnly,
    /// Windows refused to open or read it (in use, or no permission).
    Unreadable,
    /// The name cannot be stored (not valid Unicode).
    BadName,
}

impl SkipReason {
    /// Short explanation for lists.
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::Link => "Link, not followed",
            SkipReason::CloudOnly => "Online-only file",
            SkipReason::Unreadable => "Could not be read",
            SkipReason::BadName => "Name cannot be stored",
        }
    }
}

/// Something that was left out of a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    /// Full path of the item.
    pub path: String,
    pub reason: SkipReason,
    /// The system's own words, when there are any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Short description of a snapshot, enough for a list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotInfo {
    /// Identifier, also the file name: `YYYYMMDD-HHMMSS` in UTC.
    pub id: String,
    /// When the snapshot was taken, seconds since the Unix epoch.
    pub created: i64,
    /// Optional note given by the user.
    pub label: String,
    /// The backed-up locations.
    pub sources: Vec<String>,
    /// Number of files.
    pub files: u64,
    /// Number of folders.
    pub dirs: u64,
    /// Total size of the files.
    pub bytes: u64,
    /// Size of the file contents that were not in the repository before.
    pub new_bytes: u64,
    /// Bytes this snapshot added to the repository after compression.
    pub stored_bytes: u64,
    /// Files taken over unchanged from the previous snapshot.
    pub reused_files: u64,
    /// Number of items that were left out.
    pub skipped: u64,
}

/// Everything recorded about one snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version, see [`MANIFEST_FORMAT`].
    pub format: u32,
    /// The summary shown in lists.
    pub info: SnapshotInfo,
    /// Size of all chunks but the last of each file.
    pub chunk_size: u64,
    /// Where the snapshot was taken from.
    pub roots: Vec<SourceRoot>,
    /// Folders and files, sorted by root and path.
    pub entries: Vec<Entry>,
    /// What was left out and why.
    #[serde(default)]
    pub skipped: Vec<Skipped>,
}

/// True for a path part that is safe to create below a restore target.
fn is_safe_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part
            .chars()
            .any(|c| matches!(c, '\\' | '/' | ':' | '\0'..='\u{1f}'))
}

/// True for a relative path that cannot escape the folder it is joined to.
pub(crate) fn is_safe_relative(path: &str) -> bool {
    path.split('\\').all(is_safe_part)
}

impl Manifest {
    /// Rejects manifests this build cannot read or that could make a restore
    /// write outside its target (a damaged or tampered file).
    pub fn validate(&self) -> Result<()> {
        if self.format == 0 || self.format > MANIFEST_FORMAT {
            return Err(Error::Invalid(format!(
                "snapshot {} was written by a newer version (format {})",
                self.info.id, self.format
            )));
        }
        for root in &self.roots {
            if !is_safe_part(&root.name) {
                return Err(Error::Invalid(format!(
                    "snapshot {} has an unusable source name",
                    self.info.id
                )));
            }
        }
        for entry in &self.entries {
            let root = self.roots.get(entry.root as usize).ok_or_else(|| {
                Error::Invalid(format!(
                    "snapshot {} refers to a source it does not list",
                    self.info.id
                ))
            })?;
            let file_root = root.kind == RootKind::File;
            let ok = if entry.path.is_empty() {
                file_root && entry.kind == EntryKind::File
            } else {
                !file_root && is_safe_relative(&entry.path)
            };
            if !ok {
                return Err(Error::Invalid(format!(
                    "snapshot {} contains an unusable path: {:?}",
                    self.info.id, entry.path
                )));
            }
        }
        Ok(())
    }

    /// Uncompressed length of chunk `index` of `entry`.
    pub fn chunk_len(&self, entry: &Entry, index: usize) -> u64 {
        let before = self.chunk_size.saturating_mul(index as u64);
        entry.size.saturating_sub(before).min(self.chunk_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(roots: Vec<SourceRoot>, entries: Vec<Entry>) -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            info: SnapshotInfo {
                id: "20260101-000000".into(),
                ..SnapshotInfo::default()
            },
            chunk_size: 1024,
            roots,
            entries,
            skipped: Vec::new(),
        }
    }

    fn folder_root() -> SourceRoot {
        SourceRoot {
            path: r"C:\Data".into(),
            name: "Data".into(),
            kind: RootKind::Folder,
        }
    }

    fn file(path: &str) -> Entry {
        Entry {
            root: 0,
            path: path.into(),
            kind: EntryKind::File,
            size: 0,
            modified: 0,
            created: 0,
            attributes: 0,
            chunks: Vec::new(),
        }
    }

    #[test]
    fn object_ids_round_trip_through_hex() {
        let id = ObjectId::of(b"zest");
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(ObjectId::from_hex(&hex), Some(id));
        assert_eq!(ObjectId::from_hex(&hex.to_uppercase()), Some(id));
        assert_eq!(ObjectId::from_hex("abc"), None);
        assert_eq!(ObjectId::from_hex(&"g".repeat(64)), None);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<ObjectId>(&json).unwrap(), id);
    }

    #[test]
    fn validation_accepts_ordinary_paths() {
        let m = manifest(
            vec![folder_root()],
            vec![
                file("a.txt"),
                file(r"sub\b.txt"),
                file(r"sub\dots..in.name"),
            ],
        );
        assert!(m.validate().is_ok());
    }

    #[test]
    fn validation_rejects_paths_that_escape() {
        for bad in [
            r"..\outside.txt",
            r"sub\..\..\outside.txt",
            r"\rooted.txt",
            r"C:\absolute.txt",
            "with/slash.txt",
            r"sub\\double.txt",
            "",
        ] {
            let m = manifest(vec![folder_root()], vec![file(bad)]);
            assert!(m.validate().is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn validation_rejects_unknown_roots_and_newer_formats() {
        let mut entry = file("a.txt");
        entry.root = 3;
        assert!(
            manifest(vec![folder_root()], vec![entry])
                .validate()
                .is_err()
        );

        let mut newer = manifest(vec![folder_root()], vec![file("a.txt")]);
        newer.format = MANIFEST_FORMAT + 1;
        assert!(newer.validate().is_err());

        let mut bad_root = folder_root();
        bad_root.name = r"..\up".into();
        assert!(manifest(vec![bad_root], Vec::new()).validate().is_err());
    }

    #[test]
    fn a_file_root_holds_exactly_one_nameless_file() {
        let root = SourceRoot {
            path: r"C:\Data\notes.txt".into(),
            name: "notes.txt".into(),
            kind: RootKind::File,
        };
        assert!(
            manifest(vec![root.clone()], vec![file("")])
                .validate()
                .is_ok()
        );
        assert!(
            manifest(vec![root], vec![file("x.txt")])
                .validate()
                .is_err()
        );
    }

    #[test]
    fn chunk_lengths_follow_the_file_size() {
        let m = manifest(vec![folder_root()], Vec::new());
        let mut entry = file("big.bin");
        entry.size = 2500;
        assert_eq!(m.chunk_len(&entry, 0), 1024);
        assert_eq!(m.chunk_len(&entry, 1), 1024);
        assert_eq!(m.chunk_len(&entry, 2), 452);
        assert_eq!(m.chunk_len(&entry, 3), 0);
    }
}
