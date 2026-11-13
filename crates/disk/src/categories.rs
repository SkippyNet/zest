//! File type categories used by the disk analyzer.

use crate::pool::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Broad kind of a file, derived from its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FileCategory {
    /// Photos and other images.
    Pictures,
    /// Audio files.
    Music,
    /// Video files.
    Video,
    /// Office documents, PDFs, e-books and plain text.
    Documents,
    /// Compressed archives and disk images.
    Archives,
    /// Mail stores and saved messages.
    Email,
    /// Programs, installers and scripts that start programs.
    Applications,
    /// Source code and developer data files.
    Code,
    /// Operating system and driver files.
    System,
    /// Everything else.
    Other,
}

impl FileCategory {
    /// Every category, in display order.
    pub const ALL: [FileCategory; 10] = [
        FileCategory::Pictures,
        FileCategory::Music,
        FileCategory::Video,
        FileCategory::Documents,
        FileCategory::Archives,
        FileCategory::Email,
        FileCategory::Applications,
        FileCategory::Code,
        FileCategory::System,
        FileCategory::Other,
    ];

    /// Number of categories.
    pub const COUNT: usize = Self::ALL.len();

    /// Position of this category in [`FileCategory::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            FileCategory::Pictures => "Pictures",
            FileCategory::Music => "Music",
            FileCategory::Video => "Video",
            FileCategory::Documents => "Documents",
            FileCategory::Archives => "Archives",
            FileCategory::Email => "Email",
            FileCategory::Applications => "Applications",
            FileCategory::Code => "Code",
            FileCategory::System => "System",
            FileCategory::Other => "Other",
        }
    }

    /// One-line explanation of what the category contains.
    pub fn description(self) -> &'static str {
        match self {
            FileCategory::Pictures => "Photos, screenshots, drawings and icons",
            FileCategory::Music => "Songs, recordings and other audio",
            FileCategory::Video => "Movies, clips and screen recordings",
            FileCategory::Documents => "Office documents, PDFs, e-books and text files",
            FileCategory::Archives => "Compressed archives and disk images",
            FileCategory::Email => "Mailboxes and saved messages",
            FileCategory::Applications => "Programs, installers and scripts",
            FileCategory::Code => "Source code and developer data",
            FileCategory::System => "Windows components, drivers and libraries",
            FileCategory::Other => "Files of any other type",
        }
    }

    /// The category of a lower-case extension without the dot.
    pub fn of_extension(extension: &str) -> FileCategory {
        table()
            .get(extension)
            .copied()
            .unwrap_or(FileCategory::Other)
    }

    /// The category of a file name or path (by its extension).
    pub fn of_path(path: &std::path::Path) -> FileCategory {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => Self::of_extension(&ext.to_ascii_lowercase()),
            None => FileCategory::Other,
        }
    }
}

const PICTURES: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "webp", "heic",
    "heif", "avif", "jxl", "svg", "svgz", "ico", "cur", "psd", "psb", "ai", "eps", "xcf", "kra",
    "raw", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "dng", "pef",
    "srw", "x3f", "tga", "pcx", "ppm", "pgm", "pbm", "exr", "hdr", "dds", "jp2", "j2k", "wmf",
    "emf", "apng", "indd", "sketch", "fig", "afphoto", "afdesign", "cdr",
];

const MUSIC: &[&str] = &[
    "mp3", "wav", "wave", "flac", "aac", "m4a", "m4b", "ogg", "oga", "opus", "wma", "aif", "aiff",
    "aifc", "alac", "ape", "wv", "mid", "midi", "mka", "ac3", "dts", "amr", "ra", "mpc", "dsf",
    "dff", "au", "snd", "cda", "m3u", "m3u8", "pls", "weba", "xm", "mod", "s3m", "it",
];

const VIDEO: &[&str] = &[
    "mp4", "m4v", "mkv", "avi", "mov", "qt", "wmv", "asf", "flv", "f4v", "webm", "mpg", "mpeg",
    "mpe", "m2v", "m2ts", "mts", "vob", "3gp", "3g2", "ogv", "ogm", "divx", "xvid", "rm", "rmvb",
    "mxf", "dv", "swf", "wtv", "dvr-ms", "h264", "h265", "hevc", "264", "265", "y4m", "ifo", "bik",
    "srt", "sub", "ass", "ssa", "vtt",
];

const DOCUMENTS: &[&str] = &[
    "doc", "docx", "docm", "dot", "dotx", "dotm", "odt", "ott", "rtf", "txt", "text", "md",
    "markdown", "pdf", "xps", "oxps", "xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "ods",
    "csv", "tsv", "ppt", "pptx", "pptm", "pps", "ppsx", "pot", "potx", "odp", "key", "numbers",
    "pages", "epub", "mobi", "azw", "azw3", "fb2", "djvu", "djv", "cbz", "cbr", "tex", "wpd",
    "wps", "one", "onetoc2", "pub", "vsd", "vsdx", "mpp", "accdb", "mdb", "odg", "odf", "abw",
    "log", "nfo", "chm", "hlp", "ps",
];

