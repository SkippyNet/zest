//! Obsolete software.
//!
//! Two kinds of leftovers:
//!
//! * **Uninstall entries** whose uninstaller is confirmed missing, and whose
//!   install location (when one is recorded) is confirmed missing too. If
//!   any recorded evidence – install location, display icon – still exists,
//!   or the uninstaller cannot be judged (installer-service packages, script
//!   hosts), the entry is left alone.
//! * **Empty vendor keys** directly below `HKCU\Software`: no values and no
//!   sub-keys.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::cmdline::{self, Target};
use zest_core::reg::{self, RegPath};

const UNINSTALL_KEY: &str = r"Microsoft\Windows\CurrentVersion\Uninstall";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for uninstall in super::distinct_children(super::software_roots(), UNINSTALL_KEY) {
        scan_uninstall(&uninstall, progress, out);
    }
    scan_empty_vendor_keys(&RegPath::hkcu("Software"), progress, out);
}

/// Scans the entries directly below an `Uninstall`-shaped key.
pub(crate) fn scan_uninstall(uninstall: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = uninstall.open() else {
        return;
    };
    for name in root.enum_keys().flatten() {
        if progress.is_cancelled() {
            return;
        }
        let Ok(entry) = root.open_subkey_with_flags(&name, KEY_READ) else {
            continue;
        };
        // Installer-service packages and hidden system components are
        // managed elsewhere; their entries are not ours to judge.
        if check::dword_value(&entry, "WindowsInstaller") == Some(1)
            || check::dword_value(&entry, "SystemComponent") == Some(1)
        {
            continue;
        }
        let Some(uninstall_raw) = check::raw_string_value(&entry, "UninstallString") else {
            continue;
        };
        let Some(uninstaller) = check::missing_command(uninstall_raw.trim()) else {
            continue;
        };
        let entry_path = uninstall.child(&name);
        let mut conditions = vec![
            Condition::ValueEquals {
                key: entry_path.clone(),
                name: "UninstallString".to_owned(),
                data: uninstall_raw,
            },
            Condition::PathMissing(uninstaller.clone()),
        ];

        // The quiet uninstaller, when present, must be just as dead.
        if let Some(quiet_raw) = check::raw_string_value(&entry, "QuietUninstallString")
            && !quiet_raw.trim().is_empty()
        {
            match check::missing_command(quiet_raw.trim()) {
                Some(path) => {
                    conditions.push(Condition::ValueEquals {
                        key: entry_path.clone(),
                        name: "QuietUninstallString".to_owned(),
                        data: quiet_raw,
                    });
                    conditions.push(Condition::PathMissing(path));
                }
                None => continue,
            }
        }

        // Install location: when recorded it must be confirmed missing.
        let mut confidence = Confidence::Review;
        if let Some(location_raw) = check::raw_string_value(&entry, "InstallLocation")
            && !location_raw.trim().trim_matches('"').trim().is_empty()
        {
            match check::missing_path(location_raw.trim()) {
                Some(path) => {
                    conditions.push(Condition::ValueEquals {
                        key: entry_path.clone(),
                        name: "InstallLocation".to_owned(),
                        data: location_raw,
                    });
                    conditions.push(Condition::PathMissing(path));
                    confidence = Confidence::Safe;
                }
                None => continue,
            }
        }

        // A display icon that still exists means something is still installed.
        if let Some(icon_raw) = check::raw_string_value(&entry, "DisplayIcon")
            && !icon_raw.trim().is_empty()
        {
            match cmdline::check_path(icon_raw.trim()) {
                Target::Exists(_) => continue,
                Target::Missing(path) => {
                    if !check::definitely_absent(&path) {
                        continue;
                    }
                    conditions.push(Condition::ValueEquals {
                        key: entry_path.clone(),
                        name: "DisplayIcon".to_owned(),
                        data: icon_raw,
                    });
                    conditions.push(Condition::PathMissing(path));
                }
                Target::Unknown => {}
            }
        }

        let display = check::string_value(&entry, "DisplayName").unwrap_or_else(|| name.clone());
        out.push(Issue::for_key(
            Category::ObsoleteSoftware,
            &entry_path,
            "Uninstall entry for software that is no longer installed",
            format!("{display} -> {}", uninstaller.display()),
            confidence,
            conditions,
        ));
    }
}

