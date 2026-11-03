<#
.SYNOPSIS
Install, update or remove Open Hardware Info from its GitHub releases.

.DESCRIPTION
Install or update to the latest release:

    irm https://github.com/__REPO__/releases/latest/download/install.ps1 | iex

A specific version, or uninstall:

    & ([scriptblock]::Create((irm https://github.com/__REPO__/releases/latest/download/install.ps1))) -Version 0.2.0
    & ([scriptblock]::Create((irm https://github.com/__REPO__/releases/latest/download/install.ps1))) -Uninstall

The app installs for the current user and needs no administrator rights. Your
settings in %APPDATA%\OpenHardwareInfo are kept on update and uninstall.

This script must keep working in Windows PowerShell 5.1, must never call `exit`
(that would close the user's shell under `irm | iex`), and must verify checksums
before installing.
#>
param(
    # Release to install, e.g. 0.2.0. Defaults to the latest.
    [string]$Version = 'latest',
    # Remove the app instead of installing it.
    [switch]$Uninstall,
    # GitHub repository (owner/name) to install from. Filled in when the
    # script is published with a release.
    [string]$Repo = '__REPO__',
    # Install this local package instead of downloading a release.
    [string]$PackagePath,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\OpenHardwareInfo')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is much faster without the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$exeName = 'open-hardware-info.exe'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Open Hardware Info.lnk'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }

# The app must be closed before its file can be replaced or removed. It closes
# itself when asked, so it can still remove its tray icon.
function Stop-App {
    $running = @(Get-Process open-hardware-info -ErrorAction SilentlyContinue)
    if (-not $running) { return }
    $path = ($running | Where-Object Path | Select-Object -First 1).Path
    if ($path) { Start-Process $path -ArgumentList '--quit' -Wait }
    $running | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
    Get-Process open-hardware-info -ErrorAction SilentlyContinue | Stop-Process -Force
    $running | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
}

function Get-Package([string]$Destination) {
    if ($PackagePath) { return (Resolve-Path $PackagePath).Path }
    if ($Repo -like '*__REPO*') {
        throw 'This copy of the installer has no repository set. Download it from a release, or pass -Repo owner/name.'
    }
    $api = "https://api.github.com/repos/$Repo/releases/" + $(if ($Version -eq 'latest') { 'latest' } else { "tags/v$($Version.TrimStart('v'))" })
    Write-Step "Looking up the $Version release of $Repo"
    $release = Invoke-RestMethod $api -Headers @{ 'User-Agent' = 'open-hardware-info-installer' }
    $zip = $release.assets | Where-Object name -like 'OpenHardwareInfo-*-windows-x64.zip' | Select-Object -First 1
    $sums = $release.assets | Where-Object name -eq 'SHA256SUMS.txt' | Select-Object -First 1
    if (-not $zip -or -not $sums) { throw "Release $($release.tag_name) has no Windows package." }

    Write-Step "Downloading $($zip.name)"
    $zipPath = Join-Path $Destination $zip.name
    Invoke-WebRequest $zip.browser_download_url -OutFile $zipPath -UseBasicParsing
    $sumsText = (Invoke-WebRequest $sums.browser_download_url -UseBasicParsing).Content
    if ($sumsText -is [byte[]]) { $sumsText = [Text.Encoding]::ASCII.GetString($sumsText) }
    $expected = ($sumsText -split "`n" | Where-Object { $_ -match [regex]::Escape($zip.name) } | ForEach-Object { ($_ -split '\s+')[0] }) | Select-Object -First 1
    $actual = (Get-FileHash $zipPath -Algorithm SHA256).Hash
    if (-not $expected -or $actual -ne $expected.Trim().ToUpper()) { throw "Checksum mismatch for $($zip.name); not installing." }
    return $zipPath
}

$work = Join-Path $env:TEMP ("open-hardware-info-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null

try {
    if ($Uninstall) {
        if (-not (Test-Path $InstallDir)) { throw "Open Hardware Info is not installed in $InstallDir." }
        Write-Step 'Closing the app'
        Stop-App
        # Only this install's entry: another copy may have its own.
        $autostart = (Get-ItemProperty $runKey -ErrorAction SilentlyContinue).OpenHardwareInfo
        if ($autostart -and $autostart -like "*$InstallDir*") {
            Remove-ItemProperty $runKey -Name OpenHardwareInfo
        }
        Remove-Item $shortcut -ErrorAction SilentlyContinue
        Remove-Item $InstallDir -Recurse -Force
        Write-Step 'Open Hardware Info was removed. Your settings remain in %APPDATA%\OpenHardwareInfo.'
        return
    }

    $zipPath = Get-Package $work
    $files = Join-Path $work 'files'
    Expand-Archive $zipPath $files
    if (-not (Test-Path (Join-Path $files $exeName))) { throw "The package has no $exeName." }

    Write-Step 'Closing the running app, if any'
    Stop-App

    Write-Step "Installing to $InstallDir"
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Copy-Item (Join-Path $files '*') $InstallDir -Recurse -Force
    $exe = Join-Path $InstallDir $exeName

    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = $exe
    $link.WorkingDirectory = $InstallDir
    $link.Description = 'Resource monitor for the Windows taskbar'
    $link.Save()

    Start-Process $exe
    Write-Step 'Open Hardware Info is installed and running. It is also in the Start menu.'
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