const ARCHIVES: &[&str] = &[
    "zip", "zipx", "rar", "7z", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zst",
    "zstd", "lz", "lz4", "lzma", "lzh", "lha", "arj", "ace", "cab", "z", "cpio", "rpm", "deb",
    "pkg", "dmg", "iso", "img", "bin", "cue", "nrg", "mdf", "mds", "vhd", "vhdx", "vmdk", "vdi",
    "qcow", "qcow2", "ova", "ovf", "wim", "esd", "swm", "sit", "sitx", "br", "war", "ear", "nupkg",
    "whl", "crate", "gem", "apk", "aab", "ipa", "xapk", "001", "r00", "bak", "bkf", "tib", "gho",
];

const EMAIL: &[&str] = &[
    "pst", "ost", "eml", "emlx", "msg", "mbox", "mbx", "dbx", "nsf", "olm", "oft", "wab", "pab",
    "vcf", "ics", "tnef", "mbs", "msf",
];

const APPLICATIONS: &[&str] = &[
    "exe",
    "msi",
    "msp",
    "msu",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appinstaller",
    "com",
    "scr",
    "bat",
    "cmd",
    "lnk",
    "url",
    "pif",
    "gadget",
    "jar",
    "application",
    "appref-ms",
    "cpl",
    "msc",
    "hta",
    "wsf",
    "vbs",
    "vbe",
    "jse",
    "wsh",
    "run",
    "appimage",
    "air",
    "xpi",
    "crx",
    "vsix",
];

const CODE: &[&str] = &[
    "rs",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hxx",
    "hh",
    "inl",
    "ipp",
    "m",
    "mm",
    "cs",
    "vb",
    "fs",
    "fsx",
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "gradle",
    "go",
    "py",
    "pyw",
    "pyi",
    "pyc",
    "pyo",
    "pyd",
    "ipynb",
    "rb",
    "erb",
    "php",
    "pl",
    "pm",
    "lua",
    "r",
    "jl",
    "swift",
    "dart",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "vue",
    "svelte",
    "html",
    "htm",
    "xhtml",
    "css",
    "scss",
    "sass",
    "less",
    "json",
    "json5",
    "jsonl",
    "ndjson",
    "xml",
    "xsd",
    "xsl",
    "xslt",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "config",
    "props",
    "targets",
    "properties",
    "env",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "psm1",
    "psd1",
    "sql",
    "sqlite",
    "sqlite3",
    "db",
    "asm",
    "s",
    "f",
    "f90",
    "pas",
    "d",
    "zig",
    "nim",
    "hs",
    "ml",
    "ex",
    "exs",
    "erl",
    "clj",
    "cljs",
    "lisp",
    "el",
    "vim",
    "tf",
    "proto",
    "graphql",
    "cmake",
    "mk",
    "make",
    "sln",
    "csproj",
    "vcxproj",
    "vbproj",
    "fsproj",
    "xcodeproj",
    "pbxproj",
    "lock",
    "map",
    "wasm",
    "wat",
    "obj",
    "o",
    "a",
    "lib",
    "pdb",
    "ilk",
    "exp",
    "class",
    "rlib",
    "rmeta",
    "node",
    "diff",
    "patch",
    "gitignore",
    "gitattributes",
    "editorconfig",
    "dockerfile",
    "resx",
    "xaml",
    "razor",
    "cshtml",
    "aspx",
    "jsp",
    "twig",
    "tpl",
    "ejs",
    "hbs",
    "pug",
    "nix",
    "bazel",
    "bzl",
    "ninja",
    "idl",
];

const SYSTEM: &[&str] = &[
    "dll",
    "sys",
    "drv",
    "ocx",
    "ax",
    "mui",
    "cat",
    "inf",
    "pnf",
    "manifest",
    "mum",
    "efi",
    "nls",
    "tlb",
    "olb",
    "dat",
    "etl",
    "evtx",
    "dmp",
    "mdmp",
    "hdmp",
    "pf",
    "sdb",
    "mof",
    "cpi",
    "icm",
    "icc",
    "ttf",
    "otf",
    "ttc",
    "fon",
    "fnt",
    "woff",
    "woff2",
    "eot",
    "regtrans-ms",
    "blf",
    "msstyles",
    "theme",
    "deskthemepack",
    "ani",
    "library-ms",
    "winmd",
    "pri",
    "p7b",
    "p7x",
    "cer",
    "crt",
    "pfx",
    "edb",
    "jrs",
    "chk",
    "sdi",
    "ldf",
    "tmp",
    "cache",
    "vxd",
    "386",
    "rom",
    "firmware",
    "cdf-ms",
    "db-wal",
    "db-shm",
    "prx",
    "lex",
    "ngr",
    "tbres",
    "mun",
    "psf",
    "rtp",
    "mpb",
    "vdm",
    "reg",
    "pol",
    "admx",
    "adml",
    "job",
    "wer",
];

