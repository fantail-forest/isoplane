# Build locally, provisioning missing tools before starting Python.
param([Parameter(ValueFromRemainingArguments=$true)][string[]]$BuildArguments)
$ErrorActionPreference = 'Stop'
$python = Get-Command python.exe -All -ErrorAction SilentlyContinue | Where-Object { $_.Source -notlike '*\WindowsApps\*' } | Select-Object -First 1
if ($python) {
    & $python.Source -c 'import sys; sys.exit(sys.version_info < (3, 11))'
    if ($LASTEXITCODE -eq 0) {
        & $python.Source (Join-Path $PSScriptRoot 'build-windows.py') @BuildArguments
        exit $LASTEXITCODE
    }
}
$setup = @{}
$index = [Array]::IndexOf($BuildArguments, '--msys-root')
if ($index -ge 0) {
    if ($index + 1 -ge $BuildArguments.Count) { throw '--msys-root requires a directory.' }
    $setup.MsysRoot = $BuildArguments[$index + 1]
}
if ($BuildArguments -contains '--check-prerequisites') { $setup.CheckOnly = $true }
& (Join-Path $PSScriptRoot 'setup-windows.ps1') @setup
& python.exe (Join-Path $PSScriptRoot 'build-windows.py') @BuildArguments
exit $LASTEXITCODE
