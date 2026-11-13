//! Discovery of installed browsers and their profiles.

use crate::model::GroupIcon;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zest_core::paths::UserDirs;

/// Browser engine family; decides the on-disk layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BrowserKind {
    /// Chrome, Edge, Brave, Opera, Vivaldi, ...
    Chromium,
    /// Firefox and its derivatives.
    Firefox,
}

/// Where the profile data of a browser lives, relative to a user's folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    Local,
    Roaming,
}

struct BrowserDef {
    id: &'static str,
    name: &'static str,
    kind: BrowserKind,
    icon: GroupIcon,
    processes: &'static [&'static str],
    /// Root that holds the profiles (Chromium "User Data", Firefox "Profiles").
    data: (Base, &'static str),
    /// Root that holds caches when it differs from `data`.
    cache: Option<(Base, &'static str)>,
}

const BROWSERS: &[BrowserDef] = &[
    BrowserDef {
        id: "chrome",
        name: "Google Chrome",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Chrome,
        processes: &["chrome.exe"],
        data: (Base::Local, r"Google\Chrome\User Data"),
        cache: None,
    },
    BrowserDef {
        id: "edge",
        name: "Microsoft Edge",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Edge,
        processes: &["msedge.exe"],
        data: (Base::Local, r"Microsoft\Edge\User Data"),
        cache: None,
    },
    BrowserDef {
        id: "firefox",
        name: "Mozilla Firefox",
        kind: BrowserKind::Firefox,
        icon: GroupIcon::Firefox,
        processes: &["firefox.exe"],
        data: (Base::Roaming, r"Mozilla\Firefox\Profiles"),
        cache: Some((Base::Local, r"Mozilla\Firefox\Profiles")),
    },
    BrowserDef {
        id: "brave",
        name: "Brave",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Brave,
        processes: &["brave.exe"],
        data: (Base::Local, r"BraveSoftware\Brave-Browser\User Data"),
        cache: None,
    },
    BrowserDef {
        id: "opera",
        name: "Opera",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Opera,
        processes: &["opera.exe"],
        data: (Base::Roaming, r"Opera Software\Opera Stable"),
        cache: Some((Base::Local, r"Opera Software\Opera Stable")),
    },
    BrowserDef {
        id: "operagx",
        name: "Opera GX",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Opera,
        processes: &["opera.exe"],
        data: (Base::Roaming, r"Opera Software\Opera GX Stable"),
        cache: Some((Base::Local, r"Opera Software\Opera GX Stable")),
    },
    BrowserDef {
        id: "vivaldi",
        name: "Vivaldi",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Vivaldi,
        processes: &["vivaldi.exe"],
        data: (Base::Local, r"Vivaldi\User Data"),
        cache: None,
    },
    BrowserDef {
        id: "chromium",
        name: "Chromium",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Browser,
        processes: &["chrome.exe"],
        data: (Base::Local, r"Chromium\User Data"),
        cache: None,
    },
    BrowserDef {
        id: "yandex",
        name: "Yandex Browser",
        kind: BrowserKind::Chromium,
        icon: GroupIcon::Browser,
        processes: &["browser.exe"],
        data: (Base::Local, r"Yandex\YandexBrowser\User Data"),
        cache: None,
    },
    BrowserDef {
        id: "librewolf",
        name: "LibreWolf",
        kind: BrowserKind::Firefox,
        icon: GroupIcon::Firefox,
        processes: &["librewolf.exe"],
        data: (Base::Roaming, r"librewolf\Profiles"),
        cache: Some((Base::Local, r"librewolf\Profiles")),
    },
    BrowserDef {
        id: "waterfox",
        name: "Waterfox",
        kind: BrowserKind::Firefox,
        icon: GroupIcon::Firefox,
        processes: &["waterfox.exe"],
        data: (Base::Roaming, r"Waterfox\Profiles"),
        cache: Some((Base::Local, r"Waterfox\Profiles")),
    },
];

/// One browser profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserProfile {
    /// Friendly name (falls back to the folder name).
    pub name: String,
    /// Folder name, e.g. `Default` or `Profile 2`.
    pub dir_name: String,
    /// Folder holding history, cookies, preferences.
    pub data_dir: PathBuf,
    /// Folder holding caches (often the same as `data_dir`).
    pub cache_dir: PathBuf,
}

/// One browser installation of one Windows user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Browser {
    /// Stable identifier, e.g. `chrome`.
    pub id: String,
    /// Display name, e.g. `Google Chrome`.
    pub name: String,
    /// Engine family.
    pub kind: BrowserKind,
    /// Icon hint.
    pub icon: GroupIcon,
    /// Executable names of the running browser.
    pub processes: Vec<String>,
    /// Windows account the data belongs to.
    pub user: String,
    /// Root of the profile data.
    pub root: PathBuf,
    /// Root of the caches.
    pub cache_root: PathBuf,
    /// The profiles found.
    pub profiles: Vec<BrowserProfile>,
}

fn base_dir(user: &UserDirs, base: Base) -> &Path {
    match base {
        Base::Local => &user.local,
        Base::Roaming => &user.roaming,
    }
}

/// Reads friendly profile names from Chromium's `Local State` file.
fn chromium_profile_names(root: &Path) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("Local State")) else {
        return names;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return names;
    };
    if let Some(cache) = json
        .pointer("/profile/info_cache")
        .and_then(|v| v.as_object())
    {
        for (dir, info) in cache {
            if let Some(name) = info.get("name").and_then(|n| n.as_str())
                && !name.trim().is_empty()
            {
                names.insert(dir.clone(), name.trim().to_owned());
            }
        }
    }
    names
}

