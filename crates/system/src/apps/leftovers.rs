//! Finding what an uninstaller left behind.
//!
//! The scan is deliberately strict. A folder or registry key is only ever
//! reported when its name is *exactly* the program's install-folder name or
//! display name (optionally below a folder named exactly like the
//! publisher), the name is specific enough to be unambiguous, and no other
//! installed program shares it. There is no fuzzy matching. The scan only
//! reports; removal is a separate, explicit step that goes through the
//! shared deletion and registry guards.

use super::{InstalledApp, ListOptions, is_still_installed, list_installed};
use crate::shortcut::read_shortcut;
use crate::util::{is_within, norm_path};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zest_core::delete::{DeleteOptions, DeleteReport};
use zest_core::reg::{RegBackup, RegPath, RegRoot, is_protected_key};
use zest_core::{Progress, paths, safety, walk};

/// Names that say nothing about which program they belong to. A candidate
/// equal to one of these (case-insensitive) is never used for matching.
const GENERIC_NAMES: &[&str] = &[
    "adobe",
    "amd",
    "android",
    "app",
    "apple",
    "application",
    "applications",
    "apps",
    "asus",
    "backup",
    "bin",
    "browser",
    "cache",
    "client",
    "common",
    "common files",
    "config",
    "core",
    "data",
    "default",
    "desktop",
    "documents",
    "downloads",
    "driver",
    "drivers",
    "games",
    "google",
    "help",
    "install",
    "installer",
    "intel",
    "internet",
    "java",
    "launcher",
    "lenovo",
    "local",
    "logs",
    "microsoft",
    "mozilla",
    "nvidia",
    "office",
    "oracle",
    "packages",
    "player",
    "plugins",
    "policies",
    "program files",
    "programs",
    "public",
    "realtek",
    "roaming",
    "runtime",
    "server",
    "service",
    "services",
    "settings",
    "setup",
    "shared",
    "software",
    "steam",
    "system",
    "temp",
    "tools",
    "uninstall",
    "update",
    "updater",
    "updates",
    "user",
    "users",
    "utilities",
    "windows",
    "classes",
    "wow6432node",
    "clients",
    "registeredapplications",
];

/// Shortest name considered specific enough to match on.
const MIN_NAME_LEN: usize = 4;

/// What kind of thing was left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LeftoverKind {
    /// The program's own install folder still exists.
    InstallFolder,
    /// A data folder named exactly like the program.
    DataFolder,
    /// A Start Menu shortcut (or folder of shortcuts) whose target is gone.
    StartMenu,
    /// A settings key named exactly like the program.
    RegistryKey,
}

impl LeftoverKind {
    /// Short label, e.g. `Data folder`.
    pub fn label(self) -> &'static str {
        match self {
            LeftoverKind::InstallFolder => "Install folder",
            LeftoverKind::DataFolder => "Data folder",
            LeftoverKind::StartMenu => "Start Menu",
            LeftoverKind::RegistryKey => "Registry key",
        }
    }
}

/// One item an uninstaller left behind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leftover {
    /// What kind of item this is.
    pub kind: LeftoverKind,
    /// The file or folder (for file system items).
    pub path: Option<PathBuf>,
    /// The registry key (for registry items).
    pub key: Option<RegPath>,
    /// Size of the file or folder in bytes (zero for registry keys).
    pub size_bytes: u64,
    /// Number of files inside (zero for registry keys).
    pub files: u64,
    /// Why this item was matched, for display.
    pub reason: String,
}

impl Leftover {
    /// The location as text, whichever kind of item this is.
    pub fn location(&self) -> String {
        match (&self.path, &self.key) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(key)) => key.to_string(),
            (None, None) => String::new(),
        }
    }
}

/// Result of [`remove_leftovers`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LeftoverReport {
    /// Number of leftover items removed completely.
    pub removed: usize,
    /// Bytes freed on disk.
    pub bytes_freed: u64,
    /// Files deleted.
    pub files_deleted: u64,
    /// Items that could not be removed: location and reason.
    pub failures: Vec<(String, String)>,
}

/// The places a scan looks in. Tests point these at a sandbox.
pub(crate) struct LeftoverRoots {
    /// Per-user and machine-wide application data folders.
    pub data_dirs: Vec<PathBuf>,
    /// Start Menu "Programs" folders.
    pub start_menus: Vec<PathBuf>,
    /// `Software` keys to look for settings in.
    pub software_keys: Vec<RegPath>,
}

