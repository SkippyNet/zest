//! Microsoft Store / MSIX packages, through the Windows package manager.

use super::{AppSource, InstalledApp};
use crate::Scope;
use crate::com::ComGuard;
use crate::util::win_err;
use chrono::NaiveDate;
use rayon::prelude::*;
use std::path::PathBuf;
use windows::ApplicationModel::{Package, PackageSignatureKind};
use windows::Management::Deployment::PackageManager;
use windows::System::ProcessorArchitecture;
use windows::core::HSTRING;
use zest_core::{Error, Result};

/// Converts WinRT `DateTime` ticks (100 ns since 1601, UTC) to a local date.
fn ticks_to_local_date(ticks: i64) -> Option<NaiveDate> {
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    if ticks <= UNIX_EPOCH_TICKS {
        return None;
    }
    let secs = (ticks - UNIX_EPOCH_TICKS) / 10_000_000;
    let utc = chrono::DateTime::from_timestamp(secs, 0)?;
    Some(utc.with_timezone(&chrono::Local).date_naive())
}

/// Display strings that failed to resolve come back empty or as a raw
/// resource reference; neither is fit to show.
fn usable(text: windows::core::Result<HSTRING>) -> Option<String> {
    let text = text.ok()?.to_string();
    let text = text.trim();
    (!text.is_empty() && !text.starts_with("ms-resource:")).then(|| text.to_owned())
}

/// Extracts the `CN=` part of a publisher distinguished name.
fn publisher_common_name(distinguished: &str) -> Option<String> {
    let start = distinguished.find("CN=")? + 3;
    let rest = &distinguished[start..];
    let name = if let Some(quoted) = rest.strip_prefix('"') {
        quoted.split('"').next().unwrap_or("")
    } else {
        rest.split(',').next().unwrap_or("")
    };
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn describe(package: &Package) -> Option<InstalledApp> {
    // Frameworks, resource packs and Windows' own system packages are not
    // things a user installs or can remove.
    if package.IsFramework().unwrap_or(false)
        || package.IsResourcePackage().unwrap_or(false)
        || package.SignatureKind().ok() == Some(PackageSignatureKind::System)
    {
        return None;
    }
    let id = package.Id().ok()?;
    let full_name = id.FullName().ok()?.to_string();
    let family_name = id.FamilyName().ok().map(|f| f.to_string());
    let identity_name = id.Name().ok()?.to_string();

    let name = usable(package.DisplayName()).unwrap_or_else(|| identity_name.clone());
    let publisher = usable(package.PublisherDisplayName()).or_else(|| {
        id.Publisher()
            .ok()
            .and_then(|dn| publisher_common_name(&dn.to_string()))
    });
    let version = id
        .Version()
        .ok()
        .map(|v| format!("{}.{}.{}.{}", v.Major, v.Minor, v.Build, v.Revision));
    let install_location = package
        .InstalledPath()
        .ok()
        .map(|p| PathBuf::from(p.to_string()))
        .filter(|p| p.is_absolute());
    let is_64bit = !matches!(
        id.Architecture().ok(),
        Some(ProcessorArchitecture::X86) | Some(ProcessorArchitecture::Arm)
    );
    let icon_path = package
        .Logo()
        .ok()
        .and_then(|uri| uri.RawUri().ok())
        .map(|raw| raw.to_string())
        .filter(|raw| !raw.is_empty());

    Some(InstalledApp {
        id: format!("store|{full_name}"),
        source: AppSource::Store,
        name,
        publisher,
        version,
        install_date: package
            .InstalledDate()
            .ok()
            .and_then(|date| ticks_to_local_date(date.UniversalTime)),
        install_date_estimated: false,
        size_bytes: None,
        install_location,
        uninstall_string: None,
        quiet_uninstall_string: None,
        modify_path: None,
        is_system_component: false,
        is_update: false,
        is_msi: false,
        product_code: None,
        icon_path,
        key: None,
        is_64bit,
        scope: Scope::User,
        requires_admin: false,
        can_uninstall: true,
        can_repair: false,
        package_full_name: Some(full_name),
        package_family_name: family_name,
        about_url: None,
    })
}

/// Display details of a package, looked up from its family name.
#[derive(Clone, Debug, Default)]
pub(crate) struct PackageDisplay {
    /// Friendly name, when it resolves.
    pub name: Option<String>,
    /// Publisher display name.
    pub publisher: Option<String>,
    /// Folder the package is installed in.
    pub location: Option<PathBuf>,
}

/// Looks up how a package family presents itself to the user. Returns empty
/// details when the family is not installed or the lookup fails.
pub(crate) fn display_for_family(family: &str) -> PackageDisplay {
    let _com = ComGuard::new();
    let Ok(manager) = PackageManager::new() else {
        return PackageDisplay::default();
    };
    let Ok(packages) = manager
        .FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(family))
    else {
        return PackageDisplay::default();
    };
    for package in packages {
        if package.IsResourcePackage().unwrap_or(false) {
            continue;
        }
        let publisher = usable(package.PublisherDisplayName()).or_else(|| {
            package
                .Id()
                .and_then(|id| id.Publisher())
                .ok()
                .and_then(|dn| publisher_common_name(&dn.to_string()))
        });
        return PackageDisplay {
            name: usable(package.DisplayName()),
            publisher,
            location: package
                .InstalledPath()
                .ok()
                .map(|p| PathBuf::from(p.to_string()))
                .filter(|p| p.is_absolute()),
        };
    }
    PackageDisplay::default()
}

