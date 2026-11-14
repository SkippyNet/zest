# Command line

`zest-cli.exe` sits beside `zest.exe`. It uses the same engine and the same saved settings as the
app, so what you tick in Custom clean is what `zest-cli clean` cleans.

```text
zest-cli [--json] <command>
```

`--json` works with every command and prints machine-readable output. A command that fails exits
with code 1 and a message on standard error.

## Commands that only read

| Command | What it shows |
|---|---|
| `zest-cli health` | Privacy, space, speed and security at a glance |
| `zest-cli analyze` | What a clean would remove with your selection. `--all` analyzes every rule |
| `zest-cli rules` | Every cleaning rule and whether it is selected |
| `zest-cli registry scan` | Invalid registry entries |
| `zest-cli tools list` | Windows' own repair and maintenance tools |
| `zest-cli startup list` | Startup apps, scheduled tasks, services, context-menu entries |
| `zest-cli apps list` | Installed programs |
| `zest-cli updates list` | Programs with updates waiting |
| `zest-cli drivers list` | Installed third-party drivers |
| `zest-cli disk analyze <folder>` | What uses the space under a folder or drive |
| `zest-cli duplicates <folder>...` | Identical files. `--min-size <bytes>` (default 8192) |
| `zest-cli privacy list` | Windows privacy settings and where each one stands |
| `zest-cli system` | Hardware, drive health and battery |
| `zest-cli backup list --dest <folder>` | Backups in a destination |
| `zest-cli backup verify --dest <folder>` | Reads stored data back and checks it. `--snapshot <id>` for one backup |
| `zest-cli update check` | Whether a newer Zest has been published |

## Commands that change something

### Clean

```text
zest-cli clean --dry-run     show what would be removed; removes nothing
zest-cli clean               ask, then clean
zest-cli clean --yes         clean without asking (for scripts and schedules)
```

### Registry, privacy and Windows tools

Each of these saves a `.reg` backup before it changes the registry and asks first unless you add
`--yes`.

```text
zest-cli registry fix [--yes]                 remove what `registry scan` finds
zest-cli privacy apply --recommended [--yes]  switch the recommended settings to private
zest-cli privacy apply <id>... [--yes]        switch named settings (ids from `privacy list`)
zest-cli privacy revert --all [--yes]         back to the Windows defaults
zest-cli privacy revert <id>... [--yes]
zest-cli tools run <id> [--yes]               run one tool and print what it reports
```

Tools that open a window (Event Viewer, Services, ...) are started from the app, not from here.

### Back up

```text
zest-cli backup create [folders...] --dest <folder>
        [--exclude <pattern>]...   leave out matching names or paths
        [--label <text>]           a note stored with the backup
        [--keep-last <n>]          afterwards keep only the newest n backups
        [--reread]                 read every file again instead of trusting size and date
```

Without arguments, `backup create` uses the folders, destination and exclusions saved in the app.

```text
zest-cli backup restore --dest <folder> (--to <folder> | --original)
        [--snapshot <id>]          which backup (default: the newest)
        [--path <part>]...         only this part, e.g. Documents\Letters
        [--overwrite skip|keep-both|replace]   when a different file is already there (default: skip)
```

### Update Zest

```text
zest-cli update install [--yes]
```

## Examples

Free space every night and keep a log:

```powershell
zest-cli clean --yes --json | Out-File "$env:USERPROFILE\zest-last-clean.json"
```

Fail a script when more than 5 GB of junk has built up:

```powershell
$found = zest-cli analyze --json | ConvertFrom-Json
if ($found.bytes -gt 5GB) { Write-Warning "Time to clean"; exit 1 }
```

Back up two folders to an external drive and keep the last ten versions:

```powershell
zest-cli backup create "$env:USERPROFILE\Documents" "$env:USERPROFILE\Pictures" --dest E:\ --keep-last 10
zest-cli backup verify --dest E:\
```

List programs with updates as a table:

```powershell
(zest-cli updates list --json | ConvertFrom-Json).updates |
    Format-Table name, installed_version, available_version
```

## Using other settings

Set `ZEST_DATA_DIR` to a folder and the command line (and the app) read and write settings,
history and backups there instead of your profile. Handy for trying things out:

```powershell
$env:ZEST_DATA_DIR = "$env:TEMP\zest-trial"
zest-cli analyze
```
