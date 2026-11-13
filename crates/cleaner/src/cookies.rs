//! Cookie keep list and cookie inventory.

use crate::browsers::{self, BrowserKind};
use crate::sqlite;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zest_core::paths::UserDirs;

/// Sites people are commonly signed in to. Offered as a starting keep list
/// when their cookies are present, so a first clean does not sign the user
/// out of everything.
const COMMON_SIGN_IN_DOMAINS: &[&str] = &[
    "amazon.com",
    "apple.com",
    "atlassian.com",
    "chatgpt.com",
    "claude.ai",
    "discord.com",
    "dropbox.com",
    "facebook.com",
    "github.com",
    "gitlab.com",
    "google.com",
    "icloud.com",
    "instagram.com",
    "linkedin.com",
    "live.com",
    "microsoft.com",
    "microsoftonline.com",
    "netflix.com",
    "notion.so",
    "office.com",
    "openai.com",
    "outlook.com",
    "paypal.com",
    "proton.me",
    "reddit.com",
    "slack.com",
    "spotify.com",
    "stackoverflow.com",
    "steamcommunity.com",
    "steampowered.com",
    "twitch.tv",
    "twitter.com",
    "whatsapp.com",
    "x.com",
    "yahoo.com",
    "youtube.com",
    "zoom.us",
];

/// Turns user input (`https://www.Example.com/path`, `.example.com`,
/// `*.example.com`) into a bare lower-case domain. Returns `None` for input
/// that is not a plausible host name.
pub fn normalize_domain(input: &str) -> Option<String> {
    let mut s = input.trim().to_lowercase();
    if let Some((_, rest)) = s.split_once("://") {
        s = rest.to_owned();
    }
    if let Some((host, _)) = s.split_once(['/', '?', '#']) {
        s = host.to_owned();
    }
    if let Some((_, host)) = s.rsplit_once('@') {
        s = host.to_owned();
    }
    // Strip a port, but leave IPv6 literals alone.
    if !s.starts_with('[')
        && let Some((host, port)) = s.rsplit_once(':')
        && port.chars().all(|c| c.is_ascii_digit())
    {
        s = host.to_owned();
    }
    let s = s.trim_start_matches("*.").trim_matches('.').to_owned();
    let valid = !s.is_empty()
        && s.len() <= 253
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']'));
    valid.then_some(s)
}

/// The set of domains whose cookies and site data survive cleaning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeepList(Vec<String>);

impl KeepList {
    /// Builds a keep list, normalising every entry and dropping invalid ones.
    pub fn new(entries: &[String]) -> Self {
        let mut domains: Vec<String> = entries.iter().filter_map(|e| normalize_domain(e)).collect();
        domains.sort();
        domains.dedup();
        KeepList(domains)
    }

    /// True when nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The normalised domains.
    pub fn domains(&self) -> &[String] {
        &self.0
    }

    /// True when cookies of `host` are kept: the host equals a kept domain
    /// or is a sub-domain of one.
    pub fn keeps(&self, host: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let host = host.trim().trim_start_matches('.').to_lowercase();
        self.0.iter().any(|domain| {
            host == *domain
                || (host.len() > domain.len()
                    && host.ends_with(domain.as_str())
                    && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
        })
    }
}

/// Cookies stored for one host across all browsers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieHost {
    /// Host name without a leading dot.
    pub host: String,
    /// Number of cookies.
    pub count: u64,
    /// Display names of the browsers holding them.
    pub browsers: Vec<String>,
}

/// Outcome of a cookie inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieInventory {
    /// All hosts with cookies, sorted by name.
    pub hosts: Vec<CookieHost>,
    /// Browsers whose cookie database could not be read (usually because
    /// the browser is running).
    pub unreadable: Vec<String>,
}

/// Lists the hosts that have cookies in any supported browser.
pub fn inventory(users: &[UserDirs]) -> CookieInventory {
    let mut hosts: BTreeMap<String, CookieHost> = BTreeMap::new();
    let mut unreadable: Vec<String> = Vec::new();
    for browser in browsers::discover(users) {
        for profile in &browser.profiles {
            let (dbs, table, column): (Vec<_>, _, _) = match browser.kind {
                BrowserKind::Chromium => (
                    vec![
                        profile.data_dir.join("Network").join("Cookies"),
                        profile.data_dir.join("Cookies"),
                    ],
                    "cookies",
                    "host_key",
                ),
                BrowserKind::Firefox => (
                    vec![profile.data_dir.join("cookies.sqlite")],
                    "moz_cookies",
                    "host",
                ),
            };
            for db in dbs.iter().filter(|db| db.is_file()) {
                match sqlite::cookie_hosts(db, table, column) {
                    Ok(found) => {
                        for (host, count) in found {
                            let host = host.trim_start_matches('.').to_lowercase();
                            if host.is_empty() {
                                continue;
                            }
                            let entry = hosts.entry(host.clone()).or_insert_with(|| CookieHost {
                                host,
                                count: 0,
                                browsers: Vec::new(),
                            });
                            entry.count += count;
                            if !entry.browsers.contains(&browser.name) {
                                entry.browsers.push(browser.name.clone());
                            }
                        }
                    }
                    Err(e) => {
                        log::debug!("cannot read cookies from {}: {e}", db.display());
                        if !unreadable.contains(&browser.name) {
                            unreadable.push(browser.name.clone());
                        }
                    }
                }
            }
        }
    }
    CookieInventory {
        hosts: hosts.into_values().collect(),
        unreadable,
    }
}

/// Suggests keep-list entries: common sign-in sites that actually have
/// cookies on this PC.
pub fn suggest_keep(hosts: &[CookieHost]) -> Vec<String> {
    COMMON_SIGN_IN_DOMAINS
        .iter()
        .filter(|domain| {
            let single = KeepList(vec![(**domain).to_owned()]);
            hosts.iter().any(|h| single.keeps(&h.host))
        })
        .map(|d| (*d).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_normalisation() {
        assert_eq!(
            normalize_domain(" https://WWW.Example.com/path?q=1 ").as_deref(),
            Some("www.example.com")
        );
        assert_eq!(
            normalize_domain(".example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_domain("*.example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_domain("example.com:8443").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_domain("user@example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(normalize_domain("localhost").as_deref(), Some("localhost"));
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("not a domain"), None);
        assert_eq!(normalize_domain("   "), None);
    }

    #[test]
    fn keep_matching() {
        let keep = KeepList::new(&[
            "Example.com".to_owned(),
            "https://login.site.org/".to_owned(),
            "bad entry".to_owned(),
        ]);
        assert_eq!(keep.domains(), ["example.com", "login.site.org"]);
        assert!(keep.keeps("example.com"));
        assert!(keep.keeps(".example.com"));
        assert!(keep.keeps("accounts.EXAMPLE.com"));
        assert!(!keep.keeps("notexample.com"));
        assert!(!keep.keeps("example.com.evil.net"));
        assert!(keep.keeps("login.site.org"));
        assert!(!keep.keeps("site.org"));
        assert!(!KeepList::default().keeps("example.com"));
    }

    #[test]
    fn suggestions_only_for_present_sites() {
        let hosts = vec![
            CookieHost {
                host: "accounts.google.com".into(),
                count: 3,
                browsers: vec![],
            },
            CookieHost {
                host: "github.com".into(),
                count: 1,
                browsers: vec![],
            },
            CookieHost {
                host: "tracker.adnetwork.example".into(),
                count: 9,
                browsers: vec![],
            },
        ];
        assert_eq!(suggest_keep(&hosts), vec!["github.com", "google.com"]);
        assert!(suggest_keep(&[]).is_empty());
    }
}