/// Lists the removable packages installed for the current user.
pub(crate) fn list() -> Result<Vec<InstalledApp>> {
    let _com = ComGuard::new();
    let action = "list Store apps";
    let manager = PackageManager::new().map_err(|e| win_err(e, action))?;
    // An empty security identifier selects the current user and, unlike
    // listing for all users, needs no administrator rights.
    let packages: Vec<Package> = manager
        .FindPackagesByUserSecurityId(&HSTRING::new())
        .map_err(|e| win_err(e, action))?
        .into_iter()
        .collect();
    // Resolving display names loads each package's resources, which is the
    // slow part; the package objects are free-threaded, so do it in parallel.
    Ok(packages
        .par_iter()
        .filter_map(|package| {
            let _com = ComGuard::new();
            describe(package)
        })
        .collect())
}

/// Removes a package for the current user and waits for it to finish.
pub(crate) fn remove(package_full_name: &str) -> Result<()> {
    if package_full_name.trim().is_empty() {
        return Err(Error::Invalid("empty package name".to_owned()));
    }
    let _com = ComGuard::new();
    let action = format!("remove the app package {package_full_name}");
    let manager = PackageManager::new().map_err(|e| win_err(e, &action))?;
    // Never hand the package manager a name that is not installed.
    let installed = manager
        .FindPackageByUserSecurityIdPackageFullName(
            &HSTRING::new(),
            &HSTRING::from(package_full_name),
        )
        .is_ok_and(|package| package.Id().is_ok());
    if !installed {
        return Err(Error::NotFound(format!("app package {package_full_name}")));
    }
    let result = manager
        .RemovePackageAsync(&HSTRING::from(package_full_name))
        .map_err(|e| win_err(e, &action))?
        .join()
        .map_err(|e| win_err(e, &action))?;
    let code = result
        .ExtendedErrorCode()
        .map_err(|e| win_err(e, &action))?;
    if code.is_ok() {
        return Ok(());
    }
    let detail = result
        .ErrorText()
        .map(|t| t.to_string())
        .unwrap_or_default();
    Err(Error::Windows(format!(
        "could not {action}: {} (0x{:08X})",
        if detail.trim().is_empty() {
            "the package manager reported an error"
        } else {
            detail.trim()
        },
        code.0 as u32
    )))
}

/// True when the package is still installed for the current user.
pub(crate) fn is_installed(package_full_name: &str) -> bool {
    let _com = ComGuard::new();
    let Ok(manager) = PackageManager::new() else {
        return true;
    };
    manager
        .FindPackageByUserSecurityIdPackageFullName(
            &HSTRING::new(),
            &HSTRING::from(package_full_name),
        )
        .is_ok_and(|package| package.Id().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_conversion() {
        assert_eq!(ticks_to_local_date(0), None);
        // 2024-06-15 12:00:00 UTC
        let ticks = 116_444_736_000_000_000 + 1_718_452_800i64 * 10_000_000;
        let date = ticks_to_local_date(ticks).unwrap();
        // Local time zones shift the date by at most one day.
        assert!(
            (date - NaiveDate::from_ymd_opt(2024, 6, 15).unwrap())
                .num_days()
                .abs()
                <= 1
        );
    }

    #[test]
    fn publisher_names() {
        assert_eq!(
            publisher_common_name("CN=Example Corp, O=Example Corp, L=City, C=US").as_deref(),
            Some("Example Corp")
        );
        assert_eq!(
            publisher_common_name(r#"CN="Example, Inc.", O="Example, Inc.""#).as_deref(),
            Some("Example, Inc.")
        );
        assert_eq!(publisher_common_name("O=NoCommonName"), None);
        assert_eq!(publisher_common_name(""), None);
    }

    #[test]
    fn unusable_display_strings_are_rejected() {
        assert_eq!(
            usable(Ok(HSTRING::from("  Photos "))).as_deref(),
            Some("Photos")
        );
        assert_eq!(usable(Ok(HSTRING::from("ms-resource:AppName"))), None);
        assert_eq!(usable(Ok(HSTRING::new())), None);
    }

    #[test]
    fn real_listing_is_sane() {
        let apps = list().expect("the package manager should be available");
        let mut ids = std::collections::HashSet::new();
        for app in &apps {
            assert_eq!(app.source, AppSource::Store);
            assert!(app.id.starts_with("store|"));
            assert!(ids.insert(app.id.clone()), "duplicate {}", app.id);
            assert!(!app.name.trim().is_empty());
            assert!(app.package_full_name.is_some());
            assert!(app.can_uninstall && !app.requires_admin);
        }
    }

    #[test]
    fn unknown_package_is_not_installed_and_nothing_is_removed() {
        let ghost = "Zest.Tests.NoSuchPackage_1.0.0.0_x64__0000000000000";
        assert!(!is_installed(ghost));
        assert!(matches!(remove("  "), Err(Error::Invalid(_))));
        // A package that is not installed is rejected before the package
        // manager is asked to remove anything.
        assert!(matches!(remove(ghost), Err(Error::NotFound(_))));
    }
}
