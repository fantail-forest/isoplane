# Run the SDK installation journey on a disposable Windows machine.
param(
    [Parameter(Mandatory)][string]$Release,
    [switch]$KeepArtifacts
)
$ErrorActionPreference = 'Stop'
$Release = (Resolve-Path $Release).Path
$manifest = Get-Content "$Release\release.json" -Raw | ConvertFrom-Json
if ($manifest.runtimeIdentifier -ne 'win-x64') { throw 'The journey requires a Windows x64 release.' }
$sdk = Join-Path $env:ProgramW6432 "Isoplane\SDK\$($manifest.packageVersion)"
if (Test-Path $sdk) { throw "The journey requires this release to be absent: $sdk" }
$runs = Join-Path (Split-Path $PSScriptRoot -Parent) '.test-runs\win-x64\installation'
$workspace = Join-Path $runs ([Guid]::NewGuid().ToString('N'))
$previousHome = $env:ISOPLANE_HOME
$previousPackages = $env:NUGET_PACKAGES
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
$previousLocation = Get-Location
New-Item -ItemType Directory $workspace -Force | Out-Null
$scratch = Join-Path $workspace 'tmp'
New-Item -ItemType Directory $scratch | Out-Null
$env:TEMP = $scratch
$env:TMP = $scratch
$env:ISOPLANE_HOME = Join-Path $workspace 'installation'
$env:NUGET_PACKAGES = Join-Path $workspace 'packages'
$toolDirectory = Join-Path $workspace 'tools'
$Cli = Join-Path $toolDirectory 'isoplane.exe'
function Invoke-Cli {
    & $Cli @args
    if ($LASTEXITCODE -ne 0) { throw "Isoplane failed: $args (exit $LASTEXITCODE)" }
}
function Invoke-Dotnet {
    & dotnet @args
    if ($LASTEXITCODE -ne 0) { throw "dotnet failed: $args (exit $LASTEXITCODE)" }
}
function Test-HttpGuest([string]$Profile, [string]$Configuration) {
    Invoke-Dotnet run --project (Join-Path $workspace 'Consumer') --no-restore -- (Get-Location).Path $Cli $Configuration
    Write-Output "PASS: $Profile through the installed testing package"
}

try {
    Set-Location $workspace
    Invoke-Dotnet tool install Isoplane.Cli --tool-path $toolDirectory --add-source "$Release\packages" --version $manifest.packageVersion
    Invoke-Cli setup --source $Release --yes
    Invoke-Cli setup --source $Release --yes
    Invoke-Dotnet new isoplane-web --name Sample
    $consumer = Join-Path $workspace 'Consumer'
    New-Item -ItemType Directory $consumer | Out-Null
    Set-Content -Encoding UTF8 "$consumer/Consumer.csproj" @"
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup>
  <ItemGroup><PackageReference Include="Isoplane.Testing" Version="$($manifest.packageVersion)" /></ItemGroup>
</Project>
"@
    Set-Content -Encoding UTF8 "$consumer/Program.cs" @'
using Isoplane.Testing;
using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(10));
var app = await IsoplaneApplication.StartAsync(args[0],
    new ApplicationOptions { CliPath = args[1], Configuration = args[2] }, deadline.Token);
var address = app.Client.BaseAddress!;
try
{
    using var response = await app.Client.GetAsync("/", deadline.Token);
    var body = await response.Content.ReadAsStringAsync(deadline.Token);
    if (!response.IsSuccessStatusCode || body != "Hello from Isoplane!")
        throw new Exception($"The sandbox returned an unexpected response: {response.StatusCode} {body}\n{app.DiagnosticOutput}");
}
finally { await app.DisposeAsync(); }
using var client = new HttpClient();
try { await client.GetAsync(address, deadline.Token); }
catch (HttpRequestException) { return; }
throw new Exception("Disposal left the application listener running.");
'@
    Invoke-Dotnet restore "$consumer/Consumer.csproj"
    Set-Location (Join-Path $workspace 'Sample')
    Invoke-Dotnet build
    Test-HttpGuest Mono Debug
    Invoke-Dotnet build -c Release
    Test-HttpGuest Release Release
    Remove-Item "$sdk\bin\wasm-tools.exe"
    Invoke-Cli setup --source $Release --yes
    if (!(Test-Path "$sdk\bin\wasm-tools.exe")) { throw 'Repair did not restore the missing tool.' }
    Set-Content "$sdk\customer-owned.txt" 'Customer content'
    Invoke-Cli uninstall-sdk
    if (Test-Path "$sdk\Sdk\Sdk.props") { throw 'Removal left the SDK installed.' }
    if (!(Test-Path "$sdk\customer-owned.txt")) { throw 'Removal deleted customer-owned content.' }
    Remove-Item "$sdk\customer-owned.txt"
    # An empty customer directory is deliberately not an installation owned by setup.
    Remove-Item $sdk
    Invoke-Cli setup --source $Release --yes
    Invoke-Cli doctor
    Invoke-Cli uninstall-sdk
    Write-Output 'PASS: install, repeat setup, both guest profiles, repair, removal, preservation, reinstall'
}
finally {
    Set-Location $previousLocation
    $env:ISOPLANE_HOME = $previousHome
    $env:NUGET_PACKAGES = $previousPackages
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
    if ($KeepArtifacts) {
        Write-Output "Journey artifacts (caller-managed): $workspace"
    } else {
        Remove-Item -LiteralPath $workspace -Recurse -Force
    }
}
