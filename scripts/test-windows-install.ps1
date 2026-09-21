# Run the SDK installation journey on a disposable Windows machine.
param(
    [Parameter(Mandatory)][string]$Release
)
$ErrorActionPreference = 'Stop'
$Release = (Resolve-Path $Release).Path
$manifest = Get-Content "$Release\release.json" -Raw | ConvertFrom-Json
if ($manifest.runtimeIdentifier -ne 'win-x64') { throw 'The journey requires a Windows x64 release.' }
$sdk = Join-Path $env:ProgramW6432 "Isoplane\SDK\$($manifest.packageVersion)"
if (Test-Path $sdk) { throw "The journey requires this release to be absent: $sdk" }
$workspace = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
$previousHome = $env:ISOPLANE_HOME
$previousPackages = $env:NUGET_PACKAGES
$previousLocation = Get-Location
New-Item -ItemType Directory $workspace | Out-Null
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
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $start = [Diagnostics.ProcessStartInfo]::new($Cli)
    $start.Arguments = 'dev --project "' + (Get-Location).Path + '" --configuration ' + $Configuration + ' --no-watch --shutdown-on-stdin-close --port ' + $port
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.Encoding]::UTF8
    $start.StandardErrorEncoding = [Text.Encoding]::UTF8
    $process = [Diagnostics.Process]::Start($start)
    $output = $process.StandardOutput.ReadToEndAsync()
    $errors = $process.StandardError.ReadToEndAsync()
    try {
        $deadline = [DateTime]::UtcNow.AddMinutes(10)
        $response = $null
        while ([DateTime]::UtcNow -lt $deadline -and !$process.HasExited) {
            try { $response = Invoke-WebRequest "http://127.0.0.1:$port/" -UseBasicParsing -TimeoutSec 5 }
            catch [Net.WebException] { Start-Sleep -Milliseconds 100; continue }
            break
        }
        if ($null -eq $response -or $response.Content -ne 'Hello from Isoplane!') {
            throw "$Profile did not serve the template response."
        }
        $process.StandardInput.Close()
        if (!$process.WaitForExit(30000)) { throw "$Profile did not stop after stdin closed." }
        if ($process.ExitCode -ne 0) { throw "$Profile exited with $($process.ExitCode)." }
    }
    finally {
        if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
        Set-Content -Encoding UTF8 (Join-Path $workspace "$Profile-http.log") ($output.GetAwaiter().GetResult() + $errors.GetAwaiter().GetResult())
        $process.Dispose()
    }
}
try {
    Set-Location $workspace
    Invoke-Dotnet tool install Isoplane.Cli --tool-path $toolDirectory --add-source "$Release\packages" --version $manifest.packageVersion
    Invoke-Cli setup --source $Release --yes
    Invoke-Cli setup --source $Release --yes
    Invoke-Dotnet new isoplane-web --name Sample
    Set-Location (Join-Path $workspace 'Sample')
    Invoke-Dotnet build
    Test-HttpGuest Mono Debug
    Invoke-Dotnet build -c IsoplaneNativeAotRelease
    Test-HttpGuest NativeAOT IsoplaneNativeAotRelease
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
    Write-Output "Journey artifacts: $workspace"
}
