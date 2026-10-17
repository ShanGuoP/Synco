#Requires -Version 5.1
# Run from any working directory. Tauri invokes prepare-build.ps1 before Cargo.
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$BuildArguments)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location -LiteralPath $projectRoot
try {
    & npx --yes '@tauri-apps/cli@2.12.2' build @BuildArguments
    if ($LASTEXITCODE -ne 0) { throw "Packaging failed (exit code $LASTEXITCODE)." }
} finally {
    Pop-Location
}
