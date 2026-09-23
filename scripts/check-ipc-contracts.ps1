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

if ($contractNames.Count -ne 71) {
	throw "expected 71 Tauri command contracts, found $($contractNames.Count)"
}

Write-Host "IPC contract registry verified: $($contractNames.Count) commands, handlers/registration/frontend/docs agree."

function Get-RequiredMatch([string] $text, [string] $pattern, [string] $label) {
    $match = [regex]::Match($text, $pattern)
    if (-not $match.Success) {
        throw "could not find $label"
    }
    $match
}

function Get-StructFields([string] $body, [string] $label) {
    $fields = @{}
    foreach ($match in [regex]::Matches($body, '(?m)^\s*(?:pub\s+)?([a-zA-Z_][a-zA-Z0-9_]*)(\?)?\s*:\s*([^,;]+)[,;]')) {
        $name = $match.Groups[1].Value
        if ($fields.ContainsKey($name)) {
            throw "$label contains duplicate field '$name'"
        }
        $fields[$name] = @{
            Optional = $match.Groups[2].Success
            Type = ($match.Groups[3].Value.Trim() -replace '\s+', '')
        }
    }
    if ($fields.Count -eq 0) {
        throw "could not parse fields for $label"
    }
    $fields
}

function Assert-FieldContract([string] $label, [hashtable] $rust, [hashtable] $ts) {
    Assert-SetEqual "$label fields" @($rust.Keys) @($ts.Keys)
    foreach ($name in $rust.Keys) {
        $rustType = $rust[$name].Type
        $tsType = $ts[$name].Type
        $tsOptional = $ts[$name].Optional
        $expectedType = switch ($rustType) {
            'String' { 'string' }
            'i32' { 'number' }
            'ActionKind' { 'ActionKind' }
            'Option<String>' { 'string' }
            'Option<ActionStatus>' { 'ActionStatus' }
            'Option<i32>' { 'number' }
            default { throw "$label has unsupported Rust type '$rustType' for '$name'" }
        }
        if ($tsType -ne $expectedType) {
            throw "$label field '$name' type mismatch: Rust '$rustType' vs TypeScript '$tsType'"
        }
        $expectedOptional = $rustType.StartsWith('Option<')
        if ($tsOptional -ne $expectedOptional) {
            throw "$label field '$name' optionality mismatch"
        }
    }
}

$rustEvents = Get-Content (Join-Path $root 'crates/app-binary/src/events.rs') -Raw
$tsAction = Get-Content (Join-Path $root 'ui/src/lib/contracts/action.ts') -Raw
$rustActionStruct = Get-RequiredMatch $rustEvents '(?ms)pub\s+struct\s+ActionEvent\s*\{(.*?)\n\}' 'Rust ActionEvent'
$tsActionStruct = Get-RequiredMatch $tsAction '(?ms)interface\s+ActionWirePayload\s*\{(.*?)\n\}' 'TypeScript ActionWirePayload'
$rustFields = Get-StructFields $rustActionStruct.Groups[1].Value 'Rust ActionEvent'
$tsFields = Get-StructFields $tsActionStruct.Groups[1].Value 'TypeScript ActionWirePayload'
Assert-FieldContract 'Action IPC payload' $rustFields $tsFields

$rustKind = Get-RequiredMatch $rustEvents '(?ms)pub\s+enum\s+ActionKind\s*\{(.*?)\n\}' 'Rust ActionKind'
$tsKind = Get-RequiredMatch $tsAction '(?m)export\s+type\s+ActionKind\s*=\s*([^;]+);' 'TypeScript ActionKind'
$rustKindValues = @([regex]::Matches($rustKind.Groups[1].Value, '(?m)^\s*([A-Z][A-Za-z0-9_]*)\s*,') | ForEach-Object {
    $_.Groups[1].Value -creplace '([a-z])([A-Z])', '$1_$2' | ForEach-Object { $_.ToLowerInvariant() }
})
$tsKindValues = @([regex]::Matches($tsKind.Groups[1].Value, '''([^'']+)''') | ForEach-Object { $_.Groups[1].Value })
Assert-SetEqual 'ActionKind values' $rustKindValues $tsKindValues

$rustStatus = Get-RequiredMatch (Get-Content (Join-Path $root 'crates/common/src/lifecycle.rs') -Raw) '(?ms)impl\s+ActionStatus\s*\{.*?pub\s+const\s+fn\s+as_str\(self\)\s*->\s*&\x27static\s+str\s*\{\s*match\s+self\s*\{(.*?)\n\s*\}\s*\n\s*\}' 'Rust ActionStatus::as_str'
$tsStatus = Get-RequiredMatch $tsAction '(?m)export\s+type\s+ActionStatus\s*=\s*([^;]+);' 'TypeScript ActionStatus'
$rustStatusValues = @([regex]::Matches($rustStatus.Groups[1].Value, 'Self::\w+\s*=>\s*"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
$tsStatusValues = @([regex]::Matches($tsStatus.Groups[1].Value, '''([^'']+)''') | ForEach-Object { $_.Groups[1].Value })
Assert-SetEqual 'ActionStatus values' $rustStatusValues $tsStatusValues

Write-Host "Action IPC payload and enum contracts verified."

