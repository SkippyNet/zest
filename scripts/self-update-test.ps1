<#
.SYNOPSIS
    Proves that Zest can update itself to the latest published release.

.DESCRIPTION
    Two rounds, each for a portable copy and for an installed copy:

    1. A copy built from this source tree with a lowered version number (-OldBuild) updates
       itself to the latest release. This works from the very first release on.
    2. When at least two releases exist, the release before the latest one is downloaded and
       updates itself too. That is the updater people actually have, meeting the new release.

    Each copy runs `zest-cli update install`, and afterwards has to report the version of the
    latest release. Runs on a throw-away GitHub Actions machine only.

.PARAMETER OldBuild
    Folder with a zest.exe and zest-cli.exe built with a version lower than the latest release.
#>
[CmdletBinding()]
param(
    [string]$OldBuild = "",
    [string]$Repo = $env:GITHUB_REPOSITORY
)

$ErrorActionPreference = "Stop"
if ($env:GITHUB_ACTIONS -ne "true") {
    Write-Error "This script installs and updates software on the machine it runs on. It only runs on GitHub Actions."
    exit 2
}

$releases = @(gh release list --repo $Repo --exclude-drafts --exclude-pre-releases --limit 10 --json tagName,isLatest | ConvertFrom-Json)
if ($releases.Count -lt 1) {
    Write-Host "No release is published yet: there is nothing to update to."
    exit 0
}
$latest = ($releases | Where-Object { $_.isLatest } | Select-Object -First 1).tagName
$previous = ($releases | Where-Object { -not $_.isLatest } | Select-Object -First 1).tagName
$want = $latest.TrimStart("v")
Write-Host "Latest release: $latest"

$work = Join-Path $env:RUNNER_TEMP "zest-self-update"
New-Item -ItemType Directory -Force $work | Out-Null
# Installed copies keep their settings out of the runner's profile.
$env:ZEST_DATA_DIR = Join-Path $work "data"
$script:failures = New-Object System.Collections.Generic.List[string]
$script:passed = New-Object System.Collections.Generic.List[string]

function Version-Of([string]$Cli) {
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $text = (& $Cli --version 2>&1 | Out-String).Trim()
    $ErrorActionPreference = $previousPreference
    $text
}

function Wait-For-Version([string]$Cli, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        Start-Sleep -Seconds 3
        if ((Test-Path $Cli) -and ((Version-Of $Cli) -match [regex]::Escape($want))) { return $true }
    } while ((Get-Date) -lt $deadline)
    $false
}

# Lets the copy in $Dir update itself and checks the version afterwards.
function Update-Copy([string]$Name, [string]$Dir, [int]$Seconds) {
    $cli = Join-Path $Dir "zest-cli.exe"
    $before = Version-Of $cli
    Write-Host "Before: $before"
    if ($before -match [regex]::Escape($want)) {
        $script:failures.Add("${Name}: the copy already is $want, so nothing was tested")
        return
    }
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $cli update check 2>&1 | Out-Host
    & $cli update install --yes 2>&1 | Out-Host
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previousPreference
    if ($code -ne 0) { $script:failures.Add("${Name}: update install exited with $code") }
    if (Wait-For-Version $cli $Seconds) {
        Write-Host "After:  $(Version-Of $cli)"
        $script:passed.Add("$Name ($before to $want)")
    } else {
        $script:failures.Add("${Name}: still reports '$(Version-Of $cli)', expected $want")
    }
    # The installer starts Zest again after an update; this machine does not need it.
    Get-Process zest -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Install-Silently([string]$Setup, [string]$Dir, [string]$Name) {
    $p = Start-Process $Setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', '/NOLAUNCH', "/DIR=$Dir" -Wait -PassThru
    if ($p.ExitCode -ne 0) { $script:failures.Add("${Name}: the installer exited with $($p.ExitCode)") }
}

function Remove-Silently([string]$Dir) {
    $uninstaller = Join-Path $Dir "unins000.exe"
    if (Test-Path $uninstaller) {
        Start-Process $uninstaller -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait | Out-Null
    }
}

# ── Round 1: a copy built from this source with a lowered version ─────
if ($OldBuild) {
    $OldBuild = (Resolve-Path $OldBuild).Path

    Write-Host "`n== Lowered-version build, portable" -ForegroundColor Cyan
    $portable = Join-Path $work "built-portable"
    New-Item -ItemType Directory -Force $portable | Out-Null
    Copy-Item (Join-Path $OldBuild "zest.exe"), (Join-Path $OldBuild "zest-cli.exe") $portable
    New-Item -ItemType File (Join-Path $portable "zest.portable") | Out-Null
    Update-Copy "built portable copy" $portable 90
    if (-not (Test-Path (Join-Path $portable "zest.portable"))) { $script:failures.Add("built portable copy: the portable marker is gone") }

    Write-Host "`n== Lowered-version build, installed" -ForegroundColor Cyan
    $oldVersion = ((Version-Of (Join-Path $OldBuild "zest-cli.exe")) -split '\s+')[-1]
    $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
    if (-not $iscc) {
        $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
        if (-not (Test-Path $iscc)) { choco install innosetup --no-progress -y | Out-Null }
    }
    & $iscc "/DAppVersion=$oldVersion" installer\zest.iss | Select-Object -Last 2 | Out-Host
    $setup = "dist\Zest-$oldVersion-x64-setup.exe"
    if (-not (Test-Path $setup)) {
        $script:failures.Add("built installed copy: the installer for $oldVersion did not compile")
    } else {
        $installed = Join-Path $work "built-installed"
        Install-Silently (Resolve-Path $setup).Path $installed "built installed copy"
        Update-Copy "built installed copy" $installed 240
        Remove-Silently $installed
    }
}

# ── Round 2: the previous release, as people have it ──────────────────
if ($previous) {
    Write-Host "`n== Previous release $previous, portable" -ForegroundColor Cyan
    $download = Join-Path $work "previous"
    New-Item -ItemType Directory -Force $download | Out-Null
    gh release download $previous --repo $Repo --pattern "*-x64-portable.zip" --pattern "*-x64-setup.exe" --dir $download
    $portable = Join-Path $work "previous-portable"
    Expand-Archive (Get-ChildItem $download -Filter "*-x64-portable.zip" | Select-Object -First 1).FullName -DestinationPath $portable
    Update-Copy "portable copy of $previous" $portable 90

    Write-Host "`n== Previous release $previous, installed" -ForegroundColor Cyan
    $installed = Join-Path $work "previous-installed"
    Install-Silently (Get-ChildItem $download -Filter "*-x64-setup.exe" | Select-Object -First 1).FullName $installed "installed copy of $previous"
    Update-Copy "installed copy of $previous" $installed 240
    Remove-Silently $installed
} else {
    Write-Host "`nOnly one release exists, so there is no previous release to update from yet."
}

Write-Host ""
$lines = @("# Self-update to $latest", "")
foreach ($p in $script:passed) { Write-Host "ok    $p"; $lines += "- passed: $p" }
foreach ($f in $script:failures) { Write-Host "FAIL  $f" -ForegroundColor Red; $lines += "- **FAILED**: $f" }
if ($env:GITHUB_STEP_SUMMARY) { $lines | Add-Content $env:GITHUB_STEP_SUMMARY -Encoding utf8 }
if ($script:failures.Count -gt 0 -or $script:passed.Count -eq 0) { exit 1 }
exit 0
