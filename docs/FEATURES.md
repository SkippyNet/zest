# Feature plan and status

Every feature Zest ships with, where it lives in the code, and how far it has been verified.
A cleaner changes other people's PCs, so this page separates what has really been run from what
has only been tested in a sandbox.

## Status key

| Mark | Meaning |
|---|---|
| **Proven** | The real action has been run on real Windows machines and checked: by the [live test](../.github/workflows/live-test.yml) on throw-away machines (Windows Server 2022 and 2025, as administrator), or, for tools that only read, on a developer PC (Windows 11). |
| **Read-verified** | The part that only reads (analyze, scan, list) has been run on a real PC. The part that changes something has only run on sandboxed data: temporary folders, scratch registry keys, processes started by the tests. |
| **Sandbox only** | Tested on sandboxed data and canned tool output only; never run for real, usually because it needs administrator rights or hardware the developers did not have. |
| **Planned** | Not built. |

Two limits apply to everything below:

- **Nobody has clicked through the app by hand yet.** Every page has been rendered and inspected
  in screenshot mode on three machines, and the engines behind the buttons are tested, but dialogs
  and multi-step flows have not been walked through by a person.
- **The live test runs on Windows Server**, which shares its code base with Windows 10 and 11 but
  is not identical. Nothing has been run for real on Windows 10.

