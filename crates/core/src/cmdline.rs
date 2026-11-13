//! Best-effort understanding of Windows command lines found in the registry,
//! in shortcuts and in scheduled tasks.
//!
//! The registry stores launch commands in many loose forms: quoted and
//! unquoted paths containing spaces, environment variables, resource
//! references (`@shell32.dll,-123`), NT object paths (`\??\C:\...`) and bare
//! program names resolved through `PATH`. Scanners that flag "target is
//! missing" depend on this module to be **conservative**: whenever a command
//! cannot be understood with confidence the answer is [`Target::Unknown`],
//! never [`Target::Missing`].

use crate::paths;
use std::path::{Path, PathBuf};

/// Extensions that mark the end of the program part of an unquoted command.
const PROGRAM_EXTENSIONS: &[&str] = &[
    ".exe", ".dll", ".com", ".bat", ".cmd", ".scr", ".cpl", ".msc", ".sys", ".ocx", ".ax", ".vbs",
    ".js", ".ps1", ".msi", ".lnk", ".jar", ".tlb", ".olb", ".chm", ".hlp", ".ttf", ".otf", ".ttc",
    ".fon", ".wav", ".ico", ".mui", ".drv", ".acm", ".pyd", ".py",
];

/// Programs whose first argument, not the program itself, is the real target.
const LAUNCHERS: &[&str] = &["rundll32.exe", "rundll32", "regsvr32.exe", "regsvr32"];

/// Programs that run arbitrary scripts/commands; their real target cannot be
/// determined reliably.
const OPAQUE_HOSTS: &[&str] = &[
    "cmd.exe",
    "cmd",
    "powershell.exe",
    "powershell",
    "pwsh.exe",
    "pwsh",
    "wscript.exe",
    "wscript",
    "cscript.exe",
    "cscript",
    "mshta.exe",
    "mshta",
    "msiexec.exe",
    "msiexec",
    "explorer.exe",
    "explorer",
    "conhost.exe",
    "conhost",
    "schtasks.exe",
    "schtasks",
    "sc.exe",
    "sc",
    "start",
    "java.exe",
    "javaw.exe",
    "python.exe",
    "pythonw.exe",
    "node.exe",
    "dotnet.exe",
    "wsl.exe",
    "bash.exe",
];

/// What a command line points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The target file exists at this location.
    Exists(PathBuf),
    /// The target is an absolute path on a local fixed drive and is not there.
    Missing(PathBuf),
    /// The command could not be resolved with confidence (network path,
    /// removable drive, opaque script host, unknown bare name, ...).
    Unknown,
}

impl Target {
    /// True only for [`Target::Missing`].
    pub fn is_missing(&self) -> bool {
        matches!(self, Target::Missing(_))
    }

    /// The resolved path, whether it exists or not.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Target::Exists(p) | Target::Missing(p) => Some(p),
            Target::Unknown => None,
        }
    }
}

/// Rewrites registry-style path spellings into ordinary Win32 paths and
/// expands environment variables.
pub fn normalize_path_text(raw: &str) -> String {
    let mut s = paths::expand_env(raw.trim());
    // Resource references such as "@%SystemRoot%\system32\shell32.dll,-21770".
    if let Some(rest) = s.strip_prefix('@') {
        s = rest.to_owned();
    }
    let lower = s.to_ascii_lowercase();
    let windows = paths::windows_dir();
    if let Some(rest) = lower.strip_prefix("\\??\\") {
        s = s[s.len() - rest.len()..].to_owned();
    } else if lower.starts_with("\\systemroot\\") {
        s = format!("{}{}", windows.display(), &s["\\systemroot".len()..]);
    } else if lower.starts_with("system32\\") || lower.starts_with("syswow64\\") {
        s = format!("{}\\{}", windows.display(), s);
    }
    s
}

