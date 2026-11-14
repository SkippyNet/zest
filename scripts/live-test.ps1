<#
.SYNOPSIS
    Runs Zest for real on a disposable Windows machine and checks what it did.

.DESCRIPTION
    Unit tests only ever touch temporary folders and scratch registry keys. This script is the
    other half: it plants known junk, runs the real commands against the real machine and checks
    that the junk is gone and everything else is still there.

    IT CLEANS AND CHANGES THE MACHINE IT RUNS ON. It is meant for the GitHub Actions runner (a
    virtual machine that is thrown away afterwards) and refuses to start anywhere else unless
    -ThisMachineIsDisposable is given.

.PARAMETER Bin
    Folder that holds zest.exe and zest-cli.exe.

.PARAMETER Out
    Folder for the report, command output and screenshots.
#>
[CmdletBinding()]
param(
    [string]$Bin = "target\release",
    [string]$Out = "live-test-output",
    [switch]$ThisMachineIsDisposable
)

$ErrorActionPreference = "Stop"

if ($env:GITHUB_ACTIONS -ne "true" -and -not $ThisMachineIsDisposable) {
    Write-Error "This script really cleans and changes the PC it runs on. It only runs on GitHub Actions, or with -ThisMachineIsDisposable on a machine you can throw away."
    exit 2
}

$Bin = (Resolve-Path $Bin).Path
$cli = Join-Path $Bin "zest-cli.exe"
$gui = Join-Path $Bin "zest.exe"
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

# Settings, history and registry backups of this run go to their own folder.
$env:ZEST_DATA_DIR = Join-Path $Out "data"
New-Item -ItemType Directory -Force $env:ZEST_DATA_DIR | Out-Null

$script:results = New-Object System.Collections.Generic.List[object]

function Check([string]$Name, [scriptblock]$Test) {
    $ok = $false
    $detail = ""
    try {
        $value = & $Test
        $ok = [bool]$value
        if (-not $ok) { $detail = "condition was false" }
    } catch {
        $detail = $_.Exception.Message
    }
    $script:results.Add([pscustomobject]@{ Check = $Name; Passed = $ok; Detail = $detail })
    if ($ok) { Write-Host "  ok    $Name" } else { Write-Host "  FAIL  $Name  ($detail)" -ForegroundColor Red }
}

# Runs zest-cli, saves its output, returns exit code and text.
function Zest {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
    $name = ($Arguments -join "_") -replace '[^A-Za-z0-9_.-]', '_'
    if ($name.Length -gt 80) { $name = $name.Substring(0, 80) }
    $file = Join-Path $Out "$name.txt"
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $text = & $cli @Arguments 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    Set-Content -Path $file -Value $text -Encoding utf8
    [pscustomobject]@{ Code = $code; Text = $text }
}

function Json([string]$Text) {
    # Skip any log lines printed before the JSON document.
    $start = $Text.IndexOfAny(@('{', '['))
    if ($start -lt 0) { throw "no JSON in output" }
    $Text.Substring($start) | ConvertFrom-Json
}

function Section([string]$Title) { Write-Host ""; Write-Host "== $Title" -ForegroundColor Cyan }

$elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$os = (Get-CimInstance Win32_OperatingSystem)
Write-Host "Machine: $($os.Caption) build $($os.BuildNumber), elevated: $elevated"

# ── Basics ────────────────────────────────────────────────────────────
Section "Basics"
$version = Zest --version
Check "zest-cli --version runs" { $version.Code -eq 0 -and $version.Text -match '\d+\.\d+\.\d+' }
$help = Zest --help
Check "zest-cli --help runs" { $help.Code -eq 0 }

# ── A real clean ──────────────────────────────────────────────────────
Section "Clean"
$old = (Get-Date).AddDays(-5)
$junkDir = Join-Path $env:TEMP "zest-live-junk"
New-Item -ItemType Directory -Force $junkDir | Out-Null
$junk = @()
foreach ($i in 1..5) {
    $f = Join-Path $junkDir "old-$i.tmp"
    [IO.File]::WriteAllBytes($f, (New-Object byte[] (256KB)))
    (Get-Item $f).LastWriteTime = $old
    $junk += $f
}
(Get-Item $junkDir).LastWriteTime = $old
# A fresh temp file: younger than the 24 hour limit, so it must survive.
$fresh = Join-Path $env:TEMP "zest-live-fresh.tmp"
Set-Content $fresh "still in use"
# Things a cleaner must never touch.
$docs = [Environment]::GetFolderPath("MyDocuments")
New-Item -ItemType Directory -Force $docs | Out-Null
$keepDoc = Join-Path $docs "zest-live-keep.txt"
Set-Content $keepDoc "my document"
$keepProfile = Join-Path $env:USERPROFILE "zest-live-keep.txt"
Set-Content $keepProfile "in my profile"
# A junction inside the temp folder that points at the documents: a clean must
# remove the link at most, never follow it.
$link = Join-Path $junkDir "link-to-documents"
cmd /c mklink /J "$link" "$docs" | Out-Null

