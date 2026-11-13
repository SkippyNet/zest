//! Windows services.
//!
//! A service or driver whose `ImagePath` is confirmed missing cannot start.
//! Because removing a service registration is the riskiest fix this crate
//! offers, every issue here is [`Confidence::Review`], the category is not
//! part of the default scan, and several kinds of entries are never
//! reported:
//!
//! * boot-start and system-start drivers (`Start` 0 or 1);
//! * services already marked for deletion;
//! * services hosted by a shared process or a script host, whose real
//!   module cannot be determined from `ImagePath`.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::reg::RegPath;

const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    scan_root(&RegPath::hklm(SERVICES_KEY), progress, out);
}

/// Scans the service keys directly below a `Services`-shaped key.
pub(crate) fn scan_root(services: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = services.open() else {
        return;
    };
    for (index, name) in root.enum_keys().flatten().enumerate() {
        if index % 32 == 0 && progress.is_cancelled() {
            return;
        }
        let Ok(service) = root.open_subkey_with_flags(&name, KEY_READ) else {
            continue;
        };
        let Some(raw) = check::raw_string_value(&service, "ImagePath") else {
            continue;
        };
        let image = raw.trim();
        if image.is_empty() {
            continue;
        }
        // Without a start type the entry is not a complete service record.
        let Some(start) = check::dword_value(&service, "Start") else {
            continue;
        };
        if start <= 1 || check::dword_value(&service, "DeleteFlag").is_some() {
            continue;
        }
        let Some(path) = check::missing_command(image) else {
            continue;
        };
        let service_path = services.child(&name);
        let display = check::string_value(&service, "DisplayName")
            .filter(|d| !d.starts_with('@'))
            .unwrap_or_else(|| name.clone());
        out.push(Issue::for_key(
            Category::Services,
            &service_path,
            "Service refers to a missing program file",
            format!("{display} -> {}", path.display()),
            Confidence::Review,
            vec![
                Condition::ValueEquals {
                    key: service_path.clone(),
                    name: "ImagePath".to_owned(),
                    data: raw,
                },
                Condition::PathMissing(path),
            ],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};
    use zest_core::paths;

    #[test]
    fn only_startable_services_with_missing_images() {
        let scratch = Scratch::new("services");
        let root = scratch.key("Services");
        let service = |name: &str, image: &str, start: u32| {
            scratch.set(&root.child(name), "ImagePath", image);
            scratch.set_dword(&root.child(name), "Start", start);
        };

        service(
            "DeadService",
            &format!("\"{}\" --service", gone("svc.exe")),
            2,
        );
        scratch.set(&root.child("DeadService"), "DisplayName", "Dead Service");
        service("DeadDriver", &format!(r"\??\{}", gone("driver.sys")), 3);
        service("AliveService", &format!("\"{}\"", existing_file()), 2);
        service(
            "SystemRootDriver",
            r"\SystemRoot\System32\drivers\ntfs.sys",
            3,
        );
        service("BootDriver", &format!(r"\??\{}", gone("boot.sys")), 0);
        service("SystemDriver", &format!(r"\??\{}", gone("system.sys")), 1);
        service("Shared", r"%SystemRoot%\System32\svchost.exe -k netsvcs", 2);
        service("PendingDelete", &format!("\"{}\"", gone("pending.exe")), 3);
        scratch.set_dword(&root.child("PendingDelete"), "DeleteFlag", 1);
        // Records without an image path or without a start type.
        scratch.set_dword(&root.child("NoImage"), "Start", 3);
        scratch.set(&root.child("NoStart"), "ImagePath", &gone("nostart.exe"));

        let mut out = Vec::new();
        scan_root(&root, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().map(|i| i.key.leaf()).collect();
        found.sort();
        assert_eq!(found, vec!["DeadDriver", "DeadService"], "{out:#?}");
        assert!(out.iter().all(|i| {
            i.confidence == Confidence::Review && !i.selected_by_default() && i.still_valid()
        }));
        let dead = out.iter().find(|i| i.key.leaf() == "DeadService").unwrap();
        assert!(
            dead.detail.starts_with("Dead Service -> "),
            "{}",
            dead.detail
        );

        // Sanity: the driver used as the "alive" example really exists.
        assert!(paths::system32_dir().join(r"drivers\ntfs.sys").exists());
    }
}