/// Splits a command line into the program and the remaining argument text.
///
/// Unquoted paths containing spaces are handled by probing the file system
/// for the longest prefix that exists, then by looking for a known program
/// extension.
pub fn split_program(command: &str) -> (String, String) {
    let command = normalize_path_text(command);
    let trimmed = command.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => (rest[..end].to_owned(), rest[end + 1..].trim().to_owned()),
            None => (rest.to_owned(), String::new()),
        };
    }

    // The whole string may simply be a path.
    if Path::new(trimmed).is_file() {
        return (trimmed.to_owned(), String::new());
    }

    // Probe prefixes ending at each space: "C:\Program Files\App\app.exe /x".
    let mut best: Option<usize> = None;
    for (i, ch) in trimmed.char_indices() {
        if ch != ' ' && ch != '\t' {
            continue;
        }
        let candidate = trimmed[..i].trim_end_matches(',');
        if candidate.is_empty() {
            continue;
        }
        let path = Path::new(candidate);
        if path.is_file() || Path::new(&format!("{candidate}.exe")).is_file() {
            best = Some(i);
            // Keep going only if a longer prefix could also be a file; the
            // first hit is right in practice ("C:\Program.exe" is rare) but a
            // longer existing file is the more specific answer.
        }
    }
    if let Some(i) = best {
        return (trimmed[..i].to_owned(), trimmed[i..].trim().to_owned());
    }

    // Nothing exists: cut after the first known program extension.
    let lower = trimmed.to_ascii_lowercase();
    let mut cut: Option<usize> = None;
    for ext in PROGRAM_EXTENSIONS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(ext) {
            let end = from + pos + ext.len();
            let boundary = lower[end..]
                .chars()
                .next()
                .is_none_or(|c| c == ' ' || c == '\t' || c == ',' || c == '"');
            if boundary {
                cut = Some(cut.map_or(end, |c: usize| c.min(end)));
                break;
            }
            from = end;
        }
    }
    if let Some(end) = cut {
        return (trimmed[..end].to_owned(), trimmed[end..].trim().to_owned());
    }

    match trimmed.split_once([' ', '\t']) {
        Some((program, args)) => (program.to_owned(), args.trim().to_owned()),
        None => (trimmed.to_owned(), String::new()),
    }
}

/// Removes a trailing icon/resource index: `shell32.dll,-154` -> `shell32.dll`.
pub fn strip_resource_index(path: &str) -> &str {
    if let Some(comma) = path.rfind(',') {
        let tail = path[comma + 1..].trim();
        let numeric = !tail.is_empty()
            && tail
                .trim_start_matches('-')
                .chars()
                .all(|c| c.is_ascii_digit());
        if numeric {
            return path[..comma].trim_end();
        }
    }
    path
}

fn drive_is_local_fixed(path: &Path) -> bool {
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows::core::PCWSTR;
    const DRIVE_FIXED: u32 = 3;

    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    let root: Vec<u16> = format!("{}:\\", bytes[0] as char)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: `root` is a NUL terminated drive root string.
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) == DRIVE_FIXED }
}

/// The 32-bit/64-bit twin of a path, if it lies in a folder that Windows
/// duplicates per architecture.
///
/// 32-bit registry entries name `System32` while meaning `SysWOW64`, and a
/// 64-bit process expands `%ProgramFiles%` / `%CommonProgramFiles%` to the
/// 64-bit folders even when the entry belongs to a 32-bit program. A target
/// that exists in the twin location is therefore not missing.
fn wow_twin(path: &Path) -> Option<PathBuf> {
    let lower = path.to_string_lossy().to_lowercase();
    let under = |root: &Path| -> Option<String> {
        let prefix = format!("{}\\", root.to_string_lossy().to_lowercase());
        lower
            .strip_prefix(&prefix)
            // Lower-casing ASCII keeps byte offsets, so slice the original.
            .map(|rest| path.to_string_lossy()[lower.len() - rest.len()..].to_owned())
    };

    let windows = paths::windows_dir();
    if let Some(rel) = under(&windows) {
        let (folder, rest) = rel.split_once('\\')?;
        let twin = match folder.to_lowercase().as_str() {
            "system32" | "sysnative" => "SysWOW64",
            "syswow64" => "System32",
            _ => return None,
        };
        return Some(windows.join(twin).join(rest));
    }
    let (pf, pf86) = (paths::program_files(), paths::program_files_x86());
    // Check the longer "(x86)" root first: it shares a prefix with the other.
    if let Some(rel) = under(&pf86) {
        return Some(pf.join(rel));
    }
    if let Some(rel) = under(&pf) {
        return Some(pf86.join(rel));
    }
    None
}

