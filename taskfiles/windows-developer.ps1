# Run a development command in the Visual Studio build environment.
$ErrorActionPreference = 'Stop'
$locator = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
$components = Get-Content "$PSScriptRoot/../scripts/windows-build-components.json" -Raw | ConvertFrom-Json
$installation = & $locator -latest -products '*' -requires $components -property installationPath
if ($LASTEXITCODE -ne 0 -or !$installation) { throw 'Install Visual Studio C++ Build Tools, including ATL and CMake tools.' }
$environment = & cmd.exe /d /s /c "`"call `"$installation\Common7\Tools\VsDevCmd.bat`" -arch=amd64 -host_arch=amd64 >nul && set`""
if ($LASTEXITCODE -ne 0) { throw 'Visual Studio environment setup failed.' }
foreach ($line in $environment) {
    if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process') }
}
$env:MSYSTEM = 'MINGW64'
$env:MSYS2_PATH_TYPE = 'inherit'
if ($args.Count -eq 0) { throw 'Provide a development command to run.' }
$command = $args[0]
$commandArguments = @($args | Select-Object -Skip 1)
& $command @commandArguments
exit $LASTEXITCODE
