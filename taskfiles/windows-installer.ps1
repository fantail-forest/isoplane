# Harvest the prepared payload with msitools and package the Windows installer.
param([Parameter(Mandatory)][string]$Stage, [Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
$payload = (Resolve-Path "$Stage/payload").Path
$bundle = New-Item -ItemType Directory -Force "$Stage/bundle"
$files = Get-ChildItem -File -Recurse $payload | ForEach-Object { $_.FullName }
$fragment = $files | & wixl-heat --directory-ref INSTALLDIR --component-group Payload --var var.Payload --prefix "$payload\" --win64
if ($LASTEXITCODE -ne 0) { throw 'WiX payload harvesting failed.' }
$fragment | Set-Content -Encoding UTF8 "$Stage/payload.wxs"
$product = [Guid]::NewGuid().ToString().ToUpperInvariant()
& wixl -a x64 -D "Payload=$payload" -D 'Win64=yes' -D "ProductCode=$product" -D "Version=$Version" -D "NumericVersion=$($Version.Split('-')[0])" -o "$bundle/sdk.msi" "$PSScriptRoot/WindowsSdk.wxs" "$Stage/payload.wxs"
if ($LASTEXITCODE -ne 0) { throw 'MSI packaging failed.' }
$locator = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
$visualStudio = & $locator -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or !$visualStudio) { throw 'Visual Studio C++ Build Tools were not found.' }
$redist = Get-ChildItem "$visualStudio/VC/Redist/MSVC/*/vc_redist.x64.exe" | Sort-Object FullName | Select-Object -Last 1
if (!$redist) { throw 'The Visual C++ redistributable was not found.' }
Copy-Item $redist.FullName "$bundle/vc_redist.x64.exe"
@{Version=$Version; ProductCode=$product} | ConvertTo-Json | Set-Content -Encoding UTF8 "$bundle/WindowsInstaller.json"
