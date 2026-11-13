# Feature plan and status

This is the working plan for Zest: every feature the product is meant to ship with,
where it lives in the code, and how far it has been verified. It is the checklist to
review before the repository goes public.

**Status key**

| Mark | Meaning |
|---|---|
| Done | Implemented, covered by automated tests, and exercised against a real PC |
| Built | Implemented and tested on sandboxed data; the real action has not been run on a live system yet |
| Planned | Not started |

"Exercised against a real PC" always means the read-only half (analyze, scan, list). Anything
that changes a system was only ever run on temporary folders, scratch registry keys and
processes created by the tests themselves.

## Clean

| Feature | Status | Where | Notes |
|---|---|---|---|
| Health check (privacy, space, speed, security) | Done | `engine/health.rs`, `app/pages/health.rs` | One-click fix cleans privacy and space; speed and security open their pages for review |
| Custom clean: Windows rules | Done | `cleaner/catalog/windows.rs` | Recycle Bin, temp files, dumps, logs, error reports, thumbnails, shader cache, Store caches, Delivery Optimization, update cache, broken shortcuts, clipboard, DNS |
| Custom clean: Explorer and advanced privacy rules | Done | `cleaner/catalog/windows.rs` | Recent documents, Run history, search and dialog history, jump lists, event logs, prefetch, tray and window caches, User Assist |
| Custom clean: browsers | Done | `cleaner/catalog/browser_rules.rs` | Chrome, Edge, Brave, Opera, Opera GX, Vivaldi, Chromium, Yandex, Firefox, LibreWolf, Waterfox; every profile |
| Custom clean: applications | Done | `cleaner/catalog/apps.rs` | Chat, media, games, graphics drivers, productivity and developer tools; groups appear only when installed |
| Analyze before clean, with per-item review and exclude | Done | `cleaner/scan.rs`, `app/pages/clean.rs` | |
| Cookie keep list with suggestions | Done | `cleaner/cookies.rs`, `cleaner/sqlite.rs` | Selective deletion inside the browser databases |
| Firefox history removal that keeps bookmarks | Built | `cleaner/sqlite.rs` | Tested on a synthetic database; no Firefox profile was available on the development PC |
| Secure deletion (1, 3, 7, 35 passes) | Done | `core/delete.rs` | |
| Custom include and exclude lists | Done | `cleaner/exclude.rs`, `app/pages/options.rs` | |
| Multi-user cleaning | Built | `core/paths.rs`, `engine/ops.rs` | Needs an elevated run with a second account to verify end to end |
| Registry cleaner, 15 categories | Done | `registry/` | Scan verified on a real registry; fixes verified on scratch keys under the current user only |
| Registry backup and restore | Done | `core/reg.rs`, `app/pages/registry.rs` | Round trip tested through the system's own importer |

## Speed

| Feature | Status | Where | Notes |
|---|---|---|---|
| Startup apps, scheduled tasks, services, context menu | Done (listing) / Built (changes) | `system/startup/` | Listings matched Windows' own tools entry for entry; enable, disable and delete ran only on test data |
| Performance optimizer: live usage per app | Done | `performance/monitor.rs` | Figures matched Windows counters |
| Sleep (efficiency mode or freeze), wake on use | Built | `performance/control.rs` | Verified on helper processes started by the tests, not yet on everyday apps |
| Free up memory, power plan switch | Built | `performance/` | |

## Software

| Feature | Status | Where | Notes |
|---|---|---|---|
| Uninstaller with leftover scan | Done (listing) / Built (uninstall) | `system/apps/` | No real program was uninstalled during development |
| Microsoft Store app removal | Built | `system/apps/store.rs` | |
| Software updater | Done (check) / Built (install) | `updates/software.rs` | Listing parser validated line by line against live output; no real upgrade was run |
| Driver updater | Done (list, search) / Built (install, backup) | `updates/drivers.rs` | Windows Update search verified; install and backup need an elevated test |
| Browser extension inventory with access rating | Done | `cleaner/extensions.rs` | Removal tested on synthetic profiles |

## Storage

| Feature | Status | Where | Notes |
|---|---|---|---|
| Disk analyzer | Done | `disk/analyze.rs` | About 1 million files in under 5 seconds on an NVMe drive |
| Duplicate finder | Done (scan) / Built (delete) | `disk/duplicates.rs` | Always keeps one verified copy per set |
| Large-file and empty-folder finders | Done (scan) | `disk/` | Engines exist; empty-folder finder has no page yet |
| Free-space wipe and file shredder | Built | `disk/wipe.rs` | Tested with a small size limit on temporary folders |
| Whole-drive erase | Built | `disk/wipe.rs` | Refusal logic and the folder-level core are tested; never run on a real drive |

## System and automation

| Feature | Status | Where | Notes |
|---|---|---|---|
| System Restore: list, create, remove | Built | `system/restore.rs` | Needs an elevated test; only the access-denied path has run |
| Scheduled cleaning | Done | `engine/schedule.rs` | Task definitions accepted by Task Scheduler for every frequency |
| Smart cleaning tray agent | Done | `app/agent.rs`, `engine/smart.rs` | Starts, idles at about 18 MB and exits when switched off; notifications not yet seen by a person |
| Command line interface | Done | `cli/` | All read-only commands and `clean --dry-run` exercised |
| Settings, history, portable mode | Done | `engine/settings.rs`, `engine/history.rs`, `core/paths.rs` | |
| Light and dark themes | Done | `app/theme.rs` | |

## Before going public

1. Run the app elevated once and walk through the "Built" rows above on a test machine or VM:
   a real clean, a registry fix and restore, disabling a startup entry, an uninstall with leftovers,
   one software update, a restore point, and a free-space wipe on a spare drive.
2. README screenshots come from `zest.exe --demo --page <name> --screenshots docs/screenshots`,
   which shows made-up data. Add more pages to `crates/app/src/demo.rs` to picture other features;
   never publish screenshots of a real PC without checking what they reveal.
3. License: Apache-2.0. Product name: Zest.
4. Releases: signed installer and portable zip, versioning, protected `main` branch.
5. In-app update check against the latest release.

## Deliberately out of scope

- Driver downloads from anywhere other than Windows Update.
- "RAM booster" style features that show a number without a lasting effect.
- Anything that needs a kernel driver or disables Windows security features.
