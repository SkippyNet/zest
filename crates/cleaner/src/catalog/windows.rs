//! Rules for Windows itself.

use super::{Ctx, group, rule};
use crate::model::{Group, GroupIcon, Section, Special};
use zest_core::reg::RegPath;

const EXPLORER: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer";
const LOCAL_SETTINGS: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows";

pub(super) fn groups(ctx: &Ctx) -> Vec<Group> {
    [system(ctx), explorer(ctx), advanced(ctx)]
        .into_iter()
        .flatten()
        .collect()
}

fn system(ctx: &Ctx) -> Option<Group> {
    let rules = vec![
        rule(
            "win.recycle_bin",
            "Recycle Bin",
            "Permanently removes everything in the Recycle Bin on all drives.",
        )
        .special(Special::RecycleBin)
        .build(),
        rule(
            "win.temp",
            "Temporary files",
            "Leftover working files in your temporary folder. Files still in use are skipped.",
        )
        .aged()
        .contents(ctx.user("{temp}"))
        .build(),
        rule(
            "win.temp_system",
            "System temporary files",
            "Leftover working files in the Windows temporary folder.",
        )
        .aged()
        .admin()
        .contents(ctx.machine(r"{windows}\Temp"))
        .build(),
        rule(
            "win.memory_dumps",
            "Memory dumps",
            "Crash dumps written when Windows or a program stops unexpectedly. Only useful for debugging.",
        )
        .admin()
        .file(ctx.machine(r"{windows}\MEMORY.DMP"))
        .files(ctx.machine(r"{windows}\Minidump"), "*.dmp", false)
        .files(ctx.machine(r"{windows}\LiveKernelReports"), "*.dmp", true)
        .contents(ctx.user(r"{local}\CrashDumps"))
        .build(),
        rule(
            "win.error_reports",
            "Windows Error Reporting",
            "Queued and archived problem reports.",
        )
        .contents(ctx.user(r"{local}\Microsoft\Windows\WER\ReportArchive"))
        .contents(ctx.user(r"{local}\Microsoft\Windows\WER\ReportQueue"))
        .contents(ctx.machine(r"{programdata}\Microsoft\Windows\WER\ReportArchive"))
        .contents(ctx.machine(r"{programdata}\Microsoft\Windows\WER\ReportQueue"))
        .contents(ctx.machine(r"{programdata}\Microsoft\Windows\WER\Temp"))
        .build(),
        rule(
            "win.logs",
            "Windows log files",
            "Setup, servicing and diagnostic logs that Windows no longer needs.",
        )
        .admin()
        .files(ctx.machine(r"{windows}\Logs"), "*.log;*.etl;*.cab;*.bak", true)
        .files(ctx.machine(r"{windows}"), "*.log", false)
        .files(ctx.machine(r"{windows}\inf"), "*.log", false)
        .files(ctx.machine(r"{windows}\Panther"), "*.log;*.etl", true)
        .files(ctx.machine(r"{windows}\debug"), "*.log", true)
        .files(ctx.machine(r"{windows}\security\logs"), "*.log;*.old", false)
        .files(ctx.machine(r"{windows}\SoftwareDistribution\DataStore\Logs"), "*.log;*.jrs", false)
        .files(ctx.machine(r"{windows}\System32\wbem\Logs"), "*.log;*.lo_", false)
        .files(ctx.machine(r"{windows}\System32\LogFiles"), "*.log;*.etl;*.bak", true)
        .build(),
        rule(
            "win.thumbnails",
            "Thumbnail cache",
            "Cached picture and icon previews. Windows rebuilds them as you browse.",
        )
        .files(
            ctx.user(r"{local}\Microsoft\Windows\Explorer"),
            "thumbcache_*.db;iconcache_*.db",
            false,
        )
        .build(),
        rule(
            "win.inet_cache",
            "Temporary Internet files",
            "Web content cached by Windows components and built-in web views.",
        )
        .contents(ctx.user(r"{local}\Microsoft\Windows\INetCache"))
        .build(),
        rule(
            "win.shader_cache",
            "DirectX shader cache",
            "Compiled graphics shaders. Games and apps recreate them on next start.",
        )
        .contents(ctx.user(r"{local}\D3DSCache"))
        .build(),
        rule(
            "win.store_cache",
            "Microsoft Store app caches",
            "Temporary files and web caches of apps installed from the Microsoft Store.",
        )
        .contents(ctx.user(r"{local}\Packages\*\AC\Temp"))
        .contents(ctx.user(r"{local}\Packages\*\AC\INetCache"))
        .build(),
        rule(
            "win.delivery_optimization",
            "Delivery Optimization files",
            "Update pieces Windows keeps to share with other PCs.",
        )
        .admin()
        .contents(ctx.machine(
            r"{windows}\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
        ))
        .build(),
        rule(
            "win.chkdsk",
            "Chkdsk file fragments",
            "Fragments of lost files recovered by disk checks.",
        )
        .admin()
        .files(ctx.machine(r"{systemdrive}\FOUND.*"), "*.chk", false)
        .build(),
        rule(
            "win.update_cache",
            "Windows Update download cache",
            "Downloaded update installers. Windows downloads them again if an update is still pending.",
        )
        .admin()
        .off()
        .contents(ctx.machine(r"{windows}\SoftwareDistribution\Download"))
        .build(),
        broken_shortcuts(ctx),
        rule(
            "win.clipboard",
            "Clipboard",
            "Clears whatever is currently copied to the clipboard.",
        )
        .privacy()
        .special(Special::Clipboard)
        .build(),
        rule(
            "win.dns_cache",
            "DNS cache",
            "Clears the list of recently resolved website addresses.",
        )
        .privacy()
        .special(Special::DnsCache)
        .build(),
    ];
    // Wildcard-only rules (e.g. chkdsk fragments) have no actions when
    // nothing matches on this PC.
    let rules = rules
        .into_iter()
        .filter(|r| !r.actions.is_empty())
        .collect();
    group(
        "win.system",
        Section::Windows,
        "System",
        GroupIcon::System,
        &[],
        rules,
    )
}

