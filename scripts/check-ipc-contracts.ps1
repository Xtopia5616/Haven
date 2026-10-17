$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$commandsRoot = Join-Path $root 'crates/app-binary/src/commands'

& (Join-Path $PSScriptRoot 'generate-ipc-contracts.ps1') -Check
if ($LASTEXITCODE -ne 0) {
    throw "Generated Rust IPC contracts are stale (exit code $LASTEXITCODE)"
}

function Get-Matches([string] $text, [string] $pattern) {
    @([regex]::Matches($text, $pattern) | ForEach-Object { $_.Groups[1].Value })
}

function Get-LiteralInvokeCommands([string] $text) {
    $delimiters = [string]::Concat([char]39, [char]34, [char]96)
    $pattern = 'invoke\s*\(\s*([' + $delimiters + '])([a-z0-9_]+)\1'
    @([regex]::Matches($text, $pattern) | ForEach-Object { $_.Groups[2].Value })
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

function Get-Source([string] $relativePath) {
    Get-Content (Join-Path $root $relativePath) -Raw
}

function Assert-Contains([string] $source, [string] $pattern, [string] $label) {
    if (-not [regex]::IsMatch($source, $pattern)) {
        throw "missing IPC invariant: $label"
    }
}

function Assert-NotContains([string] $source, [string] $pattern, [string] $label) {
    if ([regex]::IsMatch($source, $pattern)) {
        throw "unexpected IPC bypass: $label"
    }
}

# The generator owns request/response shapes. This inventory only confirms
# every live handler is registered, documented, and present in the reviewed
# boundary/security directory.
$implemented = @()
Get-ChildItem $commandsRoot -Filter '*.rs' | ForEach-Object {
    $implemented += Get-Matches (Get-Content $_.FullName -Raw) '(?ms)#\[tauri::command[^\]]*\].{0,180}?pub\s+(?:async\s+)?fn\s+([a-z0-9_]+)'
}
$implemented = @($implemented | Sort-Object -Unique)

$bootstrap = Get-Source 'crates/app-binary/src/bootstrap.rs'
$registered = Get-Matches $bootstrap 'commands::[a-z_]+::([a-z0-9_]+)'

$rustContracts = Get-Source 'crates/app-binary/src/commands/contracts.rs'
$rustContractNames = Get-Matches $rustContracts 'name:\s*"([a-z0-9_]+)"'

$tsContracts = Get-Source 'ui/src/lib/contracts/commands.ts'
$tsSectionMatch = [regex]::Match($tsContracts, '(?s)TAURI_COMMAND_CONTRACTS\s*=\s*\{(.*?)\}\s*as const')
if (-not $tsSectionMatch.Success) { throw 'could not find frontend command boundary/security registry' }
$frontendNames = Get-Matches $tsSectionMatch.Groups[1].Value '(?m)^\s*([a-z][a-z0-9_]*)\s*:\s*\{\s*boundary\s*:'

$docs = Get-Source 'docs/ipc-contracts.md'
$documentedNames = Get-Matches $docs '(?m)^\|\s*`([a-z][a-z0-9_]*)`\s*\|'

Assert-SetEqual 'Tauri handlers vs generate_handler' $implemented $registered
Assert-SetEqual 'Tauri handlers vs Rust boundary/security registry' $implemented $rustContractNames
Assert-SetEqual 'Rust registry vs frontend boundary/security registry' $rustContractNames $frontendNames
Assert-SetEqual 'Tauri handlers vs IPC contract docs' $implemented $documentedNames

$generated = Get-Source 'ui/src/lib/contracts/generatedCommands.ts'
$generatedNames = Get-Matches $generated '(?m)^\s{1,2}([a-z][a-z0-9_]*)\s*:\s*\{\s*request:'
Assert-SetEqual 'Tauri handlers vs generated command map' $implemented $generatedNames
Assert-Contains $generated '(?m)^export interface TauriCommandMap\s*\{' 'generated command map export'
Assert-NotContains $generated '\bany\b' 'generated IPC contracts must not widen values to any'
$responseShapeRegistry = [regex]::Match($generated, '(?sm)export const TAURI_COMMAND_RESPONSE_SHAPES = \{(.*?)^\} as const;')
if (-not $responseShapeRegistry.Success) { throw 'generated Rust response shape registry is missing' }
$responseShapeNames = Get-Matches $responseShapeRegistry.Groups[1].Value '(?m)^\s*([a-z][a-z0-9_]*)\s*:'
Assert-SetEqual 'generated commands vs runtime response shape registry' $generatedNames $responseShapeNames

# No second manually maintained source may describe the wire shapes.
Assert-NotContains $rustContracts '(?m)^\s*pub\s+(?:request|response)\s*:' 'Rust registry must not duplicate request/response DTO names'
Assert-NotContains $tsContracts '(?m)^\s*(?:request|response)\s*:' 'frontend registry must not duplicate request/response DTO names'

# invoke exposes the generated map but treats raw Tauri data as untrusted.
$tauri = Get-Source 'ui/src/lib/tauri.ts'
Assert-Contains $tauri 'type RawTauriInvoke\s*=\s*\(cmd:\s*string,\s*args\?:\s*unknown\)\s*=>\s*Promise<unknown>' 'raw Tauri invoke uses unknown wire values'
Assert-Contains $tauri 'export async function invoke<K extends TauriCommandName>' 'public invoke is constrained to generated command names'
Assert-Contains $tauri 'Promise<TauriCommandResponse<K>>' 'public invoke returns the generated response type'
Assert-Contains $tauri 'TAURI_COMMAND_RESPONSE_SHAPES\[cmd\]' 'every command response is checked against its Rust-generated shape'
Assert-Contains $tauri 'findGeneratedWireShapeMismatch\(response,' 'command invoke rejects malformed generated response shapes'
Assert-NotContains $tauri 'Promise<any>|args\?:\s*any' 'invoke boundary must not use any'

# Runtime parsers remain the trust boundary for structured responses even
# though compile-time request/response shapes are generated from Rust.
$toolRunCommands = Get-Source 'ui/src/lib/toolRunCommands.ts'
Assert-Contains $toolRunCommands 'const rows:\s*unknown\s*=\s*await invoke\(''list_tool_runs''\)' 'tool run rows enter as unknown'
Assert-Contains $toolRunCommands 'rows\.map\(\s*\(\s*row\s*\)\s*=>\s*mapToolRunCommandRow\(row(?:,\s*sessionId)?\)\s*\)' 'tool run command rows pass through the command-row mapper'

$toolRunContract = Get-Source 'ui/src/lib/contracts/toolRun.ts'
Assert-Contains $toolRunContract 'function mapToolRunPayload\s*\(\s*payload:\s*unknown(?:,\s*expectedSessionId\?:\s*string)?(?:,\s*)?\):\s*ToolRunPayload\s*\|\s*null' 'tool run mapper accepts unknown'
Assert-Contains $toolRunContract 'export function mapToolRunCommandRow\([\s\S]*?\):\s*ToolRunCommandRow\s*\|\s*null' 'tool run command mapper returns the required-status row type'
Assert-Contains $toolRunContract 'mapped\.status\s*===\s*undefined' 'tool run command rows require status'

$settingsCommands = Get-Source 'ui/src/lib/settingsCommands.ts'
Assert-Contains $settingsCommands 'invoke\(''get_settings''\)\.then\(parseSettingsPayload\)' 'settings response uses its runtime parser'
$settingsContract = Get-Source 'ui/src/lib/contracts/settings.ts'
Assert-Contains $settingsContract 'function parseSettingsPayload\(value:\s*unknown\):\s*SettingsPayload\s*\|\s*null' 'settings parser accepts unknown'
Assert-Contains $generated 'export const SETTINGS_RESPONSE_SHAPE\s*=' 'Settings runtime shape is generated from Rust response DTOs'
Assert-Contains $settingsContract 'findGeneratedWireShapeMismatch\(value,\s*SETTINGS_RESPONSE_SHAPE\)' 'Settings response validates its generated full wire shape'
Assert-Contains $settingsContract 'isCapabilityScope\(grant\.capability\)' 'session permission grants validate Rust capability scope syntax before renderer use'

$toolsCommands = Get-Source 'ui/src/lib/toolsCommands.ts'
$mcpContract = Get-Source 'ui/src/lib/contracts/mcp.ts'
Assert-Contains $toolsCommands 'invoke\(''list_mcp_servers''\)\.then\(\(value:\s*unknown\)\s*=>\s*parseMcpServerSnapshots\(value\)\)' 'MCP server list response uses its runtime validator and renderer mapper'
Assert-Contains $toolsCommands 'export function listMcpServers\(\):\s*Promise<McpServerSnapshotView\[\]>' 'MCP snapshot wrapper names the renderer view'
Assert-Contains $toolsCommands 'invoke\(''refresh_mcp_servers''\)\.then\(\(value:\s*unknown\)\s*=>\s*parseMcpRefreshResult\(value\)\)' 'MCP refresh result uses its runtime validator'
Assert-Contains $mcpContract 'function isMcpServerSnapshot\(value:\s*unknown\):\s*value is ValidatedMcpServerSnapshot' 'MCP list parser validates complete generated rows'
Assert-Contains $mcpContract 'lastError:\s*snapshot\.last_error' 'MCP list parser maps snake_case diagnostics to renderer camelCase'
Assert-Contains $mcpContract 'function isUniqueMcpServerNameList\(value:\s*unknown\):\s*value is string\[\]' 'MCP refresh parser validates profile identity arrays'
Assert-NotContains $toolsCommands '\blistMcpTools\b|\blist_mcp_tools\b' 'retired MCP tool-list command and wrapper are not retained as aliases'
Assert-Contains (Get-Source 'ui/src/lib/toolManifest.ts') 'function parseToolManifest\(value:\s*unknown\):\s*ToolManifestView\s*\|\s*null' 'tool manifest parser accepts unknown and returns its renderer view'

$diagnosticsCommands = Get-Source 'ui/src/lib/diagnosticsCommands.ts'
foreach ($parser in @('parseLogInfo', 'parseLogTail', 'parseShellAvailability', 'parseApiKeyStatus')) {
    Assert-Contains $diagnosticsCommands ("\.then\(" + $parser + '\)') "diagnostics response uses $parser"
}
Assert-Contains $diagnosticsCommands 'parsePerformanceMetricsSnapshot\(value\)' 'performance metrics are checked for safe integer precision'
Assert-Contains $diagnosticsCommands "from './contracts/diagnostics.ts'" 'diagnostics command adapters use the diagnostics contract owner'
$countContracts = Get-Source 'ui/src/lib/contracts/count.ts'
Assert-Contains $countContracts 'isSafeUnsignedInteger\(value\)' 'scalar count responses are non-negative safe integers'
$countCommands = @('clear_facts', 'clear_tool_run_history', 'count_session_history', 'count_session_history_search', 'delete_all_sessions', 'reset_session_permissions', 'run_memory_maintenance')
$numericResponses = Get-Matches $generated '(?m)^\s{1,2}([a-z][a-z0-9_]*):\s*\{[^\r\n]*response:\s*number(?:;|\s*\})'
Assert-SetEqual 'numeric scalar command responses vs count adapters' $numericResponses $countCommands
foreach ($command in $countCommands) {
    $source = switch ($command) {
        'clear_facts' { Get-Source 'ui/src/lib/memoryCommands.ts' }
        'clear_tool_run_history' { Get-Source 'ui/src/lib/toolRunCommands.ts' }
        { $_ -in @('count_session_history', 'count_session_history_search', 'delete_all_sessions') } { Get-Source 'ui/src/lib/sessionCommands.ts' }
        { $_ -in @('reset_session_permissions', 'run_memory_maintenance') } { Get-Source 'ui/src/lib/settingsCommands.ts' }
    }
    Assert-Contains $source ("invoke\('" + $command + "'[\s\S]{0,180}?parseCountResponse\(value,\s*'" + $command + "'\)") "$command response uses safe count validation"
}
$booleanContract = Get-Source 'ui/src/lib/contracts/boolean.ts'
Assert-Contains $booleanContract 'typeof value !== ''boolean''' 'scalar boolean responses require an actual boolean value'
$booleanCommands = @('cancel_tool_run', 'delete_tool_run', 'discard_sandbox_output_review', 'is_autostart_enabled')
$booleanResponses = Get-Matches $generated '(?m)^\s{1,2}([a-z][a-z0-9_]*):\s*\{[^\r\n]*response:\s*boolean(?:;|\s*\})'
Assert-SetEqual 'boolean scalar command responses vs reviewed boolean commands' $booleanResponses $booleanCommands
Assert-Contains (Get-Source 'ui/src/lib/toolRunCommands.ts') "invoke\('cancel_tool_run'[\s\S]{0,180}?parseBooleanResponse\(value,\s*'cancel_tool_run'\)" 'cancel_tool_run response uses boolean validation'
Assert-Contains (Get-Source 'ui/src/lib/settingsCommands.ts') "invoke\('is_autostart_enabled'\)[\s\S]{0,180}?parseBooleanResponse\(value,\s*'is_autostart_enabled'\)" 'is_autostart_enabled response uses boolean validation'
function Assert-MetricsFieldCoverage([string] $contractType, [string] $validatorConstant) {
    $contractMatch = [regex]::Match($generated, "(?s)export interface $([regex]::Escape($contractType))\s*\{([^}]*)\}")
    if (-not $contractMatch.Success) { throw "could not find generated metric DTO $contractType" }
    $contractFields = Get-Matches $contractMatch.Groups[1].Value '\b([a-z][a-z0-9_]*)\s*:'

    $validatorMatch = [regex]::Match($diagnosticsContract, "(?s)const $([regex]::Escape($validatorConstant))\s*=\s*\[([^\]]*)\]\s*as const")
    if (-not $validatorMatch.Success) { throw "could not find metrics validator field list $validatorConstant" }
    $validatedFields = Get-Matches $validatorMatch.Groups[1].Value "'([a-z][a-z0-9_]*)'"
    Assert-SetEqual "$contractType generated fields vs $validatorConstant" $contractFields $validatedFields
}
$diagnosticsContract = Get-Source 'ui/src/lib/contracts/diagnostics.ts'
Assert-Contains $generated 'export const AGENT_METRICS_PHASE_COUNT = [0-9]+ as const' 'generated IPC contract exposes the Rust metrics phase count'
Assert-Contains $diagnosticsContract 'value\.phases\.length !== AGENT_METRICS_PHASE_COUNT' 'performance metrics parser enforces the Rust fixed phase count'
foreach ($metricsFieldSet in @(
    @{ Contract = 'PhaseSnapshot'; Validator = 'PHASE_FIELDS' },
    @{ Contract = 'CounterSnapshot'; Validator = 'COUNTER_FIELDS' },
    @{ Contract = 'GaugeSnapshot'; Validator = 'GAUGE_FIELDS' },
    @{ Contract = 'UiMetricsSnapshot'; Validator = 'UI_METRIC_FIELDS' }
)) {
    Assert-MetricsFieldCoverage $metricsFieldSet.Contract $metricsFieldSet.Validator
}
$metricsSource = Get-Source 'crates/agent/src/react/metrics.rs'
$rustPhaseCount = [regex]::Match($metricsSource, 'pub const METRICS_PHASE_COUNT:\s*usize\s*=\s*(\d+)')
$generatedPhaseCount = [regex]::Match($generated, 'export const AGENT_METRICS_PHASE_COUNT = (\d+) as const')
if (-not $rustPhaseCount.Success -or -not $generatedPhaseCount.Success -or $rustPhaseCount.Groups[1].Value -ne $generatedPhaseCount.Groups[1].Value) {
    throw 'generated metrics phase count must match the Rust response array length'
}
$settingsContract = Get-Source 'ui/src/lib/contracts/settings.ts'
Assert-Contains $generated 'export interface LogInfo\s*\{[^}]*level:\s*LogLevel;' 'LogInfo uses the generated Rust LogLevel enum'
Assert-Contains $diagnosticsContract 'isOneOf\(value\.level,\s*LOG_LEVEL_VALUES\)' 'log info parser validates the generated closed log-level enum'
foreach ($diagnosticsParser in @('parseLogInfo', 'parseLogTail', 'parseShellAvailability', 'parseApiKeyStatus')) {
    Assert-Contains $diagnosticsContract ("export function " + $diagnosticsParser + '\(') "$diagnosticsParser is owned by diagnostics contracts"
    Assert-NotContains $settingsContract ("\b" + $diagnosticsParser + '\b') "$diagnosticsParser is not owned by settings contracts"
}

$uiFiles = Get-ChildItem (Join-Path $root 'ui/src') -Recurse -File | Where-Object { $_.Extension -in @('.ts', '.svelte') }
$tauriCoreApiFiles = @($uiFiles | Where-Object { (Get-Content $_.FullName -Raw).Contains('@tauri-apps/api/core') } | ForEach-Object { $_.Name })
Assert-SetEqual 'direct Tauri core API imports' @('tauri.ts', 'tauri.test.ts') $tauriCoreApiFiles
$layout = Get-Source 'ui/src/routes/+layout.svelte'
Assert-Contains $layout '(?s)const report = await invoke\(''check_llm_connection''\);\s*if \(generation === llmProbeGeneration\) applyLlmConnectionReport\(report\);' 'LLM connection report is normalized only for the current probe'
$settingsView = Get-Source 'ui/src/lib/views/SettingsView.svelte'
Assert-Contains $settingsView 'registerListeners\(\s*appEventListeners\(\{[\s\S]{0,500}?''llm:config_changed''' 'SettingsView LLM config event passes through the generated app-event mapper'
Assert-NotContains $settingsView 'registerListeners\(\s*\{\s*''llm:config_changed''' 'SettingsView must not register the raw LLM config event'
$llmConnection = Get-Source 'ui/src/lib/llmConnection.ts'
Assert-Contains $llmConnection 'LLM_CONNECTION_STATUS_VALUES' 'LLM connection parser reuses generated status values'
Assert-Contains $llmConnection 'LLM_CONNECTION_FAILURE_REASON_VALUES' 'LLM connection parser reuses generated failure-reason values'
Assert-Contains $llmConnection 'isOneOf\(value\.status,\s*LLM_CONNECTION_STATUS_VALUES\)' 'LLM connection parser validates the generated closed status enum'
Assert-Contains $llmConnection 'isString\(value\.provider\)' 'LLM connection parser requires a string provider'
Assert-Contains $llmConnection 'isString\(value\.model\)' 'LLM connection parser requires a string model'
Assert-Contains $llmConnection 'status === ''disconnected''[\s\S]*?isOneOf\(value\.reason,\s*LLM_CONNECTION_FAILURE_REASON_VALUES\)' 'disconnected LLM reports require a generated closed failure reason'
Assert-Contains $llmConnection 'else\s*\{\s*if\s*\(Object\.hasOwn\(value,\s*''reason''\)\)' 'ready and unconfigured LLM reports reject a failure reason'
Assert-Contains $llmConnection 'let report:\s*LlmConnectionReport' 'validated LLM response is materialized as its generated DTO before renderer mapping'
Assert-Contains $layout "invoke\('get_bootstrap_status'\)" 'App shell owns the startup readiness probe'
Assert-Contains $layout "invoke\('resolve_confirmation',\s*confirmationRequest\)" 'App shell owns global confirmation resolution'
Assert-Contains $layout 'parseConfirmationResolutionResult\([\s\S]*?invoke\(''resolve_confirmation''' 'confirmation status is checked before state transition'

$recordingOverlayController = Get-Source 'ui/src/lib/recordingOverlayController.ts'
Assert-Contains $recordingOverlayController "type RecordingCommandName = Extract<\s*TauriCommandName,\s*'start_recording'\s*\|\s*'stop_recording'\s*\|\s*'cancel_recording'\s*>" 'recording controller narrows its dynamic invoke port to the recording lifecycle'
Assert-Contains $recordingOverlayController 'invoke:\s*\(command:\s*RecordingCommandName\)\s*=>\s*invoke\(command\)' 'recording controller is the sole dynamic invoke owner'
$submit = Get-Source 'ui/src/lib/submit.ts'
Assert-Contains $submit 'processTranscript\(request\)' 'submit coordinator uses the session command adapter'
$sessionCommands = Get-Source 'ui/src/lib/sessionCommands.ts'
Assert-Contains $sessionCommands "invoke\('process_transcript',\s*request\)" 'session command adapter owns transcript processing'
Assert-Contains $sessionCommands 'parseProcessResult\(value,\s*request\.activeSessionId\)' 'process transcript response identity is validated against the active session request'
$errorHandling = Get-Source 'ui/src/lib/errorHandling.ts'
Assert-Contains $errorHandling "invoke\('log_frontend_error',\s*\{\s*failureKind:\s*options\.failureKind\s*\?\?\s*'operation_failure',?\s*\}\)" 'error handling sends only a stable frontend failure category'
$externalRef = Get-Source 'ui/src/lib/externalRef.ts'
Assert-Contains $externalRef 'openExternalTarget\(\{\s*target:\s*value\s*\}\)' 'external reference policy uses the external command adapter'
$externalCommands = Get-Source 'ui/src/lib/externalCommands.ts'
Assert-Contains $externalCommands "invoke\('open_external',\s*request\)" 'external command adapter owns external opening'
foreach ($file in $uiFiles) {
    Assert-NotContains (Get-Content $file.FullName -Raw) "invoke\s*\(\s*'delete_tool_run'" "unused delete_tool_run has no unreviewed renderer caller: $($file.Name)"
    if ($file.Name -eq 'recordingOverlayController.ts') { continue }
    Assert-NotContains (Get-Content $file.FullName -Raw) '\binvoke\s*\(\s*(?:command|cmd)\s*\)' "dynamic Tauri invoke is not allowed outside the narrowed recording controller: $($file.Name)"
}

# Keep command-family ownership explicit. Domain adapters own feature commands;
# route/infrastructure modules may invoke only commands whose full lifecycle or
# policy they already own.
$ownedCommands = @{
    '+layout.svelte' = @('get_bootstrap_status', 'check_llm_connection', 'resolve_confirmation')
    'startupCommands.ts' = @('report_startup_ui_ready')
    'errorHandling.ts' = @('log_frontend_error')
    'externalCommands.ts' = @('open_external')
    'recordingOverlayController.ts' = @('start_recording', 'stop_recording', 'cancel_recording')
    'toolRunCommands.ts' = @('list_tool_runs', 'list_tool_run_history', 'clear_tool_run_history', 'cancel_tool_run')
    'toolsCommands.ts' = @('list_builtin_tool_manifests', 'list_mcp_servers', 'list_mcp_resources', 'read_mcp_resource', 'list_mcp_prompts', 'get_mcp_prompt', 'list_skills', 'reset_tool_circuits', 'refresh_mcp_servers', 'set_skill_enabled', 'set_tool_enabled', 'refresh_skills', 'open_skills_dir', 'add_mcp_server', 'update_mcp_server', 'remove_mcp_server', 'reconnect_mcp_server', 'toggle_mcp_server', 'execute_skill', 'get_sandbox_output_review', 'discard_sandbox_output_review', 'pick_sandbox_output_destination', 'promote_sandbox_output')
    'memoryCommands.ts' = @('list_facts', 'add_fact', 'delete_fact', 'clear_facts', 'recall_memory')
    'sessionCommands.ts' = @('list_runtime_sessions', 'get_session_lineage', 'list_session_history', 'search_session_history_filtered', 'count_session_history', 'count_session_history_search', 'get_session_for_resume', 'get_latest_session_for_resume', 'process_transcript', 'reopen_session', 'delete_session', 'delete_all_sessions', 'update_session_title', 'rollback_session', 'fork_session_from_message', 'end_session', 'interrupt_session', 'continue_session')
    'modelDiscoveryCommands.ts' = @('discover_models')
    'diagnosticsCommands.ts' = @('get_log_info', 'read_log_tail', 'check_shell_available', 'get_api_key_status', 'get_performance_metrics')
    'settingsCommands.ts' = @('get_settings', 'pick_execution_workspace_directory', 'discard_staged_credentials', 'list_session_permissions', 'is_autostart_enabled', 'run_memory_maintenance', 'revoke_permission', 'revoke_session_permission', 'reset_permissions', 'reset_session_permissions', 'set_hotkey_capture_active', 'stage_provider_credential', 'stage_independent_search_credential', 'stage_ocr_credential', 'update_settings', 'enable_autostart', 'disable_autostart')
    'chatModelCommands.ts' = @('switch_model', 'set_reasoning_effort', 'set_web_search')
}
$reviewedNonUiCommands = @{
    delete_tool_run = 'Retired command is absent from the live renderer; ToolRun removal flows use cancellation and history cleanup.'
    export_session_history = 'No renderer export flow exists; session history is currently consumed through typed read and search commands.'
    get_recording_state = 'Recording state is driven by recording lifecycle events and the app bootstrap snapshot.'
    list_permissions = 'Global permissions are loaded by the authorization owner outside the renderer command surface.'
    search_session_history = 'Superseded by the filtered and paginated session history commands; no renderer caller remains.'
    search_session_history_paginated = 'No renderer caller exists; current history search uses the filtered command and explicit count query.'
}
$ownedCommandNames = @($ownedCommands.Values | ForEach-Object { $_ })
$duplicateOwners = @($ownedCommandNames | Group-Object | Where-Object Count -gt 1 | ForEach-Object Name)
if ($duplicateOwners.Count -gt 0) {
    throw "UI command owner map assigns commands more than once: $($duplicateOwners -join ', ')"
}
$literalInvokeCommands = @()
foreach ($file in $uiFiles | Where-Object { $_.Name -notmatch '\.(test|spec)\.' }) {
    $literalInvokeCommands += Get-LiteralInvokeCommands (Get-Content $file.FullName -Raw)
}
Assert-SetEqual 'production literal invokes vs reviewed UI command owners' $literalInvokeCommands $ownedCommandNames
Assert-SetEqual 'generated commands vs UI and reviewed non-UI dispositions' $generatedNames (@($ownedCommandNames) + @($reviewedNonUiCommands.Keys))
if (@($reviewedNonUiCommands.GetEnumerator() | Where-Object { [string]::IsNullOrWhiteSpace($_.Value) }).Count -gt 0) {
    throw 'every non-UI command disposition must include a review reason'
}
Assert-SetEqual 'reviewed non-UI commands have no renderer call' @() @($literalInvokeCommands | Where-Object { $reviewedNonUiCommands.ContainsKey($_) })

foreach ($file in $uiFiles | Where-Object { $_.Name -notmatch '\.(test|spec)\.' }) {
    $fileInvokes = @(Get-LiteralInvokeCommands (Get-Content $file.FullName -Raw))
    foreach ($owner in $ownedCommands.Keys) {
        if ($file.Name -eq $owner) { continue }
        foreach ($command in $ownedCommands[$owner]) {
            if ($fileInvokes -contains $command) {
                throw "$($file.FullName.Substring($root.Length + 1)) bypasses $owner for '$command'"
            }
        }
    }
}

$sessionHistoryContract = Get-Source 'ui/src/lib/contracts/sessionHistory.ts'
Assert-Contains (Get-Source 'ui/src/lib/contracts/agent.ts') 'function nullableCostIsValid\(value:\s*unknown\):\s*boolean\s*\{\s*return value === null \|\| \(isFiniteNumber\(value\) && value >= 0\)' 'Agent usage costs are finite, nullable and non-negative'
Assert-Contains $sessionHistoryContract 'isNonNegativeFiniteNumber\(value\.cost_usd\)' 'persisted aggregate and detailed usage costs are non-negative'
foreach ($shape in @(
    'RUNTIME_SESSION_LIST_RESPONSE_SHAPE',
    'SESSION_RECORD_RESPONSE_SHAPE',
    'SESSION_LINEAGE_RESPONSE_SHAPE',
    'SESSION_RESUME_RESPONSE_SHAPE'
)) {
    Assert-Contains $generated ("export const " + $shape + '\s*=') "$shape is generated from Rust response DTOs"
    Assert-Contains $sessionHistoryContract ("requireWireShape\([^\r\n]*" + $shape) "$shape is used by Session runtime response validation"
}
Assert-Contains $sessionHistoryContract 'isI32\(' 'Rust i32 Session fields retain signed 32-bit bounds'
Assert-Contains $sessionHistoryContract 'isU32\(' 'Rust u32 Session fields retain unsigned 32-bit bounds'
Assert-Contains $sessionHistoryContract 'isSafeUnsignedInteger\(' 'Rust u64 Session fields retain JavaScript safe-integer bounds'
Assert-Contains $sessionHistoryContract "isCanonicalEntityId\(value\.session_id,\s*'ses'\)" 'Session response rows require canonical session identities'

$memoryView = Get-Source 'ui/src/lib/views/MemoryView.svelte'
Assert-NotContains $memoryView 'invoke\s*\(\s*''(?:list_facts|add_fact|delete_fact|clear_facts|recall_memory)''' 'MemoryView must use memoryCommands.ts'
$memoryCommands = Get-Source 'crates/app-binary/src/commands/memory.rs'
$memoryDtos = Get-Source 'crates/app-binary/src/commands/contracts.rs'
$memoryUiContracts = Get-Source 'ui/src/lib/contracts/memory.ts'
Assert-Contains $memoryCommands 'Result\s*<\s*MemoryFactPageResponse\s*,\s*String\s*>' 'list_facts returns the bounded App-owned MemoryFactPageResponse DTO'
Assert-Contains $memoryCommands 'Result\s*<\s*MemoryFactResponse\s*,\s*String\s*>' 'add_fact returns the App-owned MemoryFactResponse DTO'
Assert-Contains $memoryDtos 'pub\s+struct\s+MemoryFactResponse' 'MemoryFactResponse is declared at the App IPC boundary'
Assert-Contains $memoryDtos 'pub\s+struct\s+MemoryFactPageResponse' 'MemoryFactPageResponse is declared at the App IPC boundary'
Assert-Contains $memoryDtos 'pub\s+struct\s+MemoryFactCursor' 'MemoryFactCursor is declared at the App IPC boundary'
Assert-Contains $memoryDtos 'pub\s+struct\s+MemoryFactSourceRef' 'MemoryFactSourceRef is declared at the App IPC boundary'
Assert-Contains $memoryUiContracts 'MemoryFactResponse\s+as\s+GeneratedMemoryFactResponse' 'UI Fact alias imports its generated Rust response type'
Assert-Contains $generated 'export const MEMORY_FACT_RESPONSE_SHAPE\s*=' 'MemoryFactResponse runtime shape is generated from Rust'
Assert-Contains $generated 'export const MEMORY_RECALL_ITEM_RESPONSE_SHAPE\s*=' 'MemoryRecallItem runtime shape is generated from Rust'
Assert-Contains $memoryUiContracts 'findGeneratedWireShapeMismatch\(value,\s*MEMORY_FACT_RESPONSE_SHAPE\)' 'Memory fact response validates its generated full wire shape'
Assert-Contains $memoryUiContracts 'findGeneratedWireShapeMismatch\(item,\s*MEMORY_RECALL_ITEM_RESPONSE_SHAPE\)' 'Memory recall response validates its generated full wire shape'
Assert-Contains $memoryUiContracts "isCanonicalEntityId\(value\.fact_id,\s*'fact'\)" 'Memory fact rows require canonical fact identities'
Assert-Contains $memoryUiContracts "isCanonicalEntityId\(value\.source_ref\.message_id,\s*'msg'\)" 'Memory fact source references require canonical message identities'
Assert-Contains $memoryUiContracts 'fact\.confidence\s*<\s*0|fact\.confidence\s*>\s*1' 'Memory fact confidence stays within the persisted domain'
Assert-Contains $memoryUiContracts 'fact\.durability\s*<\s*0|fact\.durability\s*>\s*1' 'Memory fact durability stays within the persisted domain'
Assert-Contains $memoryUiContracts 'Number\.isSafeInteger\(fact\.mention_count\)' 'Memory fact mention counts stay within the JavaScript safe integer domain'
Assert-NotContains $memoryUiContracts 'MemoryFactSourceRef\s+as\s+GeneratedMemoryFactSourceRef' 'UI does not reintroduce the unused FactSourceRef alias'
Assert-NotContains $memoryCommands 'Result\s*<\s*Vec\s*<\s*Fact\s*>' 'repository Fact must not be returned as the Tauri list_facts wire type'

$toolsView = Get-Source 'ui/src/lib/views/ToolsView.svelte'
Assert-NotContains $toolsView '\binvoke\s*\(' 'ToolsView must use toolsCommands.ts'
$skillCard = Get-Source 'ui/src/lib/SkillCard.svelte'
Assert-NotContains $skillCard "invoke\s*\(\s*'execute_skill'" 'SkillCard must delegate execution through its ToolsView owner'

Write-Host "IPC contract verified: $($implemented.Count) handlers agree across Rust registration, generated TypeScript, reviewed security metadata, docs, and complete UI/non-UI dispositions; invoke ownership and semantic validators remain in place."
