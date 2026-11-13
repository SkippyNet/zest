//! Rules for web browsers, generated from the profiles found on disk.

use super::{group, rule};
use crate::browsers::{Browser, BrowserKind};
use crate::model::{Group, Rule, Section, Special};
use std::path::PathBuf;

const COOKIE_WARNING: &str =
    "You will be signed out of websites that are not on your cookie keep list.";
const SESSION_WARNING: &str =
    "Your open tabs will not be restored the next time the browser starts.";
const PASSWORD_WARNING: &str = "Every password saved in this browser is permanently removed. Make sure they are stored elsewhere.";

pub(super) fn groups(browsers: &[Browser]) -> Vec<Group> {
    let mut ids: Vec<&str> = Vec::new();
    for browser in browsers {
        if !ids.contains(&browser.id.as_str()) {
            ids.push(&browser.id);
        }
    }
    ids.into_iter()
        .filter_map(|id| {
            let installs: Vec<&Browser> = browsers.iter().filter(|b| b.id == id).collect();
            let first = installs[0];
            let rules = match first.kind {
                BrowserKind::Chromium => chromium_rules(id, &installs),
                BrowserKind::Firefox => firefox_rules(id, &installs),
            };
            let processes: Vec<&str> = first.processes.iter().map(String::as_str).collect();
            group(
                id,
                Section::Browsers,
                &first.name,
                first.icon,
                &processes,
                rules,
            )
        })
        .collect()
}

