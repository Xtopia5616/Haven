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

$generated = Get-Content (Join-Path $root 'ui/src/lib/contracts/generatedCommands.ts') -Raw
$generatedEventArrays = Get-Matches $generated '(?ms)^export const [A-Z_]+_EVENT_NAMES = \[(.*?)^\] as const;'
$generatedEvents = @()
foreach ($array in $generatedEventArrays) {
    $generatedEvents += Get-Matches $array "'([^']+:[^']+)'"
}
Assert-SetEqual 'Rust event directory vs generated frontend event channels' $rustEvents $generatedEvents
$eventShapeRegistry = [regex]::Match($generated, '(?sm)export const IPC_EVENT_PAYLOAD_SHAPES = \{(.*?)^\} as const;')
if (-not $eventShapeRegistry.Success) { throw 'generated Rust event payload shape registry is missing' }
$eventShapeNames = Get-Matches $eventShapeRegistry.Groups[1].Value "(?m)^\s*'([^']+:[^']+)'\s*:"
Assert-SetEqual 'Rust event directory vs generated payload shape registry' $rustEvents $eventShapeNames

$eventDtoRegistry = [regex]::Match($generated, '(?sm)export const IPC_EVENT_PAYLOAD_DTO_TYPES = \{(.*?)^\} as const;')
if (-not $eventDtoRegistry.Success) { throw 'generated Rust event DTO pairing registry is missing' }
$eventDtoPairs = @{}
foreach ($match in [regex]::Matches($eventDtoRegistry.Groups[1].Value, '(?m)^\s*''([^'']+:[^'']+)''\s*:\s*(?:''([^'']+)''|"([^"]+)"|(null))\s*,?')) {
    $dtoType = if ($match.Groups[2].Success) { $match.Groups[2].Value } elseif ($match.Groups[3].Success) { $match.Groups[3].Value } else { $null }
    $eventDtoPairs[$match.Groups[1].Value] = $dtoType
}
$eventConstants = @{}
foreach ($match in [regex]::Matches($events, '(?m)^pub\(crate\) const ([A-Z0-9_]+): &str = "([^"]+)";')) {
    $eventConstants[$match.Groups[1].Value] = $match.Groups[2].Value
}
$eventBridge = Get-Content (Join-Path $root 'crates/app-binary/src/event_bridge.rs') -Raw
$agentProjectionMatch = [regex]::Match($eventBridge, '(?s)fn project_with_chunk_seq\(.*?(?=/// 保留原有按变体区分的 tracing 日志)')
if (-not $agentProjectionMatch.Success) { throw 'single AgentEvent projection function is missing' }
$agentProjectionPairs = @()
foreach ($match in [regex]::Matches($agentProjectionMatch.Value, 'AgentEvent::([A-Za-z0-9_]+)[\s\S]*?=>\s*project\(\s*([A-Z0-9_]+_EVENT)\s*,\s*serialize\(\s*([A-Za-z0-9_]+)')) {
    $channelConstant = $match.Groups[2].Value
    if (-not $eventConstants.ContainsKey($channelConstant)) { throw "AgentEvent projection references unknown channel constant $channelConstant" }
    $channel = $eventConstants[$channelConstant]
    $agentProjectionPairs += "$channel|$($match.Groups[3].Value)"
}
$expectedAgentProjectionPairs = @($eventDtoPairs.GetEnumerator() | Where-Object {
    ($_.Key.StartsWith('agent:') -and $_.Key -ne 'agent:tool_output') -or
    ($_.Key -in @('session:lifecycle', 'notification:show'))
} | ForEach-Object { "$($_.Key)|$($_.Value)" })
Assert-SetEqual 'AgentEvent channel/DTO projections vs generated event shape registry' $expectedAgentProjectionPairs $agentProjectionPairs

