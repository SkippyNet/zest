//! Installed programs: listing, uninstalling, repairing and cleaning up.
//!
//! Two sources are merged into one [`InstalledApp`] list:
//!
//! * classic installers, read from the three `Uninstall` registry keys
//!   (64-bit machine-wide, 32-bit machine-wide, current user);
//! * Microsoft Store / MSIX packages installed for the current user.
//!
//! Uninstalling always runs the program's own uninstaller (or Windows
//! Installer / the package manager). Nothing is ever force-removed. The
//! separate [`find_leftovers`] scan reports what an uninstaller left behind,
//! using strict exact-name matching only.

mod leftovers;
mod store;
mod uninstall;

pub use leftovers::{Leftover, LeftoverKind, LeftoverReport, find_leftovers, remove_leftovers};
pub(crate) use store::{PackageDisplay, display_for_family as store_display};
pub use uninstall::{
    LaunchCommand, UninstallOutcome, is_still_installed, remove_entry, rename_entry, repair,
    repair_command, uninstall, uninstall_command,
};

use crate::Scope;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zest_core::reg::{RegPath, RegRoot};
use zest_core::{Progress, cmdline, paths, walk};

const UNINSTALL: &str = r"Microsoft\Windows\CurrentVersion\Uninstall";

/// Where an installed program was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppSource {
    /// A classic installer registered under an `Uninstall` registry key.
    Registry,
    /// A Microsoft Store / MSIX package.
    Store,
}

impl AppSource {
    /// Short label, e.g. `Store app`.
    pub fn label(self) -> &'static str {
        match self {
            AppSource::Registry => "Desktop app",
            AppSource::Store => "Store app",
        }
    }
}

/// One installed program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledApp {
    /// Stable identifier: `win32|<registry key>` or `store|<package full name>`.
    pub id: String,
    /// Classic installer or Store package.
    pub source: AppSource,
    /// Display name.
    pub name: String,
    /// Publisher, if declared.
    pub publisher: Option<String>,
    /// Version text, if declared.
    pub version: Option<String>,
    /// Installation date.
    pub install_date: Option<NaiveDate>,
    /// True when `install_date` was not declared by the installer and was
    /// taken from the time the registration was last written instead.
    pub install_date_estimated: bool,
    /// Size on disk as declared by the installer. `None` when unknown; see
    /// [`measure_install_size`].
    pub size_bytes: Option<u64>,
    /// Folder the program is installed in, if declared.
    pub install_location: Option<PathBuf>,
    /// Command that starts the uninstaller.
    pub uninstall_string: Option<String>,
    /// Command that uninstalls without asking questions, if the installer
    /// provides one.
    pub quiet_uninstall_string: Option<String>,
    /// Command that changes or repairs the installation.
    pub modify_path: Option<String>,
    /// True for components that Windows hides from its own list.
    pub is_system_component: bool,
    /// True for updates and patches that belong to another entry.
    pub is_update: bool,
    /// True when the program was installed with Windows Installer (MSI).
    pub is_msi: bool,
    /// The Windows Installer product code, e.g. `{GUID}`.
    pub product_code: Option<String>,
    /// Icon location as declared (may carry an index: `file.exe,0`).
    pub icon_path: Option<String>,
    /// The registry key that registers the program (classic installers only).
    pub key: Option<RegPath>,
    /// False for 32-bit programs.
    pub is_64bit: bool,
    /// Installed for the current user or for everyone.
    pub scope: Scope,
    /// True when removing or changing the registration needs administrator
    /// rights. (Uninstallers ask for elevation themselves when they need it.)
    pub requires_admin: bool,
    /// Whether [`uninstall`] can do anything for this entry.
    pub can_uninstall: bool,
    /// Whether [`repair`] can do anything for this entry.
    pub can_repair: bool,
    /// Package full name (Store packages only).
    pub package_full_name: Option<String>,
    /// Package family name (Store packages only).
    pub package_family_name: Option<String>,
    /// Product web page, if declared.
    pub about_url: Option<String>,
}