/// Reports keys directly below `software` that have no values and no sub-keys.
pub(crate) fn scan_empty_vendor_keys(
    software: &RegPath,
    progress: &Progress,
    out: &mut Vec<Issue>,
) {
    let Ok(root) = software.open() else {
        return;
    };
    for name in root.enum_keys().flatten() {
        if progress.is_cancelled() {
            return;
        }
        let vendor_path = software.child(&name);
        if reg::is_protected_key(&vendor_path) {
            continue;
        }
        let Ok(vendor) = root.open_subkey_with_flags(&name, KEY_READ) else {
            continue;
        };
        let Ok(info) = vendor.query_info() else {
            continue;
        };
        if info.sub_keys != 0 || info.values != 0 {
            continue;
        }
        out.push(Issue::for_key(
            Category::ObsoleteSoftware,
            &vendor_path,
            "Empty software key",
            name.clone(),
            Confidence::Safe,
            vec![Condition::KeyShape {
                key: vendor_path.clone(),
                sub_keys: 0,
                values: 0,
            }],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone, gone_dir};

    #[test]
    fn uninstall_entries_need_all_evidence_gone() {
        let scratch = Scratch::new("uninstall");
        let root = scratch.key("Uninstall");
        let entry = |name: &str| root.child(name);
        let dead_uninstaller = format!("\"{}\" /S", gone("uninstall.exe"));

        // Uninstaller and install location both gone: safe.
        scratch.set(&entry("Dead"), "DisplayName", "Dead Program");
        scratch.set(&entry("Dead"), "UninstallString", &dead_uninstaller);
        scratch.set(&entry("Dead"), "InstallLocation", &gone_dir());
        scratch.set(
            &entry("Dead"),
            "DisplayIcon",
            &format!("{},0", gone("app.exe")),
        );

        // Uninstaller gone, no location recorded: review.
        scratch.set(&entry("NoLocation"), "UninstallString", &dead_uninstaller);

        // Uninstaller gone but the install folder is still there.
        let tmp = tempfile::tempdir().unwrap();
        scratch.set(
            &entry("FolderRemains"),
            "UninstallString",
            &dead_uninstaller,
        );
        scratch.set(
            &entry("FolderRemains"),
            "InstallLocation",
            &tmp.path().display().to_string(),
        );

        // Uninstaller gone but the icon still exists.
        scratch.set(&entry("IconRemains"), "UninstallString", &dead_uninstaller);
        scratch.set(&entry("IconRemains"), "DisplayIcon", &existing_file());

        // Uninstaller exists.
        scratch.set(
            &entry("Installed"),
            "UninstallString",
            &format!("\"{}\"", existing_file()),
        );

        // Installer-service package.
        scratch.set(
            &entry("{12345678-1234-1234-1234-123456789012}"),
            "UninstallString",
            "MsiExec.exe /X{12345678-1234-1234-1234-123456789012}",
        );
        scratch.set(&entry("Managed"), "UninstallString", &dead_uninstaller);
        scratch.set_dword(&entry("Managed"), "WindowsInstaller", 1);
        scratch.set(&entry("Component"), "UninstallString", &dead_uninstaller);
        scratch.set_dword(&entry("Component"), "SystemComponent", 1);

        // Quiet uninstaller still exists.
        scratch.set(&entry("QuietAlive"), "UninstallString", &dead_uninstaller);
        scratch.set(
            &entry("QuietAlive"),
            "QuietUninstallString",
            &format!("\"{}\" /quiet", existing_file()),
        );

        // No uninstaller recorded at all.
        scratch.set(&entry("NameOnly"), "DisplayName", "Name Only");

        let mut out = Vec::new();
        scan_uninstall(&root, &Progress::new(), &mut out);
        let mut found: Vec<(&str, Confidence)> =
            out.iter().map(|i| (i.key.leaf(), i.confidence)).collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("Dead", Confidence::Safe),
                ("NoLocation", Confidence::Review)
            ],
            "{out:#?}"
        );
        let dead = out.iter().find(|i| i.key.leaf() == "Dead").unwrap();
        assert!(
            dead.detail.starts_with("Dead Program -> "),
            "{}",
            dead.detail
        );
        // Uninstaller, location and icon: a data and a path condition each.
        assert_eq!(dead.conditions.len(), 6);
        assert!(out.iter().all(Issue::still_valid));
    }

    #[test]
    fn empty_vendor_keys() {
        let scratch = Scratch::new("vendors");
        let software = scratch.key("Software");
        scratch.key(r"Software\EmptyVendor");
        scratch.key(r"Software\VendorWithProduct\Product");
        scratch.set(&software.child("VendorWithValue"), "Setting", "1");

        let mut out = Vec::new();
        scan_empty_vendor_keys(&software, &Progress::new(), &mut out);
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].key.leaf(), "EmptyVendor");
        assert!(out[0].still_valid());

        // The vendor starts using its key again.
        scratch.set(&software.child("EmptyVendor"), "Setting", "1");
        assert!(!out[0].still_valid());
    }

    #[test]
    fn structural_keys_are_never_reported_as_empty() {
        // Keys such as HKCU\Software\Microsoft are protected by the core
        // guard; the scanner must not even offer them.
        let mut out = Vec::new();
        scan_empty_vendor_keys(&RegPath::hkcu("Software"), &Progress::new(), &mut out);
        assert!(
            out.iter().all(|i| !reg::is_protected_key(&i.key)),
            "{out:#?}"
        );
    }
}
