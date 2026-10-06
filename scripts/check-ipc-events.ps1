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
    'toolRun.ts',
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

# Runtime event validators must consume the enum vocabularies emitted by the
# Rust IPC generator. check-ipc-contracts.ps1 verifies those generated values
# against Rust; these checks prevent validators from growing a second list.
$sessionStatus = Get-Content (Join-Path $root 'ui/src/lib/sessionStatus.ts') -Raw
$toolRunContract = Get-Content (Join-Path $root 'ui/src/lib/contracts/toolRun.ts') -Raw
if (-not [regex]::IsMatch($sessionStatus, "(?s)import\s*\{[^}]*SESSION_STATUS_VALUES[^}]*SESSION_WAITING_REASON_VALUES[^}]*\}\s*from\s*'\./contracts/generatedCommands\.ts'") -or
    -not [regex]::IsMatch($sessionStatus, 'SESSION_STATUSES\s*=\s*SESSION_STATUS_VALUES') -or
    -not [regex]::IsMatch($sessionStatus, 'SESSION_WAITING_REASONS\s*=\s*SESSION_WAITING_REASON_VALUES')) {
    throw 'session event validators must use the generated Rust session enum vocabularies'
}
$sessionWire = Get-Content (Join-Path $root 'crates/app-binary/src/events.rs') -Raw
$sessionContract = Get-Content (Join-Path $root 'ui/src/lib/contracts/session.ts') -Raw
if (-not [regex]::IsMatch($sessionWire, 'SESSION_LIFECYCLE_EVENT:\s*&str\s*=\s*"session:lifecycle"') -or
    -not [regex]::IsMatch($sessionWire, '(?s)#\[serde\(\s*tag\s*=\s*"type".*?enum\s+SessionLifecycleEvent')) {
    throw 'session lifecycle must use one Rust channel and a tagged SessionLifecycleEvent enum'
}
if (-not [regex]::IsMatch($sessionContract, "(?s)SESSION_EVENT_NAMES\s*=\s*\['session:lifecycle'\]\s+as\s+const") -or
    [regex]::IsMatch($sessionContract, 'occurrenceId|occurrence_id')) {
    throw 'the frontend session contract must register one lifecycle channel and omit occurrence identity'
}
if (-not [regex]::IsMatch($sessionWire, '(?s)Completed\s*\{[^}]*reason:\s*String') -or
    -not [regex]::IsMatch($sessionWire, '(?s)Error\s*\{[^}]*error:\s*String') -or
    -not [regex]::IsMatch($sessionWire, '(?s)Updated\s*\{[^}]*status:\s*SessionUpdateStatus')) {
    throw 'terminal detail fields must be required and ordinary lifecycle updates must use SessionUpdateStatus'
}
if (-not [regex]::IsMatch($toolRunContract, "(?s)import\s*\{[^}]*TOOL_RUN_KIND_VALUES[^}]*TOOL_RUN_STATUS_VALUES[^}]*\}\s*from\s*'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($toolRunContract, 'TOOL_RUN_KIND_VALUES\s+as\s+readonly\s+unknown\[\]\)\.includes\(value\)') -or
    -not [regex]::IsMatch($toolRunContract, 'TOOL_RUN_STATUS_VALUES\s+as\s+readonly\s+unknown\[\]\)\.includes\(value\)')) {
    throw 'ToolRun event validators must use the generated Rust enum vocabularies'
}

$agentEventsPath = Join-Path $root 'crates/app-binary/src/events.rs'
$agentEventBridgePath = Join-Path $root 'crates/app-binary/src/event_bridge.rs'
$agentContractPath = Join-Path $root 'ui/src/lib/contracts/agent.ts'
$agentWirePath = Join-Path $root 'crates/agent/src/event.rs'
$agentEvents = Get-Content $agentEventsPath -Raw
$agentEventBridge = Get-Content $agentEventBridgePath -Raw
$agentContract = Get-Content $agentContractPath -Raw
$agentWire = Get-Content $agentWirePath -Raw
if (-not [regex]::IsMatch($agentEvents, '(?s)struct\s+AgentNotificationEvent\s*\{[^}]*notification_kind[^}]*tool_run_kind[^}]*tool_run_id[^}]*tool_run_status')) {
    throw 'AgentNotificationEvent must keep the ToolRun completion notification discriminator and routing fields in its named wire DTO'
}
if (-not [regex]::IsMatch($agentEventBridge, 'ToolRunCompletionNotification\s*\{') -or
    -not [regex]::IsMatch($agentEventBridge, 'notification_kind:\s*Some\(AgentNotificationKind::ToolRunCompletion\)')) {
    throw 'ToolRun completion AgentEvent must be projected as the explicit notification_kind marker'
}
if (-not [regex]::IsMatch($agentContract, "(?s)export interface AgentNotificationPayload\s*\{[^}]*notificationKind\?:\s*'tool_run_completion'[^}]*toolRunKind\?:\s*'background'\s*\|\s*'scheduled'[^}]*toolRunId\?:\s*string[^}]*toolRunStatus\?:\s*'completed'\s*\|\s*'failed'")) {
    throw 'frontend notification contract must declare ToolRun completion source, identity, and terminal status fields'
}
if (-not [regex]::IsMatch($agentContract, "(?s)notificationKind\s*===\s*'tool_run_completion'.*?optionalSessionId\(payload\).*?if\s*\(notificationKind\s*!==\s*undefined\).*?const\s+sessionId\s*=\s*optionalSessionId\(payload\)")) {
    throw 'generic and ToolRun completion notifications must accept an absent session association through the optional session mapper'
}
if (-not [regex]::IsMatch($agentContract, "(?s)function\s+optionalSessionId\([^)]*\)[^{]*\{[^}]*hasOwn\(record,\s*'session_id'\)[^}]*return\s+value\s*&&\s*value\.length\s*>\s*0\s*\?\s*value\s*:\s*null")) {
    throw 'optional notification session IDs must be omitted when absent and reject empty or malformed values'
}
if (-not [regex]::IsMatch($agentContract, '(?s)export interface AgentNotificationPayload\s*\{[^}]*sessionId\?:\s*string')) {
    throw 'frontend notification contract must make the real session association optional'
}
if (-not [regex]::IsMatch($agentWire, 'renderer:\s*String') -or
    -not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentObservationPayload\s*\{[^}]*renderer:\s*string;') -or
    -not [regex]::IsMatch($agentContract, "requiredString\(payload,\s*'renderer'\)")) {
    throw 'agent observation renderer must be required by both Rust and the frontend event mapper'
}
if (-not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentToolCallPayload\s*\{[^}]*toolCallId:\s*string\s*\|\s*null;') -or
    -not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentObservationPayload\s*\{[^}]*toolCallId:\s*string\s*\|\s*null;[^}]*result:\s*AgentToolResultEnvelope;') -or
    -not [regex]::IsMatch($agentContract, 'const\s+result\s*=\s*mapToolResult\(payload\.result\)')) {
    throw 'Agent ToolCall and Observation tool_call_id/result fields must match the required Rust DTO shape'
}

if ($rustEvents.Count -ne 35) {
    throw "expected 35 public IPC events, found $($rustEvents.Count)"
}

Write-Host "IPC event directory verified: $($rustEvents.Count) channels agree, and runtime enum validators use generated Rust vocabularies."