/// What [`list_installed`] should include.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListOptions {
    /// Include hidden system components and entries without a display name.
    pub include_system_components: bool,
    /// Include updates and patches that belong to another entry.
    pub include_updates: bool,
    /// Include Microsoft Store / MSIX packages.
    pub include_store_apps: bool,
}

impl Default for ListOptions {
    fn default() -> Self {
        ListOptions {
            include_system_components: false,
            include_updates: false,
            include_store_apps: true,
        }
    }
}

impl ListOptions {
    /// Everything, including what Windows hides.
    pub fn everything() -> Self {
        ListOptions {
            include_system_components: true,
            include_updates: true,
            include_store_apps: true,
        }
    }
}

/// One `Uninstall` key to read.
pub(crate) struct UninstallLocation {
    pub key: RegPath,
    pub scope: Scope,
    pub is_64bit: bool,
}

fn locations() -> Vec<UninstallLocation> {
    vec![
        UninstallLocation {
            key: RegPath::new(RegRoot::Hklm, format!(r"SOFTWARE\{UNINSTALL}")),
            scope: Scope::Machine,
            is_64bit: true,
        },
        UninstallLocation {
            key: RegPath::new(RegRoot::Hklm, format!(r"SOFTWARE\WOW6432Node\{UNINSTALL}")),
            scope: Scope::Machine,
            is_64bit: false,
        },
        UninstallLocation {
            key: RegPath::new(RegRoot::Hkcu, format!(r"Software\{UNINSTALL}")),
            scope: Scope::User,
            is_64bit: true,
        },
    ]
}

fn clean(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().trim_matches('\0').trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// Parses the `InstallDate` value (`YYYYMMDD`; a few installers write
/// `YYYY-MM-DD` or `YYYY/MM/DD`).
fn parse_install_date(raw: &str) -> Option<NaiveDate> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 8 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(
        digits[0..4].parse().ok()?,
        digits[4..6].parse().ok()?,
        digits[6..8].parse().ok()?,
    )?;
    use chrono::Datelike;
    (1990..=2100).contains(&date.year()).then_some(date)
}

/// True for text of the form `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`.
pub(crate) fn looks_like_guid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && bytes[1..37].iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Finds a product code inside a Windows Installer command line such as
/// `MsiExec.exe /X{GUID}`.
fn product_code_from_command(command: &str) -> Option<String> {
    let lower = command.to_lowercase();
    if !lower.trim_start_matches('"').starts_with("msiexec") && !lower.contains("\\msiexec") {
        return None;
    }
    let start = command.find('{')?;
    let candidate = command.get(start..start + 38)?;
    looks_like_guid(candidate).then(|| candidate.to_owned())
}

