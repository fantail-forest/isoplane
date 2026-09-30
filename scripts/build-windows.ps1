# Build a Windows release from shared Linux outputs.
param([Parameter(ValueFromRemainingArguments=$true)][string[]]$BuildArguments)
$ErrorActionPreference = 'Stop'
& "$env:USERPROFILE/.local/bin/task.exe" --dir "$PSScriptRoot/.." release @BuildArguments
exit $LASTEXITCODE
