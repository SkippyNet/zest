//! Made-up data for `--demo`, so screenshots for documentation never show
//! anything from a real PC. Nothing here touches the system.

use std::path::PathBuf;
use std::time::Duration;
use zest_engine::cleaner::{
    Action, Catalog, CleanOptions, Finding, Group, GroupIcon, Rule, RuleKind, RuleResult,
    ScanReport, Section, Target,
};
use zest_engine::disk::{
    Analysis as DiskAnalysis, CategoryStat, DriveInfo, DriveKind, ExtensionStat, FileCategory,
    FileEntry, FolderNode,
};
use zest_engine::health::HealthReport;
use zest_engine::ops::Analysis;
use zest_engine::system::startup::StartupItem;
use zest_engine::updates::software::SoftwareUpdate;

const GB: u64 = 1024 * 1024 * 1024;
const MB: u64 = 1024 * 1024;

fn rule(id: &str, name: &str, kind: RuleKind, on: bool, admin: bool) -> Rule {
    Rule {
        id: id.to_owned(),
        name: name.to_owned(),
        description: String::new(),
        kind,
        default_enabled: on,
        warning: None,
        requires_admin: admin,
        age_filtered: false,
        actions: vec![Action::File(PathBuf::from(r"C:\Demo\placeholder"))],
    }
}

fn group(id: &str, section: Section, name: &str, icon: GroupIcon, rules: Vec<Rule>) -> Group {
    Group {
        id: id.to_owned(),
        section,
        name: name.to_owned(),
        icon,
        processes: Vec::new(),
        rules,
    }
}

fn result(group: &str, rule: &str, bytes: u64, items: u64) -> RuleResult {
    RuleResult {
        rule_id: rule.to_owned(),
        group_id: group.to_owned(),
        findings: vec![Finding {
            target: Target::File(PathBuf::from(r"C:\Demo\placeholder")),
            size: bytes,
            items,
            note: None,
        }],
        bytes,
        items,
        blocked_by: None,
        needs_admin: false,
        prune_roots: Vec::new(),
    }
}

/// A plausible catalog with analysis results for the Custom clean page.
pub fn clean_analysis() -> Analysis {
    use RuleKind::{Junk, Privacy};
    let browser = |id: &str| {
        vec![
            rule(&format!("{id}.cache"), "Internet cache", Junk, true, false),
            rule(
                &format!("{id}.diagnostics"),
                "Crash reports and metrics",
                Junk,
                true,
                false,
            ),
            rule(
                &format!("{id}.history"),
                "Browsing history",
                Privacy,
                false,
                false,
            ),
            rule(&format!("{id}.cookies"), "Cookies", Privacy, false, false),
            rule(&format!("{id}.session"), "Session", Privacy, false, false),
        ]
    };
    let catalog = Catalog {
        groups: vec![
            group(
                "win.system",
                Section::Windows,
                "System",
                GroupIcon::System,
                vec![
                    rule("win.recycle_bin", "Recycle Bin", Junk, true, false),
                    rule("win.temp", "Temporary files", Junk, true, false),
                    rule("win.memory_dumps", "Memory dumps", Junk, true, false),
                    rule(
                        "win.error_reports",
                        "Windows Error Reporting",
                        Junk,
                        true,
                        false,
                    ),
                    rule("win.logs", "Windows log files", Junk, true, false),
                    rule("win.thumbnails", "Thumbnail cache", Junk, true, false),
                    rule(
                        "win.shader_cache",
                        "DirectX shader cache",
                        Junk,
                        true,
                        false,
                    ),
                    rule(
                        "win.store_cache",
                        "Microsoft Store app caches",
                        Junk,
                        true,
                        false,
                    ),
                    rule(
                        "win.update_cache",
                        "Windows Update download cache",
                        Junk,
                        false,
                        false,
                    ),
                    rule("win.clipboard", "Clipboard", Privacy, false, false),
                    rule("win.dns_cache", "DNS cache", Privacy, false, false),
                ],
            ),
            group(
                "win.explorer",
                Section::Windows,
                "Windows Explorer",
                GroupIcon::Explorer,
                vec![
                    rule("win.recent_docs", "Recent documents", Privacy, false, false),
                    rule("win.run_history", "Run history", Privacy, false, false),
                    rule(
                        "win.jump_lists",
                        "Taskbar jump lists",
                        Privacy,
                        false,
                        false,
                    ),
                ],
            ),
            group(
                "chrome",
                Section::Browsers,
                "Google Chrome",
                GroupIcon::Chrome,
                browser("chrome"),
            ),
            group(
                "firefox",
                Section::Browsers,
                "Mozilla Firefox",
                GroupIcon::Firefox,
                browser("firefox"),
            ),
            group(
                "discord",
                Section::Applications,
                "Discord",
                GroupIcon::Chat,
                vec![rule("discord.cache", "Cache", Junk, true, false)],
            ),
            group(
                "spotify",
                Section::Applications,
                "Spotify",
                GroupIcon::Media,
                vec![rule("spotify.cache", "Streaming cache", Junk, true, false)],
            ),
            group(
                "steam",
                Section::Applications,
                "Steam",
                GroupIcon::Game,
                vec![
                    rule("steam.cache", "Web cache", Junk, true, false),
                    rule("steam.logs", "Logs and crash dumps", Junk, true, false),
                ],
            ),
        ],
    };
    let results = vec![
        result("win.system", "win.recycle_bin", 2 * GB + 310 * MB, 184),
        result("win.system", "win.temp", 3 * GB + 120 * MB, 2_914),
        result("win.system", "win.memory_dumps", 512 * MB, 4),
        result("win.system", "win.error_reports", 46 * MB, 212),
        result("win.system", "win.logs", 318 * MB, 640),
        result("win.system", "win.thumbnails", 97 * MB, 28),
        result("win.system", "win.shader_cache", 41 * MB, 356),
        result("win.system", "win.store_cache", 64 * MB, 1_102),
        result("chrome", "chrome.cache", GB + 740 * MB, 18_406),
        result("chrome", "chrome.diagnostics", 12 * MB, 9),
        result("firefox", "firefox.cache", 688 * MB, 9_231),
        result("discord", "discord.cache", 402 * MB, 3_118),
        result("spotify", "spotify.cache", 936 * MB, 5_407),
        result("steam", "steam.cache", 371 * MB, 2_260),
        result("steam", "steam.logs", 18 * MB, 64),
    ];
    let options = CleanOptions {
        enabled: catalog.default_selection(),
        min_age_hours: 24,
        ..Default::default()
    };
    Analysis {
        scan: ScanReport {
            bytes: results.iter().map(|r| r.bytes).sum(),
            items: results.iter().map(|r| r.items).sum(),
            results,
            elapsed: Duration::from_millis(184),
        },
        catalog,
        options,
    }
}