/// Path helpers over all profiles of all installs of one browser.
struct Paths<'a>(&'a [&'a Browser]);

impl Paths<'_> {
    /// `rel` inside every profile's data folder.
    fn data(&self, rel: &str) -> Vec<PathBuf> {
        self.0
            .iter()
            .flat_map(|b| b.profiles.iter().map(move |p| p.data_dir.join(rel)))
            .collect()
    }

    /// `rel` inside every profile's data folder and cache folder.
    fn both(&self, rel: &str) -> Vec<PathBuf> {
        let mut out = self.data(rel);
        for browser in self.0 {
            for profile in &browser.profiles {
                let path = profile.cache_dir.join(rel);
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
        out
    }

    /// `rel` inside the browser's data root and cache root.
    fn root(&self, rel: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for browser in self.0 {
            for base in [&browser.root, &browser.cache_root] {
                let path = base.join(rel);
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
        out
    }

    /// Every profile's data folder.
    fn profiles(&self) -> Vec<PathBuf> {
        self.data("")
    }
}

fn chromium_rules(id: &str, installs: &[&Browser]) -> Vec<Rule> {
    let p = Paths(installs);
    let rid = |suffix: &str| format!("{id}.{suffix}");

    let mut cache = rule(
        &rid("cache"),
        "Internet cache",
        "Cached copies of pages, images, scripts and compiled code. Sites reload them as needed.",
    );
    for dir in [
        "Cache",
        "Code Cache",
        "GPUCache",
        "DawnCache",
        "DawnGraphiteCache",
        "DawnWebGPUCache",
        "Media Cache",
        "Application Cache",
        r"Service Worker\CacheStorage",
        r"Service Worker\ScriptCache",
        "JumpListIconsRecentClosed",
        "JumpListIconsTopSites",
        "JumpListIconsMostVisited",
    ] {
        cache = cache.contents(p.both(dir));
    }
    for dir in [
        "GrShaderCache",
        "ShaderCache",
        "GraphiteDawnCache",
        "GPUPersistentCache",
        "component_crx_cache",
        "extensions_crx_cache",
    ] {
        cache = cache.contents(p.root(dir));
    }

    let diagnostics = rule(
        &rid("diagnostics"),
        "Crash reports and metrics",
        "Stored crash dumps and usage-metrics files.",
    )
    .contents(p.root(r"Crashpad\reports"))
    .files(p.root("BrowserMetrics"), "*.pma", false)
    .contents(p.root("DeferredBrowserMetrics"));

    let mut history = rule(
        &rid("history"),
        "Browsing history",
        "Visited pages, address-bar suggestions, most-visited tiles and download records.",
    )
    .privacy();
    for file in [
        "History",
        "History-journal",
        "Visited Links",
        "Top Sites",
        "Top Sites-journal",
        "Network Action Predictor",
        "Network Action Predictor-journal",
        "Shortcuts",
        "Shortcuts-journal",
        "History Provider Cache",
        "DIPS",
        "DIPS-wal",
        r"Network\Reporting and NEL",
        r"Network\Reporting and NEL-journal",
    ] {
        history = history.file(p.data(file));
    }

    let mut downloads = rule(
        &rid("downloads"),
        "Download history",
        "The list of downloaded files. The files themselves are not touched.",
    )
    .privacy();
    for db in p.data("History") {
        downloads = downloads.special(Special::ChromiumDownloads { db });
    }

    let mut cookies = rule(
        &rid("cookies"),
        "Cookies",
        "Sign-in and tracking cookies. Sites on your keep list are preserved.",
    )
    .privacy()
    .warn(COOKIE_WARNING);
    for db in p
        .data(r"Network\Cookies")
        .into_iter()
        .chain(p.data("Cookies"))
    {
        cookies = cookies.special(Special::ChromiumCookies { db });
    }

    let mut site_data = rule(
        &rid("site_data"),
        "Site data",
        "Offline storage that websites keep on this PC (local storage, databases, service workers).",
    )
    .privacy()
    .warn("Web apps lose their offline data and some sites will sign you out.");
    for profile in p.profiles() {
        site_data = site_data.special(Special::ChromiumSiteData { profile });
    }

    let mut session = rule(
        &rid("session"),
        "Session",
        "The record of open and recently closed tabs and windows.",
    )
    .privacy()
    .warn(SESSION_WARNING)
    .contents(p.data("Sessions"))
    .contents(p.data("Session Storage"));
    for file in [
        "Current Session",
        "Current Tabs",
        "Last Session",
        "Last Tabs",
    ] {
        session = session.file(p.data(file));
    }

    let mut autofill = rule(
        &rid("autofill"),
        "Saved form entries",
        "Text previously typed into web forms. Saved addresses and payment methods are kept.",
    )
    .privacy();
    for db in p.data("Web Data") {
        autofill = autofill.special(Special::ChromiumAutofill { db });
    }

    let mut passwords = rule(
        &rid("passwords"),
        "Saved passwords",
        "Passwords stored in the browser's built-in password manager.",
    )
    .privacy()
    .warn(PASSWORD_WARNING);
    for file in [
        "Login Data",
        "Login Data-journal",
        "Login Data For Account",
        "Login Data For Account-journal",
    ] {
        passwords = passwords.file(p.data(file));
    }

    let mut last_location = rule(
        &rid("download_location"),
        "Last download location",
        "The folder most recently used in Save and Open dialogs.",
    )
    .privacy();
    for file in p.data("Preferences") {
        last_location = last_location.special(Special::JsonRemoveKeys {
            file,
            keys: vec![
                "savefile.default_directory".to_owned(),
                "selectfile.last_directory".to_owned(),
            ],
        });
    }

    let mut compact = rule(
        &rid("compact"),
        "Compact databases",
        "Rebuilds the browser's databases to reclaim unused space inside them.",
    )
    .off();
    for profile in p.profiles() {
        compact = compact.special(Special::SqliteVacuum {
            dbs: [
                "History",
                r"Network\Cookies",
                "Web Data",
                "Favicons",
                "Top Sites",
                "Shortcuts",
            ]
            .iter()
            .map(|f| profile.join(f))
            .collect(),
        });
    }

    vec![
        cache.build(),
        diagnostics.build(),
        history.build(),
        downloads.build(),
        cookies.build(),
        site_data.build(),
        session.build(),
        autofill.build(),
        passwords.build(),
        last_location.build(),
        compact.build(),
    ]
}

fn firefox_rules(id: &str, installs: &[&Browser]) -> Vec<Rule> {
    let p = Paths(installs);
    let rid = |suffix: &str| format!("{id}.{suffix}");

    let mut cache = rule(
        &rid("cache"),
        "Internet cache",
        "Cached copies of pages, images and scripts. Sites reload them as needed.",
    );
    for dir in [
        "cache2",
        "startupCache",
        "jumpListCache",
        "OfflineCache",
        "thumbnails",
        "shader-cache",
    ] {
        cache = cache.contents(p.both(dir));
    }

    let diagnostics = rule(
        &rid("diagnostics"),
        "Crash reports",
        "Stored crash dumps and crash submission records.",
    )
    .contents(p.data("minidumps"))
    .contents(p.data("crashes"))
    .contents(
        installs
            .iter()
            .filter_map(|b| b.root.parent().map(|parent| parent.join("Crash Reports")))
            .collect(),
    );

    let mut history = rule(
        &rid("history"),
        "Browsing history",
        "Visited pages, address-bar suggestions and download records. Bookmarks are kept.",
    )
    .privacy();
    for db in p.data("places.sqlite") {
        history = history.special(Special::FirefoxHistory { db });
    }

    let mut cookies = rule(
        &rid("cookies"),
        "Cookies",
        "Sign-in and tracking cookies. Sites on your keep list are preserved.",
    )
    .privacy()
    .warn(COOKIE_WARNING);
    for db in p.data("cookies.sqlite") {
        cookies = cookies.special(Special::FirefoxCookies { db });
    }

    let mut site_data = rule(
        &rid("site_data"),
        "Site data",
        "Offline storage that websites keep on this PC (local storage, databases, service workers).",
    )
    .privacy()
    .warn("Web apps lose their offline data and some sites will sign you out.");
    for profile in p.profiles() {
        site_data = site_data.special(Special::FirefoxSiteData { profile });
    }

    let session = rule(
        &rid("session"),
        "Session",
        "The record of open and recently closed tabs and windows.",
    )
    .privacy()
    .warn(SESSION_WARNING)
    .file(p.data("sessionstore.jsonlz4"))
    .file(p.data("sessionCheckpoints.json"))
    .contents(p.data("sessionstore-backups"));

    let autofill = rule(
        &rid("autofill"),
        "Saved form entries",
        "Text previously typed into web forms and the search bar.",
    )
    .privacy()
    .file(p.data("formhistory.sqlite"));

    let passwords = rule(
        &rid("passwords"),
        "Saved passwords",
        "Passwords stored in the browser's built-in password manager.",
    )
    .privacy()
    .warn(PASSWORD_WARNING)
    .file(p.data("logins.json"))
    .file(p.data("logins-backup.json"))
    .file(p.data("key4.db"));

    let mut compact = rule(
        &rid("compact"),
        "Compact databases",
        "Rebuilds the browser's databases to reclaim unused space inside them.",
    )
    .off();
    for profile in p.profiles() {
        compact = compact.special(Special::SqliteVacuum {
            dbs: [
                "places.sqlite",
                "favicons.sqlite",
                "cookies.sqlite",
                "formhistory.sqlite",
                "permissions.sqlite",
                "content-prefs.sqlite",
                "webappsstore.sqlite",
            ]
            .iter()
            .map(|f| profile.join(f))
            .collect(),
        });
    }

    vec![
        cache.build(),
        diagnostics.build(),
        history.build(),
        cookies.build(),
        site_data.build(),
        session.build(),
        autofill.build(),
        passwords.build(),
        compact.build(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browsers::discover;
    use crate::model::{Action, RuleKind};
    use zest_core::paths::UserDirs;

    #[test]
    fn two_users_merge_into_one_group() {
        let tmp = tempfile::tempdir().unwrap();
        let users: Vec<UserDirs> = ["a", "b"]
            .iter()
            .map(|n| UserDirs::for_profile(tmp.path().join("Users").join(n), None))
            .collect();
        for user in &users {
            let profile = user.local.join(r"Google\Chrome\User Data\Default");
            std::fs::create_dir_all(&profile).unwrap();
            std::fs::write(profile.join("Preferences"), "{}").unwrap();
        }
        let ff = users[0]
            .roaming
            .join(r"Mozilla\Firefox\Profiles\x1.default");
        std::fs::create_dir_all(&ff).unwrap();
        std::fs::write(ff.join("prefs.js"), "").unwrap();

        let groups = groups(&discover(&users));
        assert_eq!(
            groups.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
            vec!["chrome", "firefox"]
        );

        let chrome = &groups[0];
        assert_eq!(chrome.processes, vec!["chrome.exe"]);
        let cookies = chrome
            .rules
            .iter()
            .find(|r| r.id == "chrome.cookies")
            .unwrap();
        assert_eq!(cookies.kind, RuleKind::Privacy);
        assert!(!cookies.default_enabled && cookies.warning.is_some());
        // Modern + legacy cookie database for each of the two users.
        assert_eq!(cookies.actions.len(), 4);
        let cache = chrome
            .rules
            .iter()
            .find(|r| r.id == "chrome.cache")
            .unwrap();
        assert!(cache.default_enabled);
        assert!(cache.actions.iter().all(|a| matches!(
            a,
            Action::Files {
                recursive: true,
                ..
            }
        )));

        let firefox = &groups[1];
        let ff_cache = firefox
            .rules
            .iter()
            .find(|r| r.id == "firefox.cache")
            .unwrap();
        // Firefox caches live under Local, data under Roaming: both are covered.
        assert!(
            ff_cache
                .actions
                .iter()
                .any(|a| matches!(a, Action::Files { dir, .. }
            if dir.starts_with(&users[0].local) && dir.ends_with("cache2")))
        );
        assert!(firefox.rules.iter().any(|r| r.id == "firefox.history"
            && matches!(
                &r.actions[0],
                Action::Special(Special::FirefoxHistory { .. })
            )));
    }
}
