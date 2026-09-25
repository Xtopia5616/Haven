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

function Get-RustCommandParameters([string] $signature, [string] $label) {
    $fields = @{}
    foreach ($match in [regex]::Matches($signature, '(?:^|,)\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*:\s*([^,\r\n\)]+)')) {
        $name = $match.Groups[1].Value
        $fields[$name] = @{
            Optional = $false
            Type = ($match.Groups[2].Value.Trim() -replace '\s+', '')
        }
    }
    if ($fields.Count -eq 0) {
        throw "could not parse command parameters for $label"
    }
    $fields
}

function Convert-SnakeToCamel([string] $name) {
    $parts = @($name -split '_')
    if ($parts.Count -eq 1) { return $name }
    $tail = @($parts | Select-Object -Skip 1 | ForEach-Object {
        $_.Substring(0, 1).ToUpperInvariant() + $_.Substring(1)
    })
    $parts[0] + ($tail -join '')
}

function Assert-ActionFieldContract([hashtable] $rust, [hashtable] $ts) {
    $mappedNames = @($rust.Keys | ForEach-Object { Convert-SnakeToCamel $_ })
    Assert-SetEqual 'Action IPC payload fields' $mappedNames @($ts.Keys)
    foreach ($name in $rust.Keys) {
        $uiName = Convert-SnakeToCamel $name
        $rustType = $rust[$name].Type
        $tsType = $ts[$uiName].Type
        $tsOptional = $ts[$uiName].Optional
        $expectedType = switch ($rustType) {
            'String' { 'string' }
            'i32' { 'number' }
            'ActionKind' { 'ActionKind' }
            'Option<String>' { 'string' }
            'Option<ActionStatus>' { 'ActionStatus' }
            'Option<i32>' { 'number' }
            default { throw "ActionEvent has unsupported Rust type '$rustType' for '$name'" }
        }
        if ($tsType -ne $expectedType) {
            throw "ActionEvent field '$name' maps to '$uiName' with mismatched type: Rust '$rustType' vs TypeScript '$tsType'"
        }
        $expectedOptional = $rustType.StartsWith('Option<')
        if ($tsOptional -ne $expectedOptional) {
            throw "ActionEvent field '$name' maps to '$uiName' with mismatched optionality"
        }
    }
}

function Assert-ActionFieldMapped([string] $mapper, [string] $rustName) {
    $uiName = Convert-SnakeToCamel $rustName
    $uiField = [regex]::Escape($uiName)
    $wireField = [regex]::Escape($rustName)
    $pattern = "(?m)(?:\b$uiField\s*:\s*[^,\r\n}]*\bpayload\.$wireField\b|\bmapped\.$uiField\s*=\s*[^;\r\n]*\bpayload\.$wireField\b)"
    if (-not [regex]::IsMatch($mapper, $pattern)) {
        throw "ActionEvent field '$rustName' is missing from mapActionPayload's '$uiName' mapping"
    }
}

$rustEvents = Get-Content (Join-Path $root 'crates/app-binary/src/events.rs') -Raw
$tsAction = Get-Content (Join-Path $root 'ui/src/lib/contracts/action.ts') -Raw
$uiActionCommands = Get-Content (Join-Path $root 'ui/src/lib/actionCommands.ts') -Raw
$actionStore = Get-Content (Join-Path $root 'ui/src/lib/actionStore.ts') -Raw
$actionLayout = Get-Content (Join-Path $root 'ui/src/routes/+layout.svelte') -Raw
$actionCommands = Get-Content (Join-Path $commandsRoot 'action.rs') -Raw
$eventBridge = Get-Content (Join-Path $root 'crates/app-binary/src/event_bridge.rs') -Raw
if ([regex]::Matches($rustEvents, '(?m)^pub\s+struct\s+ActionEvent\b').Count -ne 1) {
    throw 'expected exactly one Rust ActionEvent wire DTO'
}
if ([regex]::Matches($tsAction, '(?m)^export\s+interface\s+ActionPayload\b').Count -ne 1) {
    throw 'expected exactly one TypeScript ActionPayload contract'
}
if ([regex]::Matches($tsAction, '(?m)^export\s+function\s+mapActionPayload\b').Count -ne 1) {
    throw 'expected exactly one TypeScript mapActionPayload contract mapper'
}
if ([regex]::IsMatch($tsAction, '\bActionWirePayload\b')) {
    throw 'legacy TypeScript ActionWirePayload must not be restored; use ActionPayload and mapActionPayload'
}
$rustActionStruct = Get-RequiredMatch $rustEvents '(?ms)pub\s+struct\s+ActionEvent\s*\{(.*?)\n\}' 'Rust ActionEvent'
$tsActionStruct = Get-RequiredMatch $tsAction '(?ms)export\s+interface\s+ActionPayload\s*\{(.*?)\n\}' 'TypeScript ActionPayload'
$tsActionMapper = Get-RequiredMatch $tsAction '(?ms)export\s+function\s+mapActionPayload\s*\(\s*payload:\s*unknown\s*\):\s*ActionPayload\s*\|\s*null\s*\{(.*?)\n\}' 'TypeScript mapActionPayload'
$tsEventMapper = Get-RequiredMatch $tsAction '(?ms)export\s+function\s+mapActionEvent\s*\(.*?\):\s*TauriEvent<ActionPayload>\s*\|\s*null\s*\{(.*?)\n\}' 'TypeScript mapActionEvent'
if (-not [regex]::IsMatch($tsEventMapper.Groups[1].Value, 'mapActionPayload\s*\(\s*event\.payload\s*\)')) {
    throw 'mapActionEvent must route event payloads through mapActionPayload'
}
if (-not [regex]::IsMatch($uiActionCommands, "(?s)invoke\('list_actions'\).*?rows\.map\(mapActionPayload\)")) {
    throw 'list_actions command boundary must pass response rows through mapActionPayload'
}