fn real_roots() -> LeftoverRoots {
    let user = paths::UserDirs::current();
    const PROGRAMS: &str = r"Microsoft\Windows\Start Menu\Programs";
    LeftoverRoots {
        data_dirs: vec![
            user.roaming.clone(),
            user.local.clone(),
            paths::program_data(),
        ],
        start_menus: vec![
            user.roaming.join(PROGRAMS),
            paths::program_data().join(PROGRAMS),
        ],
        software_keys: vec![
            RegPath::new(RegRoot::Hkcu, "Software"),
            RegPath::new(RegRoot::Hklm, "SOFTWARE"),
            RegPath::new(RegRoot::Hklm, r"SOFTWARE\WOW6432Node"),
        ],
    }
}

/// True when `name` is distinctive enough to match folders and keys on.
fn is_specific_name(name: &str) -> bool {
    let trimmed = name.trim();
    let lower = trimmed.to_lowercase();
    trimmed.chars().count() >= MIN_NAME_LEN
        && !trimmed.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        && !trimmed.starts_with('.')
        && !trimmed.ends_with(['.', ' '])
        && trimmed.chars().any(char::is_alphabetic)
        && !GENERIC_NAMES.contains(&lower.as_str())
}

/// The install location, when it is a place that can belong to one program:
/// not a system or shared folder, and with a distinctive folder name.
fn own_location(app: &InstalledApp) -> Option<&Path> {
    let location = app.install_location.as_deref()?;
    let leaf = location.file_name()?.to_string_lossy();
    (is_specific_name(&leaf) && !safety::is_protected(location)).then_some(location)
}

