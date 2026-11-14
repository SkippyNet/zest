# Installing

Zest runs on Windows 10 (version 1809 or later) and Windows 11, 64-bit.

Get it from the [latest release](https://github.com/SkippyNet/zest/releases/latest). There are two
downloads; pick one.

## Installer

`Zest-<version>-x64-setup.exe`

- Installs for your Windows account only, into `%LOCALAPPDATA%\Programs\Zest`. It does not ask for
  administrator rights. (Choose *Install for all users* in the first dialog if you want it in
  `Program Files`; that does ask.)
- Adds a Start menu entry and, if you tick it, a desktop shortcut.
- Can update itself later: *Options → About → Check for updates*.

To remove it: *Settings → Apps → Installed apps → Zest → Uninstall*. The uninstaller asks whether
to delete your settings, history and registry backups as well.

## Portable

`Zest-<version>-x64-portable.zip`

Unzip it anywhere and start `zest.exe`. Nothing is installed and nothing is written outside that
folder: settings, history and registry backups go into a `data` folder beside the program. That
makes it suitable for a USB stick.

It is portable because of the empty file `zest.portable` in the folder. Delete that file and Zest
uses `%APPDATA%\Zest` like the installed version.

## "Windows protected your PC"

Windows SmartScreen shows this for downloads it has not seen often, and Zest's files are not
code-signed yet. Choose **More info**, then **Run anyway**.

If you would rather check the file first:

```powershell
# Compare the result with the line for your file in SHA256SUMS.txt (same release page)
Get-FileHash .\Zest-<version>-x64-setup.exe -Algorithm SHA256

# Or, with the GitHub CLI: confirm the file was built by this repository's release workflow
gh attestation verify .\Zest-<version>-x64-setup.exe --repo SkippyNet/zest
```

Every release is compiled by GitHub Actions from the tagged source code; nobody uploads files
built on their own computer.

## Where Zest keeps its things

| What | Installed | Portable |
|---|---|---|
| Settings | `%APPDATA%\Zest\settings.json` | `data\settings.json` |
| Clean history | `%APPDATA%\Zest\history.json` | `data\history.json` |
| Registry backups | `%APPDATA%\Zest\backups\` | `data\backups\` |
| Log | `%APPDATA%\Zest\logs\zest.log` | `data\logs\zest.log` |

*Options → About → Open data folder* takes you there.

## Building it yourself

See [CONTRIBUTING](https://github.com/SkippyNet/zest/blob/main/CONTRIBUTING.md#building).
