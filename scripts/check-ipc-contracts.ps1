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
Assert-Contains $toolsCommands 'invoke\(''list_mcp_tools''\)\.then\(\(value:\s*unknown\)\s*=>\s*validateMcpServerSnapshots\(value\)\)' 'MCP status response uses its runtime validator'
Assert-Contains (Get-Source 'ui/src/lib/toolManifest.ts') 'function parseToolManifest\(value:\s*unknown\):\s*ToolManifest\s*\|\s*null' 'tool manifest parser accepts unknown'

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
    'memoryCommands.ts' = @('list_facts', 'add_fact', 'delete_fact', 'recall_memory')
    'sessionHistoryCommands.ts' = @('get_sessions', 'search_history_filtered', 'get_last_conversation', 'reopen_session', 'delete_session', 'clear_history', 'update_session_title')
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

$toolsView = Get-Source 'ui/src/lib/views/ToolsView.svelte'
Assert-NotContains $toolsView '\binvoke\s*\(' 'ToolsView must use toolsCommands.ts'

Write-Host "IPC contract verified: $($implemented.Count) handlers agree across Rust registration, generated TypeScript, reviewed security metadata, and docs; runtime validators and audited UI owners remain in place."
