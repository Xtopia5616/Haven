$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$uiPackagesRoot = Join-Path $repositoryRoot 'ui/node_modules/.pnpm'
$outputPath = Join-Path $repositoryRoot 'docs/dependency-license-inventory.md'

if (-not (Test-Path -LiteralPath $uiPackagesRoot)) {
    throw 'Install the locked UI dependencies with corepack pnpm --dir ui install --frozen-lockfile first.'
}

$cargoOutput = & cargo metadata --manifest-path (Join-Path $repositoryRoot 'Cargo.toml') --locked --format-version 1
if ($LASTEXITCODE -ne 0) {
    throw 'cargo metadata failed; the dependency inventory was not updated.'
}
$cargoMetadata = ($cargoOutput -join "`n") | ConvertFrom-Json -AsHashtable

function Get-LicenseExpression($value) {
    if ($null -eq $value) { return 'UNDECLARED' }
    if ($value -is [string]) { return $value }
    if ($value -is [System.Collections.IDictionary]) {
        if ($value.type) { return [string]$value.type }
        if ($value.url) { return "See license URL: $($value.url)" }
    }
    if ($value -is [System.Collections.IEnumerable]) {
        $parts = @($value | ForEach-Object { Get-LicenseExpression $_ })
        return $parts -join ' OR '
    }
    return 'See package metadata'
}

function Get-NpmPackageManifests([string]$packagesRoot) {
    foreach ($directory in Get-ChildItem -LiteralPath $packagesRoot -Directory) {
        $moduleRoot = Join-Path $directory.FullName 'node_modules'
        if (-not (Test-Path -LiteralPath $moduleRoot)) { continue }
        foreach ($entry in Get-ChildItem -LiteralPath $moduleRoot -Directory -ErrorAction SilentlyContinue) {
            if ($entry.Name.StartsWith('@')) {
                foreach ($scopedEntry in Get-ChildItem -LiteralPath $entry.FullName -Directory -ErrorAction SilentlyContinue) {
                    $manifestPath = Join-Path $scopedEntry.FullName 'package.json'
                    if (Test-Path -LiteralPath $manifestPath) { Get-Item -LiteralPath $manifestPath }
                }
            } else {
                $manifestPath = Join-Path $entry.FullName 'package.json'
                if (Test-Path -LiteralPath $manifestPath) { Get-Item -LiteralPath $manifestPath }
            }
        }
    }
}

$cargoPackages = @(
    $cargoMetadata.packages |
        Where-Object { $_.source } |
        ForEach-Object {
            [pscustomobject]@{
                Name = [string]$_.name
                Version = [string]$_.version
                License = Get-LicenseExpression $_.license
            }
        } |
        Sort-Object Name, Version -Unique
)

$npmPackagesById = @{}
foreach ($manifest in Get-NpmPackageManifests $uiPackagesRoot) {
    $package = Get-Content -LiteralPath $manifest.FullName -Raw | ConvertFrom-Json -AsHashtable
    if (-not $package.name -or -not $package.version) { continue }
    $id = "$($package.name)@$($package.version)"
    $npmPackagesById[$id] = [pscustomobject]@{
        Name = [string]$package.name
        Version = [string]$package.version
        License = Get-LicenseExpression $package.license
    }
}
$npmPackages = @($npmPackagesById.Values | Sort-Object Name, Version)

$undeclared = @(
    @($cargoPackages | Where-Object { $_.License -eq 'UNDECLARED' }) +
    @($npmPackages | Where-Object { $_.License -eq 'UNDECLARED' })
)
if ($undeclared.Count -gt 0) {
    $names = ($undeclared | ForEach-Object { "$($_.Name)@$($_.Version)" }) -join ', '
    throw "Dependency license metadata is missing for: $names"
}

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('# Resolved dependency license inventory')
$lines.Add('')
$lines.Add('Generated from `Cargo.lock`, `ui/pnpm-lock.yaml`, Cargo package metadata, and the installed pnpm package manifests. This inventory includes production, development, build, optional, and target-specific dependencies. Each package keeps the license declared by its upstream project; the Haven MIT License does not relicense dependencies.')
$lines.Add('')
$lines.Add('Regenerate after installing the locked UI dependencies with `pwsh -NoProfile -File scripts/generate-third-party-license-inventory.ps1`. The versions below are resolved package versions, not version ranges.')
$lines.Add('')
$lines.Add("Cargo crates: $($cargoPackages.Count). UI packages: $($npmPackages.Count).")
$lines.Add('')
$lines.Add('## Cargo crates')
$lines.Add('')
$lines.Add('| Package | Version | Declared license |')
$lines.Add('|---|---:|---|')
foreach ($package in $cargoPackages) {
    $escapedLicense = $package.License.Replace('|', '\|')
    $lines.Add("| [$($package.Name)](https://crates.io/crates/$($package.Name)) | $($package.Version) | $escapedLicense |")
}
$lines.Add('')
$lines.Add('## pnpm packages')
$lines.Add('')
$lines.Add('| Package | Version | Declared license |')
$lines.Add('|---|---:|---|')
foreach ($package in $npmPackages) {
    $escapedLicense = $package.License.Replace('|', '\|')
    $escapedName = $package.Name.Replace('|', '\|')
    $lines.Add("| [$escapedName](https://www.npmjs.com/package/$($package.Name)) | $($package.Version) | $escapedLicense |")
}

[System.IO.File]::WriteAllLines($outputPath, $lines, [System.Text.UTF8Encoding]::new($false))
Write-Output "Wrote $outputPath ($($cargoPackages.Count) Cargo crates, $($npmPackages.Count) pnpm packages)."