fn table() -> &'static FxHashMap<&'static str, FileCategory> {
    static TABLE: OnceLock<FxHashMap<&'static str, FileCategory>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = FxHashMap::default();
        // Later groups never override earlier ones, so the order below
        // decides ambiguous extensions (e.g. "ts", "bin", "db").
        let groups: [(&[&str], FileCategory); 9] = [
            (PICTURES, FileCategory::Pictures),
            (MUSIC, FileCategory::Music),
            (VIDEO, FileCategory::Video),
            (DOCUMENTS, FileCategory::Documents),
            (EMAIL, FileCategory::Email),
            (APPLICATIONS, FileCategory::Applications),
            (ARCHIVES, FileCategory::Archives),
            (CODE, FileCategory::Code),
            (SYSTEM, FileCategory::System),
        ];
        for (extensions, category) in groups {
            for ext in extensions {
                map.entry(*ext).or_insert(category);
            }
        }
        map
    })
}

/// Longest extension the analyzer tracks individually.
pub(crate) const MAX_EXTENSION_LEN: usize = 16;

/// A lower-cased ASCII extension held on the stack.
#[derive(Clone, Copy)]
pub(crate) struct ExtensionKey {
    buf: [u8; MAX_EXTENSION_LEN],
    len: u8,
}

impl ExtensionKey {
    /// The extension text (empty when the file has none).
    pub fn as_str(&self) -> &str {
        // Only ASCII bytes are ever stored.
        std::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }

    /// Extracts the extension from a UTF-16 file name.
    ///
    /// Follows the standard library's convention: a leading dot does not
    /// start an extension (`.gitignore` has none). Extensions that are too
    /// long or contain unusual characters are treated as "no extension" so
    /// random suffixes cannot flood the statistics.
    pub fn from_name(name: &[u16]) -> Self {
        const DOT: u16 = b'.' as u16;
        let mut key = ExtensionKey {
            buf: [0; MAX_EXTENSION_LEN],
            len: 0,
        };
        let Some(dot) = name.iter().rposition(|&c| c == DOT) else {
            return key;
        };
        let ext = &name[dot + 1..];
        if dot == 0 || ext.is_empty() || ext.len() > MAX_EXTENSION_LEN {
            return key;
        }
        for (i, &unit) in ext.iter().enumerate() {
            let ok = unit < 128
                && ((unit as u8).is_ascii_alphanumeric()
                    || matches!(unit as u8, b'_' | b'-' | b'+' | b'~' | b'$' | b'#' | b'!'));
            if !ok {
                key.len = 0;
                return key;
            }
            key.buf[i] = (unit as u8).to_ascii_lowercase();
        }
        key.len = ext.len() as u8;
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> String {
        let wide: Vec<u16> = name.encode_utf16().collect();
        ExtensionKey::from_name(&wide).as_str().to_owned()
    }

    #[test]
    fn extension_extraction() {
        assert_eq!(key("photo.JPG"), "jpg");
        assert_eq!(key("archive.tar.gz"), "gz");
        assert_eq!(key("README"), "");
        assert_eq!(key(".gitignore"), "");
        assert_eq!(key("trailing."), "");
        assert_eq!(key("weird.ex t"), "");
        assert_eq!(key("unicode.désolé"), "");
        assert_eq!(key("x.abcdefghijklmnopqrstuvwxyz"), "");
        assert_eq!(key("lib.so.1"), "1");
        assert_eq!(key("video.dvr-ms"), "dvr-ms");
    }

    #[test]
    fn categories_by_extension() {
        assert_eq!(FileCategory::of_extension("jpg"), FileCategory::Pictures);
        assert_eq!(FileCategory::of_extension("flac"), FileCategory::Music);
        assert_eq!(FileCategory::of_extension("mkv"), FileCategory::Video);
        assert_eq!(FileCategory::of_extension("docx"), FileCategory::Documents);
        assert_eq!(FileCategory::of_extension("7z"), FileCategory::Archives);
        assert_eq!(FileCategory::of_extension("pst"), FileCategory::Email);
        assert_eq!(
            FileCategory::of_extension("exe"),
            FileCategory::Applications
        );
        assert_eq!(FileCategory::of_extension("rs"), FileCategory::Code);
        assert_eq!(FileCategory::of_extension("dll"), FileCategory::System);
        assert_eq!(FileCategory::of_extension("xyz"), FileCategory::Other);
        assert_eq!(FileCategory::of_extension(""), FileCategory::Other);
        // Ambiguous extensions resolve by table order.
        assert_eq!(FileCategory::of_extension("ts"), FileCategory::Code);
        assert_eq!(FileCategory::of_extension("apk"), FileCategory::Archives);
        assert_eq!(
            FileCategory::of_path(std::path::Path::new(r"C:\x\Holiday.MP4")),
            FileCategory::Video
        );
    }

    #[test]
    fn indices_match_all() {
        for (i, cat) in FileCategory::ALL.iter().enumerate() {
            assert_eq!(cat.index(), i);
            assert!(!cat.label().is_empty());
            assert!(!cat.description().is_empty());
        }
        assert_eq!(FileCategory::COUNT, 10);
    }

    #[test]
    fn no_extension_is_listed_in_upper_case() {
        for ext in table().keys() {
            assert_eq!(*ext, ext.to_ascii_lowercase());
            assert!(ext.len() <= MAX_EXTENSION_LEN, "{ext} is too long to match");
        }
    }
}
