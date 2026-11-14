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

# ── Later sections are added by the features that have a command for them ──
$extra = Join-Path $PSScriptRoot "live-test-extra.ps1"
if (Test-Path $extra) { . $extra }

# ── The window ────────────────────────────────────────────────────────
Section "Application window"
$shots = Join-Path $Out "screenshots"
$p = Start-Process $gui -ArgumentList "--demo", "--screenshots", $shots -PassThru
if (-not $p.WaitForExit(600000)) { $p.Kill() }
Check "the app starts, draws every page and exits" { $p.HasExited -and $p.ExitCode -eq 0 }
Check "a picture of every page was saved" { (Get-ChildItem $shots -Filter *.png -ErrorAction SilentlyContinue).Count -ge 14 }
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
