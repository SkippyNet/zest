//! The privacy settings Zest can switch.
//!
//! Every registry value below was checked against Microsoft's own
//! documentation; the `source` of each entry says where. Values that only
//! community guides describe are deliberately left out, as is anything that
//! would weaken Windows security or Windows Update.

use super::{Category, Mechanism, RegWrite, Scope, Tweak};

/// "Manage connections from Windows operating system components to Microsoft services".
const MANAGE: &str = "https://learn.microsoft.com/windows/privacy/manage-connections-from-windows-operating-system-components-to-microsoft-services";
/// "Optimizing Windows 10, version 2004 for a Virtual Desktop Infrastructure (VDI) role".
const VDI: &str = "https://learn.microsoft.com/windows-server/remote/remote-desktop-services/rds-vdi-recommendations-2004";
const CSP_SYSTEM: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-system";
const CSP_EXPERIENCE: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-experience";
const CSP_PRIVACY: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-privacy";
const CSP_SEARCH: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-search";
const CSP_TEXT_INPUT: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-textinput";
const CSP_ERROR_REPORTING: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-errorreporting";
const CSP_START: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-start";
const CSP_NEWS: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-newsandinterests";
const CSP_EXPLORER: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-admx-windowsexplorer";
const CSP_WINDOWS_AI: &str =
    "https://learn.microsoft.com/windows/client-management/mdm/policy-csp-windowsai";

const CLOUD_CONTENT: &str = r"Software\Policies\Microsoft\Windows\CloudContent";
const CONTENT_DELIVERY: &str = r"Software\Microsoft\Windows\CurrentVersion\ContentDeliveryManager";
const INPUT_PERSONALIZATION: &str = r"Software\Microsoft\InputPersonalization";
const POLICY_EXPLORER: &str = r"Software\Policies\Microsoft\Windows\Explorer";
const POLICY_SEARCH: &str = r"Software\Policies\Microsoft\Windows\Windows Search";
const POLICY_SYSTEM: &str = r"Software\Policies\Microsoft\Windows\System";

/// A value that only exists once it has been set: the Windows default is
/// "not there".
const fn unset(key: &'static str, name: &'static str, private: u32) -> RegWrite {
    RegWrite {
        key,
        name,
        private,
        default: None,
    }
}

/// A value with a well-defined default that Windows itself writes.
const fn toggle(key: &'static str, name: &'static str, private: u32, default: u32) -> RegWrite {
    RegWrite {
        key,
        name,
        private,
        default: Some(default),
    }
}

// Windows build numbers the catalog refers to.
const BUILD_WIN10_2004: u32 = 19041;
const BUILD_WIN10_20H2: u32 = 19042;
const BUILD_WIN10_22H2: u32 = 19045;
const BUILD_WIN11_21H2: u32 = 22000;
const BUILD_WIN11_22H2: u32 = 22621;
const BUILD_WIN11_23H2: u32 = 22631;
const BUILD_WIN11_24H2: u32 = 26100;