function Assert-MemoryWireFieldContract([string] $label, [hashtable] $rust, [hashtable] $ts) {
    Assert-SetEqual "$label fields" @($rust.Keys) @($ts.Keys)
    foreach ($name in $rust.Keys) {
        $expectedType = switch ($rust[$name].Type) {
            'String' { 'string' }
            'f64' { 'number' }
            'i64' { 'number' }
            'Vec<String>' { 'string[]' }
            'Option<String>' { 'string|null' }
            'Option<FactSourceRef>' { 'FactSourceRef|null' }
            default { throw "$label has unsupported Rust type '$($rust[$name].Type)' for '$name'" }
        }
        if ($ts[$name].Type -ne $expectedType) {
            throw "$label field '$name' type mismatch: Rust '$($rust[$name].Type)' vs TypeScript '$($ts[$name].Type)'"
        }
        if ($ts[$name].Optional) {
            throw "$label field '$name' must be present on the wire; nullable values use explicit null"
        }
    }
}

function Assert-MemoryRequestContract([string] $label, [hashtable] $rust, [hashtable] $ts) {
    $mapped = @{}
    foreach ($name in $rust.Keys) {
        if ($name -eq 'state') { continue }
        $mapped[(Convert-SnakeToCamel $name)] = $rust[$name]
    }
    Assert-SetEqual "$label request fields" @($mapped.Keys) @($ts.Keys)
    foreach ($name in $mapped.Keys) {
        $rustType = $mapped[$name].Type
        $expectedType = switch ($rustType) {
            'String' { 'string' }
            'Option<String>' { 'string|null' }
            'Option<usize>' { 'number|null' }
            'Option<Vec<String>>' { 'string[]|null' }
            default { throw "$label has unsupported Rust request type '$rustType' for '$name'" }
        }
        if ($ts[$name].Type -ne $expectedType) {
            throw "$label request field '$name' type mismatch: Rust '$rustType' vs TypeScript '$($ts[$name].Type)'"
        }
        $rustOptional = $rustType.StartsWith('Option<')
        if ($ts[$name].Optional -ne $rustOptional) {
            throw "$label request field '$name' optionality mismatch"
        }
    }
}
if ([regex]::IsMatch($actionStore, "invoke\s*\(\s*'(?:list_actions|cancel_action)'")) {
    throw 'Action store must not bypass the action command boundary'
}
if (-not [regex]::IsMatch($uiActionCommands, 'cancelActionCommand\s*\(request:\s*CancelActionRequest\):\s*Promise<boolean>')) {
    throw 'cancel_action must use a named request and boolean result contract'
}
if (-not [regex]::IsMatch($uiActionCommands, "(?s)cancelActionCommand\s*\(request:\s*CancelActionRequest\).*?invoke\('cancel_action',\s*request\)")) {
    throw 'cancel_action command must receive the named request unchanged'
}
$rustFields = Get-StructFields $rustActionStruct.Groups[1].Value 'Rust ActionEvent'
$tsFields = Get-StructFields $tsActionStruct.Groups[1].Value 'TypeScript ActionPayload'
Assert-ActionFieldContract $rustFields $tsFields
foreach ($field in $rustFields.Keys) {
    Assert-ActionFieldMapped $tsActionMapper.Groups[1].Value $field
}

