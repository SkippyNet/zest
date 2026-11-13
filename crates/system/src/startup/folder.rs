//! Startup entries stored as files in the Startup folders.

use super::{FolderEntry, StartupDetails, StartupItem, StartupKind, approved, resolve_command};
use crate::Scope;
use crate::shortcut::read_shortcut;
use crate::util::{InfoCache, is_microsoft};
use std::path::{Path, PathBuf};
use zest_core::delete::DeleteOptions;
use zest_core::reg::{RegBackup, RegPath, RegRoot};
use zest_core::{Result, paths};

const USER_SHELL_FOLDERS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders";
const DEFAULT_RELATIVE: &str = r"Microsoft\Windows\Start Menu\Programs\Startup";

/// One Startup folder.
pub(crate) struct FolderLocation {
    /// The folder itself.
    pub dir: PathBuf,
    /// Where the enabled flags for this folder live.
    pub approved: Option<RegPath>,
    /// User or machine.
    pub scope: Scope,
}

fn shell_folder(root: RegRoot, value: &str, fallback: PathBuf) -> PathBuf {
    RegPath::new(root, USER_SHELL_FOLDERS)
        .read_string(value)
        .map(|raw| PathBuf::from(paths::expand_env(&raw)))
        .filter(|p| p.is_absolute())
        .unwrap_or(fallback)
}

/// The per-user and the all-users Startup folder.
fn locations() -> Vec<FolderLocation> {
    let user = paths::UserDirs::current();
    vec![
        FolderLocation {
            dir: shell_folder(
                RegRoot::Hkcu,
                "Startup",
                user.roaming.join(DEFAULT_RELATIVE),
            ),
            approved: Some(approved::approved_key(RegRoot::Hkcu, "StartupFolder")),
            scope: Scope::User,
        },
        FolderLocation {
            dir: shell_folder(
                RegRoot::Hklm,
                "Common Startup",
                paths::program_data().join(DEFAULT_RELATIVE),
            ),
            approved: Some(approved::approved_key(RegRoot::Hklm, "StartupFolder")),
            scope: Scope::Machine,
        },
    ]
}

/// The name under which Windows files the enabled flag of a Startup-folder
/// entry: its file name including the extension.
pub(crate) fn approved_name(file: &Path) -> String {
    file.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Lists the entries of both Startup folders.
pub(crate) fn list_all(cache: &mut InfoCache) -> Vec<StartupItem> {
    locations()
        .iter()
        .flat_map(|loc| list_folder(loc, cache))
        .collect()
}

/// Lists the entries of one folder. A missing folder yields nothing.
pub(crate) fn list_folder(loc: &FolderLocation, cache: &mut InfoCache) -> Vec<StartupItem> {
    let Ok(entries) = std::fs::read_dir(&loc.dir) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let file = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let file_name = approved_name(&file);
        if meta.is_dir() || file_name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        items.push(make_item(loc, file, file_name, cache));
    }
    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    items
}

fn make_item(
    loc: &FolderLocation,
    file: PathBuf,
    file_name: String,
    cache: &mut InfoCache,
) -> StartupItem {
    let is_link = file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"));
    let (command, arguments) = if is_link {
        match read_shortcut(&file) {
            Ok(shortcut) => (shortcut.command_line(), shortcut.arguments),
            Err(e) => {
                log::debug!("shortcut {} could not be read: {e}", file.display());
                (String::new(), String::new())
            }
        }
    } else {
        (format!("\"{}\"", file.display()), String::new())
    };
    let resolved = resolve_command(&command, cache);
    let enabled = loc
        .approved
        .as_ref()
        .is_none_or(|key| approved::is_enabled(key, &file_name));
    let machine = loc.scope == Scope::Machine;
    let name = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.clone());
    StartupItem {
        id: format!("folder|{}", file.display()),
        kind: StartupKind::StartupFolder,
        name,
        command,
        target: resolved.target,
        target_missing: resolved.target_missing,
        is_microsoft: is_microsoft(resolved.publisher.as_deref()),
        publisher: resolved.publisher,
        description: resolved.description,
        location: loc.dir.display().to_string(),
        scope: loc.scope,
        enabled,
        requires_admin: machine,
        toggle_requires_admin: machine,
        can_toggle: loc.approved.is_some(),
        can_delete: true,
        details: StartupDetails::Folder(FolderEntry {
            file,
            arguments,
            approved_key: loc.approved.clone(),
        }),
    }
}

/// Moves the file to the Recycle Bin and drops its enabled flag.
pub(crate) fn delete(entry: &FolderEntry, backup: Option<&mut RegBackup>) -> Result<()> {
    delete_with(entry, backup, &DeleteOptions::recycle())
}