$rules = Zest rules --json
Check "rules lists the catalog" { $rules.Code -eq 0 -and (Json $rules.Text).Count -gt 20 }

$analyze = Zest analyze --json
Check "analyze succeeds" { $analyze.Code -eq 0 }
$found = Json $analyze.Text
Check "analyze finds the planted junk" { $found.bytes -ge 5 * 256KB -and $found.items -ge 5 }

$dry = Zest clean --dry-run
Check "dry run succeeds" { $dry.Code -eq 0 }
Check "dry run removes nothing" { ($junk | Where-Object { Test-Path $_ }).Count -eq 5 }

$clean = Zest clean --yes --json
Check "clean succeeds" { $clean.Code -eq 0 }
Check "planted junk is gone" { ($junk | Where-Object { Test-Path $_ }).Count -eq 0 }
Check "a fresh temp file is left alone" { Test-Path $fresh }
Check "a document is left alone" { (Get-Content $keepDoc) -eq "my document" }
Check "a file in the profile folder is left alone" { (Get-Content $keepProfile) -eq "in my profile" }
Check "the junction was not followed" { (Test-Path $docs) -and (Test-Path $keepDoc) }
Check "the clean is in the history" { Test-Path (Join-Path $env:ZEST_DATA_DIR "history.json") }
$report = Json $clean.Text
Check "the report counts what was freed" { $report.bytes -ge 5 * 256KB }

$again = Zest analyze --json
Check "a second analysis finds less than the first" { (Json $again.Text).bytes -lt $found.bytes }

# ── Read-only tools ───────────────────────────────────────────────────
Section "Read-only tools"
$health = Zest health --json
Check "health check runs" { $health.Code -eq 0 }
$registry = Zest registry scan --json
Check "registry scan runs" { $registry.Code -eq 0 }
$startup = Zest startup list --json
Check "startup list runs" { $startup.Code -eq 0 }
$apps = Zest apps list --json
Check "installed programs are listed" { $apps.Code -eq 0 -and (Json $apps.Text).Count -gt 0 }
$drivers = Zest drivers list --json
Check "drivers list runs" { $drivers.Code -eq 0 }
$updates = Zest updates list --json
Check "software updates: answers or explains (no crash)" { $updates.Code -in 0, 1 }

$dupDir = Join-Path $Out "dups"
New-Item -ItemType Directory -Force "$dupDir\a", "$dupDir\b" | Out-Null
$payload = New-Object byte[] (64KB); (New-Object Random 7).NextBytes($payload)
[IO.File]::WriteAllBytes("$dupDir\a\one.bin", $payload)
[IO.File]::WriteAllBytes("$dupDir\b\two.bin", $payload)
[IO.File]::WriteAllBytes("$dupDir\b\different.bin", (New-Object byte[] (64KB)))
$dups = Zest duplicates $dupDir --json
Check "duplicate finder pairs the two identical files" { $dups.Code -eq 0 -and $dups.Text -match 'one\.bin' -and $dups.Text -match 'two\.bin' -and $dups.Text -notmatch 'different\.bin' }
$disk = Zest disk analyze $dupDir --json
Check "disk analyzer measures a folder" { $disk.Code -eq 0 }
$bad = Zest disk analyze "Z:\no\such\folder"
Check "a missing folder is an error, not a crash" { $bad.Code -eq 1 }

$system = Zest system --json
Check "system info reads the hardware" { $system.Code -eq 0 -and $system.Text -match 'processor' }
$update = Zest update check
Check "update check answers (no crash)" { $update.Code -eq 0 }