/// Reads the entries of one `Uninstall` key.
pub(crate) fn list_location(loc: &UninstallLocation, options: &ListOptions) -> Vec<InstalledApp> {
    let Ok(root) = loc.key.open() else {
        return Vec::new();
    };
    let mut apps = Vec::new();
    for sub in root.enum_keys().flatten() {
        let key_path = loc.key.child(&sub);
        let Ok(key) = key_path.open() else {
            continue;
        };
        let text = |name: &str| clean(key.get_value::<String, _>(name).ok());
        let number = |name: &str| key.get_value::<u32, _>(name).ok();

        let display_name = text("DisplayName");
        let is_system_component = number("SystemComponent") == Some(1) || display_name.is_none();
        let is_update = text("ParentKeyName").is_some()
            || text("ReleaseType").is_some_and(|t| {
                let t = t.to_lowercase();
                t.contains("update") || t.contains("hotfix") || t.contains("service pack")
            });
        if is_system_component && !options.include_system_components {
            continue;
        }
        if is_update && !options.include_updates {
            continue;
        }

        let uninstall_string = text("UninstallString");
        let windows_installer = number("WindowsInstaller") == Some(1);
        let product_code = if windows_installer && looks_like_guid(&sub) {
            Some(sub.clone())
        } else {
            uninstall_string
                .as_deref()
                .and_then(product_code_from_command)
        };
        let is_msi = windows_installer || product_code.is_some();
        let modify_path = text("ModifyPath");

        let declared_date = text("InstallDate").and_then(|raw| parse_install_date(&raw));
        let (install_date, install_date_estimated) = match declared_date {
            Some(date) => (Some(date), false),
            None => {
                let written = key.query_info().ok().and_then(|info| {
                    let st = info.get_last_write_time_system();
                    NaiveDate::from_ymd_opt(st.wYear.into(), st.wMonth.into(), st.wDay.into())
                });
                (written, written.is_some())
            }
        };

        let install_location = text("InstallLocation")
            .map(|raw| PathBuf::from(paths::expand_env(raw.trim_matches('"'))))
            .filter(|p| p.is_absolute());

        let no_remove = number("NoRemove") == Some(1);
        let can_uninstall =
            !no_remove && (uninstall_string.is_some() || (is_msi && product_code.is_some()));
        let can_repair = (is_msi && product_code.is_some() && number("NoRepair") != Some(1))
            || (!is_msi && modify_path.is_some() && number("NoModify") != Some(1));

        apps.push(InstalledApp {
            id: format!("win32|{key_path}"),
            source: AppSource::Registry,
            name: display_name.unwrap_or_else(|| sub.clone()),
            publisher: text("Publisher"),
            version: text("DisplayVersion"),
            install_date,
            install_date_estimated,
            size_bytes: number("EstimatedSize")
                .filter(|kb| *kb > 0)
                .map(|kb| u64::from(kb) * 1024),
            install_location,
            uninstall_string,
            quiet_uninstall_string: text("QuietUninstallString"),
            modify_path,
            is_system_component,
            is_update,
            is_msi,
            product_code,
            icon_path: text("DisplayIcon"),
            key: Some(key_path),
            is_64bit: loc.is_64bit,
            scope: loc.scope,
            requires_admin: loc.scope == Scope::Machine,
            can_uninstall,
            can_repair,
            package_full_name: None,
            package_family_name: None,
            about_url: text("URLInfoAbout").or_else(|| text("HelpLink")),
        });
    }
    apps
}

/// Lists classic (registry registered) programs only.
pub fn list_desktop_apps(options: &ListOptions) -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = locations()
        .iter()
        .flat_map(|loc| list_location(loc, options))
        .collect();
    sort_apps(&mut apps);
    apps
}

/// Lists Microsoft Store / MSIX packages installed for the current user.
/// Frameworks, resource packages and non-removable system packages are left
/// out. Returns an empty list when the package manager is unavailable.
pub fn list_store_apps() -> Vec<InstalledApp> {
    match store::list() {
        Ok(mut apps) => {
            sort_apps(&mut apps);
            apps
        }
        Err(e) => {
            log::warn!("Store packages could not be listed: {e}");
            Vec::new()
        }
    }
}

/// Lists installed programs, sorted by name.
///
/// Never fails: a source that cannot be read is logged and skipped.
pub fn list_installed(options: &ListOptions) -> Vec<InstalledApp> {
    let mut apps = list_desktop_apps(options);
    if options.include_store_apps {
        apps.extend(list_store_apps());
    }
    sort_apps(&mut apps);
    apps
}