fn install_folder_name(app: &InstalledApp) -> Option<String> {
    own_location(app)?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// The publisher as a single folder / key name, if it can be one. It is only
/// ever used as the parent of an exact program name, never matched alone.
fn publisher_component(app: &InstalledApp) -> Option<&str> {
    app.publisher.as_deref().map(str::trim).filter(|p| {
        p.chars().count() >= 2 && !p.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
    })
}

/// The exact names a program's leftovers may carry: its install-folder name
/// and its display name – minus anything generic or shared with another
/// installed program.
fn candidate_names(app: &InstalledApp, others: &[InstalledApp]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in [install_folder_name(app), Some(app.name.clone())]
        .into_iter()
        .flatten()
    {
        let name = name.trim().to_owned();
        if !is_specific_name(&name) || names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let shared = others.iter().any(|other| {
            other.name.trim().eq_ignore_ascii_case(&name)
                || install_folder_name(other).is_some_and(|n| n.trim().eq_ignore_ascii_case(&name))
        });
        if !shared {
            names.push(name);
        }
    }
    names
}

fn folder_leftover(
    kind: LeftoverKind,
    path: PathBuf,
    reason: String,
    progress: &Progress,
) -> Leftover {
    let size = walk::tree_size(&path, Some(progress));
    Leftover {
        kind,
        path: Some(path),
        key: None,
        size_bytes: size.bytes,
        files: size.files,
        reason,
    }
}

/// The install folder, if it survived and is safe to offer.
fn install_folder(
    app: &InstalledApp,
    others: &[InstalledApp],
    progress: &Progress,
) -> Option<Leftover> {
    let location = own_location(app)?;
    if !location.is_dir() {
        return None;
    }
    // A folder that another installed program lives in (or that contains
    // another program) is shared and must be left alone. So is the folder of
    // a program that is registered a second time under the same name.
    let shared = others.iter().any(|other| {
        other.name.trim().eq_ignore_ascii_case(app.name.trim())
            || other
                .install_location
                .as_ref()
                .is_some_and(|theirs| is_within(theirs, location) || is_within(location, theirs))
    });
    if shared {
        return None;
    }
    Some(folder_leftover(
        LeftoverKind::InstallFolder,
        location.to_path_buf(),
        format!("Install folder of {}", app.name),
        progress,
    ))
}

fn data_folders(
    app: &InstalledApp,
    names: &[String],
    roots: &LeftoverRoots,
    already: &[Leftover],
    progress: &Progress,
    out: &mut Vec<Leftover>,
) {
    let publisher = publisher_component(app);
    for base in &roots.data_dirs {
        for name in names {
            let mut candidates = vec![(base.join(name), format!("Folder named \"{name}\""))];
            if let Some(publisher) = publisher {
                candidates.push((
                    base.join(publisher).join(name),
                    format!("Folder named \"{publisher}\\{name}\""),
                ));
            }
            for (path, reason) in candidates {
                if !path.is_dir() || safety::is_protected(&path) {
                    continue;
                }
                let known = already
                    .iter()
                    .chain(out.iter())
                    .filter_map(|l| l.path.as_ref())
                    .any(|p| norm_path(p) == norm_path(&path));
                if known {
                    continue;
                }
                out.push(folder_leftover(
                    LeftoverKind::DataFolder,
                    path,
                    reason,
                    progress,
                ));
            }
        }
    }
}

/// Where a shortcut points, classified for the leftover scan.
enum LinkState {
    /// Points into the removed program's folder and the target is gone.
    Dead,
    /// Anything else: still valid, or not this program's business.
    Other,
}

fn link_state(link: &Path, install_location: Option<&Path>) -> LinkState {
    let Some(location) = install_location else {
        return LinkState::Other;
    };
    match read_shortcut(link).ok().and_then(|s| s.target) {
        Some(target) if is_within(&target, location) && !target.exists() => LinkState::Dead,
        _ => LinkState::Other,
    }
}

fn collect_links(dir: &Path, links: &mut Vec<PathBuf>, others: &mut usize) {
    walk::walk_tree(dir, None, &mut |path, _, kind| {
        if kind == walk::EntryKind::Dir {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.ends_with(".lnk") {
            links.push(path.to_path_buf());
        } else if name != "desktop.ini" {
            *others += 1;
        }
    });
}

fn start_menu(
    app: &InstalledApp,
    names: &[String],
    roots: &LeftoverRoots,
    progress: &Progress,
    out: &mut Vec<Leftover>,
) {
    let location = own_location(app);
    for programs in &roots.start_menus {
        if progress.is_cancelled() {
            return;
        }
        // A program folder in the Start Menu named exactly like the program,
        // holding nothing but dead shortcuts, goes as a whole.
        let mut claimed: Vec<PathBuf> = Vec::new();
        for name in names {
            let folder = programs.join(name);
            if !folder.is_dir() || safety::is_protected(&folder) {
                continue;
            }
            let mut links = Vec::new();
            let mut other_files = 0usize;
            collect_links(&folder, &mut links, &mut other_files);
            let all_dead = !links.is_empty()
                && other_files == 0
                && links
                    .iter()
                    .all(|link| matches!(link_state(link, location), LinkState::Dead));
            if all_dead {
                out.push(folder_leftover(
                    LeftoverKind::StartMenu,
                    folder.clone(),
                    format!("Start Menu folder \"{name}\" with only broken shortcuts"),
                    progress,
                ));
                claimed.push(folder);
            }
        }

        // Individual shortcuts into the removed install folder.
        if location.is_none() {
            continue;
        }
        let mut links = Vec::new();
        let mut ignored = 0usize;
        collect_links(programs, &mut links, &mut ignored);
        for link in links {
            if claimed.iter().any(|folder| is_within(&link, folder)) {
                continue;
            }
            if matches!(link_state(&link, location), LinkState::Dead) {
                let size = std::fs::metadata(&link).map(|m| m.len()).unwrap_or(0);
                out.push(Leftover {
                    kind: LeftoverKind::StartMenu,
                    path: Some(link),
                    key: None,
                    size_bytes: size,
                    files: 1,
                    reason: "Shortcut to a file that no longer exists".to_owned(),
                });
            }
        }
    }
}

fn registry_keys(
    app: &InstalledApp,
    names: &[String],
    roots: &LeftoverRoots,
    out: &mut Vec<Leftover>,
) {
    let publisher = publisher_component(app);
    for base in &roots.software_keys {
        for name in names {
            let mut candidates = vec![(base.child(name), format!("Settings key named \"{name}\""))];
            if let Some(publisher) = publisher {
                candidates.push((
                    base.child(publisher).child(name),
                    format!("Settings key named \"{publisher}\\{name}\""),
                ));
            }
            for (key, reason) in candidates {
                if !key.exists() || is_protected_key(&key) {
                    continue;
                }
                if out.iter().any(|l| l.key.as_ref() == Some(&key)) {
                    continue;
                }
                out.push(Leftover {
                    kind: LeftoverKind::RegistryKey,
                    path: None,
                    key: Some(key),
                    size_bytes: 0,
                    files: 0,
                    reason,
                });
            }
        }
    }
}

/// The scan itself, against explicit roots and an explicit list of the
/// programs that are still installed.
pub(crate) fn find_in(
    app: &InstalledApp,
    roots: &LeftoverRoots,
    others: &[InstalledApp],
    progress: &Progress,
) -> Vec<Leftover> {
    let names = candidate_names(app, others);
    let mut found = Vec::new();
    progress.set_message(format!("Looking for leftovers of {}", app.name));
    if let Some(folder) = install_folder(app, others, progress) {
        found.push(folder);
    }
    let mut data = Vec::new();
    data_folders(app, &names, roots, &found, progress, &mut data);
    found.extend(data);
    start_menu(app, &names, roots, progress, &mut found);
    registry_keys(app, &names, roots, &mut found);
    found
}

/// Lists what a program left behind after it was uninstalled.
///
/// Returns an empty list while the program is still registered as installed
/// (so a cancelled uninstall can never lead to live data being offered for
/// removal). Matching is strict and exact; see the module documentation.
/// Nothing is changed.
pub fn find_leftovers(app: &InstalledApp, progress: &Progress) -> Vec<Leftover> {
    if is_still_installed(app) {
        return Vec::new();
    }
    let others: Vec<InstalledApp> = list_installed(&ListOptions::everything())
        .into_iter()
        .filter(|other| other.id != app.id)
        .collect();
    find_in(app, &real_roots(), &others, progress)
}

/// Removes leftovers chosen by the user.
///
/// Files and folders are deleted according to `options` (use
/// [`DeleteOptions::recycle`] to keep them recoverable) and pass through the
/// shared safety guard; registry keys are exported to `backup` first.
/// Failures never abort the batch.
pub fn remove_leftovers(
    items: &[Leftover],
    options: &DeleteOptions,
    mut backup: Option<&mut RegBackup>,
    progress: &Progress,
) -> LeftoverReport {
    let mut report = LeftoverReport::default();
    progress.set_total(items.len() as u64);
    for item in items {
        if progress.is_cancelled() {
            break;
        }
        progress.set_message(format!("Removing {}", item.location()));
        match (&item.path, &item.key) {
            (Some(path), _) => {
                let mut deleted = DeleteReport::default();
                zest_core::delete::delete_tree(path, false, options, Some(progress), &mut deleted);
                report.bytes_freed += deleted.bytes;
                report.files_deleted += deleted.files;
                if deleted.failed == 0 {
                    report.removed += 1;
                } else {
                    for (failed_path, reason) in deleted.failures {
                        report
                            .failures
                            .push((failed_path.display().to_string(), reason));
                    }
                }
            }
            (None, Some(key)) => {
                let result = if is_protected_key(key) {
                    Err(zest_core::Error::Other(format!(
                        "refused to delete protected registry key {key}"
                    )))
                } else if options.dry_run {
                    Ok(())
                } else {
                    zest_core::reg::delete_key(key, backup.as_deref_mut())
                };
                match result {
                    Ok(()) => report.removed += 1,
                    Err(e) => report.failures.push((key.to_string(), e.to_string())),
                }
            }
            (None, None) => {}
        }
        progress.inc(1);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::super::AppSource;
    use super::*;
    use crate::Scope;
    use crate::shortcut::{Shortcut, write_shortcut};
    use crate::test_support::Scratch;

    fn app(name: &str, publisher: Option<&str>, location: Option<PathBuf>) -> InstalledApp {
        InstalledApp {
            id: format!("win32|test|{name}"),
            source: AppSource::Registry,
            name: name.to_owned(),
            publisher: publisher.map(str::to_owned),
            version: None,
            install_date: None,
            install_date_estimated: false,
            size_bytes: None,
            install_location: location,
            uninstall_string: None,
            quiet_uninstall_string: None,
            modify_path: None,
            is_system_component: false,
            is_update: false,
            is_msi: false,
            product_code: None,
            icon_path: None,
            key: None,
            is_64bit: true,
            scope: Scope::User,
            requires_admin: false,
            can_uninstall: true,
            can_repair: false,
            package_full_name: None,
            package_family_name: None,
            about_url: None,
        }
    }

    struct Sandbox {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        roots: LeftoverRoots,
        _scratch: Scratch,
        software: RegPath,
    }

    fn sandbox(tag: &str) -> Sandbox {
        let tmp = tempfile::Builder::new()
            .prefix("zest-leftovers-")
            .tempdir()
            .unwrap();
        let root = tmp.path().to_path_buf();
        for dir in ["Roaming", "Local", "ProgramData", "StartMenu", "Programs"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let scratch = Scratch::new(tag);
        let software = scratch.child("Software");
        let roots = LeftoverRoots {
            data_dirs: vec![
                root.join("Roaming"),
                root.join("Local"),
                root.join("ProgramData"),
            ],
            start_menus: vec![root.join("StartMenu")],
            software_keys: vec![software.clone()],
        };
        Sandbox {
            _tmp: tmp,
            root,
            roots,
            _scratch: scratch,
            software,
        }
    }

    fn create_key(path: &RegPath) {
        RegRoot::Hkcu.key().create_subkey(&path.path).unwrap();
    }

    #[test]
    fn name_specificity() {
        for good in [
            "7-Zip",
            "Notepad++",
            "Example Studio",
            "VLC media player",
            "Acme",
        ] {
            assert!(is_specific_name(good), "{good}");
        }
        for bad in [
            "",
            "   ",
            "Git",
            "App",
            "Microsoft",
            "COMMON FILES",
            "Temp",
            "Data",
            "1234",
            "a\\b",
            "x:y",
            ".git",
            "name.",
            "Windows",
            "Program Files",
            "Steam",
            "Software",
        ] {
            assert!(!is_specific_name(bad), "{bad}");
        }
    }

    #[test]
    fn finds_exact_matches_only() {
        let sb = sandbox("leftovers-find");
        let install = sb.root.join("Programs").join("Example Studio");
        // The install folder survived with a log file in it.
        std::fs::create_dir_all(install.join("logs")).unwrap();
        std::fs::write(install.join("logs").join("old.log"), vec![0u8; 300]).unwrap();

        // Exact-name data folders (plain and below the publisher).
        std::fs::create_dir_all(sb.root.join("Roaming").join("Example Studio")).unwrap();
        std::fs::write(
            sb.root.join("Roaming/Example Studio/settings.json"),
            vec![0u8; 50],
        )
        .unwrap();
        std::fs::create_dir_all(sb.root.join("Local/Acme Software/Example Studio/cache")).unwrap();
        // Near misses that must NOT be reported.
        std::fs::create_dir_all(sb.root.join("Roaming").join("Example Studio 2")).unwrap();
        std::fs::create_dir_all(sb.root.join("Local").join("Example")).unwrap();
        std::fs::create_dir_all(sb.root.join("ProgramData").join("Acme Software")).unwrap();
        std::fs::create_dir_all(sb.root.join("ProgramData/Acme Software/Other Product")).unwrap();

        // Start Menu: a folder with only dead shortcuts, plus a stray dead
        // shortcut and an unrelated live one.
        let menu = sb.root.join("StartMenu");
        std::fs::create_dir_all(menu.join("Example Studio")).unwrap();
        let dead_target = install.join("studio.exe");
        for link in [
            menu.join("Example Studio").join("Example Studio.lnk"),
            menu.join("Example Studio").join("Uninstall.lnk"),
            menu.join("Studio Quick Start.lnk"),
        ] {
            write_shortcut(
                &link,
                &Shortcut {
                    target: Some(dead_target.clone()),
                    ..Shortcut::default()
                },
            )
            .unwrap();
        }
        write_shortcut(
            &menu.join("Command Prompt.lnk"),
            &Shortcut {
                target: Some(paths::system32_dir().join("cmd.exe")),
                ..Shortcut::default()
            },
        )
        .unwrap();

        // Registry: exact keys, a publisher-only key and a near miss.
        create_key(&sb.software.child("Example Studio"));
        create_key(&sb.software.child(r"Acme Software\Example Studio"));
        create_key(&sb.software.child(r"Acme Software\Other Product"));
        create_key(&sb.software.child("Example Studio Helper"));

        let target = app(
            "Example Studio",
            Some("Acme Software"),
            Some(install.clone()),
        );
        let found = find_in(&target, &sb.roots, &[], &Progress::new());
        let mut locations: Vec<String> = found
            .iter()
            .map(|l| {
                l.location()
                    .replace(&sb.root.display().to_string(), "")
                    .replace(&sb.software.to_string(), "REG")
            })
            .collect();
        locations.sort();
        assert_eq!(
            locations,
            [
                r"REG\Acme Software\Example Studio",
                r"REG\Example Studio",
                r"\Local\Acme Software\Example Studio",
                r"\Programs\Example Studio",
                r"\Roaming\Example Studio",
                r"\StartMenu\Example Studio",
                r"\StartMenu\Studio Quick Start.lnk",
            ],
            "{found:#?}"
        );
        let install_item = found
            .iter()
            .find(|l| l.kind == LeftoverKind::InstallFolder)
            .unwrap();
        assert_eq!((install_item.size_bytes, install_item.files), (300, 1));
        assert!(found.iter().all(|l| !l.reason.is_empty()));

        // Removing: files permanently (sandbox), keys with a backup.
        let mut backup = RegBackup::at(sb.root.join("leftovers.reg"));
        let report = remove_leftovers(
            &found,
            &DeleteOptions::permanent(),
            Some(&mut backup),
            &Progress::new(),
        );
        assert_eq!(report.removed, 7, "{report:?}");
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(report.bytes_freed >= 350);
        assert_eq!(backup.len(), 2);
        assert!(!install.exists());
        assert!(!sb.software.child("Example Studio").exists());
        // Everything that was not matched is untouched.
        assert!(sb.root.join("Roaming").join("Example Studio 2").is_dir());
        assert!(sb.root.join("Local").join("Example").is_dir());
        assert!(
            sb.root
                .join("ProgramData/Acme Software/Other Product")
                .is_dir()
        );
        assert!(menu.join("Command Prompt.lnk").is_file());
        assert!(sb.software.child(r"Acme Software\Other Product").exists());
        assert!(sb.software.child("Example Studio Helper").exists());
        assert!(find_in(&target, &sb.roots, &[], &Progress::new()).is_empty());
    }

    #[test]
    fn generic_and_shared_names_are_never_matched() {
        let sb = sandbox("leftovers-generic");
        // Generic install-folder name and generic display name.
        let install = sb.root.join("Programs").join("Tools");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(sb.root.join("Roaming").join("Tools")).unwrap();
        std::fs::create_dir_all(sb.root.join("Roaming").join("Update")).unwrap();
        create_key(&sb.software.child("Tools"));
        create_key(&sb.software.child("Update"));
        let generic = app("Update", Some("Microsoft"), Some(install));
        assert!(find_in(&generic, &sb.roots, &[], &Progress::new()).is_empty());

        // A specific name that another installed program also uses.
        let shared_dir = sb.root.join("Programs").join("Example Suite");
        std::fs::create_dir_all(&shared_dir).unwrap();
        std::fs::create_dir_all(sb.root.join("Roaming").join("Example Suite")).unwrap();
        create_key(&sb.software.child("Example Suite"));
        let removed = app("Example Suite", None, Some(shared_dir.clone()));
        let sibling = app(
            "Example Suite Extras",
            None,
            Some(shared_dir.join("Extras")),
        );
        let twin = app("Example Suite", None, None);

        // Another program lives inside the install folder: folder is kept,
        // but exact-name data is still this program's.
        let with_sibling = find_in(&removed, &sb.roots, &[sibling], &Progress::new());
        assert!(
            with_sibling
                .iter()
                .all(|l| l.kind != LeftoverKind::InstallFolder)
        );
        assert_eq!(with_sibling.len(), 2, "{with_sibling:#?}");

        // Another program registered under the very same name: nothing at all.
        let with_twin = find_in(&removed, &sb.roots, &[twin], &Progress::new());
        assert!(with_twin.is_empty(), "{with_twin:#?}");
    }

    #[test]
    fn protected_and_missing_locations_are_skipped() {
        let sb = sandbox("leftovers-protected");
        // An install location pointing at a protected system folder.
        let system = app("Example Driver Pack", None, Some(paths::system32_dir()));
        assert!(find_in(&system, &sb.roots, &[], &Progress::new()).is_empty());
        let program_files = app("Example Thing", None, Some(paths::program_files()));
        assert!(find_in(&program_files, &sb.roots, &[], &Progress::new()).is_empty());
        // An install location that no longer exists.
        let gone = app(
            "Example Gone",
            None,
            Some(sb.root.join("Programs").join("Example Gone")),
        );
        assert!(find_in(&gone, &sb.roots, &[], &Progress::new()).is_empty());
        // No install location at all: shortcuts cannot be attributed.
        write_shortcut(
            &sb.root.join("StartMenu").join("Example Gone.lnk"),
            &Shortcut {
                target: Some(sb.root.join("Programs").join("Example Gone").join("x.exe")),
                ..Shortcut::default()
            },
        )
        .unwrap();
        let unknown = app("Example Gone", None, None);
        assert!(find_in(&unknown, &sb.roots, &[], &Progress::new()).is_empty());
        // With the location known, the dead shortcut is attributed.
        let found = find_in(&gone, &sb.roots, &[], &Progress::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, LeftoverKind::StartMenu);
    }

    #[test]
    fn a_start_menu_folder_with_live_content_is_kept() {
        let sb = sandbox("leftovers-menu");
        let install = sb.root.join("Programs").join("Example Player");
        let menu_folder = sb.root.join("StartMenu").join("Example Player");
        std::fs::create_dir_all(&menu_folder).unwrap();
        write_shortcut(
            &menu_folder.join("Example Player.lnk"),
            &Shortcut {
                target: Some(install.join("player.exe")),
                ..Shortcut::default()
            },
        )
        .unwrap();
        // A shortcut that still works keeps the folder alive.
        write_shortcut(
            &menu_folder.join("Help.lnk"),
            &Shortcut {
                target: Some(paths::system32_dir().join("notepad.exe")),
                ..Shortcut::default()
            },
        )
        .unwrap();
        let target = app("Example Player", None, Some(install));
        let found = find_in(&target, &sb.roots, &[], &Progress::new());
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0]
                .path
                .as_ref()
                .unwrap()
                .ends_with("Example Player.lnk")
        );
    }

    #[test]
    fn installed_programs_have_no_leftovers() {
        // A real, installed program must never yield leftovers.
        let installed = list_installed(&ListOptions::default());
        if let Some(real) = installed.iter().find(|a| a.source == AppSource::Registry) {
            assert!(is_still_installed(real));
            assert!(find_leftovers(real, &Progress::new()).is_empty());
        }
    }

    #[test]
    fn removal_respects_cancellation_and_reports_failures() {
        let sb = sandbox("leftovers-remove");
        let folder = sb.root.join("Roaming").join("Example Thing");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), b"data").unwrap();
        let own_key = sb.software.child("Example Thing");
        create_key(&own_key);
        let sandbox_items = vec![
            Leftover {
                kind: LeftoverKind::DataFolder,
                path: Some(folder.clone()),
                key: None,
                size_bytes: 4,
                files: 1,
                reason: "test".to_owned(),
            },
            Leftover {
                kind: LeftoverKind::RegistryKey,
                path: None,
                key: Some(own_key.clone()),
                size_bytes: 0,
                files: 0,
                reason: "test".to_owned(),
            },
        ];
        // Guarded locations. These are only ever passed in dry-run mode, and
        // the registry key does not even exist, so nothing real is at stake.
        let mut guarded = sandbox_items.clone();
        guarded.push(Leftover {
            kind: LeftoverKind::RegistryKey,
            path: None,
            key: Some(RegPath::hkcu("ZestTestsNoSuchTopLevelKey")),
            size_bytes: 0,
            files: 0,
            reason: "test".to_owned(),
        });
        guarded.push(Leftover {
            kind: LeftoverKind::InstallFolder,
            path: Some(paths::windows_dir()),
            key: None,
            size_bytes: 0,
            files: 0,
            reason: "test".to_owned(),
        });

        let cancelled = Progress::new();
        cancelled.cancel();
        let report = remove_leftovers(
            &sandbox_items,
            &DeleteOptions::permanent(),
            None,
            &cancelled,
        );
        assert_eq!(report.removed, 0);
        assert!(folder.exists() && own_key.exists());

        // Dry run: the guards refuse what they must, nothing is touched.
        let dry = DeleteOptions::permanent().with_dry_run(true);
        let report = remove_leftovers(&guarded, &dry, None, &Progress::new());
        assert_eq!(report.removed, 2, "{report:?}");
        assert_eq!(report.failures.len(), 2, "{report:?}");
        assert!(folder.join("a.txt").exists() && own_key.exists());
        assert!(paths::windows_dir().is_dir());

        let report = remove_leftovers(
            &sandbox_items,
            &DeleteOptions::permanent(),
            None,
            &Progress::new(),
        );
        assert_eq!(report.removed, 2, "{report:?}");
        assert!(report.failures.is_empty(), "{report:?}");
        assert_eq!(report.files_deleted, 1);
        assert!(!folder.exists() && !own_key.exists());
    }
}