# ── A scheduled clean, the way Task Scheduler starts it ───────────────
Section "Scheduled clean"
$scheduled = Join-Path $env:TEMP "zest-live-scheduled.tmp"
[IO.File]::WriteAllBytes($scheduled, (New-Object byte[] (128KB)))
(Get-Item $scheduled).LastWriteTime = $old
$run = Start-Process $gui -ArgumentList "--scheduled-clean" -PassThru
if (-not $run.WaitForExit(300000)) { $run.Kill() }
Check "the scheduled clean runs to the end" { $run.HasExited -and $run.ExitCode -eq 0 }
Check "the scheduled clean removed the junk" { -not (Test-Path $scheduled) }
Check "the scheduled clean left the document alone" { Test-Path $keepDoc }

# ── Registry cleaner: plant, fix, restore from the backup ─────────────
Section "Registry"
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$appPath = "HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\zestlive.exe"
New-Item -Path $runKey -Force | Out-Null
Set-ItemProperty -Path $runKey -Name "ZestLiveMissing" -Value '"C:\zest-live-missing\gone.exe" --tray'
New-Item -Path $appPath -Force | Out-Null
Set-ItemProperty -Path $appPath -Name "(default)" -Value 'C:\zest-live-missing\zestlive.exe'
# A healthy entry next to them that must survive.
Set-ItemProperty -Path $runKey -Name "ZestLiveHealthy" -Value "$env:WINDIR\System32\notepad.exe"

