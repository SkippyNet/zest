//! One scanner per [`Category`].
//!
//! Every scanner has a `scan` entry point that knows the real registry
//! locations, and one or more `scan_*` functions that work on an arbitrary
//! [`RegPath`]. The tests point the latter at scratch trees, so the detection
//! logic is exercised without reading or writing real system keys.
//!
//! Scanners only ever read. They report against the *physical* hives
//! (`HKLM\SOFTWARE\Classes`, `HKCU\Software\Classes`) instead of the merged
//! `HKCR` view, so every issue names exactly the key that will be backed up
//! and removed, and the administrator requirement is unambiguous.

use crate::check;
use crate::types::{Category, Condition, Issue};
use zest_core::Progress;
use zest_core::reg::{RegPath, RegRoot};

pub(crate) mod app_paths;
pub(crate) mod applications;
pub(crate) mod com_classes;
pub(crate) mod file_extensions;
pub(crate) mod firewall;
pub(crate) mod fonts;
pub(crate) mod help_files;
pub(crate) mod installer;
pub(crate) mod mui_cache;
pub(crate) mod obsolete_software;
pub(crate) mod services;
pub(crate) mod shared_dlls;
pub(crate) mod sound_events;
pub(crate) mod startup;
pub(crate) mod type_libs;

/// Runs the scanner for one category against the real registry.
pub(crate) fn run(category: Category, progress: &Progress) -> Vec<Issue> {
    let mut out = Vec::new();
    match category {
        Category::SharedDlls => shared_dlls::scan(progress, &mut out),
        Category::FileExtensions => file_extensions::scan(progress, &mut out),
        Category::ComClasses => com_classes::scan(progress, &mut out),
        Category::TypeLibraries => type_libs::scan(progress, &mut out),
        Category::Applications => applications::scan(progress, &mut out),
        Category::Fonts => fonts::scan(progress, &mut out),
        Category::AppPaths => app_paths::scan(progress, &mut out),
        Category::HelpFiles => help_files::scan(progress, &mut out),
        Category::Installer => installer::scan(progress, &mut out),
        Category::ObsoleteSoftware => obsolete_software::scan(progress, &mut out),
        Category::Startup => startup::scan(progress, &mut out),
        Category::MuiCache => mui_cache::scan(progress, &mut out),
        Category::SoundEvents => sound_events::scan(progress, &mut out),
        Category::Services => services::scan(progress, &mut out),
        Category::FirewallRules => firewall::scan(progress, &mut out),
    }
    out.retain(|issue| !judged_through_another_users_eyes(issue));
    out
}

/// True for machine-wide entries whose judged data refers to a per-user
/// environment variable (`%LOCALAPPDATA%`, `%USERPROFILE%`, ...).
///
/// Such an entry resolves to a different file for every account. That the
/// file is missing for the account running the scan does not make the entry
/// invalid for everyone else, so it is never reported.
pub(crate) fn judged_through_another_users_eyes(issue: &Issue) -> bool {
    if issue.key.root == RegRoot::Hkcu {
        return false;
    }
    let in_value_name = issue
        .value
        .as_deref()
        .is_some_and(check::mentions_per_user_variable);
    let in_data = issue.conditions.iter().any(|condition| match condition {
        Condition::ValueEquals { data, .. } => check::mentions_per_user_variable(data),
        _ => false,
    });
    in_value_name || in_data
}

/// The `Software` roots: machine-wide 64-bit, machine-wide 32-bit, per-user
/// 64-bit, per-user 32-bit.
pub(crate) fn software_roots() -> [RegPath; 4] {
    [
        RegPath::hklm("SOFTWARE"),
        RegPath::hklm(r"SOFTWARE\WOW6432Node"),
        RegPath::hkcu("Software"),
        RegPath::hkcu(r"Software\WOW6432Node"),
    ]
}

/// The physical class hives (64-bit view).
pub(crate) fn class_roots() -> [RegPath; 2] {
    [
        RegPath::hklm(r"SOFTWARE\Classes"),
        RegPath::hkcu(r"Software\Classes"),
    ]
}

/// The physical class hives including their 32-bit halves.
pub(crate) fn class_roots_with_wow() -> [RegPath; 4] {
    [
        RegPath::hklm(r"SOFTWARE\Classes"),
        RegPath::hklm(r"SOFTWARE\Classes\WOW6432Node"),
        RegPath::hkcu(r"Software\Classes"),
        RegPath::hkcu(r"Software\Classes\WOW6432Node"),
    ]
}

/// `sub_key` below each of `roots`, keeping every physical key only once.
///
/// Some keys are shared between the 32-bit and 64-bit registry views and
/// are reachable under both names; scanning both would report every entry
/// twice. The first spelling (the 64-bit one) wins.
pub(crate) fn distinct_children(
    roots: impl IntoIterator<Item = RegPath>,
    sub_key: &str,
) -> Vec<RegPath> {
    crate::physical::distinct_roots(roots.into_iter().map(|root| root.child(sub_key)))
}

/// True for names of the form `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`.
pub(crate) fn looks_like_guid(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && bytes[1..37].iter().enumerate().all(|(i, &b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Confidence;
    use std::path::PathBuf;

    #[test]
    fn machine_wide_entries_with_per_user_variables_are_not_judged() {
        let issue = |key: RegPath, data: &str| {
            Issue::for_value(
                Category::Startup,
                &key,
                "Updater",
                "t",
                "d",
                Confidence::Safe,
                vec![
                    Condition::ValueEquals {
                        key: key.clone(),
                        name: "Updater".to_owned(),
                        data: data.to_owned(),
                    },
                    Condition::PathMissing(PathBuf::from(r"C:\x\updater.exe")),
                ],
            )
        };
        let run = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
        let per_user = r#""%LOCALAPPDATA%\Vendor\updater.exe" /tray"#;
        let machine = r#""%ProgramFiles%\Vendor\updater.exe" /tray"#;
        assert!(judged_through_another_users_eyes(&issue(
            RegPath::hklm(run),
            per_user
        )));
        assert!(!judged_through_another_users_eyes(&issue(
            RegPath::hklm(run),
            machine
        )));
        // For the current user's own hive the variable is unambiguous.
        assert!(!judged_through_another_users_eyes(&issue(
            RegPath::hkcu(run),
            per_user
        )));

        // Value names that are paths (shared file counters, cached names).
        let named = Issue::for_value(
            Category::SharedDlls,
            &RegPath::hklm(r"SOFTWARE\Microsoft\Windows\CurrentVersion\SharedDLLs"),
            r"%USERPROFILE%\lib.dll",
            "t",
            "d",
            Confidence::Safe,
            vec![],
        );
        assert!(judged_through_another_users_eyes(&named));
    }

    #[test]
    fn guid_shape() {
        assert!(looks_like_guid("{00000000-0000-0000-C000-000000000046}"));
        assert!(looks_like_guid("{b5f8350b-0548-48b1-a6ee-88bd00b4a5e7}"));
        assert!(!looks_like_guid("00000000-0000-0000-C000-000000000046"));
        assert!(!looks_like_guid("{00000000-0000-0000-C000-00000000004}"));
        assert!(!looks_like_guid("{0000000000000-0000-C000-000000000046}"));
        assert!(!looks_like_guid("{zzzzzzzz-0000-0000-C000-000000000046}"));
        assert!(!looks_like_guid(""));
    }
}
