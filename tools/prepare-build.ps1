#Requires -Version 5.1
# Tauri build output contains absolute permission paths and is not relocatable.
[CmdletBinding()]
param([switch]$CheckOnly)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$profile = if ($env:TAURI_ENV_DEBUG -eq 'true') { 'debug' } else { 'release' }

function Get-NormalPath([string]$Value) {
    if ($Value.StartsWith('\\?\')) { $Value = $Value.Substring(4) }
    return [IO.Path]::GetFullPath($Value)
}

Push-Location -LiteralPath $projectRoot
try {
    $metadataText = & cargo metadata --offline --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Could not resolve Cargo build directories.' }
    $metadata = $metadataText | ConvertFrom-Json
    $targetRoot = Get-NormalPath $metadata.target_directory
    $workspacePrefix = $projectRoot.TrimEnd('\') + '\'
    if (-not $targetRoot.StartsWith($workspacePrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Automatic cache repair requires a target directory inside this workspace: $targetRoot"
    }

    $buildRoots = @((Join-Path $targetRoot "$profile/build"))
    if ($env:TAURI_ENV_TARGET_TRIPLE) {
        $buildRoots += Join-Path $targetRoot "$($env:TAURI_ENV_TARGET_TRIPLE)/$profile/build"
    }
    $stalePackages = New-Object 'System.Collections.Generic.HashSet[string]'
    foreach ($buildRoot in $buildRoots) {
        if (-not (Test-Path -LiteralPath $buildRoot -PathType Container)) { continue }
        $buildPrefix = (Get-NormalPath $buildRoot).TrimEnd('\') + '\'
        foreach ($directory in Get-ChildItem -LiteralPath $buildRoot -Directory -Filter 'tauri-*') {
            if ($directory.Name -notmatch '^(tauri(?:-[a-z0-9-]+)?)-[a-f0-9]{16}$') { continue }
            $package = $Matches[1]
            $output = Join-Path $directory.FullName 'output'
            if (-not (Test-Path -LiteralPath $output -PathType Leaf)) { continue }
            foreach ($line in Get-Content -LiteralPath $output) {
                if ($line -notmatch 'PERMISSION_FILES_PATH=(.+)$') { continue }
                try {
                    $manifest = Get-NormalPath $Matches[1]
                    if (-not $manifest.StartsWith($buildPrefix, [StringComparison]::OrdinalIgnoreCase) -or
                        -not (Test-Path -LiteralPath $manifest -PathType Leaf)) { throw 'Stale manifest path.' }
                    $files = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
                    foreach ($file in $files) {
                        $permissionPath = Get-NormalPath $file
                        # Core permissions are generated in target; plugins can reference Cargo registry sources.
                        if (($package -eq 'tauri' -and -not $permissionPath.StartsWith($buildPrefix, [StringComparison]::OrdinalIgnoreCase)) -or
                            -not (Test-Path -LiteralPath $permissionPath -PathType Leaf)) { throw 'Stale permission path.' }
                    }
                } catch {
                    [void]$stalePackages.Add($package)
                    break
                }
            }
        }
    }

    if ($stalePackages.Count -eq 0) {
        Write-Host "Tauri $profile permission cache is valid."
        return
    }
    # Regenerate the consumer as well as the crates which produced stale paths.
    [void]$stalePackages.Add('synco-desktop')
    $packages = @($stalePackages | Sort-Object)
    Write-Host "Stale Tauri permission cache: $($packages -join ', ')"
    if ($CheckOnly) { return }
    $cleanArgs = @('clean', '--manifest-path', (Join-Path $projectRoot 'Cargo.toml'), '--target-dir', $targetRoot, '--profile', $profile)
    foreach ($package in $packages) { $cleanArgs += @('--package', $package) }
    & cargo @cleanArgs
    if ($LASTEXITCODE -ne 0) { throw 'Failed to rebuild stale Tauri cache. No packaging was started.' }
    Write-Host 'Stale Tauri cache cleared. Existing installers and application data are preserved.'
} finally {
    Pop-Location
}