fn chromium_profiles(root: &Path, cache_root: &Path) -> Vec<BrowserProfile> {
    let names = chromium_profile_names(root);
    let mut profiles = Vec::new();
    // Some Chromium browsers keep a single profile directly in the root.
    if root.join("Preferences").is_file() {
        profiles.push(BrowserProfile {
            name: "Default".to_owned(),
            dir_name: String::new(),
            data_dir: root.to_path_buf(),
            cache_dir: cache_root.to_path_buf(),
        });
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return profiles;
    };
    let mut dirs: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("Preferences").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "System Profile")
        .collect();
    dirs.sort_by_key(|name| (name != "Default", name.to_lowercase()));
    for dir in dirs {
        profiles.push(BrowserProfile {
            name: names.get(&dir).cloned().unwrap_or_else(|| dir.clone()),
            data_dir: root.join(&dir),
            cache_dir: cache_root.join(&dir),
            dir_name: dir,
        });
    }
    profiles
}

fn firefox_profiles(root: &Path, cache_root: &Path) -> Vec<BrowserProfile> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut profiles: Vec<BrowserProfile> = entries
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.join("prefs.js").is_file() || p.join("places.sqlite").is_file()
        })
        .map(|e| {
            let dir = e.file_name().to_string_lossy().into_owned();
            // "abcd1234.default-release" -> "default-release"
            let name = dir
                .split_once('.')
                .map_or(dir.as_str(), |(_, n)| n)
                .to_owned();
            BrowserProfile {
                name,
                data_dir: root.join(&dir),
                cache_dir: cache_root.join(&dir),
                dir_name: dir,
            }
        })
        .collect();
    profiles.sort_by_key(|p| p.dir_name.to_lowercase());
    profiles
}

/// Finds every supported browser that has profile data for the given users.
pub fn discover(users: &[UserDirs]) -> Vec<Browser> {
    let mut found = Vec::new();
    for def in BROWSERS {
        for user in users {
            let root = base_dir(user, def.data.0).join(def.data.1);
            if !root.is_dir() {
                continue;
            }
            let cache_root = match def.cache {
                Some((base, rel)) => base_dir(user, base).join(rel),
                None => root.clone(),
            };
            let profiles = match def.kind {
                BrowserKind::Chromium => chromium_profiles(&root, &cache_root),
                BrowserKind::Firefox => firefox_profiles(&root, &cache_root),
            };
            if profiles.is_empty() {
                continue;
            }
            found.push(Browser {
                id: def.id.to_owned(),
                name: def.name.to_owned(),
                kind: def.kind,
                icon: def.icon,
                processes: def.processes.iter().map(|p| (*p).to_owned()).collect(),
                user: user.name.clone(),
                root,
                cache_root,
                profiles,
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_user(root: &Path) -> UserDirs {
        UserDirs::for_profile(root.join("Users").join("tester"), None)
    }

    #[test]
    fn finds_chromium_profiles_with_friendly_names() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let root = user.local.join(r"Google\Chrome\User Data");
        for dir in ["Default", "Profile 1", "System Profile", "NotAProfile"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for dir in ["Default", "Profile 1", "System Profile"] {
            std::fs::write(root.join(dir).join("Preferences"), "{}").unwrap();
        }
        std::fs::write(
            root.join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Personal"},"Profile 1":{"name":"Work"}}}}"#,
        )
        .unwrap();

        let browsers = discover(&[user]);
        assert_eq!(browsers.len(), 1);
        let chrome = &browsers[0];
        assert_eq!(
            (chrome.id.as_str(), chrome.user.as_str()),
            ("chrome", "tester")
        );
        let names: Vec<_> = chrome
            .profiles
            .iter()
            .map(|p| (p.dir_name.as_str(), p.name.as_str()))
            .collect();
        assert_eq!(names, vec![("Default", "Personal"), ("Profile 1", "Work")]);
        assert_eq!(chrome.profiles[0].cache_dir, root.join("Default"));
    }

    #[test]
    fn opera_style_single_profile_and_split_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let root = user.roaming.join(r"Opera Software\Opera Stable");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Preferences"), "{}").unwrap();

        let browsers = discover(std::slice::from_ref(&user));
        assert_eq!(browsers.len(), 1);
        let opera = &browsers[0];
        assert_eq!(opera.profiles.len(), 1);
        assert_eq!(opera.profiles[0].data_dir, root);
        assert_eq!(
            opera.profiles[0].cache_dir,
            user.local.join(r"Opera Software\Opera Stable")
        );
    }

    #[test]
    fn finds_firefox_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let user = fake_user(tmp.path());
        let root = user.roaming.join(r"Mozilla\Firefox\Profiles");
        std::fs::create_dir_all(root.join("ab12cd34.default-release")).unwrap();
        std::fs::write(root.join("ab12cd34.default-release").join("prefs.js"), "").unwrap();
        std::fs::create_dir_all(root.join("empty.folder")).unwrap();

        let browsers = discover(std::slice::from_ref(&user));
        assert_eq!(browsers.len(), 1);
        let p = &browsers[0].profiles[0];
        assert_eq!(p.name, "default-release");
        assert_eq!(
            p.cache_dir,
            user.local
                .join(r"Mozilla\Firefox\Profiles\ab12cd34.default-release")
        );
    }

    #[test]
    fn nothing_installed_means_nothing_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover(&[fake_user(tmp.path())]).is_empty());
    }
}