/// [`delete`] with an explicit deletion mode (tests delete permanently so
/// they do not leave anything in the user's Recycle Bin).
pub(crate) fn delete_with(
    entry: &FolderEntry,
    backup: Option<&mut RegBackup>,
    options: &DeleteOptions,
) -> Result<()> {
    zest_core::delete::delete_file(&entry.file, options).map_err(|e| {
        if e.is_access_denied() {
            zest_core::Error::NeedsElevation(format!(
                "remove the startup file {}",
                entry.file.display()
            ))
        } else {
            e
        }
    })?;
    if let Some(approved_key) = &entry.approved_key {
        approved::forget(approved_key, &approved_name(&entry.file), backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut::{Shortcut, write_shortcut};
    use crate::test_support::Scratch;

    #[test]
    fn real_folders_are_absolute_and_enumerable() {
        let all = locations();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|l| l.dir.is_absolute()));
        assert!(
            all[0]
                .dir
                .to_string_lossy()
                .to_lowercase()
                .ends_with("startup")
        );
        for item in list_all(&mut InfoCache::default()) {
            assert_eq!(item.kind, StartupKind::StartupFolder);
            assert!(item.id.starts_with("folder|"));
        }
    }

    #[test]
    fn lists_toggles_and_deletes_sandbox_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Startup");
        std::fs::create_dir_all(dir.join("A Sub Folder")).unwrap();
        std::fs::write(dir.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        std::fs::write(dir.join("script.cmd"), b"@echo off").unwrap();

        let cmd = paths::system32_dir().join("cmd.exe");
        write_shortcut(
            &dir.join("Good Link.lnk"),
            &Shortcut {
                target: Some(cmd.clone()),
                arguments: "/c rem".to_owned(),
                ..Shortcut::default()
            },
        )
        .unwrap();
        let gone = tmp.path().join("Removed App").join("app.exe");
        write_shortcut(
            &dir.join("Orphan.lnk"),
            &Shortcut {
                target: Some(gone),
                ..Shortcut::default()
            },
        )
        .unwrap();

        let scratch = Scratch::new("folder");
        let loc = FolderLocation {
            dir: dir.clone(),
            approved: Some(scratch.child("StartupFolder")),
            scope: Scope::User,
        };
        let mut cache = InfoCache::default();
        let items = list_folder(&loc, &mut cache);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Good Link", "Orphan", "script"]);

        let good = &items[0];
        assert!(good.enabled && !good.target_missing && good.is_microsoft);
        assert!(good.command.ends_with("cmd.exe /c rem"), "{}", good.command);
        let StartupDetails::Folder(good_entry) = &good.details else {
            panic!("wrong details");
        };
        assert_eq!(good_entry.arguments, "/c rem");

        let orphan = &items[1];
        // The temp folder is on a local fixed drive, so the target is
        // positively known to be gone.
        assert!(orphan.target_missing, "{orphan:?}");

        let script = &items[2];
        assert!(!script.target_missing);
        assert!(paths::same_path(
            script.target.as_deref().unwrap(),
            &dir.join("script.cmd")
        ));

        // Toggle through the public entry point; the flag is filed under the
        // full file name, like Windows does.
        super::super::set_enabled(orphan, false).unwrap();
        assert!(!approved::is_enabled(
            loc.approved.as_ref().unwrap(),
            "Orphan.lnk"
        ));
        let items = list_folder(&loc, &mut cache);
        assert!(!items[1].enabled);
        assert!(items[0].enabled && items[2].enabled);

        // Delete (permanently here, to keep the real Recycle Bin untouched).
        let StartupDetails::Folder(orphan_entry) = &orphan.details else {
            panic!("wrong details");
        };
        let mut backup = RegBackup::at(tmp.path().join("flags.reg"));
        delete_with(orphan_entry, Some(&mut backup), &DeleteOptions::permanent()).unwrap();
        assert!(!dir.join("Orphan.lnk").exists());
        assert_eq!(backup.len(), 1);
        assert_eq!(list_folder(&loc, &mut cache).len(), 2);
        // Deleting again is harmless.
        delete_with(orphan_entry, None, &DeleteOptions::permanent()).unwrap();
    }

    #[test]
    fn missing_folder_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = FolderLocation {
            dir: tmp.path().join("nope"),
            approved: None,
            scope: Scope::User,
        };
        assert!(list_folder(&loc, &mut InfoCache::default()).is_empty());
    }

    #[test]
    fn flag_name_is_the_file_name() {
        assert_eq!(
            approved_name(Path::new(r"C:\x\Startup\Tool.lnk")),
            "Tool.lnk"
        );
        assert_eq!(approved_name(Path::new(r"C:\x\Startup\run.vbs")), "run.vbs");
    }
}
