# Provision missing Windows build tools; -CheckOnly reports without installing.
param([string]$MsysRoot = 'C:\msys64', [switch]$CheckOnly, [switch]$GitOnly, [switch]$MachineOnly)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$components = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-build-components.json') -Raw | ConvertFrom-Json

function Refresh-Path {
    $paths = @([Environment]::GetEnvironmentVariable('Path', 'Machine'),
               [Environment]::GetEnvironmentVariable('Path', 'User'), $env:Path,
               (Join-Path $env:USERPROFILE '.cargo\bin'))
    $env:Path = ($paths | Where-Object { $_ }) -join ';'
}
function Invoke-Tool([string]$Tool, [string[]]$Arguments) {
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Tool failed with exit code $LASTEXITCODE. Resolve the installer error, then rerun the build; no automatic reboot is performed." }
}
function Require-Installation([string]$Name) {
    if ($CheckOnly) { throw "Missing prerequisite: $Name. Run the build without --check-prerequisites to install it." }
    Write-Host "Installing $Name..."
}
function Install-Package([string]$Id, [string[]]$Extra = @()) {
    if (!(Get-Command winget.exe -ErrorAction SilentlyContinue)) {
        Write-Host 'Installing Windows Package Manager...'
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Install-PackageProvider -Name NuGet -Force -Scope CurrentUser | Out-Null
        Install-Module Microsoft.WinGet.Client -Repository PSGallery -Scope CurrentUser -Force | Out-Null
        Repair-WinGetPackageManager
        Refresh-Path
    }
    $winget = (Get-Command winget.exe).Source
    if ($env:SSH_CONNECTION) {
        # Packaged WinGet cannot register its catalog over SSH (microsoft/winget-cli#6334).
        $package = Get-AppxPackage Microsoft.DesktopAppInstaller
        if (!$package) { throw 'Windows Package Manager is not registered for this account. Install App Installer before using SSH provisioning.' }
        $directory = Join-Path $env:LOCALAPPDATA ("Isoplane\build-tools\winget-" + $package.Version)
        $winget = Join-Path $directory 'winget.exe'
        if (!(Test-Path -LiteralPath $directory)) {
            New-Item -ItemType Directory -Path (Split-Path $directory) -Force | Out-Null
            $staging = $directory + '.' + [Guid]::NewGuid().ToString('N')
            try {
                Copy-Item -LiteralPath $package.InstallLocation -Destination $staging -Recurse
                Move-Item -LiteralPath $staging -Destination $directory
            } finally {
                if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
            }
        }
    }
    Invoke-Tool $winget (@('install', '--id', $Id, '--exact', '--source', 'winget', '--silent',
        '--accept-source-agreements', '--accept-package-agreements', '--disable-interactivity', '--no-upgrade') + $Extra)
    Refresh-Path
}
function Find-VisualStudio {
    $locator = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $locator) {
        & $locator -latest -products '*' -requires $components -property installationPath
        if ($LASTEXITCODE -ne 0) { throw 'Visual Studio discovery failed' }
    }
}
function Install-MachineTools {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (!$admin) {
        if ($env:SSH_CONNECTION) { throw 'Machine prerequisites need elevation. Run the build once locally and accept UAC, or connect using an administrator SSH account.' }
        $command = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $PSCommandPath + '"'), '-MachineOnly')
        $process = Start-Process powershell.exe -Verb RunAs -ArgumentList $command -PassThru -Wait
        if ($process.ExitCode -ne 0) { throw "Elevated prerequisite installation failed ($($process.ExitCode)); rerun the build after resolving it." }
        return
    }
    if (!(Find-VisualStudio)) {
        $locator = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        $existing = if (Test-Path -LiteralPath $locator) { & $locator -latest -products '*' -property installationPath }
        $componentArguments = '--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended ' + (($components | ForEach-Object { '--add ' + $_ }) -join ' ')
        if ($existing) {
            $installer = Join-Path (Split-Path $locator) 'setup.exe'
            $process = Start-Process $installer -ArgumentList ('modify --installPath "' + $existing + '" --quiet --norestart ' + $componentArguments) -Wait -PassThru
            if ($process.ExitCode -eq 3010) { throw 'Visual Studio requires a reboot. Restart Windows, then rerun the build.' }
            if ($process.ExitCode -ne 0) { throw "Visual Studio modification failed ($($process.ExitCode))." }
        } else {
            Install-Package 'Microsoft.VisualStudio.2022.BuildTools' @('--override', ('--quiet --wait --norestart ' + $componentArguments))
        }
        if (!(Find-VisualStudio)) { throw 'Visual Studio C++ tools are still unavailable; inspect the installer logs and rerun the build.' }
    }
    Set-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -Type DWord -Value 1
}