$actionCommandNames = @('list_actions', 'list_action_history')
foreach ($command in $actionCommandNames) {
    $commandPattern = '(?ms)pub\s+async\s+fn\s+' + [regex]::Escape($command) + '\b[^{}]*?->\s*Result\s*<\s*Vec\s*<\s*ActionEvent\s*>\s*,\s*String\s*>'
    if (-not [regex]::IsMatch($actionCommands, $commandPattern)) {
        throw "action command '$command' must return Result<Vec<ActionEvent>, String>"
    }

    $rustContractPattern = '(?s)CommandContract\s*\{\s*name:\s*"' + [regex]::Escape($command) + '"([^}]*)\}'
    $rustCommandContract = Get-RequiredMatch $rustContracts $rustContractPattern "Rust contract for '$command'"
    $rustResponse = Get-RequiredMatch $rustCommandContract.Groups[1].Value 'response:\s*"([^"]+)"' "Rust response for '$command'"
    if ($rustResponse.Groups[1].Value -ne 'ActionEvent[]') {
        throw "Rust contract for '$command' must declare ActionEvent[]; found '$($rustResponse.Groups[1].Value)'"
    }

    $tsContractPattern = '(?ms)^\s*' + [regex]::Escape($command) + '\s*:\s*\{([^}]*)\}'
    $tsCommandContract = Get-RequiredMatch $tsContracts $tsContractPattern "frontend contract for '$command'"
    $tsResponse = Get-RequiredMatch $tsCommandContract.Groups[1].Value "response:\s*'([^']+)'" "frontend response for '$command'"
    if ($tsResponse.Groups[1].Value -ne 'ActionEvent[]') {
        throw "Frontend contract for '$command' must declare ActionEvent[]; found '$($tsResponse.Groups[1].Value)'"
    }
}

$rustActionChannels = @{}
foreach ($match in [regex]::Matches($rustEvents, '(?m)^pub\(crate\)\s+const\s+(ACTION_[A-Z0-9_]+_EVENT):\s*&str\s*=\s*"([^"]+)";')) {
    $rustActionChannels[$match.Groups[1].Value] = $match.Groups[2].Value
}
if ($rustActionChannels.Count -eq 0) {
    throw 'could not find Rust action event channel registrations'
}
$tsActionEventList = Get-RequiredMatch $tsAction '(?ms)export\s+const\s+ACTION_EVENT_NAMES\s*=\s*\[(.*?)\]\s*as\s+const' 'TypeScript ACTION_EVENT_NAMES'
$tsActionChannels = Get-Matches $tsActionEventList.Groups[1].Value "'([^']+)'"
Assert-SetEqual 'Rust ActionEvent channels vs frontend action event registry' @($rustActionChannels.Values) $tsActionChannels

$uiActionListenerRegistration = Get-RequiredMatch $actionLayout '(?s)\.\.\.actionEventListeners\s*\(\s*\{' 'UI ActionEvent listener registration'
$uiActionChannels = Get-Matches $actionLayout "(?m)^\s*'(action:[a-z-]+)'\s*:"
Assert-SetEqual 'Rust ActionEvent channels vs UI action listener registration' @($rustActionChannels.Values) $uiActionChannels

$bridgeMappings = [regex]::Matches($eventBridge, '(?m)^\s*\(ActionKind::(\w+),\s*"([^"]+)"\)\s*=>\s*\(\s*(ACTION_[A-Z0-9_]+_EVENT),\s*ActionEvent::([a-z_]+)\(payload')
if ($bridgeMappings.Count -eq 0) {
    throw 'could not find typed ActionEvent bridge registrations'
}
$bridgePairs = @{}
$bridgeChannels = @()
foreach ($mapping in $bridgeMappings) {
    $kind = $mapping.Groups[1].Value
    $eventName = $mapping.Groups[2].Value
    $channelName = $mapping.Groups[3].Value
    if (-not $rustActionChannels.ContainsKey($channelName)) {
        throw "action event bridge maps '$kind/$eventName' to unregistered channel '$channelName'"
    }
    if ($rustActionChannels[$channelName] -ne $eventName) {
        throw "action event bridge maps '$kind/$eventName' to '$channelName' registered as '$($rustActionChannels[$channelName])'"
    }
    $pair = "$kind/$eventName"
    if ($bridgePairs.ContainsKey($pair)) {
        throw "action event bridge registers '$pair' more than once"
    }
    $bridgePairs[$pair] = $channelName
    $bridgeChannels += $channelName
}
Assert-SetEqual 'Rust ActionEvent bridge vs channel registry' @($rustActionChannels.Keys) $bridgeChannels

