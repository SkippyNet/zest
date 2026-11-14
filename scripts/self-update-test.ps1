<#
.SYNOPSIS
    Proves that a copy of the previous release can update itself to the newest one.

.DESCRIPTION
    Downloads the release before the latest one, in both forms (portable zip and installer),
    lets each copy update itself with `zest-cli update install`, and checks that it then reports
    the latest version. Runs on a throw-away GitHub Actions machine.

    With fewer than two published releases there is nothing to update from, and the script says so
    and succeeds.
#>
[CmdletBinding()]
param([string]$Repo = $env:GITHUB_REPOSITORY)

$ErrorActionPreference = "Stop"
if ($env:GITHUB_ACTIONS -ne "true") {
    Write-Error "This script installs and updates software on the machine it runs on. It only runs on GitHub Actions."
    exit 2
}

$releases = gh release list --repo $Repo --exclude-drafts --exclude-pre-releases --limit 10 --json tagName,isLatest | ConvertFrom-Json
if (@($releases).Count -lt 2) {
    Write-Host "Fewer than two releases are published: there is nothing to update from yet."
    exit 0
}
$latest = ($releases | Where-Object { $_.isLatest } | Select-Object -First 1).tagName
$previous = ($releases | Where-Object { -not $_.isLatest } | Select-Object -First 1).tagName
$want = $latest.TrimStart("v")
Write-Host "Updating from $previous to $latest"

$work = Join-Path $env:RUNNER_TEMP "zest-self-update"
New-Item -ItemType Directory -Force $work | Out-Null
$failures = New-Object System.Collections.Generic.List[string]

function Version-Of([string]$Cli) {
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $text = (& $Cli --version 2>&1 | Out-String).Trim()
    $ErrorActionPreference = $previousPreference
    $text
}

function Wait-For-Version([string]$Cli, [string]$Version, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        Start-Sleep -Seconds 3
        if ((Test-Path $Cli) -and ((Version-Of $Cli) -match [regex]::Escape($Version))) { return $true }
    } while ((Get-Date) -lt $deadline)
    $false
}

function Run-Update([string]$Cli) {
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $Cli update check 2>&1 | Out-Host
    & $Cli update install --yes 2>&1 | Out-Host
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previousPreference
    $code
}

# ── Portable copy ─────────────────────────────────────────────────────
Write-Host "`n== Portable copy" -ForegroundColor Cyan
$portable = Join-Path $work "portable"
gh release download $previous --repo $Repo --pattern "*-x64-portable.zip" --dir $work
Expand-Archive (Get-ChildItem $work -Filter "*-x64-portable.zip" | Select-Object -First 1).FullName -DestinationPath $portable
$cli = Join-Path $portable "zest-cli.exe"
Write-Host "Before: $(Version-Of $cli)"
$code = Run-Update $cli
if ($code -ne 0) { $failures.Add("portable: update install exited with $code") }
if (Wait-For-Version $cli $want 60) {
    Write-Host "After:  $(Version-Of $cli)"
} else {
    $failures.Add("portable: still reports '$(Version-Of $cli)', expected $want")
}
if (-not (Test-Path (Join-Path $portable "zest.portable"))) { $failures.Add("portable: the portable marker is gone") }

# ── Installed copy ────────────────────────────────────────────────────
Write-Host "`n== Installed copy" -ForegroundColor Cyan
$installed = Join-Path $work "installed"
gh release download $previous --repo $Repo --pattern "*-x64-setup.exe" --dir $work
$setup = (Get-ChildItem $work -Filter "*-x64-setup.exe" | Select-Object -First 1).FullName
$p = Start-Process $setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', '/NOLAUNCH', "/DIR=$installed" -Wait -PassThru
if ($p.ExitCode -ne 0) { $failures.Add("installed: the previous installer exited with $($p.ExitCode)") }
$cli = Join-Path $installed "zest-cli.exe"
Write-Host "Before: $(Version-Of $cli)"
# Keep the updated copy's settings out of the runner's profile.
$env:ZEST_DATA_DIR = Join-Path $work "data"
$code = Run-Update $cli
if ($code -ne 0) { $failures.Add("installed: update install exited with $code") }
if (Wait-For-Version $cli $want 180) {
    Write-Host "After:  $(Version-Of $cli)"
} else {
    $failures.Add("installed: still reports '$(Version-Of $cli)', expected $want")
}
# The installer starts Zest again after an update; this machine does not need it.
Get-Process zest -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

Write-Host ""
if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Host "FAIL  $_" -ForegroundColor Red }
    if ($env:GITHUB_STEP_SUMMARY) { "Self-update from $previous to $latest **failed**:`n" + ($failures -join "`n") | Add-Content $env:GITHUB_STEP_SUMMARY }
    exit 1
}
Write-Host "Both copies updated themselves from $previous to $latest."
if ($env:GITHUB_STEP_SUMMARY) { "Self-update from $previous to $latest passed for the portable and the installed copy." | Add-Content $env:GITHUB_STEP_SUMMARY }
exit 0
