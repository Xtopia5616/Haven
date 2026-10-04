$ErrorActionPreference = 'Stop'

$metadata = cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json
# Resolve package names from Cargo metadata so external crates with similar
# names cannot be mistaken for internal dependency edges.
$workspace = @($metadata.packages | ForEach-Object { $_.name })
$architecturePath = Join-Path $PSScriptRoot '..\docs\architecture.md'
$dependencyPattern = 'haven-[a-z0-9-]+'
$expected = @{}

# The architecture table is the single expected inventory. Checking exact sets
# catches both undocumented Cargo edges and stale dependencies left in docs.
foreach ($line in Get-Content $architecturePath) {
    $match = [regex]::Match(
        $line,
        '^\|\s*`(?<crate>haven-[a-z0-9-]+)`\s*\|\s*(?<dependencies>[^|]*)\|'
    )
    if (-not $match.Success) {
        continue
    }

    $crate = $match.Groups['crate'].Value
    if ($expected.ContainsKey($crate)) {
        throw "Duplicate dependency inventory row for $crate in $architecturePath"
    }
    $expected[$crate] = @(
        [regex]::Matches($match.Groups['dependencies'].Value, $dependencyPattern) |
            ForEach-Object { $_.Value } |
            Sort-Object -Unique
    )
}

$missingRows = @($workspace | Where-Object { -not $expected.ContainsKey($_) })
$staleRows = @($expected.Keys | Where-Object { $workspace -notcontains $_ })
if ($missingRows.Count -gt 0 -or $staleRows.Count -gt 0) {
    throw "Architecture dependency table package rows differ from Cargo workspace; missing rows: $($missingRows -join ', '); stale rows: $($staleRows -join ', ')"
}

foreach ($package in $metadata.packages) {
    $actual = @($package.dependencies |
        Where-Object { $_.kind -ne 'dev' -and $workspace -contains $_.name } |
        ForEach-Object { $_.name } |
        Sort-Object -Unique)
    $documented = @($expected[$package.name])
    $undocumented = @($actual | Where-Object { $documented -notcontains $_ })
    $stale = @($documented | Where-Object { $actual -notcontains $_ })
    if ($undocumented.Count -gt 0 -or $stale.Count -gt 0) {
        throw "$($package.name) dependency inventory differs from Cargo; missing from architecture table: $($undocumented -join ', '); stale in architecture table: $($stale -join ', ')"
    }
}

Write-Host 'Internal crate dependency inventory matches Cargo metadata.'
