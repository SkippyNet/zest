//! Browser extension inventory: what is installed in every browser profile,
//! how much access each extension has, and removal.

use crate::browsers::{self, Browser, BrowserKind, BrowserProfile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use zest_core::delete::{self, DeleteOptions, DeleteReport};
use zest_core::paths::UserDirs;
use zest_core::{Error, Result, process, walk};

/// How much of the user's browsing an extension can reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AccessLevel {
    /// Works only on specific sites or needs no site access.
    Low,
    /// Can read every site *or* uses a sensitive browser capability.
    Medium,
    /// Can read every site *and* uses sensitive capabilities, or can take
    /// control of the browser (debugger, proxy, native programs).
    High,
}

impl AccessLevel {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            AccessLevel::Low => "Low",
            AccessLevel::Medium => "Medium",
            AccessLevel::High => "High",
        }
    }
}

/// One extension installed in one browser profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserExtension {
    /// Browser identifier, e.g. `chrome`.
    pub browser_id: String,
    /// Browser display name.
    pub browser: String,
    /// Profile display name.
    pub profile: String,
    /// Windows account the profile belongs to.
    pub user: String,
    /// Extension identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version string.
    pub version: String,
    /// Short description (may be empty).
    pub description: String,
    /// Whether the browser has it switched on (`None` when unknown).
    pub enabled: Option<bool>,
    /// Browser capabilities it requests, e.g. `cookies`, `tabs`.
    pub permissions: Vec<String>,
    /// Site patterns it can access, e.g. `<all_urls>`.
    pub hosts: Vec<String>,
    /// Summary of its reach.
    pub access: AccessLevel,
    /// Plain-language reasons behind `access`.
    pub access_reasons: Vec<String>,
    /// Folder (Chromium) or `.xpi` file (Firefox) holding the extension.
    pub path: PathBuf,
    /// Size on disk.
    pub size: u64,
    /// Store page, when it can be derived.
    pub store_url: Option<String>,
    /// False for extensions installed by policy or built into the browser.
    pub removable: bool,
    /// Executable names of the browser; it must be closed before removal.
    pub processes: Vec<String>,
}

const SENSITIVE: &[(&str, &str)] = &[
    ("cookies", "Can read and change cookies"),
    ("webRequest", "Can observe network requests"),
    ("webRequestBlocking", "Can block or modify network requests"),
    ("history", "Can read your browsing history"),
    ("clipboardRead", "Can read the clipboard"),
    ("management", "Can manage other extensions"),
    ("privacy", "Can change privacy settings"),
    ("browsingData", "Can erase browsing data"),
    ("downloads", "Can manage downloads"),
    ("tabCapture", "Can capture tab contents"),
    ("desktopCapture", "Can capture the screen"),
];

const CONTROLLING: &[(&str, &str)] = &[
    ("debugger", "Can attach a debugger to pages"),
    ("proxy", "Can control proxy settings"),
    (
        "nativeMessaging",
        "Can communicate with programs on this PC",
    ),
];

fn is_all_sites(pattern: &str) -> bool {
    matches!(
        pattern,
        "<all_urls>" | "*://*/*" | "http://*/*" | "https://*/*" | "*://*/" | "file:///*"
    ) || pattern == "*"
}

