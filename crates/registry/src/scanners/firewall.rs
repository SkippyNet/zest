//! Firewall rules.
//!
//! Each value under `FirewallRules` is one rule, encoded as
//! `v2.x|Action=Allow|Dir=In|App=C:\path\app.exe|Name=...|`. Rules that name
//! a program (`App=`) which is confirmed missing are reported. Rules without
//! a program, rules for packaged apps, and the built-in rules for programs
//! in Windows-owned locations are ignored.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use winreg::enums::RegType;
use zest_core::Progress;
use zest_core::reg::RegPath;

const RULES_KEY: &str =
    r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy\FirewallRules";

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    scan_key(&RegPath::hklm(RULES_KEY), progress, out);
}

/// Extracts a `Key=value` field from an encoded rule (key is case-insensitive).
fn field<'a>(rule: &'a str, key: &str) -> Option<&'a str> {
    rule.split('|').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        name.eq_ignore_ascii_case(key).then_some(value)
    })
}

/// Scans one `FirewallRules`-shaped key.
pub(crate) fn scan_key(key: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(handle) = key.open() else {
        return;
    };
    for (index, (name, value)) in handle.enum_values().flatten().enumerate() {
        if index % 64 == 0 && progress.is_cancelled() {
            return;
        }
        if name.is_empty() || !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
            continue;
        }
        let Some(raw) = check::decode_string(&value) else {
            continue;
        };
        let Some(app) = field(&raw, "App").map(str::trim).filter(|a| !a.is_empty()) else {
            continue;
        };
        // Windows ships rules for optional components that are not installed.
        let Some(path) = check::outside_windows(check::missing_path(app)) else {
            continue;
        };
        let rule_name = field(&raw, "Name")
            .map(str::trim)
            .filter(|n| !n.is_empty() && !n.starts_with('@'))
            .unwrap_or(&name);
        let detail = format!("{rule_name} -> {}", path.display());
        out.push(Issue::for_value(
            Category::FirewallRules,
            key,
            &name,
            "Firewall rule for a program that no longer exists",
            detail,
            Confidence::Safe,
            vec![
                Condition::ValueEquals {
                    key: key.clone(),
                    name: name.clone(),
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

    #[test]
    fn field_extraction() {
        let rule = r"v2.30|Action=Allow|Active=TRUE|Dir=In|App=C:\x\app.exe|Name=My App|Desc=a=b|";
        assert_eq!(field(rule, "App"), Some(r"C:\x\app.exe"));
        assert_eq!(field(rule, "app"), Some(r"C:\x\app.exe"));
        assert_eq!(field(rule, "Name"), Some("My App"));
        assert_eq!(field(rule, "Desc"), Some("a=b"));
        assert_eq!(field(rule, "Svc"), None);
        assert_eq!(field("", "App"), None);
    }

    #[test]
    fn rules_for_missing_programs() {
        let scratch = Scratch::new("firewall");
        let key = scratch.key("FirewallRules");
        let rule = |app: &str, name: &str| {
            format!("v2.30|Action=Allow|Active=TRUE|Dir=In|Protocol=6|App={app}|Name={name}|")
        };
        scratch.set(&key, "{DEAD-RULE}", &rule(&gone("game.exe"), "Old Game"));
        scratch.set(
            &key,
            "{DEAD-RESOURCE}",
            &rule(&gone("tool.exe"), "@tool.dll,-100"),
        );
        scratch.set(&key, "{ALIVE-RULE}", &rule(&existing_file(), "Shell"));
        scratch.set(&key, "{SYSTEM-RULE}", &rule("System", "Core Networking"));
        scratch.set(
            &key,
            "{VARIABLE-RULE}",
            &rule(r"%SystemRoot%\system32\svchost.exe", "Service Host"),
        );
        scratch.set(
            &key,
            "{PACKAGED-RULE}",
            &rule(
                r"C:\Program Files\WindowsApps\Zest.NoSuch_1.0\app.exe",
                "Packaged",
            ),
        );
        // A built-in rule for an optional Windows component.
        let optional = zest_core::paths::system32_dir()
            .join("zest-optional-feature.exe")
            .display()
            .to_string();
        scratch.set(&key, "Builtin-Feature-In-TCP", &rule(&optional, "@builtin"));
        scratch.set(
            &key,
            "{PORT-RULE}",
            "v2.30|Action=Allow|Active=TRUE|Dir=In|Protocol=6|LPort=8080|Name=Port only|",
        );

        let mut out = Vec::new();
        scan_key(&key, &Progress::new(), &mut out);
        let mut found: Vec<(&str, &str)> = out
            .iter()
            .map(|i| (i.value.as_deref().unwrap(), i.detail.as_str()))
            .collect();
        found.sort();
        let dead_resource = format!("{{DEAD-RESOURCE}} -> {}", gone("tool.exe"));
        let dead_rule = format!("Old Game -> {}", gone("game.exe"));
        assert_eq!(
            found,
            vec![
                ("{DEAD-RESOURCE}", dead_resource.as_str()),
                ("{DEAD-RULE}", dead_rule.as_str()),
            ],
            "{out:#?}"
        );
        assert!(out.iter().all(Issue::still_valid));
    }
}