$scan = Zest registry scan --json
Check "registry scan finds the planted dead entries" { $scan.Code -eq 0 -and $scan.Text -match 'ZestLiveMissing' -and $scan.Text -match 'zestlive\.exe' }
Check "registry scan does not flag the healthy entry" { $scan.Text -notmatch 'ZestLiveHealthy' }
$fix = Zest registry fix --yes --json
Check "registry fix succeeds" { $fix.Code -eq 0 }
$fixed = Json $fix.Text
Check "registry fix removed entries" { $fixed.fixed -ge 2 }
Check "the dead startup entry is gone" { -not (Get-ItemProperty -Path $runKey -Name "ZestLiveMissing" -ErrorAction SilentlyContinue) }
Check "the dead App Paths key is gone" { -not (Test-Path $appPath) }
Check "the healthy startup entry is still there" { [bool](Get-ItemProperty -Path $runKey -Name "ZestLiveHealthy" -ErrorAction SilentlyContinue) }
Check "a .reg backup was written" { $fixed.backup_file -and (Test-Path $fixed.backup_file) }
if ($fixed.backup_file -and (Test-Path $fixed.backup_file)) {
    $import = Start-Process reg.exe -ArgumentList "import", "`"$($fixed.backup_file)`"" -Wait -PassThru -WindowStyle Hidden
    Check "the backup imports cleanly" { $import.ExitCode -eq 0 }
    Check "importing the backup brings the entries back" { [bool](Get-ItemProperty -Path $runKey -Name "ZestLiveMissing" -ErrorAction SilentlyContinue) -and (Test-Path $appPath) }
}
Remove-ItemProperty -Path $runKey -Name "ZestLiveMissing", "ZestLiveHealthy" -ErrorAction SilentlyContinue
Remove-Item -Path $appPath -Recurse -ErrorAction SilentlyContinue

# ── Windows privacy: apply, read back, revert ─────────────────────────
Section "Windows privacy"
$before = Zest privacy list --json
Check "privacy list runs" { $before.Code -eq 0 -and (Json $before.Text).Count -ge 20 }
$apply = Zest privacy apply --recommended --yes --json
Check "privacy apply succeeds" { $apply.Code -eq 0 }
$applied = Json $apply.Text
$changed = @($applied.items | Where-Object { $_.outcome -eq "Changed" }).Count
Check "privacy apply changed settings" { $changed -ge 3 }
Check "privacy apply wrote a backup" { $applied.backup -and (Test-Path $applied.backup) }
$after = Json (Zest privacy list --json).Text
$private = @($after | Where-Object { $_.state -eq "Private" }).Count
Check "the applied settings read back as private" { $private -ge $changed }
$revert = Zest privacy revert --all --yes --json
Check "privacy revert succeeds" { $revert.Code -eq 0 }
$reverted = Json (Zest privacy list --json).Text
Check "after reverting, nothing is left private or custom" { @($reverted | Where-Object { $_.state -in "Private", "Custom" }).Count -eq 0 }

# ── Windows tools ─────────────────────────────────────────────────────
Section "Windows tools"
$tools = Zest tools list --json
Check "tools list runs" { $tools.Code -eq 0 -and (Json $tools.Text).Count -ge 15 }
$dns = Zest tools run dns-flush --yes
Check "a tool runs and reports (flush DNS)" { $dns.Code -eq 0 }
if ($elevated) {
    $image = Zest tools run image-check --yes
    Check "an administrator tool runs and reports (component store check)" { $image.Code -eq 0 -and $image.Text -match '\S' }
}
$window = Zest tools run services --yes
Check "a tool that opens a window is refused on the command line" { $window.Code -eq 1 }

# ── Backup: create, change, create again, verify, restore ─────────────
Section "Backup"
$source = Join-Path $Out "backup-source"
$dest = Join-Path $Out "backup-dest"
$restored = Join-Path $Out "backup-restored"
New-Item -ItemType Directory -Force "$source\letters", $dest | Out-Null
Set-Content "$source\notes.txt" "first version"
Set-Content "$source\letters\one.txt" "a letter"
$big = New-Object byte[] (9MB); (New-Object Random 11).NextBytes($big)
[IO.File]::WriteAllBytes("$source\big.bin", $big)
$first = Zest backup create $source --dest $dest --label first
Check "backup create succeeds" { $first.Code -eq 0 }
Set-Content "$source\notes.txt" "second version"
$second = Zest backup create $source --dest $dest --label second
Check "a second backup succeeds" { $second.Code -eq 0 }
$list = Zest backup list --dest $dest --json
Check "backup list shows both backups" { $list.Code -eq 0 -and $list.Text -match 'first' -and $list.Text -match 'second' }
$verify = Zest backup verify --dest $dest
Check "backup verify finds the data intact" { $verify.Code -eq 0 }
$restore = Zest backup restore --dest $dest --to $restored
Check "backup restore succeeds" { $restore.Code -eq 0 }
$notes = Get-ChildItem $restored -Recurse -Filter notes.txt | Select-Object -First 1
$bigBack = Get-ChildItem $restored -Recurse -Filter big.bin | Select-Object -First 1
Check "the newest version of a changed file comes back" { $notes -and (Get-Content $notes.FullName) -eq "second version" }
Check "a large file comes back byte for byte" { $bigBack -and (Get-FileHash $bigBack.FullName).Hash -eq (Get-FileHash "$source\big.bin").Hash }
Check "a sub-folder comes back" { [bool](Get-ChildItem $restored -Recurse -Filter one.txt) }

# ── The window ────────────────────────────────────────────────────────
Section "Application window"
$shots = Join-Path $Out "screenshots"
$p = Start-Process $gui -ArgumentList "--demo", "--screenshots", $shots -PassThru
if (-not $p.WaitForExit(600000)) { $p.Kill() }
Check "the app starts, draws every page and exits" { $p.HasExited -and $p.ExitCode -eq 0 }
Check "a picture of every page was saved" { (Get-ChildItem $shots -Filter *.png -ErrorAction SilentlyContinue).Count -ge 19 }
$log = Join-Path $env:ZEST_DATA_DIR "logs\zest.log"
Check "no panic in the log" { -not (Test-Path $log) -or -not (Select-String -Path $log -Pattern "panic" -Quiet) }

# ── Report ────────────────────────────────────────────────────────────
Section "Result"
$failed = @($script:results | Where-Object { -not $_.Passed })
$summary = "{0} of {1} checks passed on {2} (build {3}, elevated: {4})." -f ($script:results.Count - $failed.Count), $script:results.Count, $os.Caption, $os.BuildNumber, $elevated
Write-Host $summary
$lines = @("# Live test", "", $summary, "", "| Check | Result |", "|---|---|")
foreach ($r in $script:results) {
    $mark = if ($r.Passed) { "passed" } else { "**FAILED** $($r.Detail)" }
    $lines += "| $($r.Check) | $mark |"
}
$lines | Set-Content (Join-Path $Out "report.md") -Encoding utf8
if ($env:GITHUB_STEP_SUMMARY) { $lines | Add-Content $env:GITHUB_STEP_SUMMARY -Encoding utf8 }

Remove-Item $keepDoc, $keepProfile, $fresh -ErrorAction SilentlyContinue
if ($failed.Count -gt 0) { exit 1 }

# The checks decide the result, not the exit code of the last command that ran.
exit 0