/// Whether something exists at `path`: `Some(true)` / `Some(false)` when the
/// file system gives a definite answer, `None` when it does not (for example
/// "access denied", which says nothing about existence).
fn presence(path: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

fn search_bare_name(name: &str) -> Option<PathBuf> {
    let has_ext = Path::new(name).extension().is_some();
    let candidates: Vec<String> = if has_ext {
        vec![name.to_owned()]
    } else {
        vec![
            format!("{name}.exe"),
            format!("{name}.com"),
            format!("{name}.bat"),
            format!("{name}.cmd"),
        ]
    };
    let windows = paths::windows_dir();
    let mut dirs = vec![
        windows.join("System32"),
        windows.clone(),
        windows.join("SysWOW64"),
        windows.join("System32").join("wbem"),
        windows.join("System32").join("drivers"),
    ];
    if let Some(path_var) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path_var));
    }
    for dir in &dirs {
        for candidate in &candidates {
            let full = dir.join(candidate);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    // "App Paths" lets programs be started by bare name without being on PATH.
    for candidate in &candidates {
        for root in [crate::reg::RegRoot::Hkcu, crate::reg::RegRoot::Hklm] {
            let key = crate::reg::RegPath::new(
                root,
                format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{candidate}"),
            );
            if let Some(value) = key.read_string("") {
                let path = PathBuf::from(normalize_path_text(value.trim_matches('"')));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Checks whether a single path (not a command line) exists.
///
/// Returns [`Target::Unknown`] for anything that is not an absolute path on
/// a local fixed drive, so disconnected network shares and unplugged USB
/// drives never produce false "missing" reports.
pub fn check_path(raw: &str) -> Target {
    let text = normalize_path_text(strip_resource_index(raw.trim().trim_matches('"')));
    let text = text.trim().trim_matches('"');
    if text.is_empty() || text.contains(['*', '?', '<', '>', '|']) {
        return Target::Unknown;
    }
    // Unexpanded variables or shell namespaces ("shell:::{...}", "::{...}").
    if text.contains('%')
        || text.starts_with("::")
        || text.to_ascii_lowercase().starts_with("shell:")
    {
        return Target::Unknown;
    }
    let path = PathBuf::from(text);
    if text.starts_with("\\\\") {
        return Target::Unknown;
    }
    let has_separator = text.contains('\\') || text.contains('/');
    if !has_separator {
        return match search_bare_name(text) {
            Some(found) => Target::Exists(found),
            None => Target::Unknown,
        };
    }
    if !path.is_absolute() {
        return Target::Unknown;
    }
    match presence(&path) {
        Some(true) => return Target::Exists(path),
        // No definite answer (e.g. access denied): never call that missing.
        None => return Target::Unknown,
        Some(false) => {}
    }
    if let Some(twin) = wow_twin(&path) {
        match presence(&twin) {
            Some(true) => return Target::Exists(twin),
            None => return Target::Unknown,
            Some(false) => {}
        }
    }
    if !drive_is_local_fixed(&path) {
        return Target::Unknown;
    }
    Target::Missing(path)
}

/// Resolves the program a command line starts.
pub fn check_program(command: &str) -> Target {
    let (program, _) = split_program(command);
    check_path(&program)
}

/// Resolves the file a command line is really about.
///
/// For `rundll32`/`regsvr32` this is the DLL in the arguments. For script
/// hosts (`cmd /c`, `powershell`, `msiexec`, ...) the answer is `Unknown`
/// unless the host itself is missing.
pub fn check_command(command: &str) -> Target {
    let (program, args) = split_program(command);
    let name = Path::new(&program)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if LAUNCHERS.contains(&name.as_str()) {
        // Skip switches such as "/s" or "-e" in front of the DLL.
        let mut rest = args.trim();
        while rest.starts_with('/') || rest.starts_with('-') {
            rest = rest
                .split_once([' ', '\t'])
                .map_or("", |(_, r)| r.trim_start());
        }
        if rest.is_empty() {
            return Target::Unknown;
        }
        let dll = if let Some(quoted) = rest.strip_prefix('"') {
            quoted.split('"').next().unwrap_or("").to_owned()
        } else {
            // Unquoted: the DLL path ends at the comma before the entry point,
            // or at a known extension.
            let (candidate, _) = split_program(rest);
            candidate.split(',').next().unwrap_or("").to_owned()
        };
        return match check_path(&dll) {
            Target::Missing(p) => Target::Missing(p),
            Target::Exists(p) => Target::Exists(p),
            Target::Unknown => Target::Unknown,
        };
    }

    if OPAQUE_HOSTS.contains(&name.as_str()) {
        return match check_path(&program) {
            Target::Missing(p) => Target::Missing(p),
            _ => Target::Unknown,
        };
    }

    check_path(&program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(name: &str) -> String {
        paths::system32_dir().join(name).display().to_string()
    }

    #[test]
    fn splits_quoted_and_unquoted() {
        assert_eq!(
            split_program(r#""C:\Program Files\App\app.exe" --flag "x y""#),
            (
                r"C:\Program Files\App\app.exe".to_owned(),
                r#"--flag "x y""#.to_owned()
            )
        );
        // Unquoted, nothing exists: cut at the extension.
        assert_eq!(
            split_program(r"C:\No Such Folder\My App\tool.exe /silent /x"),
            (
                r"C:\No Such Folder\My App\tool.exe".to_owned(),
                "/silent /x".to_owned()
            )
        );
        // Unquoted, exists on disk.
        let cmd = sys("cmd.exe");
        assert_eq!(
            split_program(&format!("{cmd} /c echo hi")),
            (cmd.clone(), "/c echo hi".to_owned())
        );
        assert_eq!(split_program(&cmd), (cmd, String::new()));
        assert_eq!(
            split_program("notepad"),
            ("notepad".to_owned(), String::new())
        );
        assert_eq!(
            split_program("notepad.exe C:\\file.txt"),
            ("notepad.exe".to_owned(), "C:\\file.txt".to_owned())
        );
    }

    #[test]
    fn unquoted_path_with_spaces_that_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("My Tools Folder");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("my tool.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let (program, args) = split_program(&format!("{} --run now", exe.display()));
        assert_eq!(program, exe.display().to_string());
        assert_eq!(args, "--run now");
        assert_eq!(
            check_program(&format!("{} --run now", exe.display())),
            Target::Exists(exe)
        );
    }

    #[test]
    fn registry_spellings() {
        let windows = paths::windows_dir().display().to_string();
        assert_eq!(
            normalize_path_text(r"\SystemRoot\System32\drivers\x.sys"),
            format!(r"{windows}\System32\drivers\x.sys")
        );
        assert_eq!(normalize_path_text(r"\??\C:\Dir\x.sys"), r"C:\Dir\x.sys");
        assert_eq!(
            normalize_path_text(r"system32\drivers\x.sys"),
            format!(r"{windows}\system32\drivers\x.sys")
        );
        assert_eq!(
            normalize_path_text(r"@%SystemRoot%\system32\shell32.dll"),
            format!(r"{windows}\system32\shell32.dll")
        );
        assert_eq!(
            strip_resource_index(r"C:\x\shell32.dll,-154"),
            r"C:\x\shell32.dll"
        );
        assert_eq!(
            strip_resource_index(r"C:\x\shell32.dll,3"),
            r"C:\x\shell32.dll"
        );
        assert_eq!(strip_resource_index(r"C:\x\a,b.dll"), r"C:\x\a,b.dll");
    }

    #[test]
    fn existing_targets() {
        assert!(matches!(
            check_command(&sys("cmd.exe")),
            Target::Unknown | Target::Exists(_)
        ));
        assert!(matches!(check_path(&sys("shell32.dll")), Target::Exists(_)));
        assert!(matches!(
            check_path(r"@%SystemRoot%\system32\shell32.dll,-21770"),
            Target::Exists(_)
        ));
        assert!(matches!(
            check_path(r"\SystemRoot\System32\ntdll.dll"),
            Target::Exists(_)
        ));
        assert!(matches!(check_path("notepad.exe"), Target::Exists(_)));
        assert!(matches!(check_program("notepad"), Target::Exists(_)));
        assert!(matches!(
            check_command(&format!(
                r#"rundll32.exe "{}",Control_RunDLL"#,
                sys("shell32.dll")
            )),
            Target::Exists(_)
        ));
        assert!(matches!(
            check_command(&format!(
                "rundll32.exe {},Control_RunDLL desk.cpl",
                sys("shell32.dll")
            )),
            Target::Exists(_)
        ));
    }

    #[test]
    fn missing_targets() {
        let drive = paths::system_drive();
        let gone = format!(r"{}Zest No Such Dir\gone.exe", drive.display());
        assert_eq!(check_path(&gone), Target::Missing(PathBuf::from(&gone)));
        assert!(check_command(&format!(r#""{gone}" /background"#)).is_missing());
        assert!(check_command(&format!("{gone} /background")).is_missing());
        assert!(
            check_command(&format!(
                r#"rundll32.exe "{}Zest No Such Dir\x.dll",Entry"#,
                drive.display()
            ))
            .is_missing()
        );
    }

    #[test]
    fn uncertain_targets_are_never_missing() {
        // Bare names that cannot be found might still be resolvable elsewhere.
        assert_eq!(check_path("some-unknown-tool-zest.exe"), Target::Unknown);
        // Network locations may simply be offline.
        assert_eq!(check_path(r"\\server\share\app.exe"), Target::Unknown);
        // A drive letter that is not a local fixed disk.
        assert_eq!(check_path(r"B:\setup.exe"), Target::Unknown);
        // Unexpanded variables and shell namespaces.
        assert_eq!(check_path(r"%NoSuchVarZest%\app.exe"), Target::Unknown);
        assert_eq!(
            check_path("shell:::{00000000-0000-0000-0000-000000000000}"),
            Target::Unknown
        );
        assert_eq!(check_path(""), Target::Unknown);
        // Script hosts hide their real target.
        assert_eq!(
            check_command(r"cmd.exe /c C:\gone\script.bat"),
            Target::Unknown
        );
        assert_eq!(
            check_command(r"powershell -File C:\gone\x.ps1"),
            Target::Unknown
        );
        assert_eq!(check_command(r"msiexec.exe /x {GUID}"), Target::Unknown);
        assert_eq!(check_command("rundll32.exe"), Target::Unknown);
    }

    #[test]
    fn program_files_folders_are_twins() {
        // An entry of a 32-bit program written with the 64-bit folder (or the
        // other way round) must resolve when the file exists in the twin.
        let (pf, pf86) = (paths::program_files(), paths::program_files_x86());
        if !pf86.is_dir() || paths::same_path(&pf, &pf86) {
            return;
        }
        let find_unique = |here: &Path, there: &Path| -> Option<PathBuf> {
            std::fs::read_dir(here)
                .ok()?
                .flatten()
                .map(|e| e.file_name())
                .find(|name| here.join(name).is_dir() && !there.join(name).exists())
                .map(PathBuf::from)
        };
        if let Some(name) = find_unique(&pf86, &pf) {
            let via_64 = pf.join(&name).display().to_string();
            assert_eq!(
                check_path(&via_64),
                Target::Exists(pf86.join(&name)),
                "{via_64}"
            );
        }
        if let Some(name) = find_unique(&pf, &pf86) {
            let via_86 = pf86.join(&name).display().to_string();
            assert_eq!(
                check_path(&via_86),
                Target::Exists(pf.join(&name)),
                "{via_86}"
            );
        }
        // Missing in both stays missing.
        assert!(check_path(&pf.join(r"Zest No Such App\x.exe").display().to_string()).is_missing());
    }

    #[test]
    fn system32_and_syswow64_are_twins() {
        // A file that only exists in one of the two folders must not be
        // reported missing when referenced through the other.
        let windows = paths::windows_dir();
        let only_64 = windows.join("System32");
        let only_32 = windows.join("SysWOW64");
        if let (Ok(a), true) = (std::fs::read_dir(&only_64), only_32.is_dir()) {
            for entry in a.flatten().take(4000) {
                let name = entry.file_name();
                if entry.path().is_file() && !only_32.join(&name).exists() {
                    let via_wow = only_32.join(&name).display().to_string();
                    assert!(
                        matches!(check_path(&via_wow), Target::Exists(_)),
                        "{via_wow} should resolve through its System32 twin"
                    );
                    break;
                }
            }
        }
    }
}
