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

The optional CPU temperature needs the PawnIO driver and a small service. Add or
remove them from an elevated PowerShell, which leaves the app itself untouched:

    & ([scriptblock]::Create((irm https://github.com/__REPO__/releases/latest/download/install.ps1))) -InstallCpuTemp
    & ([scriptblock]::Create((irm https://github.com/__REPO__/releases/latest/download/install.ps1))) -UninstallCpuTemp

This script must keep working in Windows PowerShell 5.1, must never call `exit`
(that would close the user's shell under `irm | iex`), and must verify checksums
before installing.
#>
param(
    # Release to install, e.g. 0.2.0. Defaults to the latest.
    [string]$Version = 'latest',
    # Remove the app instead of installing it.
    [switch]$Uninstall,
    # Install PawnIO, if missing, and the service behind the CPU temperature. Needs elevation.
    [switch]$InstallCpuTemp,
    # Remove that service. PawnIO stays, because other programs may rely on it. Needs elevation.
    [switch]$UninstallCpuTemp,
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

$serviceName = 'OpenHardwareInfoSensor'
$sensorExeName = 'ohi-sensor.exe'
# This service runs as SYSTEM, so its binary must never be in a user-writable folder.
$sensorDir = Join-Path $env:ProgramFiles 'OpenHardwareInfo'
$pawnIoMinimum = [version]'2.2.0'
$pawnIoUrl = 'https://github.com/namazso/PawnIO.Setup/releases/download/2.2.0/PawnIO_setup.exe'
$pawnIoSha256 = '1F519A22E47187F70A1379A48CA604981C4FCF694F4E65B734AAA74A9FBA3032'

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }

function Test-Elevated {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Stop-Sensor {
    if (-not (Get-Service $serviceName -ErrorAction SilentlyContinue)) { return }
    Stop-Service $serviceName -Force
    # The status changes just before the process exits and releases the file.
    Get-Process ($sensorExeName -replace '\.exe$') -ErrorAction SilentlyContinue | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
}

# Runs this release's installer in a new elevated window, which stays open so its
# result can be read.
function Start-ElevatedSensorInstall {
    $url = if ($Version -eq 'latest') { "https://github.com/$Repo/releases/latest/download/install.ps1" } else { "https://github.com/$Repo/releases/download/v$($Version.TrimStart('v'))/install.ps1" }
    $command = "[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12; " +
        "try { & ([scriptblock]::Create((irm '$url'))) -InstallCpuTemp -Version '$Version' } catch { Write-Host $_ -ForegroundColor Red } finally { Read-Host 'Press Enter to close' }"
    Start-Process powershell.exe -Verb RunAs -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', "`"$command`""
}

# Offered once, on a first install at an interactive prompt: updates must not ask
# again, and a kernel driver is never installed without an explicit yes.
function Invoke-SensorOffer {
    if ($PackagePath -or -not [Environment]::UserInteractive -or [Console]::IsInputRedirected) { return }
    if (Get-Service $serviceName -ErrorAction SilentlyContinue) { return }
    Write-Host ''
    Write-Host 'Optional: CPU temperature needs the PawnIO kernel driver and a small service, which require administrator rights.'
    Write-Host 'Some anti-cheats may block games while PawnIO is installed. You can also install it later from the app settings.'
    $answer = Read-Host 'Install it now? [y/N]'
    if ($answer -notmatch '^(y|yes|s|sim)$') { return }
    try {
        Start-ElevatedSensorInstall
        Write-Step 'Finish in the new administrator window, then turn on "Temperatura da CPU" in the app settings.'
    } catch {
        Write-Warning 'The installation was not started. You can install it later from the app settings.'
    }
}

function Install-PawnIo([string]$Destination) {    $installed = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO' -ErrorAction SilentlyContinue).DisplayVersion
    if ($installed -and [version]$installed -ge $pawnIoMinimum) {
        Write-Step "PawnIO $installed is already installed"
        return
    }
    Write-Step 'Downloading the PawnIO driver'
    $setup = Join-Path $Destination 'PawnIO_setup.exe'
    Invoke-WebRequest $pawnIoUrl -OutFile $setup -UseBasicParsing
    $actual = (Get-FileHash $setup -Algorithm SHA256).Hash
    if ($actual -ne $pawnIoSha256) { throw 'Checksum mismatch for PawnIO_setup.exe; not installing.' }
    Write-Step 'Installing the PawnIO driver'
    $code = (Start-Process $setup -ArgumentList '-install', '-silent' -Wait -PassThru).ExitCode
    if ($code -eq 3010) {
        Write-Warning 'Restart Windows to finish installing PawnIO. Until then the temperature shows "--".'
    } elseif ($code -ne 0) {
        throw "The PawnIO installer failed with code $code."
    }
}

function Register-Sensor([string]$Exe) {
    if (-not (Get-Service $serviceName -ErrorAction SilentlyContinue)) {
        # The quotes matter: an unquoted path with spaces lets Windows run C:\Program.exe.
        New-Service -Name $serviceName -BinaryPathName "`"$Exe`"" -DisplayName 'Open Hardware Info Sensor' `
            -Description 'Reads the CPU temperature for Open Hardware Info.' -StartupType Automatic | Out-Null
    }
    # Drop every privilege the service does not need, and restart it if it crashes.
    & sc.exe privs $serviceName 'SeCreateGlobalPrivilege/SeChangeNotifyPrivilege' | Out-Null
    if ($LASTEXITCODE) { throw "Could not restrict the service's privileges (sc.exe code $LASTEXITCODE)." }
    & sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/60000// | Out-Null
    if ($LASTEXITCODE) { throw "Could not set the service's recovery options (sc.exe code $LASTEXITCODE)." }
}

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

$changesSensor = $InstallCpuTemp -or $UninstallCpuTemp
if ($changesSensor -and -not (Test-Elevated)) {
    throw 'The CPU temperature options need an elevated PowerShell (Run as administrator).'
}

# Files that will run elevated stay out of %TEMP%, where any unelevated program
# could replace them after the checksum is verified.
$workRoot = if ($InstallCpuTemp) { $sensorDir } else { $env:TEMP }
$work = Join-Path $workRoot ("open-hardware-info-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work -Force | Out-Null

try {
    if ($UninstallCpuTemp) {
        Write-Step 'Removing the CPU temperature service'
        Stop-Sensor
        if (Get-Service $serviceName -ErrorAction SilentlyContinue) {
            & sc.exe delete $serviceName | Out-Null
            if ($LASTEXITCODE) { throw "Could not delete the service (sc.exe code $LASTEXITCODE)." }
        }
        Remove-Item $sensorDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Step 'The service was removed. PawnIO stays, because other programs may use it; you can remove it from Windows Settings.'
        return
    }

    if ($InstallCpuTemp) {
        Install-PawnIo $work
        $zipPath = Get-Package $work
        $files = Join-Path $work 'files'
        Expand-Archive $zipPath $files
        $source = Join-Path $files $sensorExeName
        if (-not (Test-Path $source)) { throw "The package has no $sensorExeName." }

        Write-Step "Installing the service to $sensorDir"
        Stop-Sensor
        $sensorExe = Join-Path $sensorDir $sensorExeName
        Copy-Item $source $sensorExe -Force
        Register-Sensor $sensorExe
        Start-Service $serviceName
        Write-Step 'The service is running. Turn on "Temperatura da CPU" in the app settings.'
        return
    }

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
        if (Get-Service $serviceName -ErrorAction SilentlyContinue) {
            Write-Warning 'The CPU temperature service is still installed. Remove it from an elevated PowerShell with -UninstallCpuTemp.'
        }
        return
    }

    $zipPath = Get-Package $work
    $files = Join-Path $work 'files'
    Expand-Archive $zipPath $files
    if (-not (Test-Path (Join-Path $files $exeName))) { throw "The package has no $exeName." }

    Write-Step 'Closing the running app, if any'
    Stop-App

    $firstInstall = -not (Test-Path (Join-Path $InstallDir $exeName))
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

    if (Test-Elevated) {
        # The app needs no privileges and must not inherit these. Explorer opens
        # it in the unelevated desktop session instead.
        Write-Warning "This PowerShell is elevated, and the app does not need it. It was installed for the account $env:USERNAME."
        Start-Process explorer.exe -ArgumentList "`"$exe`""
    } else {
        Start-Process $exe
    }
    Write-Step 'Open Hardware Info is installed and running. It is also in the Start menu.'
    if ($firstInstall) { Invoke-SensorOffer }
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