fn startup_item(name: &str) -> Option<StartupItem> {
    serde_json::from_value(serde_json::json!({
        "id": format!("reg|demo|{name}"),
        "kind": "RegistryRun",
        "name": name,
        "command": format!("C:\\Program Files\\{name}\\{name}.exe"),
        "target": null,
        "target_missing": false,
        "publisher": null,
        "description": null,
        "location": "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        "scope": "User",
        "enabled": true,
        "requires_admin": false,
        "toggle_requires_admin": false,
        "is_microsoft": false,
        "can_toggle": true,
        "can_delete": true,
        "details": {"Registry": {"key": {"root": "Hkcu", "path": "Software\\Demo"}, "value": name, "approved_key": null}}
    }))
    .ok()
}

fn update(name: &str, installed: &str, available: &str) -> SoftwareUpdate {
    SoftwareUpdate {
        name: name.to_owned(),
        id: format!("Demo.{name}"),
        installed_version: installed.to_owned(),
        available_version: available.to_owned(),
        source: "winget".to_owned(),
        requires_explicit_upgrade: false,
        id_truncated: false,
    }
}

/// A health report for a typical, slightly neglected PC.
pub fn health_report() -> HealthReport {
    let analysis = clean_analysis();
    HealthReport {
        trackers: 1_284,
        privacy_items: 0,
        junk_bytes: analysis.scan.bytes,
        junk_items: analysis.scan.items,
        blocked: Vec::new(),
        startup: ["Spotify", "Discord", "Steam", "Dropbox", "Zoom"]
            .iter()
            .filter_map(|name| startup_item(name))
            .collect(),
        updates: Some(vec![
            update("7-Zip", "23.01", "24.09"),
            update("VLC media player", "3.0.18", "3.0.21"),
            update("Notepad++", "8.5.8", "8.7.1"),
        ]),
        drive_total: 512 * GB,
        drive_free: 96 * GB,
        analysis,
    }
}