/// Rates an extension from its permissions and host patterns.
pub fn rate_access(permissions: &[String], hosts: &[String]) -> (AccessLevel, Vec<String>) {
    let mut reasons = Vec::new();
    let all_sites = hosts.iter().any(|h| is_all_sites(h));
    if all_sites {
        reasons.push("Can read and change your data on all websites".to_owned());
    }
    let mut sensitive = false;
    for (name, text) in SENSITIVE {
        if permissions.iter().any(|p| p == name) {
            sensitive = true;
            reasons.push((*text).to_owned());
        }
    }
    let mut controlling = false;
    for (name, text) in CONTROLLING {
        if permissions.iter().any(|p| p == name) {
            controlling = true;
            reasons.push((*text).to_owned());
        }
    }
    let level = if controlling || (all_sites && sensitive) {
        AccessLevel::High
    } else if all_sites || sensitive {
        AccessLevel::Medium
    } else {
        AccessLevel::Low
    };
    (level, reasons)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn looks_like_host(pattern: &str) -> bool {
    pattern.contains("://") || pattern == "<all_urls>" || pattern == "*"
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Resolves `__MSG_key__` placeholders through the extension's locale files.
fn localized(text: &str, version_dir: &Path, default_locale: Option<&str>) -> String {
    let Some(key) = text
        .strip_prefix("__MSG_")
        .and_then(|rest| rest.strip_suffix("__"))
    else {
        return text.to_owned();
    };
    let mut locales: Vec<&str> = Vec::new();
    locales.extend(default_locale);
    locales.extend(["en", "en_US", "en_GB"]);
    for locale in locales {
        let Some(messages) = read_json(
            &version_dir
                .join("_locales")
                .join(locale)
                .join("messages.json"),
        ) else {
            continue;
        };
        let Some(map) = messages.as_object() else {
            continue;
        };
        // Message keys are case-insensitive.
        if let Some(message) = map
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.get("message"))
            .and_then(Value::as_str)
        {
            return message.to_owned();
        }
    }
    key.to_owned()
}

/// The newest version folder of a Chromium extension.
fn newest_version_dir(extension_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(extension_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().join("manifest.json").is_file())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn chromium_extensions(
    browser: &Browser,
    profile: &BrowserProfile,
    out: &mut Vec<BrowserExtension>,
) {
    let Ok(entries) = std::fs::read_dir(profile.data_dir.join("Extensions")) else {
        return;
    };
    // Extension state lives in "Secure Preferences" (current) or
    // "Preferences" (older browsers).
    let settings: Vec<Value> = ["Secure Preferences", "Preferences"]
        .iter()
        .filter_map(|f| read_json(&profile.data_dir.join(f)))
        .filter_map(|json| json.pointer("/extensions/settings").cloned())
        .collect();

    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        // Extension ids are 32 letters a-p.
        if id.len() != 32 || !id.chars().all(|c| ('a'..='p').contains(&c)) {
            continue;
        }
        let Some(version_dir) = newest_version_dir(&entry.path()) else {
            continue;
        };
        let Some(manifest) = read_json(&version_dir.join("manifest.json")) else {
            continue;
        };
        if manifest.get("theme").is_some() {
            continue;
        }
        let default_locale = manifest.get("default_locale").and_then(Value::as_str);
        let text = |key: &str| -> String {
            manifest
                .get(key)
                .and_then(Value::as_str)
                .map(|t| localized(t, &version_dir, default_locale))
                .unwrap_or_default()
        };

        let mut permissions = Vec::new();
        let mut hosts = strings(manifest.get("host_permissions"));
        for permission in strings(manifest.get("permissions")) {
            if looks_like_host(&permission) {
                hosts.push(permission);
            } else {
                permissions.push(permission);
            }
        }
        if let Some(scripts) = manifest.get("content_scripts").and_then(Value::as_array) {
            for script in scripts {
                hosts.extend(strings(script.get("matches")));
            }
        }
        hosts.sort();
        hosts.dedup();
        permissions.sort();
        permissions.dedup();

        let state = settings.iter().find_map(|s| s.get(&id));
        let location = state
            .and_then(|s| s.get("location"))
            .and_then(Value::as_i64);
        let enabled = state.map(|s| {
            let disabled_by_reason = match s.get("disable_reasons") {
                Some(Value::Array(reasons)) => !reasons.is_empty(),
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
                _ => false,
            };
            let state_flag = s.get("state").and_then(Value::as_i64);
            state_flag != Some(0) && !disabled_by_reason
        });
        // 5/10 = built-in components, 7/9 = installed by policy.
        let removable = !matches!(location, Some(5 | 7 | 9 | 10));

        let update_url = manifest
            .get("update_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let store_url = if update_url.contains("edge.microsoft.com") {
            Some(format!(
                "https://microsoftedge.microsoft.com/addons/detail/{id}"
            ))
        } else if update_url.contains("google.com") {
            Some(format!("https://chromewebstore.google.com/detail/{id}"))
        } else {
            None
        };

        let (access, access_reasons) = rate_access(&permissions, &hosts);
        let name = text("name");
        out.push(BrowserExtension {
            browser_id: browser.id.clone(),
            browser: browser.name.clone(),
            profile: profile.name.clone(),
            user: browser.user.clone(),
            name: if name.is_empty() { id.clone() } else { name },
            version: text("version"),
            description: text("description"),
            enabled,
            permissions,
            hosts,
            access,
            access_reasons,
            size: walk::tree_size(&entry.path(), None).bytes,
            path: entry.path(),
            store_url,
            removable,
            processes: browser.processes.clone(),
            id,
        });
    }
}

fn firefox_extensions(
    browser: &Browser,
    profile: &BrowserProfile,
    out: &mut Vec<BrowserExtension>,
) {
    let Some(json) = read_json(&profile.data_dir.join("extensions.json")) else {
        return;
    };
    let Some(addons) = json.get("addons").and_then(Value::as_array) else {
        return;
    };
    for addon in addons {
        let field = |key: &str| addon.get(key).and_then(Value::as_str).unwrap_or_default();
        if field("type") != "extension" || field("location") != "app-profile" {
            continue;
        }
        let id = field("id").to_owned();
        if id.is_empty() {
            continue;
        }
        let locale = |key: &str| -> String {
            addon
                .pointer(&format!("/defaultLocale/{key}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let mut permissions = strings(addon.pointer("/userPermissions/permissions"));
        let mut hosts = strings(addon.pointer("/userPermissions/origins"));
        permissions.sort();
        permissions.dedup();
        hosts.sort();
        hosts.dedup();
        let (access, access_reasons) = rate_access(&permissions, &hosts);
        let path = PathBuf::from(field("path"));
        let user_disabled = addon.get("userDisabled").and_then(Value::as_bool);
        let active = addon.get("active").and_then(Value::as_bool);
        let name = locale("name");
        out.push(BrowserExtension {
            browser_id: browser.id.clone(),
            browser: browser.name.clone(),
            profile: profile.name.clone(),
            user: browser.user.clone(),
            name: if name.is_empty() { id.clone() } else { name },
            version: field("version").to_owned(),
            description: locale("description"),
            enabled: match (active, user_disabled) {
                (Some(active), _) => Some(active),
                (None, Some(disabled)) => Some(!disabled),
                (None, None) => None,
            },
            permissions,
            hosts,
            access,
            access_reasons,
            size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            // Only files inside the profile may ever be removed.
            removable: path.starts_with(&profile.data_dir) && path.is_file(),
            path,
            store_url: Some(format!("https://addons.mozilla.org/firefox/addon/{id}/")),
            processes: browser.processes.clone(),
            id,
        });
    }
}

/// Lists the extensions of every supported browser for the given users,
/// sorted by browser, profile and name.
pub fn list_extensions(users: &[UserDirs]) -> Vec<BrowserExtension> {
    let mut out = Vec::new();
    for browser in browsers::discover(users) {
        for profile in &browser.profiles {
            match browser.kind {
                BrowserKind::Chromium => chromium_extensions(&browser, profile, &mut out),
                BrowserKind::Firefox => firefox_extensions(&browser, profile, &mut out),
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.browser, &a.profile, a.name.to_lowercase()).cmp(&(
            &b.browser,
            &b.profile,
            b.name.to_lowercase(),
        ))
    });
    out
}

/// Removes an extension's files by moving them to the Recycle Bin, so the
/// removal can be undone.
///
/// The browser must be closed. Chromium-based browsers keep a record of the
/// extension and may show it as damaged until it is removed from their own
/// extensions page; it no longer runs either way.
pub fn remove_extension(extension: &BrowserExtension) -> Result<()> {
    remove_extension_with(extension, &DeleteOptions::recycle(), true)
}

pub(crate) fn remove_extension_with(
    extension: &BrowserExtension,
    opts: &DeleteOptions,
    check_running: bool,
) -> Result<()> {
    if !extension.removable {
        return Err(Error::Other(format!(
            "{} is managed by the browser or by policy and cannot be removed here",
            extension.name
        )));
    }
    if check_running {
        let running = process::running_process_names();
        if extension
            .processes
            .iter()
            .any(|p| running.contains(&p.to_lowercase()))
        {
            return Err(Error::Other(format!(
                "Close {} before removing its extensions",
                extension.browser
            )));
        }
    }
    // Defence in depth: only ever remove something that still looks like an
    // extension inside a browser profile.
    let plausible = match extension.path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("xpi") => extension.path.is_file(),
        _ => {
            extension
                .path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "Extensions")
                && extension
                    .path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy() == extension.id)
        }
    };
    if !plausible {
        return Err(Error::Invalid(format!(
            "{} is not an extension location",
            extension.path.display()
        )));
    }
    let mut report = DeleteReport::default();
    delete::delete_tree(&extension.path, false, opts, None, &mut report);
    match report.failures.first() {
        Some((path, error)) => Err(Error::Other(format!(
            "could not remove {}: {error}",
            path.display()
        ))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXT_ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn fake_user(root: &Path) -> UserDirs {
        UserDirs::for_profile(root.join("Users").join("tester"), None)
    }

    fn build_chrome_profile(user: &UserDirs) -> PathBuf {
        let profile = user.local.join(r"Google\Chrome\User Data\Default");
        let version = profile.join("Extensions").join(EXT_ID).join("1.2.3_0");
        std::fs::create_dir_all(version.join(r"_locales\en")).unwrap();
        std::fs::write(
            version.join("manifest.json"),
            r#"{
                "name": "__MSG_appName__", "description": "__MSG_appDesc__", "version": "1.2.3",
                "default_locale": "en", "manifest_version": 3,
                "permissions": ["cookies", "storage", "webRequest"],
                "host_permissions": ["<all_urls>"],
                "content_scripts": [{"matches": ["https://*.example.com/*"]}],
                "update_url": "https://clients2.google.com/service/update2/crx"
            }"#,
        )
        .unwrap();
        std::fs::write(
            version.join(r"_locales\en\messages.json"),
            r#"{"appname": {"message": "Tab Tidy"}, "appDesc": {"message": "Keeps tabs tidy."}}"#,
        )
        .unwrap();
        std::fs::write(version.join("background.js"), vec![b'x'; 2048]).unwrap();
        // A theme and a non-extension folder must be ignored.
        std::fs::create_dir_all(profile.join("Extensions").join("Temp")).unwrap();
        let theme_id = "pppppppppppppppppppppppppppppppp";
        let theme = profile.join("Extensions").join(theme_id).join("1_0");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(
            theme.join("manifest.json"),
            r#"{"name":"Dark","version":"1","theme":{}}"#,
        )
        .unwrap();

        std::fs::write(profile.join("Preferences"), "{}").unwrap();
        std::fs::write(
            profile.join("Secure Preferences"),
            format!(r#"{{"extensions":{{"settings":{{"{EXT_ID}":{{"state":1,"location":1}}}}}}}}"#),
        )
        .unwrap();
        profile
    }

    #[test]
    fn access_rating() {
        let s = |items: &[&str]| items.iter().map(|i| (*i).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            rate_access(&s(&["storage"]), &s(&["https://example.com/*"])).0,
            AccessLevel::Low
        );
        assert_eq!(
            rate_access(&s(&["storage"]), &s(&["<all_urls>"])).0,
            AccessLevel::Medium
        );
        assert_eq!(
            rate_access(&s(&["cookies"]), &s(&[])).0,
            AccessLevel::Medium
        );
        let (level, reasons) = rate_access(&s(&["cookies", "webRequest"]), &s(&["*://*/*"]));
        assert_eq!(level, AccessLevel::High);
        assert_eq!(reasons.len(), 3);
        assert_eq!(
            rate_access(&s(&["nativeMessaging"]), &s(&[])).0,
            AccessLevel::High
        );
        assert!(AccessLevel::High > AccessLevel::Medium);
    }

    #[test]
    fn lists_chromium_extension_with_localized_name() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let profile = build_chrome_profile(&user);

        let found = list_extensions(&[user]);
        assert_eq!(found.len(), 1, "{found:#?}");
        let ext = &found[0];
        assert_eq!(ext.name, "Tab Tidy");
        assert_eq!(ext.description, "Keeps tabs tidy.");
        assert_eq!(ext.version, "1.2.3");
        assert_eq!(ext.enabled, Some(true));
        assert_eq!(ext.permissions, vec!["cookies", "storage", "webRequest"]);
        assert_eq!(ext.hosts, vec!["<all_urls>", "https://*.example.com/*"]);
        assert_eq!(ext.access, AccessLevel::High);
        assert!(ext.removable);
        assert!(ext.size >= 2048);
        assert_eq!(ext.path, profile.join("Extensions").join(EXT_ID));
        assert_eq!(
            ext.store_url.as_deref(),
            Some("https://chromewebstore.google.com/detail/abcdefghijklmnopabcdefghijklmnop")
        );
    }

    #[test]
    fn disabled_and_policy_states() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let profile = build_chrome_profile(&user);
        std::fs::write(
            profile.join("Secure Preferences"),
            format!(r#"{{"extensions":{{"settings":{{"{EXT_ID}":{{"disable_reasons":[1],"location":9}}}}}}}}"#),
        )
        .unwrap();
        let found = list_extensions(&[user]);
        assert_eq!(found[0].enabled, Some(false));
        assert!(!found[0].removable);
        assert!(remove_extension_with(&found[0], &DeleteOptions::permanent(), false).is_err());
        assert!(found[0].path.exists());
    }

    #[test]
    fn removes_only_the_extension_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let profile = build_chrome_profile(&user);
        let found = list_extensions(&[user]);
        remove_extension_with(&found[0], &DeleteOptions::permanent(), false).unwrap();
        assert!(!profile.join("Extensions").join(EXT_ID).exists());
        assert!(profile.join("Extensions").is_dir());
        assert!(profile.join("Preferences").is_file());

        // A tampered path is refused.
        let mut bogus = found[0].clone();
        bogus.path = profile.clone();
        assert!(remove_extension_with(&bogus, &DeleteOptions::permanent(), false).is_err());
        assert!(profile.is_dir());
    }

    #[test]
    fn lists_and_removes_firefox_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let profile = user
            .roaming
            .join(r"Mozilla\Firefox\Profiles\ab12.default-release");
        std::fs::create_dir_all(profile.join("extensions")).unwrap();
        std::fs::write(profile.join("prefs.js"), "").unwrap();
        let xpi = profile.join("extensions").join("tidy@example.org.xpi");
        std::fs::write(&xpi, vec![0u8; 4096]).unwrap();
        let json = serde_json::json!({
            "addons": [
                {
                    "id": "tidy@example.org", "type": "extension", "location": "app-profile",
                    "version": "4.5", "active": true, "userDisabled": false,
                    "path": xpi.to_string_lossy(),
                    "defaultLocale": {"name": "Tidy Fox", "description": "Tidies."},
                    "userPermissions": {"permissions": ["tabs"], "origins": ["https://example.org/*"]}
                },
                {"id": "builtin@mozilla.org", "type": "extension", "location": "app-builtin", "version": "1"},
                {"id": "theme@mozilla.org", "type": "theme", "location": "app-profile", "version": "1"}
            ]
        });
        std::fs::write(profile.join("extensions.json"), json.to_string()).unwrap();

        let found = list_extensions(&[user]);
        assert_eq!(found.len(), 1);
        let ext = &found[0];
        assert_eq!(
            (ext.name.as_str(), ext.version.as_str()),
            ("Tidy Fox", "4.5")
        );
        assert_eq!(ext.enabled, Some(true));
        assert_eq!(ext.access, AccessLevel::Low);
        assert_eq!(ext.size, 4096);
        assert!(ext.removable);
        remove_extension_with(ext, &DeleteOptions::permanent(), false).unwrap();
        assert!(!xpi.exists());
    }

    #[test]
    fn real_profiles_can_be_listed() {
        // Read-only smoke test against whatever browsers this PC has.
        for ext in list_extensions(&[UserDirs::current()]) {
            assert!(!ext.id.is_empty() && !ext.name.is_empty());
            assert!(ext.path.is_absolute());
        }
    }
}
