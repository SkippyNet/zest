# Safety

A cleaner is only useful if you can trust it not to remove the wrong thing. These are the rules
Zest's code follows, and where they live in the source so you can check.

## Look first, act second

Every tool has a read-only step (analyze, scan, check, list) and a separate step that changes
something. The second step works from the list the first one produced, so what you reviewed is
what is acted on.

## Locations Zest refuses to delete

Deletion goes through one guard (`crates/core/src/safety.rs`). It refuses, whatever a rule says:

- drive roots and the system folders directly under them (`Program Files`, `ProgramData`, boot
  and recovery folders, ...),
- the Windows folder, except a short allow-list of cache, log and temporary locations inside it,
- the `Users` folder, each profile folder, and a profile's own top-level folders (Documents,
  Desktop, Pictures, OneDrive, `AppData\Local`, `AppData\Roaming`, ...) as a whole,
- device paths, relative paths and paths containing `..`,
- anything reached through a junction or a symbolic link. Links are never followed; a link found
  inside a folder being cleaned is treated as a single entry, not as a door to somewhere else.

Short (8.3) file names and junctions in the parent path are resolved before the check, so a path
cannot dress up as something harmless.

## Registry changes are backed up

Before the registry cleaner, the privacy switches, the startup manager or the uninstaller's
leftover removal change the registry, the affected keys are exported to a standard `.reg` file in
the data folder. *Registry → Backups* lists them and can restore one with a click. Because they
are ordinary `.reg` files you can also double-click them in Explorer.

A second guard refuses to delete registry keys Windows needs to start.

## Deleting, and deleting for good

- A normal clean deletes files. Tools that work on *your* files (duplicate finder, empty folders)
  send them to the Recycle Bin by default; change that under *Options → General*.
- Secure deletion overwrites a file before removing it (1, 3, 7 or 35 passes). On SSDs an
  overwrite cannot be guaranteed to reach every copy of the data, because of how flash storage
  works; Zest tells you when the drive is an SSD.
- The drive wiper will not erase the drive Windows runs from.

## Cloud files

Files that exist only in the cloud (OneDrive "online-only") are recognised and never opened,
because opening them would download them.

## What Zest sends over the network

- **Update check:** once a day, a request to GitHub for the newest Zest release. It carries the
  Zest version and nothing about you or your PC. Switch it off under *Options → About*.
- **Software updates:** when you ask for them, Windows Package Manager (or the public catalog)
  is asked what is new.
- **Driver updates:** Windows Update is asked, when you press the button.

There is no telemetry, no account, no advertising and no analytics.

## What has and has not been tested

The [feature plan](https://github.com/SkippyNet/zest/blob/main/docs/FEATURES.md) lists each
feature with one of three marks: verified on real PCs, tested only on sandboxed data, or planned.
Features that need administrator rights are the ones most often still marked as sandbox-tested.
If you try one on a spare machine, please [report](https://github.com/SkippyNet/zest/issues) how
it went.
