# Clean

## Health check and Custom clean

Covered step by step in [Your first clean](Your-first-clean).

The rules behind Custom clean come in four sections:

| Section | Examples |
|---|---|
| **Windows** | Recycle Bin, temporary files, memory dumps, logs, error reports, thumbnails, shader cache, Store caches, Delivery Optimization files, Windows Update leftovers, broken shortcuts, clipboard, DNS cache; and, off by default, recent documents, Run and search history, jump lists, event logs |
| **Browsers** | Chrome, Edge, Brave, Opera, Opera GX, Vivaldi, Chromium, Yandex, Firefox, LibreWolf, Waterfox, every profile: cache, cookies, history, downloads list, form data, sessions, saved passwords |
| **Applications** | Caches and logs of chat, media, game, graphics-driver, office and developer tools. A program only appears if it is installed |
| **Custom** | Your own folders from *Options → Include and exclude* |

## Registry

The registry collects entries that point at things which are no longer there: a file type whose
program was uninstalled, a shared DLL that was deleted, a start-up entry for a missing file.

1. **Scan** looks for 15 kinds of such entries. It only reads.
2. Each issue shows the key, the value and the reason. Untick anything you want to keep, or
   exclude it permanently.
3. **Fix selected** first exports every affected key to a `.reg` file, then removes the entries.

The **Backups** tab lists those files with their date and size. **Restore** imports one back
through Windows' own registry importer, which puts every removed entry back.

Entries under `HKEY_LOCAL_MACHINE` belong to all users and need administrator rights to fix.
A small set of keys that Windows needs to start can never be removed, whatever the scan finds.

## Windows privacy

Windows has dozens of settings for what it collects, suggests and advertises, spread over several
Settings pages and policies. This page puts the documented ones in one list with a switch each:
diagnostic data, advertising ID, tailored experiences, feedback prompts, suggestions and tips,
activity history, search highlights, location, Recall snapshots, Copilot, clipboard sync and more.

- **On** means private: the feature is switched off or limited.
- The pill on each row shows the current state: *Private*, *Windows default*, or *Custom* when
  something else has set it to a third value.
- Point at a row to see exactly which registry values it changes and where Microsoft documents
  them. Zest only offers settings Microsoft documents.
- **Apply recommended** switches on a conservative set and lists what will change before it does.
  **Restore Windows defaults** puts every setting back.
- Before any change the affected keys are exported to a `.reg` backup, as with the registry
  cleaner.

Things to know:

- Many of these are policies. Settings that apply to the whole PC, and policies in general, need
  administrator rights; the page says which and offers *Restart as administrator*.
- Microsoft documents several of these policies for Windows Pro, Enterprise and Education. On
  Windows Home some may be ignored by Windows; the page marks them.
- Nothing here touches Windows Update, Microsoft Defender, SmartScreen, the firewall or User
  Account Control. Zest does not offer switches that weaken security.
- If you elevate with a *different* administrator account, per-user settings are changed for that
  account, not yours.
