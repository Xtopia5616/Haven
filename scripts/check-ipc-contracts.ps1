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

# No second manually maintained source may describe the wire shapes.
Assert-NotContains $rustContracts '(?m)^\s*pub\s+(?:request|response)\s*:' 'Rust registry must not duplicate request/response DTO names'
Assert-NotContains $tsContracts '(?m)^\s*(?:request|response)\s*:' 'frontend registry must not duplicate request/response DTO names'

# invoke exposes the generated map but treats raw Tauri data as untrusted.
$tauri = Get-Source 'ui/src/lib/tauri.ts'
Assert-Contains $tauri 'type RawTauriInvoke\s*=\s*\(cmd:\s*string,\s*args\?:\s*unknown\)\s*=>\s*Promise<unknown>' 'raw Tauri invoke uses unknown wire values'
Assert-Contains $tauri 'export async function invoke<K extends TauriCommandName>' 'public invoke is constrained to generated command names'
Assert-Contains $tauri 'Promise<TauriCommandResponse<K>>' 'public invoke returns the generated response type'
Assert-NotContains $tauri 'Promise<any>|args\?:\s*any' 'invoke boundary must not use any'

# Runtime parsers remain the trust boundary for structured responses even
# though compile-time request/response shapes are generated from Rust.
$toolRunCommands = Get-Source 'ui/src/lib/toolRunCommands.ts'
Assert-Contains $toolRunCommands 'const rows:\s*unknown\s*=\s*await invoke\(''list_tool_runs''\)' 'tool run rows enter as unknown'
Assert-Contains $toolRunCommands 'rows\.map\(mapToolRunPayload\)' 'tool run rows pass through the runtime mapper'

$toolRunContract = Get-Source 'ui/src/lib/contracts/toolRun.ts'
Assert-Contains $toolRunContract 'function mapToolRunPayload\(payload:\s*unknown\):\s*ToolRunPayload\s*\|\s*null' 'tool run mapper accepts unknown'

$settingsCommand = Get-Source 'ui/src/lib/settingsCommand.ts'
Assert-Contains $settingsCommand 'invoke\(''get_settings''\)\.then\(parseSettingsPayload\)' 'settings response uses its runtime parser'
Assert-Contains (Get-Source 'ui/src/lib/contracts/settings.ts') 'function parseSettingsPayload\(value:\s*unknown\):\s*SettingsPayload\s*\|\s*null' 'settings parser accepts unknown'

$toolsCommands = Get-Source 'ui/src/lib/toolsCommands.ts'
Assert-Contains $toolsCommands 'invoke\(''list_mcp_servers''\)\.then\(\(value:\s*unknown\)\s*=>\s*validateMcpServerSnapshots\(value\)\)' 'MCP server list response uses its runtime validator'
Assert-Contains $toolsCommands 'export function listMcpServers\(\):\s*Promise<McpServerSnapshot\[\]>' 'MCP snapshot wrapper names the returned entity'
Assert-NotContains $toolsCommands '\blistMcpTools\b|\blist_mcp_tools\b' 'retired MCP tool-list command and wrapper are not retained as aliases'
Assert-Contains (Get-Source 'ui/src/lib/toolManifest.ts') 'function parseToolManifest\(value:\s*unknown\):\s*ToolManifestView\s*\|\s*null' 'tool manifest parser accepts unknown and returns its renderer view'

$diagnosticsCommands = Get-Source 'ui/src/lib/diagnosticsCommands.ts'
foreach ($parser in @('parseLogInfo', 'parseLogTail', 'parseShellAvailability', 'parseApiKeyStatus')) {
    Assert-Contains $diagnosticsCommands ("\.then\(" + $parser + '\)') "diagnostics response uses $parser"
}

$layout = Get-Source 'ui/src/routes/+layout.svelte'
Assert-Contains $layout '(?s)const report = await invoke\(''check_llm_connection''\);\s*if \(generation === llmProbeGeneration\) applyLlmConnectionReport\(report\);' 'LLM connection report is normalized only for the current probe'