$actionSinkRegistration = Get-RequiredMatch $bootstrap '(?s)state\.services\.actions\.set_event_sink\s*\(\s*Arc::new\s*\(\s*move\s*\|event:\s*String,\s*payload:\s*serde_json::Value\|\s*\{.*?emit_action_event\s*\(\s*&action_sink_handle,\s*kind,\s*&event,\s*&payload\s*\);' 'ActionService-to-Tauri event bridge registration'

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

Write-Host "Action IPC contract verified: $($rustFields.Count) ActionEvent fields, $($actionCommandNames.Count) command responses, and $($rustActionChannels.Count) registered event channels agree."

$memoryContract = Get-Content (Join-Path $root 'ui/src/lib/contracts/memory.ts') -Raw
$memoryCommandsUi = Get-Content (Join-Path $root 'ui/src/lib/memoryCommands.ts') -Raw
$memoryView = Get-Content (Join-Path $root 'ui/src/lib/views/MemoryView.svelte') -Raw
$memoryFactsRs = Get-Content (Join-Path $root 'crates/memory/src/repositories/facts.rs') -Raw
$memoryCommandsRs = Get-Content (Join-Path $commandsRoot 'memory.rs') -Raw
$memoryCommandContractsRs = Get-Content (Join-Path $commandsRoot 'contracts.rs') -Raw

$memoryBoundaryChecks = @(
    @{
        Command = 'list_facts'
        Function = 'listFacts'
        ViewCall = 'listFacts'
        Request = 'ListFactsRequest'
        Response = 'Fact[]'
        RustResponse = 'Fact[]'
    },
    @{
        Command = 'add_fact'
        Function = 'addFact'
        ViewCall = 'addFactCommand'
        Request = 'AddFactRequest'
        Response = 'Fact'
        RustResponse = 'Fact'
    },
    @{
        Command = 'delete_fact'
        Function = 'deleteFact'
        ViewCall = 'deleteFactCommand'
        Request = 'DeleteFactRequest'
        Response = 'void'
        RustResponse = '()'
    },
    @{
        Command = 'recall_memory'
        Function = 'recallMemory'
        ViewCall = 'recallMemory'
        Request = 'RecallMemoryRequest'
        Response = 'MemoryRecallItem[]'
        RustResponse = 'MemoryRecallItem[]'
    }
)

foreach ($check in $memoryBoundaryChecks) {
    $functionName = [regex]::Escape($check.Function)
    $requestName = [regex]::Escape($check.Request)
    $responseName = [regex]::Escape($check.Response)
    $rustResponseName = [regex]::Escape($check.RustResponse)
    $commandName = [regex]::Escape($check.Command)
    $functionPattern = "(?s)export\s+function\s+$functionName\s*\(\s*request:\s*$requestName\s*\):\s*Promise<$responseName>\s*\{\s*return\s+invoke\('$commandName',\s*request\);\s*\}"
    if (-not [regex]::IsMatch($memoryCommandsUi, $functionPattern)) {
        throw "memory command '$($check.Command)' must use its named request/response helper and forward request unchanged"
    }
    if (-not [regex]::IsMatch($memoryView, ('\b' + [regex]::Escape($check.ViewCall) + '\s*\('))) {
        throw "MemoryView must call '$($check.ViewCall)' for '$($check.Command)'"
    }
    $rustContractPattern = '(?s)CommandContract\s*\{\s*name:\s*"' + $commandName + '"([^}]*)\}'
    $rustCommandContract = Get-RequiredMatch $memoryCommandContractsRs $rustContractPattern "Rust contract for '$($check.Command)'"
    if ($rustCommandContract.Groups[1].Value -notmatch ('request:\s*"' + $requestName + '"') -or
        $rustCommandContract.Groups[1].Value -notmatch ('response:\s*"' + $rustResponseName + '"')) {
        throw "Rust command contract for '$($check.Command)' does not match the frontend helper contract"
    }
    $tsContractPattern = '(?ms)^\s*' + $commandName + '\s*:\s*\{([^}]*)\}'
    $tsCommandContract = Get-RequiredMatch $tsContracts $tsContractPattern "frontend contract for '$($check.Command)'"
    if ($tsCommandContract.Groups[1].Value -notmatch ("request:\s*'" + $requestName + "'") -or
        $tsCommandContract.Groups[1].Value -notmatch ("response:\s*'" + $responseName + "'")) {
        throw "Frontend command contract for '$($check.Command)' does not match its helper"
    }
}