/// Every setting, in the order it is shown.
pub static CATALOG: &[Tweak] = &[
    // ── Diagnostics ──────────────────────────────────────────────────
    Tweak {
        id: "diagnostic-data",
        title: "Send only required diagnostic data",
        description: "Windows sends only the diagnostic data it needs to stay secure and up to date. The optional diagnostic data switch in Settings is turned off and locked.",
        note: "",
        caution: "",
        category: Category::Diagnostics,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_SYSTEM,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\DataCollection",
            "AllowTelemetry",
            1,
        )],
    },
    Tweak {
        id: "tailored-experiences",
        title: "Stop tailored experiences",
        description: "Windows stops using your diagnostic data to personalize tips, ads and recommendations.",
        note: "",
        caution: "",
        category: Category::Diagnostics,
        scope: Scope::User,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_EXPERIENCE,
        writes: &[unset(
            CLOUD_CONTENT,
            "DisableTailoredExperiencesWithDiagnosticData",
            1,
        )],
    },
    Tweak {
        id: "feedback-requests",
        title: "Stop feedback requests",
        description: "Windows no longer asks you to rate it or answer questions about it.",
        note: "",
        caution: "",
        category: Category::Diagnostics,
        scope: Scope::User,
        mechanism: Mechanism::Setting,
        recommended: true,
        min_build: None,
        max_build: None,
        source: MANAGE,
        writes: &[
            unset(r"Software\Microsoft\Siuf\Rules", "NumberOfSIUFInPeriod", 0),
            unset(r"Software\Microsoft\Siuf\Rules", "PeriodInNanoSeconds", 0),
        ],
    },
    Tweak {
        id: "typing-samples",
        title: "Stop sending typing and handwriting samples",
        description: "Windows stops sending samples of what you type and write to Microsoft to improve recognition.",
        note: "",
        caution: "",
        category: Category::Diagnostics,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_TEXT_INPUT,
        writes: &[unset(
            r"Software\Microsoft\Windows\CurrentVersion\Policies\TextInput",
            "AllowLinguisticDataCollection",
            0,
        )],
    },
    Tweak {
        id: "problem-reports",
        title: "Stop sending problem reports",
        description: "Windows no longer sends crash reports to Microsoft. Solutions to known problems stop showing up in Security and Maintenance.",
        note: "",
        caution: "",
        category: Category::Diagnostics,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: None,
        max_build: None,
        source: CSP_ERROR_REPORTING,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\Windows Error Reporting",
            "Disabled",
            1,
        )],
    },
    // ── Ads and suggestions ──────────────────────────────────────────
    Tweak {
        id: "advertising-id",
        title: "Turn off the advertising ID",
        description: "Apps can no longer use an ID tied to this PC to show you personalized ads.",
        note: "",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_PRIVACY,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\AdvertisingInfo",
            "DisabledByGroupPolicy",
            1,
        )],
    },
    Tweak {
        id: "suggestions-and-tips",
        title: "Stop suggestions and tips",
        description: "Windows stops showing suggested content in Settings, app suggestions in Start and tips while you work.",
        note: "",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::User,
        mechanism: Mechanism::Setting,
        recommended: true,
        min_build: None,
        max_build: None,
        source: VDI,
        writes: &[
            toggle(CONTENT_DELIVERY, "SubscribedContent-338393Enabled", 0, 1),
            toggle(CONTENT_DELIVERY, "SubscribedContent-353694Enabled", 0, 1),
            toggle(CONTENT_DELIVERY, "SubscribedContent-353696Enabled", 0, 1),
            toggle(CONTENT_DELIVERY, "SubscribedContent-338388Enabled", 0, 1),
            toggle(CONTENT_DELIVERY, "SubscribedContent-338389Enabled", 0, 1),
            toggle(CONTENT_DELIVERY, "SystemPaneSuggestionsEnabled", 0, 1),
        ],
    },
    Tweak {
        id: "third-party-suggestions",
        title: "Stop suggestions from other companies",
        description: "Windows no longer promotes apps and content from other publishers on the lock screen, in Start and in tips.",
        note: "",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::User,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_EXPERIENCE,
        writes: &[unset(CLOUD_CONTENT, "DisableThirdPartySuggestions", 1)],
    },
    Tweak {
        id: "setup-reminders",
        title: "Stop \u{201c}finish setting up\u{201d} reminders",
        description: "Windows stops showing the full-screen prompt that suggests more Microsoft services after updates.",
        note: "",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::User,
        mechanism: Mechanism::Setting,
        recommended: true,
        min_build: None,
        max_build: None,
        source: VDI,
        writes: &[toggle(
            r"Software\Microsoft\Windows\CurrentVersion\UserProfileEngagement",
            "ScoobeSystemSettingEnabled",
            0,
            1,
        )],
    },
    Tweak {
        id: "start-recommended-sites",
        title: "Hide recommended websites in Start",
        description: "The Recommended section of Start no longer suggests websites based on your browsing.",
        note: "",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::User,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: Some(BUILD_WIN11_22H2),
        max_build: None,
        source: CSP_START,
        writes: &[unset(
            POLICY_EXPLORER,
            "HideRecommendedPersonalizedSites",
            1,
        )],
    },
    Tweak {
        id: "widgets",
        title: "Turn off Widgets",
        description: "Removes the Widgets board and its news feed from the taskbar.",
        note: "",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: Some(BUILD_WIN11_21H2),
        max_build: None,
        source: CSP_NEWS,
        writes: &[unset(
            r"Software\Policies\Microsoft\Dsh",
            "AllowNewsAndInterests",
            0,
        )],
    },
    Tweak {
        id: "news-and-interests",
        title: "Turn off News and interests",
        description: "Removes the weather and news button from the taskbar.",
        note: "Windows 11 replaced this button with Widgets.",
        caution: "",
        category: Category::Suggestions,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: Some(BUILD_WIN10_2004),
        max_build: Some(BUILD_WIN10_22H2),
        source: MANAGE,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\Windows Feeds",
            "EnableFeeds",
            0,
        )],
    },
    // ── Activity and search ──────────────────────────────────────────
    Tweak {
        id: "activity-history",
        title: "Turn off activity history",
        description: "Windows stops keeping a history of the apps, files and sites you use and no longer sends it to Microsoft.",
        note: "",
        caution: "",
        category: Category::Activity,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_PRIVACY,
        writes: &[
            unset(POLICY_SYSTEM, "EnableActivityFeed", 0),
            unset(POLICY_SYSTEM, "PublishUserActivities", 0),
            unset(POLICY_SYSTEM, "UploadUserActivities", 0),
        ],
    },
    Tweak {
        id: "search-highlights",
        title: "Turn off search highlights",
        description: "The search box stops showing daily illustrations and trending content from the web.",
        note: "",
        caution: "",
        category: Category::Activity,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: Some(BUILD_WIN10_20H2),
        max_build: None,
        source: CSP_SEARCH,
        writes: &[unset(POLICY_SEARCH, "EnableDynamicContentInWSB", 0)],
    },
    Tweak {
        id: "search-location",
        title: "Keep location out of search",
        description: "Windows Search can no longer use the location of this PC to tailor results.",
        note: "",
        caution: "",
        category: Category::Activity,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: None,
        max_build: None,
        source: CSP_SEARCH,
        writes: &[unset(POLICY_SEARCH, "AllowSearchToUseLocation", 0)],
    },
    Tweak {
        id: "app-launch-tracking",
        title: "Stop tracking which apps you open",
        description: "Windows stops recording app launches. The Most used list in Start goes away.",
        note: "",
        caution: "",
        category: Category::Activity,
        scope: Scope::User,
        mechanism: Mechanism::Setting,
        recommended: false,
        min_build: None,
        max_build: None,
        source: MANAGE,
        writes: &[toggle(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
            "Start_TrackProgs",
            0,
            1,
        )],
    },
    Tweak {
        id: "explorer-search-history",
        title: "Stop File Explorer remembering searches",
        description: "File Explorer no longer stores or suggests what you typed into its search box.",
        note: "",
        caution: "",
        category: Category::Activity,
        scope: Scope::User,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: Some(BUILD_WIN10_2004),
        max_build: None,
        source: CSP_EXPLORER,
        writes: &[unset(POLICY_EXPLORER, "DisableSearchBoxSuggestions", 1)],
    },
    Tweak {
        id: "language-list",
        title: "Hide your language list from websites",
        description: "Websites can no longer read the list of languages set up on this PC. Some sites may open in a different language.",
        note: "",
        caution: "",
        category: Category::Activity,
        scope: Scope::User,
        mechanism: Mechanism::Setting,
        recommended: false,
        min_build: None,
        max_build: None,
        source: MANAGE,
        writes: &[unset(
            r"Control Panel\International\User Profile",
            "HttpAcceptLanguageOptOut",
            1,
        )],
    },
    // ── Typing and speech ────────────────────────────────────────────
    Tweak {
        id: "typing-personalization",
        title: "Turn off typing and handwriting personalization",
        description: "Windows stops building a personal dictionary from what you type and write. Suggestions become less personal.",
        note: "",
        caution: "",
        category: Category::Input,
        scope: Scope::User,
        mechanism: Mechanism::Setting,
        recommended: false,
        min_build: None,
        max_build: None,
        source: VDI,
        writes: &[
            toggle(INPUT_PERSONALIZATION, "RestrictImplicitInkCollection", 1, 0),
            toggle(
                INPUT_PERSONALIZATION,
                "RestrictImplicitTextCollection",
                1,
                0,
            ),
            toggle(
                r"Software\Microsoft\InputPersonalization\TrainedDataStore",
                "HarvestContacts",
                0,
                1,
            ),
            toggle(
                r"Software\Microsoft\Personalization\Settings",
                "AcceptedPrivacyPolicy",
                0,
                1,
            ),
        ],
    },
    Tweak {
        id: "online-speech",
        title: "Turn off online speech recognition",
        description: "Your voice is no longer sent to Microsoft. Voice typing and dictation that rely on the online service stop working.",
        note: "",
        caution: "",
        category: Category::Input,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: None,
        max_build: None,
        source: CSP_PRIVACY,
        writes: &[unset(
            r"Software\Policies\Microsoft\InputPersonalization",
            "AllowInputPersonalization",
            0,
        )],
    },
    // ── AI features ──────────────────────────────────────────────────
    Tweak {
        id: "recall-snapshots",
        title: "Keep Recall from saving snapshots",
        description: "Recall cannot save pictures of your screen, and snapshots it already saved are deleted.",
        note: "Only Copilot+ PCs have Recall. On other PCs this simply keeps it off.",
        caution: "Snapshots Recall has already saved are deleted and cannot be brought back.",
        category: Category::Ai,
        scope: Scope::User,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: Some(BUILD_WIN11_24H2),
        max_build: None,
        source: CSP_WINDOWS_AI,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\WindowsAI",
            "DisableAIDataAnalysis",
            1,
        )],
    },
    Tweak {
        id: "copilot-panel",
        title: "Turn off Copilot in Windows",
        description: "Removes the Copilot button and side panel from the taskbar.",
        note: "From Windows 11 24H2 on, Copilot is an ordinary app. Remove it on the Uninstall page instead.",
        caution: "",
        category: Category::Ai,
        scope: Scope::User,
        mechanism: Mechanism::Policy,
        recommended: true,
        min_build: Some(BUILD_WIN10_22H2),
        max_build: Some(BUILD_WIN11_23H2),
        source: CSP_WINDOWS_AI,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\WindowsCopilot",
            "TurnOffWindowsCopilot",
            1,
        )],
    },
    // ── Devices and location ─────────────────────────────────────────
    Tweak {
        id: "location",
        title: "Turn off location",
        description: "Windows and apps can no longer find where this PC is. Weather, maps, Find my device and the automatic time zone stop working.",
        note: "",
        caution: "",
        category: Category::Devices,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: None,
        max_build: None,
        source: CSP_SYSTEM,
        writes: &[unset(
            r"Software\Policies\Microsoft\Windows\LocationAndSensors",
            "DisableLocation",
            1,
        )],
    },
    Tweak {
        id: "clipboard-sync",
        title: "Keep the clipboard on this PC",
        description: "What you copy can no longer be synced to your other devices through your Microsoft account.",
        note: "",
        caution: "",
        category: Category::Devices,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: None,
        max_build: None,
        source: CSP_PRIVACY,
        writes: &[unset(POLICY_SYSTEM, "AllowCrossDeviceClipboard", 0)],
    },
    Tweak {
        id: "shared-experiences",
        title: "Turn off shared experiences",
        description: "Apps on your other devices can no longer open apps or continue what you were doing on this PC.",
        note: "",
        caution: "",
        category: Category::Devices,
        scope: Scope::Machine,
        mechanism: Mechanism::Policy,
        recommended: false,
        min_build: None,
        max_build: None,
        source: MANAGE,
        writes: &[unset(POLICY_SYSTEM, "EnableCdp", 0)],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_stable_looking() {
        let mut seen = HashSet::new();
        for tweak in CATALOG {
            assert!(seen.insert(tweak.id), "duplicate id {}", tweak.id);
            assert!(
                tweak
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "id {} should be kebab-case",
                tweak.id
            );
        }
    }

    #[test]
    fn every_entry_is_complete() {
        for tweak in CATALOG {
            assert!(!tweak.title.is_empty(), "{}", tweak.id);
            assert!(tweak.description.ends_with('.'), "{}", tweak.id);
            assert!(
                tweak.source.starts_with("https://learn.microsoft.com/"),
                "{} needs a Microsoft source",
                tweak.id
            );
            assert!(!tweak.writes.is_empty(), "{}", tweak.id);
            if let (Some(min), Some(max)) = (tweak.min_build, tweak.max_build) {
                assert!(min <= max, "{}", tweak.id);
            }
            for write in tweak.writes {
                assert!(
                    !write.key.starts_with('\\') && !write.key.ends_with('\\'),
                    "{}",
                    tweak.id
                );
                assert!(!write.key.to_ascii_uppercase().starts_with("HK"));
                assert!(!write.name.is_empty(), "{}", tweak.id);
                assert_ne!(
                    Some(write.private),
                    write.default,
                    "{}: private and default must differ",
                    tweak.id
                );
            }
        }
    }

    #[test]
    fn policies_live_under_a_policy_key_and_settings_do_not() {
        for tweak in CATALOG {
            for write in tweak.writes {
                let key = write.key.to_ascii_lowercase();
                let under_policies = key.starts_with(r"software\policies\")
                    || key.starts_with(r"software\microsoft\windows\currentversion\policies\");
                match tweak.mechanism {
                    Mechanism::Policy => {
                        assert!(under_policies, "{} is not under a policy key", tweak.id);
                        assert_eq!(
                            write.default, None,
                            "{}: an unset policy is the default",
                            tweak.id
                        );
                    }
                    Mechanism::Setting => {
                        assert!(!under_policies, "{} is under a policy key", tweak.id);
                        assert_eq!(
                            tweak.scope,
                            Scope::User,
                            "{}: plain settings are per user",
                            tweak.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn nothing_touches_security_or_update_components() {
        let forbidden = [
            "windows defender",
            "smartscreen",
            "windowsupdate",
            "firewall",
            "policies\\system\\enablelua",
            "wuauserv",
            "securityhealth",
            "apphost",
            "deliveryoptimization",
        ];
        for tweak in CATALOG {
            for write in tweak.writes {
                let target = format!("{}\\{}", write.key, write.name).to_ascii_lowercase();
                for word in forbidden {
                    assert!(
                        !target.contains(word),
                        "{} touches {word}: {target}",
                        tweak.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_recommended_set_has_no_feature_removals() {
        let optional = [
            "problem-reports",
            "widgets",
            "news-and-interests",
            "app-launch-tracking",
            "explorer-search-history",
            "language-list",
            "typing-personalization",
            "online-speech",
            "location",
            "clipboard-sync",
            "shared-experiences",
        ];
        for id in optional {
            let tweak = CATALOG.iter().find(|t| t.id == id).expect(id);
            assert!(!tweak.recommended, "{id} costs a feature");
        }
        assert!(CATALOG.iter().filter(|t| t.recommended).count() >= 10);
    }
}
