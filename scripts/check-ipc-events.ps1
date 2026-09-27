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

$agentEventsPath = Join-Path $root 'crates/app-binary/src/events.rs'
$agentEventBridgePath = Join-Path $root 'crates/app-binary/src/event_bridge.rs'
$agentContractPath = Join-Path $root 'ui/src/lib/contracts/agent.ts'
$agentEvents = Get-Content $agentEventsPath -Raw
$agentEventBridge = Get-Content $agentEventBridgePath -Raw
$agentContract = Get-Content $agentContractPath -Raw
if (-not [regex]::IsMatch($agentEvents, '(?s)struct\s+AgentNotificationEvent\s*\{[^}]*notification_kind[^}]*action_kind[^}]*action_id[^}]*action_status')) {
    throw 'AgentNotificationEvent must keep the action completion notification discriminator and routing fields in its named wire DTO'
}
if (-not [regex]::IsMatch($agentEventBridge, 'ActionCompletionNotification\s*\{') -or
    -not [regex]::IsMatch($agentEventBridge, 'notification_kind:\s*Some\(AgentNotificationKind::ActionCompletion\)')) {
    throw 'action completion AgentEvent must be projected as the explicit notification_kind marker'
}
if (-not [regex]::IsMatch($agentContract, "(?s)export interface AgentNotificationPayload\s*\{[^}]*notificationKind\?:\s*'action_completion'[^}]*actionKind\?:\s*'background'\s*\|\s*'scheduled'[^}]*actionId\?:\s*string[^}]*actionStatus\?:\s*'completed'\s*\|\s*'failed'")) {
    throw 'frontend notification contract must declare action completion source, identity, and terminal status fields'
}
if (-not [regex]::IsMatch($agentContract, "(?s)notificationKind\s*===\s*'action_completion'.*?requiredSessionId\(payload\)")) {
    throw 'only marked action completion notifications may accept the sessionless action event shape; generic notifications must retain requiredSessionId validation'
}

if ($rustEvents.Count -ne 40) {
    throw "expected 40 public IPC events, found $($rustEvents.Count)"
}

Write-Host "IPC event directory verified: $($rustEvents.Count) channels, Rust/frontend contracts agree."
