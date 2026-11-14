# Options and automation

Everything here is under **Options** at the bottom of the navigation rail.

## General

| Setting | What it does |
|---|---|
| **Theme** | Follow Windows, or always light or dark. |
| **Text size** | Scales the whole interface. |
| **Use the Recycle Bin for tools** | Duplicates, large files and uninstall leftovers go to the Recycle Bin instead of being deleted for good. On by default. |
| **Show Windows entries** | Lists Microsoft's own startup items, services and tasks alongside everyone else's. Off by default so you cannot switch off part of Windows by accident. |
| **Reset all settings** | Your rule selection, cookie keep list, includes, exclusions and schedule return to their defaults. History and backups are kept. |

## Cleaning

| Setting | What it does |
|---|---|
| **Ask before cleaning** | Shows a confirmation with the totals first. |
| **Keep recent temporary files** | Skips temporary files newer than the chosen age (24 hours by default), because running programs may still need what they wrote a moment ago. |
| **Secure deletion** | Overwrites file contents before deleting: 1, 3, 7 or 35 passes. Slower, and no guarantee on SSDs, which move data around internally. |
| **Clean all user accounts** | Applies your selection to every Windows account on this PC. Only works when Zest runs as administrator. |
| **Health check: Remove trackers** | Health check clears browser cookies, except for sites on your keep list. |
| **Health check: Remove browsing history** | Health check also clears visited pages and download records. Off by default. |

## Cookies

Cookies for sites on the **keep list** survive cleaning, so you stay signed in there. Sub-domains
are included: keeping `example.com` also keeps `mail.example.com`. Zest lists the sites that have
cookies in your browsers; a browser has to be closed for its cookies to be listed.

## Include and exclude

- **Include**: your own files and folders to clean, shown under "Custom" in Custom clean.
  Protected system locations are refused.
- **Exclude**: files, folders and registry keys that are always left alone, whatever a rule says.

## Scheduling

Turn on **Clean on a schedule** and choose daily, weekly or monthly and a time. Windows Task
Scheduler then starts Zest at that time; it cleans your Custom clean selection without opening a
window and closes again. If the PC is off at that time, the clean runs when it is next on.

The task is called `Zest\Scheduled Clean` in Task Scheduler. Turning the switch off removes it.

## Smart cleaning

**Watch in the background** keeps a small icon in the notification area and starts it with
Windows. It only reads until it has something to tell you.

| Setting | Choices |
|---|---|
| **When junk builds up** (past a size you choose) | Tell me, clean it automatically, or do nothing. Only junk rules are used; cookies and history are never cleaned in the background. |
| **When I close a browser** | Clean that browser with the rules you selected for it, ask first, or do nothing. |
| **Check for junk** | How often to look. |

The agent is the same `zest.exe` started with `--agent`. It exits by itself when you turn the
switch off.

## About

Version, the update check for Zest itself (see [Updating Zest](Updating-Zest)), your clean
history, and **Open data folder**, which is where settings, history, registry backups and the log
live.