Refresh-Path
if ($MachineOnly) { Install-MachineTools; return }
if (!(Get-Command git.exe -ErrorAction SilentlyContinue)) {
    Require-Installation 'Git'
    Install-Package 'Git.Git'
    if (!(Get-Command git.exe -ErrorAction SilentlyContinue)) { throw 'Git installation did not provide git.exe on PATH.' }
}
if ($GitOnly) { return }

$python = Get-Command python.exe -All -ErrorAction SilentlyContinue | Where-Object { $_.Source -notlike '*\WindowsApps\*' } | Select-Object -First 1
$pythonReady = $false
if ($python) {
    & $python.Source -c 'import sys; sys.exit(sys.version_info < (3, 11))'
    $pythonReady = $LASTEXITCODE -eq 0
}
if (!$pythonReady) {
    Require-Installation 'Python 3.13'
    Install-Package 'Python.Python.3.13'
    $python = Get-Command python.exe -All -ErrorAction SilentlyContinue | Where-Object { $_.Source -notlike '*\WindowsApps\*' } | Select-Object -First 1
    if (!$python) { throw 'Python installation did not provide python.exe on PATH.' }
}
$longPaths = (Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem').LongPathsEnabled
if (!(Find-VisualStudio) -or $longPaths -ne 1) {
    Require-Installation 'Visual Studio C++ tools and Windows long-path support'
    Install-MachineTools
}

if (!(Test-Path -LiteralPath (Join-Path $MsysRoot 'usr\bin\bash.exe'))) {
    Require-Installation 'MSYS2'
    Install-Package 'MSYS2.MSYS2' @('--location', $MsysRoot)
    if (!(Test-Path -LiteralPath (Join-Path $MsysRoot 'usr\bin\bash.exe'))) { throw "MSYS2 was not installed at $MsysRoot. Select its existing location with --msys-root." }
}
$bash = Join-Path $MsysRoot 'usr\bin\bash.exe'
$env:MSYSTEM = 'MINGW64'
$env:CHERE_INVOKING = '1'
$packages = @{
    'usr\bin\make.exe' = 'make'; 'usr\bin\perl.exe' = 'perl'; 'usr\bin\diff.exe' = 'diffutils'
    'usr\bin\patch.exe' = 'patch'; 'usr\bin\tar.exe' = 'tar'; 'usr\bin\unzip.exe' = 'unzip'
    'usr\bin\curl.exe' = 'curl'
    'mingw64\bin\gcc.exe' = 'mingw-w64-x86_64-gcc'; 'mingw64\bin\python.exe' = 'mingw-w64-x86_64-python'
}
$missing = @($packages.Keys | Where-Object { !(Test-Path -LiteralPath (Join-Path $MsysRoot $_)) } | ForEach-Object { $packages[$_] })
if ($missing.Count) {
    Require-Installation ('MSYS2 packages: ' + ($missing -join ', '))
    Invoke-Tool $bash @('-lc', 'pacman -Syu --noconfirm')
    Invoke-Tool $bash @('-lc', 'pacman -Syu --noconfirm')
    Invoke-Tool $bash @('-lc', ('pacman -S --needed --noconfirm ' + ($missing -join ' ')))
}
foreach ($path in $packages.Keys) {
    if (!(Test-Path -LiteralPath (Join-Path $MsysRoot $path))) { throw "Missing prerequisite after MSYS2 installation: $path" }
}
if (!(Get-Command rustup.exe -ErrorAction SilentlyContinue)) {
    Require-Installation 'Rustup'
    Install-Package 'Rustlang.Rustup'
}
$dockerfile = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\Dockerfile') -Raw
if ($dockerfile -notmatch '(?m)^ARG RUST_VERSION=(\S+)') { throw 'The build toolchain version is missing from Dockerfile.' }
$toolchain = $Matches[1] + '-x86_64-pc-windows-msvc'
$env:RUSTUP_AUTO_INSTALL = '0'
$toolchains = & rustup.exe toolchain list
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect installed Rust toolchains.' }
if (!($toolchains | Where-Object { $_.Split(' ')[0] -eq $toolchain })) {
    Require-Installation ("Rust " + $toolchain)
    Invoke-Tool rustup.exe @('toolchain', 'install', $toolchain, '--profile', 'minimal')
}
$env:RUSTUP_TOOLCHAIN = $toolchain
$env:Path = (Split-Path $python.Source) + ';' + $env:Path
Write-Host 'Windows build prerequisites are ready.'
