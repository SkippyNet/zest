//! Rules for third-party applications. A group only appears when the
//! application's data is actually present on this PC.

use super::{Ctx, RuleBuilder, group, rule};
use crate::model::{Group, GroupIcon, Rule, Section};
use std::path::PathBuf;
use zest_core::reg::RegPath;

/// Cache folders shared by Electron / Chromium-embedded applications.
const ELECTRON_CACHE_DIRS: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    r"Service Worker\CacheStorage",
    r"Service Worker\ScriptCache",
    r"Crashpad\reports",
];

fn electron_cache(ctx: &Ctx, mut builder: RuleBuilder, bases: &[&str]) -> RuleBuilder {
    for base in bases {
        for dir in ELECTRON_CACHE_DIRS {
            builder = builder.contents(ctx.user(&format!(r"{base}\{dir}")));
        }
    }
    builder
}

/// Builds an application group when any of `detect` exists.
fn app(
    ctx: &Ctx,
    id: &str,
    name: &str,
    icon: GroupIcon,
    processes: &[&str],
    detect: &[&str],
    rules: impl FnOnce() -> Vec<Rule>,
) -> Option<Group> {
    if !ctx.any_exists(detect) {
        return None;
    }
    group(id, Section::Applications, name, icon, processes, rules())
}

/// A simple Electron app with a cache rule and a logs rule.
fn electron_app(
    ctx: &Ctx,
    id: &str,
    name: &str,
    icon: GroupIcon,
    processes: &[&str],
    bases: &[&str],
    log_dirs: &[&str],
) -> Option<Group> {
    app(ctx, id, name, icon, processes, bases, || {
        let mut rules = vec![
            electron_cache(
                ctx,
                rule(
                    &format!("{id}.cache"),
                    "Cache",
                    "Cached web content and compiled code. Recreated automatically.",
                ),
                bases,
            )
            .build(),
        ];
        if !log_dirs.is_empty() {
            let mut logs = rule(&format!("{id}.logs"), "Logs", "Diagnostic log files.");
            for dir in log_dirs {
                logs = logs.contents(ctx.user(dir));
            }
            rules.push(logs.build());
        }
        rules
    })
}

fn steam_dirs(ctx: &Ctx) -> Vec<PathBuf> {
    let mut dirs = ctx.machine(r"{programfiles86}\Steam");
    if ctx.registry
        && let Some(path) = RegPath::hkcu(r"Software\Valve\Steam").read_string("SteamPath")
    {
        let path = PathBuf::from(path.replace('/', "\\"));
        if !dirs.iter().any(|d| zest_core::paths::same_path(d, &path)) {
            dirs.push(path);
        }
    }
    dirs
}

fn join_all(dirs: &[PathBuf], rel: &str) -> Vec<PathBuf> {
    dirs.iter().map(|d| d.join(rel)).collect()
}

fn key_if_exists(ctx: &Ctx, path: &str) -> Vec<RegPath> {
    ctx.user_keys(path)
        .into_iter()
        .filter(RegPath::exists)
        .collect()
}

