# Changelog

Notable changes in each release. Versions follow [semantic versioning](https://semver.org/).

## 0.9.1

Groundwork for code-signed releases. Nothing about cleaning has changed.

- The installer shows the [privacy policy](https://github.com/SkippyNet/zest/blob/main/PRIVACY.md) before it installs anything, and has a new option, **Check for new versions of Zest once a day**. Untick it and Zest never goes online by itself. Updating from 0.9.0 keeps the choice you made in the app.
- `zest-cli update auto on|off` switches the same daily check from the command line; `zest-cli update auto` shows where it stands.
- `zest-cli.exe` now carries the product name, version and icon in its file details, like `zest.exe`.
- The portable zip and the installed copy include the privacy policy as `PRIVACY.txt`.
- A [code signing policy](https://github.com/SkippyNet/zest#code-signing-policy) is published. Downloads are still unsigned in this release; the release pipeline is ready to sign them once the project has a signing account.

## 0.9.0

First public release.

### Clean
- Health check: privacy, space, speed and security in one view, with one button for what is safe to fix.
- Custom clean: more than 70 rules for Windows, browsers and applications, with per-item review, exclusions, a cookie keep list and optional secure deletion (1, 3, 7 or 35 passes).
- Registry cleaner: 15 categories, a `.reg` backup before every fix and one-click restore.
- Windows privacy: switches for what Windows collects, suggests and advertises, each one reversible.

### Speed
- Startup manager for apps, scheduled tasks, services and context-menu entries.
- Performance optimizer: live processor and memory use per app, sleep for background apps, memory trim, power plans.

### Software
- Uninstaller with a scan for leftovers, including Microsoft Store apps.
- Software updates through Windows Package Manager, with a catalog fallback for PCs that do not have it.
- Driver updates through Windows Update, and driver backup.
- Browser extension inventory with an access rating.

### Storage
- Disk analyzer, duplicate finder, empty-folder finder.
- Drive wiper: free-space wipe, file shredder and whole-drive erase.

### Protect
- Backup: versioned, deduplicated, compressed backups of your folders with browse, restore and verify; full system images through Windows' own imaging engine.
- System Restore: list, create and remove restore points.

### System
- System info: hardware inventory with drive and battery health.
- Windows tools: the repair and maintenance commands that ship with Windows, one click away.

### Everywhere
- Scheduled cleaning, a tray agent for smart cleaning, and `zest-cli` for scripts.
- Built-in update check against this repository's releases.
- Light and dark themes, portable mode, no telemetry.
