# Run the SDK installation journey on a disposable Windows machine.
param(
    [Parameter(Mandatory)][string]$Release,
    [switch]$KeepArtifacts
)
$ErrorActionPreference = 'Stop'
$Release = (Resolve-Path $Release).Path
$manifest = Get-Content "$Release\release.json" -Raw | ConvertFrom-Json
if ($manifest.runtimeIdentifier -ne 'win-x64') { throw 'The journey requires a Windows x64 release.' }
$runs = Join-Path (Split-Path $PSScriptRoot -Parent) '.test-runs\win-x64\installation'
$workspace = Join-Path $runs ([Guid]::NewGuid().ToString('N'))
$previousCliHome = $env:DOTNET_CLI_HOME
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
$env:DOTNET_CLI_HOME = $workspace
$env:ISOPLANE_HOME = Join-Path $workspace 'installation'
$env:NUGET_PACKAGES = Join-Path $workspace 'packages'
$toolDirectory = Join-Path $workspace 'tools'
$Cli = Join-Path $toolDirectory 'isoplane.exe'
$customerFile = $null
function Invoke-Cli {
    & $Cli @args
    if ($LASTEXITCODE -ne 0) { throw "Isoplane failed: $args (exit $LASTEXITCODE)" }
}
function Invoke-Dotnet {
    & dotnet @args
    if ($LASTEXITCODE -ne 0) { throw "dotnet failed: $args (exit $LASTEXITCODE)" }
}
function Test-HttpGuest([string]$Profile, [string]$Configuration) {
    Invoke-Dotnet run --project (Join-Path $workspace 'Consumer') --no-restore --disable-build-servers -- (Get-Location).Path $Cli $Configuration
    Write-Output "PASS: $Profile through the installed testing package"
}

try {
    Set-Location $workspace
    foreach ($file in @('Directory.Build.props', 'Directory.Build.targets', 'Directory.Packages.props')) {
        Set-Content -Encoding UTF8 (Join-Path $workspace $file) '<Project />'
    }
    Set-Content -Encoding UTF8 (Join-Path $workspace 'global.json') '{"sdk":{"version":"10.0.100","rollForward":"latestFeature","allowPrerelease":false}}'
    $feed = [Security.SecurityElement]::Escape((Join-Path $Release 'packages'))
    Set-Content -Encoding UTF8 (Join-Path $workspace 'nuget.config') @"
<configuration>
  <packageSources><clear/><add key="release" value="$feed"/><add key="nuget" value="https://api.nuget.org/v3/index.json"/></packageSources>
  <packageSourceMapping><packageSource key="release"><package pattern="Isoplane.*"/></packageSource><packageSource key="nuget"><package pattern="*"/></packageSource></packageSourceMapping>
</configuration>
"@
    Invoke-Dotnet tool install Isoplane.Cli --tool-path $toolDirectory --configfile (Join-Path $workspace 'nuget.config') --version $manifest.packageVersion
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
    Invoke-Dotnet build --disable-build-servers
    Test-HttpGuest Mono Debug
    Invoke-Dotnet build -c Release --disable-build-servers
    Test-HttpGuest Release Release
    $templatePackage = Join-Path $env:ISOPLANE_HOME "packages/Isoplane.Templates.$($manifest.packageVersion).nupkg"
    Remove-Item $templatePackage
    Invoke-Cli setup --source $Release --yes
    if (!(Test-Path $templatePackage)) { throw 'Repair did not restore the missing package.' }
    $customerFile = Join-Path $env:ISOPLANE_HOME 'customer-owned.txt'
    Set-Content $customerFile 'Customer content'
    Invoke-Cli uninstall-sdk
    if (Test-Path $templatePackage) { throw 'Removal left the owned package installed.' }
    if (!(Test-Path $customerFile)) { throw 'Removal deleted customer-owned content.' }
    Remove-Item $customerFile
    Invoke-Cli setup --source $Release --yes
    Invoke-Cli doctor
    Invoke-Cli uninstall-sdk
    Write-Output 'PASS: install, repeat setup, both guest profiles, repair, removal, preservation, reinstall'
}
finally {
    $installationRemoved = $false
    try {
        if (Test-Path (Join-Path $env:ISOPLANE_HOME 'installation.json')) {
            Invoke-Cli uninstall-sdk
        }
        if ($customerFile -and (Test-Path -LiteralPath $customerFile)) {
            Remove-Item -LiteralPath $customerFile
        }
        $installationRemoved = $true
    }
    finally {
        Set-Location $previousLocation
        $env:DOTNET_CLI_HOME = $previousCliHome
        $env:ISOPLANE_HOME = $previousHome
        $env:NUGET_PACKAGES = $previousPackages
        $env:TEMP = $previousTemp
        $env:TMP = $previousTmp
        if (!$installationRemoved) {
            Write-Warning "SDK teardown failed; recovery manifest and tool retained at $workspace"
        } elseif ($KeepArtifacts) {
            Write-Output "Journey artifacts (caller-managed): $workspace"
        } else {
            Remove-Item -LiteralPath $workspace -Recurse -Force
        }
    }
}