pub(super) fn groups(ctx: &Ctx) -> Vec<Group> {
    let mut out: Vec<Option<Group>> = Vec::new();

    // ── Communication ────────────────────────────────────────────────
    out.push(electron_app(
        ctx,
        "discord",
        "Discord",
        GroupIcon::Chat,
        &["discord.exe", "discordcanary.exe", "discordptb.exe"],
        &[
            r"{roaming}\discord",
            r"{roaming}\discordcanary",
            r"{roaming}\discordptb",
        ],
        &[],
    ));
    out.push(electron_app(
        ctx,
        "slack",
        "Slack",
        GroupIcon::Chat,
        &["slack.exe"],
        &[r"{roaming}\Slack"],
        &[r"{roaming}\Slack\logs"],
    ));
    out.push(app(
        ctx,
        "teams",
        "Microsoft Teams",
        GroupIcon::Chat,
        &["teams.exe", "ms-teams.exe"],
        &[
            r"{roaming}\Microsoft\Teams",
            r"{local}\Packages\MSTeams_8wekyb3d8bbwe",
        ],
        || {
            let mut cache = electron_cache(
                ctx,
                rule(
                    "teams.cache",
                    "Cache",
                    "Cached web content and media. Recreated automatically.",
                ),
                &[
                    r"{roaming}\Microsoft\Teams",
                    r"{local}\Packages\MSTeams_8wekyb3d8bbwe\LocalCache\Microsoft\MSTeams\EBWebView\*",
                ],
            );
            for dir in ["blob_storage", "tmp"] {
                cache = cache.contents(ctx.user(&format!(r"{{roaming}}\Microsoft\Teams\{dir}")));
            }
            vec![
                cache.build(),
                rule("teams.logs", "Logs", "Diagnostic log files.")
                    .contents(ctx.user(
                        r"{local}\Packages\MSTeams_8wekyb3d8bbwe\LocalCache\Microsoft\MSTeams\Logs",
                    ))
                    .files(ctx.user(r"{roaming}\Microsoft\Teams"), "logs.txt;old_logs_*.txt", false)
                    .build(),
            ]
        },
    ));
    out.push(electron_app(
        ctx,
        "signal",
        "Signal",
        GroupIcon::Chat,
        &["signal.exe"],
        &[r"{roaming}\Signal"],
        &[r"{roaming}\Signal\logs"],
    ));
    out.push(app(
        ctx,
        "telegram",
        "Telegram Desktop",
        GroupIcon::Chat,
        &["telegram.exe"],
        &[r"{roaming}\Telegram Desktop\tdata"],
        || {
            vec![
                rule(
                    "telegram.cache",
                    "Media cache",
                    "Cached photos, videos and stickers. Downloaded again when viewed.",
                )
                .contents(ctx.user(r"{roaming}\Telegram Desktop\tdata\user_data*\cache"))
                .contents(ctx.user(r"{roaming}\Telegram Desktop\tdata\user_data*\media_cache"))
                .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "zoom",
        "Zoom",
        GroupIcon::Chat,
        &["zoom.exe"],
        &[r"{roaming}\Zoom"],
        || {
            vec![
                rule("zoom.logs", "Logs", "Diagnostic log files.")
                    .contents(ctx.user(r"{roaming}\Zoom\logs"))
                    .build(),
            ]
        },
    ));

    // ── Media ────────────────────────────────────────────────────────
    out.push(app(
        ctx,
        "spotify",
        "Spotify",
        GroupIcon::Media,
        &["spotify.exe"],
        &[
            r"{local}\Spotify",
            r"{local}\Packages\SpotifyAB.SpotifyMusic_zpdnekdrzrea0",
        ],
        || {
            vec![
                rule(
                    "spotify.cache",
                    "Streaming cache",
                    "Cached music data and web content. Songs stream again when played.",
                )
                .contents(ctx.user(r"{local}\Spotify\Data"))
                .contents(ctx.user(r"{local}\Spotify\Browser\Cache"))
                .contents(ctx.user(r"{local}\Spotify\Browser\Code Cache"))
                .contents(ctx.user(r"{local}\Spotify\Browser\GPUCache"))
                .contents(ctx.user(
                    r"{local}\Packages\SpotifyAB.SpotifyMusic_zpdnekdrzrea0\LocalCache\Spotify\Data",
                ))
                .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "vlc",
        "VLC media player",
        GroupIcon::Media,
        &["vlc.exe"],
        &[r"{roaming}\vlc"],
        || {
            vec![
                rule(
                    "vlc.art",
                    "Album art cache",
                    "Downloaded cover art. Fetched again when needed.",
                )
                .contents(ctx.user(r"{roaming}\vlc\art"))
                .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "adobe_media",
        "Adobe media cache",
        GroupIcon::Media,
        &[],
        &[r"{roaming}\Adobe\Common"],
        || {
            vec![
                rule(
                    "adobe_media.cache",
                    "Media cache files",
                    "Conformed audio and peak files of video projects. Regenerated when a project is opened, which can take a while.",
                )
                .off()
                .contents(ctx.user(r"{roaming}\Adobe\Common\Media Cache Files"))
                .contents(ctx.user(r"{roaming}\Adobe\Common\Media Cache"))
                .contents(ctx.user(r"{roaming}\Adobe\Common\Peak Files"))
                .build(),
            ]
        },
    ));

    // ── Games ────────────────────────────────────────────────────────
    let steam = steam_dirs(ctx);
    let steam_present = steam.iter().any(|d| d.is_dir()) || ctx.any_exists(&[r"{local}\Steam"]);
    if steam_present {
        out.push(group(
            "steam",
            Section::Applications,
            "Steam",
            GroupIcon::Game,
            &["steam.exe"],
            vec![
                rule(
                    "steam.cache",
                    "Web cache",
                    "Cached store and community pages.",
                )
                .contents(ctx.user(r"{local}\Steam\htmlcache"))
                .contents(join_all(&steam, r"appcache\httpcache"))
                .build(),
                rule(
                    "steam.logs",
                    "Logs and crash dumps",
                    "Diagnostic logs and crash dumps.",
                )
                .contents(join_all(&steam, "logs"))
                .contents(join_all(&steam, "dumps"))
                .build(),
            ],
        ));
    }
    out.push(app(
        ctx,
        "epic",
        "Epic Games Launcher",
        GroupIcon::Game,
        &["epicgameslauncher.exe"],
        &[r"{local}\EpicGamesLauncher"],
        || {
            vec![
                rule("epic.cache", "Web cache", "Cached store pages.")
                    .contents(ctx.user(r"{local}\EpicGamesLauncher\Saved\webcache*"))
                    .build(),
                rule("epic.logs", "Logs", "Diagnostic log files.")
                    .contents(ctx.user(r"{local}\EpicGamesLauncher\Saved\Logs"))
                    .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "gog",
        "GOG Galaxy",
        GroupIcon::Game,
        &["galaxyclient.exe"],
        &[r"{programdata}\GOG.com\Galaxy"],
        || {
            vec![
                rule("gog.cache", "Web cache", "Cached store pages.")
                    .contents(ctx.machine(r"{programdata}\GOG.com\Galaxy\webcache"))
                    .build(),
                rule("gog.logs", "Logs", "Diagnostic log files.")
                    .contents(ctx.machine(r"{programdata}\GOG.com\Galaxy\logs"))
                    .build(),
            ]
        },
    ));

    // ── Graphics drivers ─────────────────────────────────────────────
    out.push(app(
        ctx,
        "nvidia",
        "NVIDIA",
        GroupIcon::Graphics,
        &[],
        &[
            r"{local}\NVIDIA",
            r"{programdata}\NVIDIA Corporation",
        ],
        || {
            vec![
                rule(
                    "nvidia.shader_cache",
                    "Shader cache",
                    "Compiled shaders. Games recreate them on next start.",
                )
                .contents(ctx.user(r"{local}\NVIDIA\DXCache"))
                .contents(ctx.user(r"{local}\NVIDIA\GLCache"))
                .contents(ctx.user(r"{local}\NVIDIA Corporation\NV_Cache"))
                .contents(ctx.machine(r"{programdata}\NVIDIA Corporation\NV_Cache"))
                .build(),
                rule(
                    "nvidia.installers",
                    "Downloaded driver installers",
                    "Driver packages kept after installation. Only needed to reinstall the same version offline.",
                )
                .off()
                .admin()
                .contents(ctx.machine(r"{programdata}\NVIDIA Corporation\Downloader"))
                .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "amd",
        "AMD",
        GroupIcon::Graphics,
        &[],
        &[r"{local}\AMD"],
        || {
            let mut cache = rule(
                "amd.shader_cache",
                "Shader cache",
                "Compiled shaders. Games recreate them on next start.",
            );
            for dir in ["DxCache", "DxcCache", "GLCache", "VkCache", "OglCache"] {
                cache = cache.contents(ctx.user(&format!(r"{{local}}\AMD\{dir}")));
            }
            vec![cache.build()]
        },
    ));
    out.push(app(
        ctx,
        "intel",
        "Intel Graphics",
        GroupIcon::Graphics,
        &[],
        &[r"{locallow}\Intel\ShaderCache"],
        || {
            vec![
                rule(
                    "intel.shader_cache",
                    "Shader cache",
                    "Compiled shaders. Games recreate them on next start.",
                )
                .contents(ctx.user(r"{locallow}\Intel\ShaderCache"))
                .build(),
            ]
        },
    ));

    // ── Productivity ─────────────────────────────────────────────────
    out.push(app(
        ctx,
        "acrobat",
        "Adobe Acrobat Reader",
        GroupIcon::Office,
        &["acrord32.exe", "acrobat.exe"],
        &[r"{local}\Adobe\Acrobat"],
        || {
            vec![
                rule(
                    "acrobat.cache",
                    "Cache",
                    "Cached thumbnails and web content.",
                )
                .contents(ctx.user(r"{local}\Adobe\Acrobat\*\Cache"))
                .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "onedrive",
        "Microsoft OneDrive",
        GroupIcon::Office,
        &[],
        &[r"{local}\Microsoft\OneDrive\logs"],
        || {
            vec![
                rule(
                    "onedrive.logs",
                    "Logs",
                    "Sync diagnostic logs. Your files are not touched.",
                )
                .contents(ctx.user(r"{local}\Microsoft\OneDrive\logs"))
                .build(),
            ]
        },
    ));
    out.push(app(
        ctx,
        "java",
        "Java",
        GroupIcon::App,
        &[],
        &[r"{locallow}\Sun\Java\Deployment\cache"],
        || {
            vec![
                rule(
                    "java.cache",
                    "Deployment cache",
                    "Cached applets and web-start applications.",
                )
                .contents(ctx.user(r"{locallow}\Sun\Java\Deployment\cache"))
                .build(),
            ]
        },
    ));

    // ── Recently-used lists of common tools (registry) ───────────────
    let seven_zip = key_if_exists(ctx, r"Software\7-Zip");
    if !seven_zip.is_empty() {
        let sub =
            |name: &str| -> Vec<RegPath> { seven_zip.iter().map(|k| k.child(name)).collect() };
        out.push(group(
            "sevenzip",
            Section::Applications,
            "7-Zip",
            GroupIcon::App,
            &[],
            vec![
                rule(
                    "sevenzip.history",
                    "Recent archives and folders",
                    "The lists of recently used archives and extraction folders.",
                )
                .privacy()
                .reg_value(sub("Compression"), "ArcHistory")
                .reg_value(sub("Extraction"), "PathHistory")
                .reg_value(sub("FM"), "FolderHistory")
                .reg_value(sub("FM"), "CopyHistory")
                .build(),
            ],
        ));
    }
    let winrar = key_if_exists(ctx, r"Software\WinRAR");
    if !winrar.is_empty() {
        let sub = |name: &str| -> Vec<RegPath> { winrar.iter().map(|k| k.child(name)).collect() };
        out.push(group(
            "winrar",
            Section::Applications,
            "WinRAR",
            GroupIcon::App,
            &[],
            vec![
                rule(
                    "winrar.history",
                    "Recent archives and folders",
                    "The lists of recently used archives and dialog entries.",
                )
                .privacy()
                .reg_values(sub("ArcHistory"), false)
                .reg_values(sub("DialogEditHistory"), true)
                .build(),
            ],
        ));
    }
    let applets = key_if_exists(ctx, r"Software\Microsoft\Windows\CurrentVersion\Applets");
    if !applets.is_empty() {
        let sub = |name: &str| -> Vec<RegPath> { applets.iter().map(|k| k.child(name)).collect() };
        out.push(group(
            "accessories",
            Section::Applications,
            "Windows accessories",
            GroupIcon::App,
            &[],
            vec![
                rule(
                    "accessories.recent",
                    "Recent file lists",
                    "Recently opened files in Paint and WordPad, and the last key visited in Registry Editor.",
                )
                .privacy()
                .reg_values(sub(r"Paint\Recent File List"), false)
                .reg_values(sub(r"Wordpad\Recent File List"), false)
                .reg_value(sub("Regedit"), "LastKey")
                .build(),
            ],
        ));
    }

    // ── Developer tools ──────────────────────────────────────────────
    out.push(electron_app(
        ctx,
        "vscode",
        "Visual Studio Code",
        GroupIcon::Developer,
        &["code.exe"],
        &[r"{roaming}\Code"],
        &[r"{roaming}\Code\logs"],
    ));
    out.push(electron_app(
        ctx,
        "github_desktop",
        "GitHub Desktop",
        GroupIcon::Developer,
        &["githubdesktop.exe"],
        &[r"{roaming}\GitHub Desktop"],
        &[r"{roaming}\GitHub Desktop\logs"],
    ));
    out.push(app(
        ctx,
        "jetbrains",
        "JetBrains IDEs",
        GroupIcon::Developer,
        &[],
        &[r"{local}\JetBrains"],
        || {
            vec![
                rule("jetbrains.logs", "Logs", "IDE diagnostic logs.")
                    .contents(ctx.user(r"{local}\JetBrains\*\log"))
                    .build(),
                rule(
                    "jetbrains.caches",
                    "Caches and indexes",
                    "Project indexes and caches. The IDE rebuilds them on next start, which can take several minutes.",
                )
                .off()
                .contents(ctx.user(r"{local}\JetBrains\*\caches"))
                .build(),
            ]
        },
    ));
    out.push(dev_caches(ctx));

    out.into_iter().flatten().collect()
}

/// Package-manager and build caches. Everything here is off by default:
/// it is safe to remove but has to be downloaded or rebuilt afterwards.
fn dev_caches(ctx: &Ctx) -> Option<Group> {
    let candidates: Vec<(&str, &str, &str, Vec<&str>)> = vec![
        (
            "dev.npm",
            "npm cache",
            "Downloaded npm packages. Restored from the registry on the next install.",
            vec![r"{local}\npm-cache\_cacache", r"{local}\npm-cache\_logs"],
        ),
        (
            "dev.yarn",
            "Yarn cache",
            "Downloaded Yarn packages. Restored from the registry on the next install.",
            vec![r"{local}\Yarn\Cache"],
        ),
        (
            "dev.pip",
            "pip cache",
            "Downloaded and built Python packages. Fetched again when needed.",
            vec![r"{local}\pip\cache"],
        ),
        (
            "dev.nuget",
            "NuGet HTTP cache",
            "Cached NuGet feed responses. Fetched again when needed.",
            vec![r"{local}\NuGet\v3-cache", r"{local}\NuGet\plugins-cache"],
        ),
        (
            "dev.go",
            "Go build cache",
            "Compiled Go packages. Rebuilt on the next build.",
            vec![r"{local}\go-build"],
        ),
        (
            "dev.cargo",
            "Cargo registry cache",
            "Downloaded Rust crate archives and unpacked sources. Downloaded again on the next build.",
            vec![
                r"{profile}\.cargo\registry\cache",
                r"{profile}\.cargo\registry\src",
            ],
        ),
        (
            "dev.gradle",
            "Gradle caches",
            "Downloaded dependencies and build caches. Restored on the next build.",
            vec![r"{profile}\.gradle\caches"],
        ),
    ];
    let mut rules = Vec::new();
    for (id, name, description, dirs) in candidates {
        if !ctx.any_exists(&dirs) {
            continue;
        }
        let mut builder = rule(id, name, description).off();
        for dir in dirs {
            builder = builder.contents(ctx.user(dir));
        }
        rules.push(builder.build());
    }
    group(
        "dev",
        Section::Applications,
        "Developer tools",
        GroupIcon::Developer,
        &[],
        rules,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::MachineDirs;
    use crate::model::Action;
    use zest_core::paths::UserDirs;

    #[test]
    fn groups_appear_only_when_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let machine = MachineDirs::under(tmp.path());
        let users = [UserDirs::for_profile(
            tmp.path().join("Users").join("dev"),
            None,
        )];
        let ctx = Ctx {
            users: &users,
            machine: &machine,
            registry: false,
        };
        assert!(groups(&ctx).is_empty());

        std::fs::create_dir_all(users[0].roaming.join(r"discord\Cache")).unwrap();
        std::fs::create_dir_all(users[0].local.join(r"npm-cache\_cacache")).unwrap();
        std::fs::create_dir_all(machine.program_files_x86.join(r"Steam\logs")).unwrap();
        let found = groups(&ctx);
        let ids: Vec<&str> = found.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["discord", "steam", "dev"]);

        let discord = &found[0];
        assert_eq!(discord.rules.len(), 1);
        assert!(discord.rules[0].actions.iter().any(
            |a| matches!(a, Action::Files { dir, .. } if dir == &users[0].roaming.join(r"discord\Cache"))
        ));
        // Developer caches must be opt-in.
        let dev = &found[2];
        assert_eq!(dev.rules.len(), 1);
        assert_eq!(dev.rules[0].id, "dev.npm");
        assert!(!dev.rules[0].default_enabled);
    }
}