If you try a row marked *Read-verified* or *Sandbox only* on a spare PC or a virtual machine,
please [report](https://github.com/SkippyNet/zest/issues) how it went.

## Clean

| Feature | Status | Where | Notes |
|---|---|---|---|
| Custom clean: engine and Windows junk rules | Proven | `cleaner/`, `cleaner/catalog/windows.rs` | Live test: planted junk is removed; documents, files newer than 24 hours and the target of a junction are untouched |
| Custom clean: privacy rules for Windows and Explorer | Read-verified | `cleaner/catalog/windows.rs` | Recent documents, Run and search history, jump lists, event logs |
| Custom clean: browsers | Read-verified | `cleaner/catalog/browser_rules.rs` | Chromium family and Firefox family, every profile. No Firefox profile was available for a real scan |
| Custom clean: applications | Read-verified | `cleaner/catalog/apps.rs` | Groups appear only when the program is installed |
| Health check | Read-verified | `engine/health.rs`, `app/pages/health.rs` | Its fix uses the same engine as Custom clean |
| Analyze, per-item review, exclude | Read-verified | `cleaner/scan.rs`, `app/pages/clean.rs` | |
| Cookie keep list | Read-verified | `cleaner/cookies.rs`, `cleaner/sqlite.rs` | Selective deletion inside the browser databases tested on copies |
| Secure deletion (1, 3, 7, 35 passes) | Sandbox only | `core/delete.rs` | |
| Custom include and exclude lists | Sandbox only | `cleaner/exclude.rs` | |
| Multi-user cleaning | Sandbox only | `core/paths.rs`, `engine/ops.rs` | Needs a PC with a second account |
| Registry cleaner, 15 categories | Proven (startup and App Paths entries) / Read-verified (the rest) | `registry/` | Live test: planted dead entries are found and removed, a healthy neighbour survives |
| Registry backup and restore | Proven | `core/reg.rs` | Live test: the `.reg` backup imports and the removed entries are back |
| Windows privacy, 25 settings | Proven (apply and revert) | `tweaks/privacy/` | Every value checked against Microsoft documentation. Live test applies the recommended set, reads it back, reverts. Whether Windows *Home* honours each policy is not verified |

## Speed

| Feature | Status | Where | Notes |
|---|---|---|---|
| Startup apps, tasks, services, context menu | Read-verified | `system/startup/` | Listings matched Windows' own tools; enable, disable and delete ran on test data only |
| Performance: live usage per app | Read-verified | `performance/monitor.rs` | Figures matched Windows counters |
| Sleep (efficiency mode or freeze), wake on use | Sandbox only | `performance/control.rs` | Run on helper processes started by the tests |
| Boost mode | Sandbox only | `performance/boost.rs` | Power plan and sleep changes tested with stand-ins; a journal restores the previous state after a crash |
| Free up memory, power plan switch | Sandbox only | `performance/` | |

## Software

| Feature | Status | Where | Notes |
|---|---|---|---|
| Uninstaller with leftover scan | Read-verified | `system/apps/` | No real program has been uninstalled |
| Microsoft Store app removal | Sandbox only | `system/apps/store.rs` | |
| Software updates: finding them | Read-verified | `updates/software/` | Through Windows Package Manager; parser tested against its real translations in 11 languages |
| Software updates: catalog fallback | Read-verified | `updates/software/catalog.rs` | Without Windows Package Manager. On the developer PC it agreed with Windows Package Manager on 30 of 30 updates |
| Software updates: installing | Sandbox only | `updates/software/` | No real upgrade has been run |
| Driver updates | Read-verified (list, search) / Sandbox only (install, backup) | `updates/drivers/` | |
| Browser extension inventory | Read-verified | `cleaner/extensions.rs` | Removal tested on synthetic profiles |

## Storage

| Feature | Status | Where | Notes |
|---|---|---|---|
| Disk analyzer | Proven | `disk/analyze.rs`, `core/fastdir.rs` | 1.17 million files in about 3 seconds on an NVMe drive |
| Duplicate finder | Proven (finding) / Sandbox only (deleting) | `disk/duplicates.rs` | Always keeps one verified copy per set |
| Empty folders | Read-verified | `disk/empty.rs` | |
| Free-space wipe and file shredder | Sandbox only | `disk/wipe.rs` | Tested with a size limit on temporary folders |
| Whole-drive erase | Sandbox only | `disk/wipe.rs` | Refusal logic and the folder-level core are tested; never run on a drive |

## Protect

| Feature | Status | Where | Notes |
|---|---|---|---|
| File backup: create, list, verify, restore to another folder | Proven | `backup/` | Live test: two versions, verify, restore, byte-for-byte comparison |
| File backup: restore in place with overwrite, prune, repair | Sandbox only | `backup/` | No shadow copies: files held open exclusively are skipped and listed |
| System image | Sandbox only | `backup/image.rs` | Drives Windows' own imaging tool. Command building and parsers are tested; an image has never been created or restored by the developers |
| System Restore: list, create, remove | Sandbox only | `system/restore.rs` | Only the access-denied path has run |

## System

| Feature | Status | Where | Notes |
|---|---|---|---|
| System info, drive and battery health | Proven on one laptop | `hardware/` | Cross-checked against Windows' own figures. Desktop, SATA, RAID, multi-drive and ARM paths are covered by made-up records only |
| Windows tools | Proven (DNS flush, component store check) / Sandbox only (the rest) | `tweaks/tools/` | Output handling for system file check and image repair rests on sample output |

## Automation and plumbing

| Feature | Status | Where | Notes |
|---|---|---|---|
| Command line | Proven | `cli/` | The live test drives everything through it |
| Scheduled cleaning | Proven | `engine/schedule.rs`, `app/agent.rs` | Task Scheduler accepts the task (CI); the clean it starts runs in the live test |
| Smart cleaning tray agent | Read-verified | `app/agent.rs`, `engine/smart.rs` | Starts, idles and exits when switched off; its notifications have not been seen by a person |
| Installer | Proven | `installer/zest.iss` | The release workflow installs, runs and uninstalls it before publishing |
| Zest's own updates | Proven | `updates/selfupdate/` | Live test: a portable copy and an installed copy built with a lower version number update themselves to the latest release (download, checksum, file swap or installer). The "Update now" button in the app has not been clicked by a person |
| Settings, history, portable mode | Proven | `engine/`, `core/paths.rs` | |
| App window on machines without a graphics driver | Proven | `app/gpu.rs` | The live test draws every page on a machine with no GPU |
| Light and dark themes | Read-verified | `app/theme.rs` | |

## Known gaps

- **Elevating as a different account.** If you run Zest as administrator using another account's
  password, per-user tools (Custom clean, privacy switches for the current user) act on *that*
  account. Zest detects this and shows a warning, but it cannot yet point those tools at the
  account that is signed in.
- **Proxies.** The update check and the catalog download honour proxy environment variables, not
  the Windows proxy settings.
- **Backups** are not encrypted, are not scheduled, and do not keep permissions or alternate data
  streams.
- **Code signing.** Releases are not signed; they come with checksums and build attestations.
- **Languages.** The interface is English only.

## Roadmap

- Reading the NTFS file table directly for faster scans when elevated.
- Scheduled and encrypted backups.
- Feeding drive health and privacy state into the Health check.
- A signed installer and translations.

## Deliberately out of scope

- Driver downloads from anywhere other than Windows Update.
- "Memory booster" features that show a number without a lasting effect.
- Anything that needs a kernel driver or switches off Windows security features.
