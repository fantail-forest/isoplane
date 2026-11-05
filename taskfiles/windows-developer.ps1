# Run a development command in the Visual Studio build environment.
$ErrorActionPreference = 'Stop'
$locator = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
$components = Get-Content "$PSScriptRoot/../scripts/windows-build-components.json" -Raw | ConvertFrom-Json
$installation = & $locator -latest -products '*' -requires $components -property installationPath
if ($LASTEXITCODE -ne 0 -or !$installation) { throw 'Install Visual Studio C++ Build Tools, including ATL and CMake tools.' }
& "$installation\Common7\Tools\Launch-VsDevShell.ps1" -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
$env:MSYSTEM = 'MINGW64'
$env:MSYS2_PATH_TYPE = 'inherit'
if ($args.Count -eq 0) { throw 'Provide a development command to run.' }
$command = $args[0]
$commandArguments = @($args | Select-Object -Skip 1)
& $command @commandArguments
exit $LASTEXITCODE