# Keep existing command-family ownership boundaries. Helpers are the only
# direct invoke owners for these audited command families.
$uiFiles = Get-ChildItem (Join-Path $root 'ui/src') -Recurse -File | Where-Object { $_.Extension -in @('.ts', '.svelte') }
$ownedCommands = @{
    'toolRunCommands.ts' = @('list_tool_runs', 'cancel_tool_run')
    'toolsCommands.ts' = @('list_builtin_tool_manifests', 'list_mcp_servers', 'reset_tool_circuits', 'refresh_mcp_servers', 'set_skill_enabled', 'set_tool_enabled', 'refresh_skills', 'open_skills_dir', 'add_mcp_server', 'update_mcp_server', 'remove_mcp_server', 'reconnect_mcp', 'toggle_mcp_server')
    'memoryCommands.ts' = @('list_facts', 'add_fact', 'delete_fact', 'recall_memory')
    'sessionCommands.ts' = @('list_runtime_sessions', 'list_session_history', 'count_session_history', 'search_session_history', 'search_session_history_paginated', 'count_session_history_search', 'search_session_history_filtered', 'export_session_history', 'get_latest_session_for_resume', 'reopen_session', 'delete_session', 'delete_all_sessions', 'update_session_title')
    'modelDiscoveryCommands.ts' = @('discover_models', 'discover_all_models')
    'diagnosticsCommands.ts' = @('get_log_info', 'read_log_tail', 'check_shell_available', 'get_api_key_status', 'get_performance_metrics')
}
foreach ($file in $uiFiles) {
    foreach ($owner in $ownedCommands.Keys) {
        if ($file.Name -eq $owner) { continue }
        foreach ($command in $ownedCommands[$owner]) {
            $invokePattern = "invoke\s*\(\s*'$([regex]::Escape($command))'"
            if ([regex]::IsMatch((Get-Content $file.FullName -Raw), $invokePattern)) {
                throw "$($file.FullName.Substring($root.Length + 1)) bypasses $owner for '$command'"
            }
        }
    }
}

$memoryView = Get-Source 'ui/src/lib/views/MemoryView.svelte'
Assert-NotContains $memoryView 'invoke\s*\(\s*''(?:list_facts|add_fact|delete_fact|recall_memory)''' 'MemoryView must use memoryCommands.ts'
$memoryCommands = Get-Source 'crates/app-binary/src/commands/memory.rs'
$memoryDtos = Get-Source 'crates/app-binary/src/commands/contracts.rs'
$memoryUiContracts = Get-Source 'ui/src/lib/contracts/memory.ts'
Assert-Contains $memoryCommands 'Result\s*<\s*Vec\s*<\s*MemoryFactResponse\s*>\s*,\s*String\s*>' 'list_facts returns the App-owned MemoryFactResponse DTO'
Assert-Contains $memoryCommands 'Result\s*<\s*MemoryFactResponse\s*,\s*String\s*>' 'add_fact returns the App-owned MemoryFactResponse DTO'
Assert-Contains $memoryDtos 'pub\s+struct\s+MemoryFactResponse' 'MemoryFactResponse is declared at the App IPC boundary'
Assert-Contains $memoryDtos 'pub\s+struct\s+MemoryFactSourceRef' 'MemoryFactSourceRef is declared at the App IPC boundary'
Assert-Contains $memoryUiContracts 'MemoryFactResponse\s+as\s+GeneratedMemoryFactResponse' 'UI Fact alias imports its generated Rust response type'
Assert-NotContains $memoryUiContracts 'MemoryFactSourceRef\s+as\s+GeneratedMemoryFactSourceRef' 'UI does not reintroduce the unused FactSourceRef alias'
Assert-NotContains $memoryCommands 'Result\s*<\s*Vec\s*<\s*Fact\s*>' 'repository Fact must not be returned as the Tauri list_facts wire type'

$toolsView = Get-Source 'ui/src/lib/views/ToolsView.svelte'
Assert-NotContains $toolsView '\binvoke\s*\(' 'ToolsView must use toolsCommands.ts'

Write-Host "IPC contract verified: $($implemented.Count) handlers agree across Rust registration, generated TypeScript, reviewed security metadata, and docs; runtime validators and audited UI owners remain in place."