/// One system drive and one data drive.
pub fn drives() -> Vec<DriveInfo> {
    let drive =
        |letter: char, label: &str, total: u64, free: u64, system: bool, ssd: bool| DriveInfo {
            root: PathBuf::from(format!("{letter}:\\")),
            letter,
            label: label.to_owned(),
            file_system: "NTFS".to_owned(),
            total_bytes: total,
            free_bytes: free,
            kind: DriveKind::Fixed,
            is_system: system,
            is_ssd: Some(ssd),
            is_ready: true,
        };
    vec![
        drive('C', "Windows", 512 * GB, 96 * GB, true, true),
        drive('D', "Data", 2000 * GB, 1180 * GB, false, false),
    ]
}

fn folder(name: &str, path: &str, bytes: u64, files: u64) -> FolderNode {
    FolderNode {
        name: name.to_owned(),
        path: PathBuf::from(path),
        bytes,
        files,
        dirs: files / 12,
        own_bytes: 0,
        own_files: 0,
        children: Vec::new(),
        top_files: Vec::new(),
        pruned_children: 0,
        pruned_bytes: 0,
        inaccessible: false,
    }
}

fn file(path: &str, size: u64, category: FileCategory) -> FileEntry {
    FileEntry {
        path: PathBuf::from(path),
        size,
        modified: None,
        category,
    }
}

/// A disk analysis of the demo system drive.
pub fn disk_analysis() -> DiskAnalysis {
    let mut root = folder("C:\\", "C:\\", 416 * GB, 842_117);
    root.children = vec![
        folder("Users", r"C:\Users", 214 * GB, 388_204),
        folder("Program Files", r"C:\Program Files", 96 * GB, 141_950),
        folder("Games", r"C:\Games", 52 * GB, 18_332),
        folder("Windows", r"C:\Windows", 31 * GB, 201_774),
        folder(
            "Program Files (x86)",
            r"C:\Program Files (x86)",
            14 * GB,
            64_120,
        ),
        folder("ProgramData", r"C:\ProgramData", 7 * GB, 27_530),
    ];
    root.top_files = vec![
        file(r"C:\pagefile.sys", 8 * GB, FileCategory::System),
        file(r"C:\hiberfil.sys", 6 * GB, FileCategory::System),
    ];
    let stat = |category, bytes, files| CategoryStat {
        category,
        bytes,
        files,
    };
    let ext = |extension: &str, category, bytes, files| ExtensionStat {
        extension: extension.to_owned(),
        category,
        bytes,
        files,
    };
    DiskAnalysis {
        roots: vec![PathBuf::from("C:\\")],
        total_bytes: 416 * GB,
        total_files: 842_117,
        total_dirs: 96_402,
        links_skipped: 0,
        inaccessible_dirs: 0,
        cloud_bytes: 0,
        cloud_files: 0,
        categories: vec![
            stat(FileCategory::Video, 118 * GB, 1_204),
            stat(FileCategory::Applications, 92 * GB, 61_330),
            stat(FileCategory::Pictures, 64 * GB, 48_911),
            stat(FileCategory::System, 51 * GB, 204_118),
            stat(FileCategory::Archives, 33 * GB, 2_870),
            stat(FileCategory::Other, 24 * GB, 388_260),
            stat(FileCategory::Music, 18 * GB, 9_640),
            stat(FileCategory::Documents, 11 * GB, 22_417),
            stat(FileCategory::Code, 4 * GB, 102_904),
            stat(FileCategory::Email, GB, 463),
        ],
        extensions: vec![
            ext("mp4", FileCategory::Video, 84 * GB, 640),
            ext("dll", FileCategory::Applications, 41 * GB, 38_112),
            ext("jpg", FileCategory::Pictures, 38 * GB, 31_204),
            ext("mkv", FileCategory::Video, 29 * GB, 96),
            ext("zip", FileCategory::Archives, 22 * GB, 1_410),
            ext("exe", FileCategory::Applications, 19 * GB, 6_208),
        ],
        largest_files: vec![
            file(
                r"C:\Users\Alex\Videos\Holiday 2025.mp4",
                14 * GB,
                FileCategory::Video,
            ),
            file(
                r"C:\Users\Alex\Downloads\ubuntu-24.04-desktop.iso",
                6 * GB,
                FileCategory::Archives,
            ),
            file(
                r"C:\Games\Open World\Data\Textures.pak",
                5 * GB,
                FileCategory::Other,
            ),
            file(
                r"C:\Users\Alex\Videos\Wedding raw footage.mkv",
                4 * GB,
                FileCategory::Video,
            ),
            file(
                r"C:\Users\Alex\Documents\Backups\photos-2023.zip",
                3 * GB,
                FileCategory::Archives,
            ),
        ],
        tree: vec![root],
        elapsed_ms: 3_120,
    }
}