fn broken_shortcuts(ctx: &Ctx) -> crate::model::Rule {
    let mut builder = rule(
        "win.broken_shortcuts",
        "Broken shortcuts",
        "Start Menu and Desktop shortcuts whose program no longer exists.",
    )
    .off();
    let mut dirs = ctx.user(r"{roaming}\Microsoft\Windows\Start Menu\Programs");
    dirs.extend(ctx.machine(r"{programdata}\Microsoft\Windows\Start Menu\Programs"));
    dirs.extend(ctx.user(r"{profile}\Desktop"));
    for dir in dirs {
        builder = builder.special(Special::BrokenShortcuts { dir });
    }
    builder.build()
}

fn explorer_keys(ctx: &Ctx, sub: &str) -> Vec<RegPath> {
    ctx.user_keys(&format!("{EXPLORER}\\{sub}"))
}

fn explorer(ctx: &Ctx) -> Option<Group> {
    let rules = vec![
        rule(
            "win.recent_docs",
            "Recent documents",
            "The list of recently opened files shown in File Explorer and the Start Menu.",
        )
        .privacy()
        .files(
            ctx.user(r"{roaming}\Microsoft\Windows\Recent"),
            "*.lnk",
            false,
        )
        .reg_values(explorer_keys(ctx, "RecentDocs"), true)
        .build(),
        rule(
            "win.run_history",
            "Run history",
            "Commands typed into the Run dialog (Win+R).",
        )
        .privacy()
        .reg_values(explorer_keys(ctx, "RunMRU"), false)
        .build(),
        rule(
            "win.explorer_history",
            "Explorer and search history",
            "Typed addresses, search terms and the recent locations of Open/Save dialogs.",
        )
        .privacy()
        .reg_values(explorer_keys(ctx, "TypedPaths"), false)
        .reg_values(explorer_keys(ctx, "WordWheelQuery"), false)
        .reg_values(explorer_keys(ctx, r"ComDlg32\OpenSavePidlMRU"), true)
        .reg_values(explorer_keys(ctx, r"ComDlg32\LastVisitedPidlMRU"), false)
        .reg_values(explorer_keys(ctx, r"ComDlg32\CIDSizeMRU"), false)
        .reg_values(explorer_keys(ctx, "Map Network Drive MRU"), false)
        .build(),
        rule(
            "win.jump_lists",
            "Taskbar jump lists",
            "Recent items shown when you right-click a taskbar or Start Menu icon.",
        )
        .privacy()
        .warn("Items you pinned inside jump lists are removed as well.")
        .contents(ctx.user(r"{roaming}\Microsoft\Windows\Recent\AutomaticDestinations"))
        .contents(ctx.user(r"{roaming}\Microsoft\Windows\Recent\CustomDestinations"))
        .build(),
    ];
    let rules = rules
        .into_iter()
        .filter(|r| !r.actions.is_empty())
        .collect();
    group(
        "win.explorer",
        Section::Windows,
        "Windows Explorer",
        GroupIcon::Explorer,
        &[],
        rules,
    )
}