# Keep every direct Tauri emission site visible for review. New emitters must
# add their source file and count here, then remain covered by the channel and
# DTO ownership checks below.
$emitSiteInventory = @{
    'crates/app-binary/src/bootstrap.rs' = 6
    'crates/app-binary/src/commands/confirmation.rs' = 1
    'crates/app-binary/src/commands/mcp.rs' = 1
    'crates/app-binary/src/commands/mod.rs' = 1
    'crates/app-binary/src/commands/settings.rs' = 1
    'crates/app-binary/src/event_bridge.rs' = 2
    'crates/app-binary/src/handlers.rs' = 3
}
$actualEmitFiles = @()
Get-ChildItem (Join-Path $root 'crates/app-binary/src') -Recurse -Filter '*.rs' | ForEach-Object {
    $relativePath = $_.FullName.Substring($root.Length + 1).Replace('\', '/')
    $emitCount = [regex]::Matches((Get-Content $_.FullName -Raw), '\.emit(?:_to)?\s*\(').Count
    if ($emitCount -gt 0) {
        $actualEmitFiles += $relativePath
        if (-not $emitSiteInventory.ContainsKey($relativePath) -or $emitSiteInventory[$relativePath] -ne $emitCount) {
            throw "unreviewed Tauri event emission sites in ${relativePath}: $emitCount"
        }
    }
}
Assert-SetEqual 'reviewed Tauri event emitter files' @($emitSiteInventory.Keys) $actualEmitFiles
$sessionEventArray = [regex]::Match($generated, "(?s)export const SESSION_EVENT_NAMES = \[(.*?)\]\s*as const")
if (-not $sessionEventArray.Success -or
    (Get-Matches $sessionEventArray.Groups[1].Value "'([^']+:[^']+)'" | Measure-Object).Count -ne 1 -or
    -not [regex]::IsMatch($sessionEventArray.Groups[1].Value, "^\s*'session:lifecycle',?\s*$")) {
    throw 'Session currently has exactly one lifecycle channel; update its single-channel registration deliberately if that contract changes'
}

$eventContracts = @{
    'app.ts' = 'APP_EVENT_NAMES'
    'agent.ts' = 'AGENT_EVENT_NAMES'
    'recording.ts' = 'RECORDING_EVENT_NAMES'
    'session.ts' = 'SESSION_EVENT_NAMES'
    'toolRun.ts' = 'TOOL_RUN_EVENT_NAMES'
}
foreach ($contract in $eventContracts.GetEnumerator()) {
    $text = Get-Content (Join-Path $root "ui/src/lib/contracts/$($contract.Key)") -Raw
    if (-not $text.Contains($contract.Value) -or [regex]::IsMatch($text, "export const $($contract.Value)\s*=\s*\[")) {
        throw "$($contract.Key) must consume generated $($contract.Value) instead of declaring a second event directory"
    }
}
foreach ($file in @('app.ts', 'agent.ts', 'recording.ts', 'session.ts', 'toolRun.ts')) {
    $source = Get-Content (Join-Path $root "ui/src/lib/contracts/$file") -Raw
    if (-not [regex]::IsMatch($source, 'matchesGeneratedEventPayload\(event\.event,\s*event\.payload\)')) {
        throw "$file must validate each payload against its generated Rust event shape"
    }
}

# Runtime event validators must consume the enum vocabularies emitted by the
# Rust IPC generator. check-ipc-contracts.ps1 verifies those generated values
# against Rust; these checks prevent validators from growing a second list.
$sessionContract = Get-Content (Join-Path $root 'ui/src/lib/contracts/session.ts') -Raw
$toolRunContract = Get-Content (Join-Path $root 'ui/src/lib/contracts/toolRun.ts') -Raw
$recordingContract = Get-Content (Join-Path $root 'ui/src/lib/contracts/recording.ts') -Raw
if (-not [regex]::IsMatch($sessionContract, "(?s)import\s*\{[^}]*SESSION_STATUS_VALUES[^}]*SESSION_UPDATE_STATUS_VALUES[^}]*SESSION_WAITING_REASON_VALUES[^}]*\}\s*from\s*'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($sessionContract, '(?s)function\s+mapSessionStatus\(value:\s*unknown\).*?SESSION_STATUS_VALUES') -or
    -not [regex]::IsMatch($sessionContract, '(?s)function\s+mapUpdateStatus\(value:\s*unknown\).*?SESSION_UPDATE_STATUS_VALUES') -or
    -not [regex]::IsMatch($sessionContract, '(?s)function\s+mapWaitingReason\(value:\s*unknown\).*?SESSION_WAITING_REASON_VALUES')) {
    throw 'session event validators must use the generated Rust session enum vocabularies'
}
$sessionWire = Get-Content (Join-Path $root 'crates/app-binary/src/events.rs') -Raw
if (-not [regex]::IsMatch($sessionWire, 'SESSION_LIFECYCLE_EVENT:\s*&str\s*=\s*"session:lifecycle"') -or
    -not [regex]::IsMatch($sessionWire, '(?s)#\[serde\(\s*tag\s*=\s*"type".*?enum\s+SessionLifecycleEvent')) {
    throw 'session lifecycle must use one Rust channel and a tagged SessionLifecycleEvent enum'
}
if (-not [regex]::IsMatch($sessionContract, "(?s)SESSION_EVENT_NAMES[^;]*from\s+'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($sessionContract, 'SESSION_EVENT_NAMES\[0\]') -or
    [regex]::IsMatch($sessionContract, 'occurrenceId|occurrence_id')) {
    throw 'the frontend session contract must consume the generated lifecycle channel and omit occurrence identity'
}
if (-not [regex]::IsMatch($sessionWire, '(?s)Completed\s*\{[^}]*reason:\s*String') -or
    -not [regex]::IsMatch($sessionWire, '(?s)Error\s*\{[^}]*error:\s*String') -or
    -not [regex]::IsMatch($sessionWire, '(?s)Updated\s*\{[^}]*status:\s*SessionUpdateStatus')) {
    throw 'terminal detail fields must be required and ordinary lifecycle updates must use SessionUpdateStatus'
}
if (-not [regex]::IsMatch($toolRunContract, "(?s)import\s*\{[^}]*TOOL_RUN_KIND_DTO_VALUES[^}]*TOOL_RUN_STATUS_VALUES[^}]*\}\s*from\s*'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($toolRunContract, 'isOneOf\(value,\s*TOOL_RUN_KIND_DTO_VALUES\)') -or
    -not [regex]::IsMatch($toolRunContract, 'isOneOf\(value,\s*TOOL_RUN_STATUS_VALUES\)')) {
    throw 'ToolRun event validators must use the generated Rust enum vocabularies'
}
if (-not [regex]::IsMatch($recordingContract, "(?s)import\s*\{[^}]*VAD_SIGNAL_DTO_VALUES[^}]*VAD_STATE_DTO_VALUES[^}]*\}\s*from\s*'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($recordingContract, 'isOneOf\(payload\.signal,\s*VAD_SIGNAL_DTO_VALUES\)') -or
    -not [regex]::IsMatch($recordingContract, 'isOneOf\(payload\.state,\s*VAD_STATE_DTO_VALUES\)') -or
    -not [regex]::IsMatch($recordingContract, 'isValidTauriEventId\(event\.id\)') -or
    -not [regex]::IsMatch($recordingContract, 'isRecord\(event\.payload\)')) {
    throw 'recording event validators must check the Tauri envelope and use generated Rust VAD enum vocabularies'
}
if (-not [regex]::IsMatch($recordingContract, 'payload\.is_recording\s*!==\s*true') -or
    -not [regex]::IsMatch($recordingContract, 'payload\.is_recording\s*!==\s*false') -or
    -not [regex]::IsMatch($recordingContract, 'confidence\s*<\s*0\s*\|\|\s*confidence\s*>\s*1')) {
    throw 'recording validators must enforce channel state and confidence/duration domains'
}
if (-not [regex]::IsMatch($toolRunContract, "event\.event\s*===\s*'tool_run:created'[\s\S]*?wire\.status\s*===\s*undefined") -or
    -not [regex]::IsMatch($toolRunContract, "event\.event\s*===\s*'tool_run:finished'[\s\S]*?completed[\s\S]*?failed[\s\S]*?cancelled")) {
    throw 'ToolRun event validators must enforce channel-specific created/finished fields'
}

$agentEventsPath = Join-Path $root 'crates/app-binary/src/events.rs'
$agentEventBridgePath = Join-Path $root 'crates/app-binary/src/event_bridge.rs'
$agentContractPath = Join-Path $root 'ui/src/lib/contracts/agent.ts'
$agentWirePath = Join-Path $root 'crates/agent/src/event.rs'
$agentEvents = Get-Content $agentEventsPath -Raw
$agentEventBridge = Get-Content $agentEventBridgePath -Raw
$agentContract = Get-Content $agentContractPath -Raw
$agentWire = Get-Content $agentWirePath -Raw
$committedUi = Get-Content (Join-Path $root 'crates/agent/src/react/committed_ui.rs') -Raw
$commonToolContracts = Get-Content (Join-Path $root 'crates/common/src/tools.rs') -Raw
$toolContract = Get-Content (Join-Path $root 'crates/tools/src/tool_contract.rs') -Raw
if (-not [regex]::IsMatch($agentEvents, '(?s)struct\s+AgentNotificationEvent\s*\{[^}]*notification_kind[^}]*tool_run_kind[^}]*tool_run_id[^}]*tool_run_status')) {
    throw 'AgentNotificationEvent must keep the ToolRun completion notification discriminator and routing fields in its named wire DTO'
}
if (-not [regex]::IsMatch($agentEventBridge, 'ToolRunCompletionNotification\s*\{') -or
    -not [regex]::IsMatch($agentEventBridge, 'notification_kind:\s*Some\(AgentNotificationKind::ToolRunCompletion\)')) {
    throw 'ToolRun completion AgentEvent must be projected as the explicit notification_kind marker'
}
if (-not [regex]::IsMatch($agentEvents, '(?s)struct\s+AgentNotificationEvent\s*\{[^}]*tool_run_kind:\s*Option<ToolRunKindDto>[^}]*tool_run_id[^}]*tool_run_status:\s*Option<ToolRunCompletionStatusDto>')) {
    throw 'AgentNotificationEvent must use App-owned ToolRun notification enums'
}
if (-not [regex]::IsMatch($agentWire, '(?s)Observation\s*\{[^}]*idempotency:\s*OperationIdempotency[^}]*operation_scope:\s*ToolOperationScope') -or
    -not [regex]::IsMatch($committedUi, '(?s)struct\s+StoredObservationUi\s*\{[^}]*idempotency:\s*OperationIdempotency[^}]*operation_scope:\s*ToolOperationScope') -or
    -not [regex]::IsMatch($agentEvents, '(?s)struct\s+AgentObservationEvent\s*\{[^}]*idempotency:\s*OperationIdempotency[^}]*operation_scope:\s*ToolOperationScope')) {
    throw 'observation idempotency and operation scope must retain their Common enum types through durable UI metadata and the App wire DTO'
}
if (-not [regex]::IsMatch($commonToolContracts, '(?s)pub\s+struct\s+ToolResultEnvelope\s*\{[^}]*outcome:\s*ToolExecutionOutcome[^}]*idempotency:\s*OperationIdempotency[^}]*retryability:\s*ToolRetryability') -or
    [regex]::IsMatch($toolContract, 'self\.output\.get\("idempotency"\)')) {
    throw 'ToolResultEnvelope retry metadata must be Common-owned and cannot be overridden by tool output JSON'
}
if (-not [regex]::IsMatch($commonToolContracts, '(?s)sandbox_output_review_step_ids:\s*Vec<String>') -or
    -not [regex]::IsMatch($commonToolContracts, 'MAX_SANDBOX_OUTPUT_REVIEW_STEP_IDS:\s*usize\s*=\s*64') -or
    -not [regex]::IsMatch($agentContract, '(?s)sandboxOutputReviewStepIds\?:\s*string\[\]') -or
    -not [regex]::IsMatch($agentContract, 'isCanonicalSandboxReviewStepIdArray\(value\.sandbox_output_review_step_ids\)') -or
    -not [regex]::IsMatch($agentContract, 'value\.length\s*<=\s*64')) {
    throw 'sandbox review identities must travel only through the bounded, validated UI result envelope'
}
if (-not [regex]::IsMatch($agentContract, '(?s)export interface AgentNotificationPayload\s*\{[^}]*notificationKind\?:\s*AgentNotificationKind[^}]*toolRunKind\?:\s*ToolRunKindDto[^}]*toolRunId\?:\s*string[^}]*toolRunStatus\?:\s*ToolRunCompletionStatusDto')) {
    throw 'frontend notification contract must use generated ToolRun completion source, identity, and status types'
}
if (-not [regex]::IsMatch($agentContract, '(?s)notificationKind\s*!==\s*undefined.*?AGENT_NOTIFICATION_KIND_VALUES.*?TOOL_RUN_COMPLETION_STATUS_DTO_VALUES.*?TOOL_RUN_KIND_DTO_VALUES')) {
    throw 'notification event validators must consume generated Rust enum vocabularies'
}
if (-not [regex]::IsMatch($agentContract, "(?s)notificationKind\s*===\s*'tool_run_completion'.*?optionalSessionId\(payload\).*?if\s*\(notificationKind\s*!==\s*undefined\).*?const\s+sessionId\s*=\s*optionalSessionId\(payload\)")) {
    throw 'generic and ToolRun completion notifications must accept an absent session association through the optional session mapper'
}
if (-not [regex]::IsMatch($agentContract, "(?s)function\s+optionalSessionId\([^)]*\)[^{]*\{[^}]*hasOwnWireField\(record,\s*'session_id'\)[^}]*return\s+canonicalIdField\(record,\s*'session_id',\s*'ses'\)")) {
    throw 'optional notification session IDs must be omitted when absent and reject empty or malformed values'
}
if (-not [regex]::IsMatch($agentContract, '(?s)export interface AgentNotificationPayload\s*\{[^}]*sessionId\?:\s*string')) {
    throw 'frontend notification contract must make the real session association optional'
}
if (-not [regex]::IsMatch($agentContract, "(?s)import\s*\{[^}]*OPERATION_IDEMPOTENCY_VALUES[^}]*TOOL_ERROR_CLASS_VALUES[^}]*TOOL_EXECUTION_OUTCOME_VALUES[^}]*TOOL_OPERATION_SCOPE_VALUES[^}]*TOOL_RETRYABILITY_VALUES[^}]*\}\s*from\s*'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentObservationPayload\s*\{[^}]*idempotency:\s*OperationIdempotency[^}]*operationScope:\s*ToolOperationScope') -or
    -not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentToolResultEnvelope\s*\{[^}]*outcome:\s*ToolExecutionOutcome[^}]*errorClass\?:\s*ToolErrorClass[^}]*idempotency:\s*OperationIdempotency[^}]*retryability:\s*ToolRetryability') -or
    -not [regex]::IsMatch($agentContract, 'isOneOf\(idempotency,\s*OPERATION_IDEMPOTENCY_VALUES\)') -or
    -not [regex]::IsMatch($agentContract, 'isOneOf\(operationScope,\s*TOOL_OPERATION_SCOPE_VALUES\)') -or
    -not [regex]::IsMatch($agentContract, 'isOneOf\(outcome,\s*TOOL_EXECUTION_OUTCOME_VALUES\)') -or
    -not [regex]::IsMatch($agentContract, 'isOneOf\(idempotency,\s*OPERATION_IDEMPOTENCY_VALUES\)') -or
    -not [regex]::IsMatch($agentContract, 'isOneOf\(retryability,\s*TOOL_RETRYABILITY_VALUES\)') -or
    -not [regex]::IsMatch($agentContract, 'isOneOf\(errorClass,\s*TOOL_ERROR_CLASS_VALUES\)')) {
    throw 'Agent observation and result metadata must use generated Common enum types and runtime values'
}
if (-not [regex]::IsMatch($agentContract, "(?s)import\s*\{[^}]*INJECT_SOURCE_VALUES[^}]*\}\s*from\s*'\./generatedCommands\.ts'") -or
    -not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentSupplementPayload\s*\{[^}]*injectSource\?:\s*InjectSource') -or
    -not [regex]::IsMatch($agentContract, 'payload\.inject_source\s*!==\s*undefined\s*&&\s*!isOneOf\(payload\.inject_source,\s*INJECT_SOURCE_VALUES\)')) {
    throw 'Agent supplement source must use and validate the generated Rust InjectSource enum'
}
if (-not [regex]::IsMatch($agentWire, 'renderer:\s*String') -or
    -not [regex]::IsMatch($agentContract, '(?s)interface\s+AgentObservationPayload\s*\{[^}]*renderer:\s*string;') -or
    -not [regex]::IsMatch($agentContract, "readStringField\(payload,\s*'renderer'\)") -or
    -not [regex]::IsMatch($agentContract, 'renderer\s*===\s*null')) {
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

Write-Host "IPC event directory verified: $($rustEvents.Count) channels and AgentEvent channel/DTO pairs agree with generated Rust registries; runtime enum validators and direct emitter sites match their reviewed inventories."
