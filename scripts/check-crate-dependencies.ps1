$ErrorActionPreference = 'Stop'

# The architecture document is the authority for this allow-list. Keep this
# check deliberately small: it protects internal crate direction while Cargo
# remains the authority for third-party dependency resolution.
$allowed = @{
    'haven-common' = @()
    'haven-llm' = @('haven-common')
    'haven-memory' = @('haven-common')
    'haven-skills' = @('haven-common')
    'haven-mcp' = @('haven-common', 'haven-llm')
    'haven-tools' = @('haven-common', 'haven-memory', 'haven-skills', 'haven-mcp', 'haven-llm', 'haven-input')
    'haven-input' = @('haven-common', 'haven-llm')
    'haven-agent' = @('haven-common', 'haven-llm', 'haven-memory', 'haven-tools')
    'haven-app-binary' = @('haven-common', 'haven-llm', 'haven-memory', 'haven-skills', 'haven-mcp', 'haven-tools', 'haven-input', 'haven-agent')
}

$metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
# Resolve package names from Cargo metadata so external crates with similar
# names cannot be mistaken for internal dependency edges.
$workspace = @($metadata.packages | ForEach-Object { $_.name })

foreach ($package in $metadata.packages) {
    if (-not $allowed.ContainsKey($package.name)) {
        continue
    }
    $actual = @($package.dependencies |
        Where-Object { $_.kind -ne 'dev' -and $workspace -contains $_.name } |
        ForEach-Object { $_.name } |
        Sort-Object -Unique)
    $invalid = @($actual | Where-Object { $allowed[$package.name] -notcontains $_ })
    if ($invalid.Count -gt 0) {
        throw "$($package.name) has forbidden internal dependencies: $($invalid -join ', ')"
    }
}

Write-Host 'Internal crate dependency direction verified.'