fn user_assist_keys(ctx: &Ctx) -> Vec<RegPath> {
    let mut keys = Vec::new();
    for root in explorer_keys(ctx, "UserAssist") {
        let Ok(key) = root.open() else {
            continue;
        };
        for guid in key.enum_keys().flatten() {
            keys.push(root.child(&guid).child("Count"));
        }
    }
    keys
}

fn advanced(ctx: &Ctx) -> Option<Group> {
    let rules = vec![
        rule(
            "win.event_logs",
            "Windows event logs",
            "Clears every Windows event log. Useful for privacy, but removes troubleshooting history.",
        )
        .privacy()
        .admin()
        .warn("Event logs help diagnose problems. Clearing them cannot be undone.")
        .special(Special::EventLogs)
        .build(),
        rule(
            "win.prefetch",
            "Old prefetch data",
            "Launch traces of programs that have not been started for over a month.",
        )
        .off()
        .admin()
        .old_files(ctx.machine(r"{windows}\Prefetch"), "*.pf", false, 30)
        .build(),
        rule(
            "win.menu_order",
            "Menu order cache",
            "The remembered manual ordering of Start Menu and Favorites entries.",
        )
        .privacy()
        .reg_values(explorer_keys(ctx, "MenuOrder"), true)
        .build(),
        rule(
            "win.tray_cache",
            "Tray notifications cache",
            "The history of icons that have appeared in the notification area.",
        )
        .privacy()
        .reg_value(
            ctx.user_keys(&format!(r"{LOCAL_SETTINGS}\CurrentVersion\TrayNotify")),
            "IconStreams",
        )
        .reg_value(
            ctx.user_keys(&format!(r"{LOCAL_SETTINGS}\CurrentVersion\TrayNotify")),
            "PastIconsStream",
        )
        .build(),
        rule(
            "win.window_cache",
            "Window size and location cache",
            "Remembered folder window sizes, positions and view settings.",
        )
        .privacy()
        .warn("All folders return to their default view settings.")
        .reg_values(ctx.user_keys(&format!(r"{LOCAL_SETTINGS}\Shell\Bags")), true)
        .reg_values(ctx.user_keys(&format!(r"{LOCAL_SETTINGS}\Shell\BagMRU")), true)
        .build(),
        rule(
            "win.user_assist",
            "User Assist history",
            "Counts of how often each program was started, used for the most-used list.",
        )
        .privacy()
        .reg_values(user_assist_keys(ctx), false)
        .build(),
        rule(
            "win.iis_logs",
            "IIS log files",
            "Request logs of the built-in web server.",
        )
        .off()
        .admin()
        .files(ctx.machine(r"{systemdrive}\inetpub\logs\LogFiles"), "*.log", true)
        .build(),
    ];
    // Rules whose keys do not exist on this PC end up without actions.
    let rules = rules
        .into_iter()
        .filter(|r| !r.actions.is_empty())
        .collect();
    group(
        "win.advanced",
        Section::Windows,
        "Advanced",
        GroupIcon::Advanced,
        &[],
        rules,
    )
}
