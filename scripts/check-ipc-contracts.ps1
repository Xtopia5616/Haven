$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$commandsRoot = Join-Path $root 'crates/app-binary/src/commands'

function Get-Matches([string] $text, [string] $pattern) {
    @([regex]::Matches($text, $pattern) | ForEach-Object { $_.Groups[1].Value })
}

function Assert-SetEqual([string] $label, [string[]] $expected, [string[]] $actual) {
    $expected = @($expected | Sort-Object -Unique)
    $actual = @($actual | Sort-Object -Unique)
    $missing = @($expected | Where-Object { $actual -notcontains $_ })
    $extra = @($actual | Where-Object { $expected -notcontains $_ })
    if ($missing.Count -gt 0 -or $extra.Count -gt 0) {
        $detail = @()
        if ($missing.Count -gt 0) { $detail += "missing: $($missing -join ', ')" }
        if ($extra.Count -gt 0) { $detail += "extra: $($extra -join ', ')" }
        throw "$label mismatch ($($detail -join '; '))"
    }
}

$implemented = @()
Get-ChildItem $commandsRoot -Filter '*.rs' | ForEach-Object {
    $implemented += Get-Matches (Get-Content $_.FullName -Raw) '(?ms)#\[tauri::command[^\]]*\].{0,180}?pub\s+(?:async\s+)?fn\s+([a-z0-9_]+)'
}
$implemented = @($implemented | Sort-Object -Unique)

$bootstrap = Get-Content (Join-Path $root 'crates/app-binary/src/bootstrap.rs') -Raw
$registered = Get-Matches $bootstrap 'commands::[a-z_]+::([a-z0-9_]+)'

$rustContracts = Get-Content (Join-Path $commandsRoot 'contracts.rs') -Raw
$contractNames = Get-Matches $rustContracts 'name:\s*"([a-z0-9_]+)"'

$tsContracts = Get-Content (Join-Path $root 'ui/src/lib/contracts/commands.ts') -Raw
$tsSection = [regex]::Match($tsContracts, '(?s)TAURI_COMMAND_CONTRACTS\s*=\s*\{(.*?)\}\s*as const').Groups[1].Value
$frontendNames = Get-Matches $tsSection '(?m)^\s{1,2}([a-z][a-z0-9_]*)\s*:\s*\{'

$docs = Get-Content (Join-Path $root 'docs/ipc-contracts.md') -Raw
$documentedNames = Get-Matches $docs '(?m)^\|\s*\x60([a-z][a-z0-9_]*)\x60\s*\|'

Assert-SetEqual 'Tauri handlers vs generate_handler' $implemented $registered
Assert-SetEqual 'Tauri handlers vs Rust contract registry' $implemented $contractNames
Assert-SetEqual 'Rust contract registry vs frontend contract registry' $contractNames $frontendNames
Assert-SetEqual 'Tauri handlers vs IPC contract docs' $implemented $documentedNames

if ($contractNames.Count -ne 69) {
	throw "expected 69 Tauri command contracts, found $($contractNames.Count)"
}

Write-Host "IPC contract registry verified: $($contractNames.Count) commands, handlers/registration/frontend/docs agree."