fn sort_apps(apps: &mut [InstalledApp]) {
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Measures the install folder of a program whose installer did not declare
/// a size. Returns `None` when the program has no known, existing install
/// folder, or when `progress` was cancelled.
///
/// This walks the folder and can take a moment for large programs; call it
/// lazily (for example for the selected row only).
pub fn measure_install_size(app: &InstalledApp, progress: &Progress) -> Option<u64> {
    let location = app.install_location.as_ref()?;
    if !location.is_dir() {
        return None;
    }
    let size = walk::tree_size(location, Some(progress));
    (!progress.is_cancelled()).then_some(size.bytes)
}

/// The executable an entry's icon or uninstaller points at, when it can be
/// resolved – handy for front ends that want to show an icon.
pub fn icon_file(app: &InstalledApp) -> Option<PathBuf> {
    let from_icon = app
        .icon_path
        .as_deref()
        .and_then(|icon| match cmdline::check_path(icon) {
            cmdline::Target::Exists(path) => Some(path),
            _ => None,
        });
    from_icon.or_else(|| {
        let command = app.uninstall_string.as_deref()?;
        match cmdline::check_program(command) {
            cmdline::Target::Exists(path) => Some(path),
            _ => None,
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::Scratch;

    /// Creates a fake `Uninstall` tree inside a scratch key.
    pub(crate) fn fake_uninstall_root(scratch: &Scratch) -> UninstallLocation {
        UninstallLocation {
            key: scratch.child("Uninstall"),
            scope: Scope::User,
            is_64bit: true,
        }
    }

    pub(crate) fn add_entry(loc: &UninstallLocation, name: &str) -> winreg::RegKey {
        RegRoot::Hkcu
            .key()
            .create_subkey(&loc.key.child(name).path)
            .unwrap()
            .0
    }

    #[test]
    fn install_dates() {
        assert_eq!(
            parse_install_date("20240131"),
            NaiveDate::from_ymd_opt(2024, 1, 31)
        );
        assert_eq!(
            parse_install_date("2024-01-31"),
            NaiveDate::from_ymd_opt(2024, 1, 31)
        );
        assert_eq!(parse_install_date("20241331"), None);
        assert_eq!(parse_install_date("1/31/2024"), None);
        assert_eq!(parse_install_date(""), None);
        assert_eq!(parse_install_date("00000000"), None);
        assert_eq!(parse_install_date("16010101"), None);
    }

    #[test]
    fn guid_and_product_code_detection() {
        assert!(looks_like_guid("{90160000-008C-0000-1000-0000000FF1CE}"));
        assert!(!looks_like_guid("90160000-008C-0000-1000-0000000FF1CE"));
        assert!(!looks_like_guid("{not-a-guid}"));
        assert_eq!(
            product_code_from_command("MsiExec.exe /X{90160000-008C-0000-1000-0000000FF1CE}")
                .as_deref(),
            Some("{90160000-008C-0000-1000-0000000FF1CE}")
        );
        assert_eq!(
            product_code_from_command(
                r#""C:\Windows\System32\msiexec.exe" /I {90160000-008C-0000-1000-0000000FF1CE} /qb"#
            )
            .as_deref(),
            Some("{90160000-008C-0000-1000-0000000FF1CE}")
        );
        assert_eq!(product_code_from_command(r"C:\App\uninstall.exe {x}"), None);
        assert_eq!(product_code_from_command("MsiExec.exe /X{broken"), None);
    }

    #[test]
    fn reads_a_fake_uninstall_tree() {
        let scratch = Scratch::new("apps-list");
        let loc = fake_uninstall_root(&scratch);

        let plain = add_entry(&loc, "PlainApp");
        plain.set_value("DisplayName", &"Plain App").unwrap();
        plain.set_value("DisplayVersion", &" 1.2.3 ").unwrap();
        plain.set_value("Publisher", &"Example Software").unwrap();
        plain.set_value("InstallDate", &"20250314").unwrap();
        plain.set_value("EstimatedSize", &2048u32).unwrap();
        plain
            .set_value("InstallLocation", &r#""C:\Program Files\Plain App\""#)
            .unwrap();
        plain
            .set_value(
                "UninstallString",
                &r#""C:\Program Files\Plain App\unins000.exe""#,
            )
            .unwrap();
        plain
            .set_value(
                "QuietUninstallString",
                &r#""C:\Program Files\Plain App\unins000.exe" /SILENT"#,
            )
            .unwrap();
        plain
            .set_value("ModifyPath", &r"C:\x\setup.exe /modify")
            .unwrap();

        let msi = add_entry(&loc, "{11111111-2222-3333-4444-555555555555}");
        msi.set_value("DisplayName", &"Msi App").unwrap();
        msi.set_value("WindowsInstaller", &1u32).unwrap();
        msi.set_value(
            "UninstallString",
            &"MsiExec.exe /I{11111111-2222-3333-4444-555555555555}",
        )
        .unwrap();

        let hidden = add_entry(&loc, "HiddenComponent");
        hidden
            .set_value("DisplayName", &"Runtime Component")
            .unwrap();
        hidden.set_value("SystemComponent", &1u32).unwrap();

        add_entry(&loc, "Nameless")
            .set_value("UninstallString", &"x.exe")
            .unwrap();

        let update = add_entry(&loc, "KB123");
        update
            .set_value("DisplayName", &"Update for Plain App")
            .unwrap();
        update.set_value("ParentKeyName", &"PlainApp").unwrap();

        let locked = add_entry(&loc, "Locked");
        locked.set_value("DisplayName", &"Locked App").unwrap();
        locked.set_value("UninstallString", &"x.exe").unwrap();
        locked.set_value("NoRemove", &1u32).unwrap();

        let mut apps = list_location(&loc, &ListOptions::default());
        sort_apps(&mut apps);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Locked App", "Msi App", "Plain App"]);

        let plain = &apps[2];
        assert_eq!(plain.version.as_deref(), Some("1.2.3"));
        assert_eq!(plain.publisher.as_deref(), Some("Example Software"));
        assert_eq!(plain.install_date, NaiveDate::from_ymd_opt(2025, 3, 14));
        assert!(!plain.install_date_estimated);
        assert_eq!(plain.size_bytes, Some(2048 * 1024));
        assert_eq!(
            plain.install_location.as_deref(),
            Some(std::path::Path::new(r"C:\Program Files\Plain App\"))
        );
        assert!(plain.can_uninstall && plain.can_repair && !plain.is_msi);
        assert!(!plain.requires_admin);
        assert!(plain.id.starts_with("win32|HKCU\\"));
        assert_eq!(plain.source, AppSource::Registry);

        let msi = &apps[1];
        assert!(msi.is_msi && msi.can_uninstall && msi.can_repair);
        assert_eq!(
            msi.product_code.as_deref(),
            Some("{11111111-2222-3333-4444-555555555555}")
        );
        // No InstallDate declared: estimated from the key's write time.
        assert!(msi.install_date.is_some() && msi.install_date_estimated);
        assert_eq!(msi.size_bytes, None);

        assert!(!apps[0].can_uninstall, "NoRemove must be honoured");

        let all = list_location(&loc, &ListOptions::everything());
        assert_eq!(all.len(), 6);
        let nameless = all.iter().find(|a| a.name == "Nameless").unwrap();
        assert!(nameless.is_system_component);
        assert!(
            all.iter()
                .find(|a| a.name == "Update for Plain App")
                .unwrap()
                .is_update
        );

        let json = serde_json::to_string(&all).unwrap();
        let back: Vec<InstalledApp> = serde_json::from_str(&json).unwrap();
        assert_eq!(all, back);
    }

    #[test]
    fn real_listing_is_sane() {
        let apps = list_desktop_apps(&ListOptions::default());
        assert!(!apps.is_empty(), "every PC has some installed programs");
        let mut ids = std::collections::HashSet::new();
        for app in &apps {
            assert!(ids.insert(app.id.clone()), "duplicate id {}", app.id);
            assert!(!app.name.trim().is_empty());
            assert!(!app.is_system_component && !app.is_update);
            assert!(app.key.is_some());
        }
        let everything = list_desktop_apps(&ListOptions::everything());
        assert!(everything.len() >= apps.len());
        // Sorted case-insensitively.
        assert!(
            apps.windows(2)
                .all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase())
        );
    }

    #[test]
    fn size_measurement() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("a.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(tmp.path().join("sub/b.bin"), vec![0u8; 500]).unwrap();
        let scratch = Scratch::new("apps-size");
        let loc = fake_uninstall_root(&scratch);
        let key = add_entry(&loc, "Sized");
        key.set_value("DisplayName", &"Sized").unwrap();
        key.set_value("InstallLocation", &tmp.path().display().to_string())
            .unwrap();
        let app = list_location(&loc, &ListOptions::default()).remove(0);
        assert_eq!(measure_install_size(&app, &Progress::new()), Some(1500));

        let cancelled = Progress::new();
        cancelled.cancel();
        assert_eq!(measure_install_size(&app, &cancelled), None);

        let mut gone = app.clone();
        gone.install_location = Some(tmp.path().join("missing"));
        assert_eq!(measure_install_size(&gone, &Progress::new()), None);
        gone.install_location = None;
        assert_eq!(measure_install_size(&gone, &Progress::new()), None);
    }
}