if ([regex]::IsMatch($memoryView, "invoke\s*\(\s*'(?:list_facts|add_fact|delete_fact|recall_memory)'")) {
    throw 'MemoryView must not bypass memoryCommands.ts for fact and recall commands'
}

if ([regex]::Matches($memoryContract, '(?m)^export\s+interface\s+Fact\s*\{').Count -ne 1 -or
    [regex]::Matches($memoryContract, '(?m)^export\s+interface\s+MemoryRecallItem\s*\{').Count -ne 1) {
    throw 'memory command responses must each have one named frontend contract'
}

$rustFact = Get-RequiredMatch $memoryFactsRs '(?ms)pub\s+struct\s+Fact\s*\{(.*?)\n\}' 'Rust Fact response'
$rustFactSourceRef = Get-RequiredMatch $memoryFactsRs '(?ms)pub\s+struct\s+FactSourceRef\s*\{(.*?)\n\}' 'Rust FactSourceRef response'
$rustRecall = Get-RequiredMatch $memoryCommandContractsRs '(?ms)pub\s+struct\s+MemoryRecallItem\s*\{(.*?)\n\}' 'Rust MemoryRecallItem response'
$tsFact = Get-RequiredMatch $memoryContract '(?ms)export\s+interface\s+Fact\s*\{(.*?)\n\}' 'TypeScript Fact response'
$tsFactSourceRef = Get-RequiredMatch $memoryContract '(?ms)export\s+interface\s+FactSourceRef\s*\{(.*?)\n\}' 'TypeScript FactSourceRef response'
$tsRecall = Get-RequiredMatch $memoryContract '(?ms)export\s+interface\s+MemoryRecallItem\s*\{(.*?)\n\}' 'TypeScript MemoryRecallItem response'
Assert-MemoryWireFieldContract 'Fact' (Get-StructFields $rustFact.Groups[1].Value 'Rust Fact') (Get-StructFields $tsFact.Groups[1].Value 'TypeScript Fact')
Assert-MemoryWireFieldContract 'FactSourceRef' (Get-StructFields $rustFactSourceRef.Groups[1].Value 'Rust FactSourceRef') (Get-StructFields $tsFactSourceRef.Groups[1].Value 'TypeScript FactSourceRef')
Assert-MemoryWireFieldContract 'MemoryRecallItem' (Get-StructFields $rustRecall.Groups[1].Value 'Rust MemoryRecallItem') (Get-StructFields $tsRecall.Groups[1].Value 'TypeScript MemoryRecallItem')

$memoryRequestChecks = @(
    @{ Function = 'recall_memory'; Request = 'RecallMemoryRequest' },
    @{ Function = 'list_facts'; Request = 'ListFactsRequest' },
    @{ Function = 'add_fact'; Request = 'AddFactRequest' },
    @{ Function = 'delete_fact'; Request = 'DeleteFactRequest' }
)
foreach ($check in $memoryRequestChecks) {
    $functionName = [regex]::Escape($check.Function)
    $requestName = [regex]::Escape($check.Request)
    $rustFunction = Get-RequiredMatch $memoryCommandsRs ('(?ms)pub\s+async\s+fn\s+' + $functionName + '\s*\((.*?)\)\s*->') "Rust parameters for '$($check.Function)'"
    $tsRequest = Get-RequiredMatch $tsContracts ('(?ms)export\s+interface\s+' + $requestName + '\s*\{(.*?)\n\}') "TypeScript request '$($check.Request)'"
    Assert-MemoryRequestContract $check.Function (Get-RustCommandParameters $rustFunction.Groups[1].Value "Rust '$($check.Function)' parameters") (Get-StructFields $tsRequest.Groups[1].Value "TypeScript '$($check.Request)'")
}

Write-Host "Memory IPC contract verified: $($memoryBoundaryChecks.Count) typed command helpers and MemoryView call boundary agree."

