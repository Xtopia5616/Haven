$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

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

$events = Get-Content (Join-Path $root 'crates/app-binary/src/events.rs') -Raw
$rustEvents = Get-Matches $events '(?m)^pub\(crate\) const [A-Z0-9_]+_EVENT: &str = "([^"]+)";'

$contractFiles = @(
    'session.ts',
    'action.ts',
    'recording.ts',
    'app.ts',
    'agent.ts'
)
$frontendEvents = @()
foreach ($file in $contractFiles) {
    $text = Get-Content (Join-Path $root "ui/src/lib/contracts/$file") -Raw
    $frontendEvents += Get-Matches $text "(?s)(?:EVENT_NAMES\s*=\s*\[)(.*?)(?:\]\s*as const)" |
        ForEach-Object { Get-Matches $_ "'([^']+:[^']+)'" }
}

Assert-SetEqual 'Rust event directory vs frontend event directory' $rustEvents $frontendEvents

if ($rustEvents.Count -ne 39) {
    throw "expected 39 public IPC events, found $($rustEvents.Count)"
}

Write-Host "IPC event directory verified: $($rustEvents.Count) channels, Rust/frontend contracts agree."
