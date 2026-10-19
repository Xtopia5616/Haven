use haven_agent::METRICS_PHASE_COUNT;
use haven_common::config::MAX_FACTS_IN_EXTRACTION_PROMPT;
use quote::ToTokens;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::{Expr, FnArg, Item, Lit, Pat, ReturnType, Type};

#[path = "../ipc_codegen_types.rs"]
mod ipc_types;
use ipc_types::{RustTypeGraph, TypeContext, TypeUse};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Command {
    name: String,
    request: String,
    response: String,
    response_shape: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSecurityContract {
    boundary: &'static str,
    security: String,
}

const GENERATED_COMMAND_METADATA_START: &str = "<!-- BEGIN GENERATED COMMAND SECURITY METADATA -->";
const GENERATED_COMMAND_METADATA_END: &str = "<!-- END GENERATED COMMAND SECURITY METADATA -->";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("ui/src/lib/contracts/generatedCommands.ts");
    let docs = root.join("docs/ipc-contracts.md");
    let (generated, command_table) =
        generate(&root).unwrap_or_else(|error| panic!("IPC contract generation failed: {error}"));

    match std::env::args().nth(1).as_deref() {
        Some("--check") => {
            let current = fs::read_to_string(&output).unwrap_or_else(|_| {
                panic!(
                    "{} is missing; run scripts/generate-ipc-contracts.ps1",
                    output.display()
                )
            });
            if current != generated {
                panic!(
                    "{} is stale; run scripts/generate-ipc-contracts.ps1",
                    output.display()
                );
            }
            let current_docs = fs::read_to_string(&docs).unwrap_or_else(|_| {
                panic!(
                    "{} is missing; run scripts/generate-ipc-contracts.ps1",
                    docs.display()
                )
            });
            let expected_docs = replace_generated_command_table(&current_docs, &command_table)
                .unwrap_or_else(|error| panic!("IPC documentation generation failed: {error}"));
            if current_docs != expected_docs {
                panic!(
                    "{} has stale command security metadata; run scripts/generate-ipc-contracts.ps1",
                    docs.display()
                );
            }
        }
        Some("--write") | None => {
            fs::write(&output, generated)
                .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
            let current_docs = fs::read_to_string(&docs)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", docs.display()));
            let updated_docs = replace_generated_command_table(&current_docs, &command_table)
                .unwrap_or_else(|error| panic!("IPC documentation generation failed: {error}"));
            fs::write(&docs, updated_docs)
                .unwrap_or_else(|error| panic!("failed to write {}: {error}", docs.display()));
        }
        Some(argument) => panic!("unsupported argument {argument:?}; expected --check or --write"),
    }
}

fn generate(root: &Path) -> Result<(String, String), String> {
    let commands_root = root.join("crates/app-binary/src/commands");
    let mut type_graph = RustTypeGraph::load(root)?;
    let mut commands = BTreeMap::new();
    let mut files = fs::read_dir(&commands_root)
        .map_err(|error| format!("read {}: {error}", commands_root.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("list command files: {error}"))?;
    files.sort_by_key(|entry| entry.file_name());

    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let file = syn::parse_file(&source)
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        let context = RustTypeGraph::context_for_command(root, &path, &file.items)?;
        for item in &file.items {
            let Item::Fn(function) = item else { continue };
            if !is_tauri_command(&function.attrs)
                || !matches!(function.vis, syn::Visibility::Public(_))
            {
                continue;
            }
            let name = function.sig.ident.to_string();
            let request = request_type(
                &function.sig.inputs,
                argument_case(&function.attrs)?,
                &mut type_graph,
                &context,
            )
            .map_err(|error| format!("command {name} request: {error}"))?;
            let response = response_type(&function.sig.output, &mut type_graph, &context)
                .map_err(|error| format!("command {name} response: {error}"))?;
            let response_shape = format!("{}_RESPONSE_SHAPE", name.to_ascii_uppercase());
            let response_success = response_success_type(&function.sig.output)
                .map_err(|error| format!("command {name} response: {error}"))?;
            type_graph
                .emit_response_shape_for_type(&response_success, &context, &response_shape)
                .map_err(|error| format!("command {name} response shape: {error}"))?;
            let command = Command {
                name: name.clone(),
                request,
                response,
                response_shape,
            };
            if commands.insert(name.clone(), command).is_some() {
                return Err(format!("command {name} is declared more than once"));
            }
        }
    }

    if commands.is_empty() {
        return Err("no #[tauri::command] handlers found".into());
    }

    let command_contracts =
        command_security_contracts(&root.join("crates/app-binary/src/commands/contracts.rs"))?;
    let handler_names = commands.keys().cloned().collect::<HashSet<_>>();
    let contract_names = command_contracts.keys().cloned().collect::<HashSet<_>>();
    if handler_names != contract_names {
        return Err(format!(
            "command security registry does not match Tauri handlers (missing: {:?}; unknown: {:?})",
            handler_names
                .difference(&contract_names)
                .collect::<Vec<_>>(),
            contract_names
                .difference(&handler_names)
                .collect::<Vec<_>>(),
        ));
    }

    let event_channels = event_channels(&root.join("crates/app-binary/src/events.rs"))?;

    // Session lifecycle events are public IPC contracts even though event
    // payloads are not returned from Tauri commands. Keep their tagged union
    // generated from the same Rust Serde authority as command responses.
    type_graph.emit_definition(
        "haven_app_binary_lib::events::SessionLifecycleEvent",
        TypeUse::Response,
    )?;
    type_graph.emit_definition(
        "haven_app_binary_lib::events::VadStatusEvent",
        TypeUse::Response,
    )?;
    // Notification event fields use the App-owned wire vocabularies. Keep
    // their TypeScript types and validator values generated from these enums.
    for event_enum in [
        "RecordingStopReasonDto",
        "VadSignalDto",
        "VadStateDto",
        "ToolRunKindDto",
        "ToolRunCompletionStatusDto",
        "AgentNotificationKind",
    ] {
        type_graph.emit_definition(
            &format!("haven_app_binary_lib::events::{event_enum}"),
            TypeUse::Response,
        )?;
    }
    for event_type in [
        "AppBootstrapEvent",
        "TrayStatusChangedEvent",
        "MuteChangedEvent",
        "McpStatusChangedEvent",
        "McpResourceChangedEvent",
        "SkillsStatusChangedEvent",
        "HotkeyConflictEvent",
        "HotkeyRebindEvent",
    ] {
        type_graph.emit_definition(
            &format!("haven_app_binary_lib::events::{event_type}"),
            TypeUse::Response,
        )?;
    }
    for usage_type in [
        "CacheAccounting",
        "LlmCallKind",
        "PromptCacheStrategy",
        "CacheDiagnosticOutcome",
        "CacheUsageSource",
        "CacheDiagnostics",
    ] {
        type_graph.emit_definition(
            &format!("haven_common::usage::{usage_type}"),
            TypeUse::Response,
        )?;
    }
    for media_enum in ["MediaProjectionMode", "MediaPlanNoticeCode"] {
        let key = format!("haven_common::media::{media_enum}");
        type_graph.emit_definition(&key, TypeUse::Response)?;
    }
    type_graph.emit_definition("haven_common::types::InjectSource", TypeUse::Response)?;
    type_graph.emit_definition("haven_llm::types::WebSearchPhase", TypeUse::Response)?;
    for result_enum in [
        "OperationIdempotency",
        "ToolErrorClass",
        "ToolExecutionOutcome",
        "ToolRetryability",
    ] {
        type_graph.emit_definition(
            &format!("haven_common::tools::{result_enum}"),
            TypeUse::Response,
        )?;
    }
    type_graph.emit_definition("haven_mcp::protocol::McpClientStatus", TypeUse::Response)?;
    type_graph.emit_external_unit_variant_values(
        "haven_mcp::protocol::McpClientStatus",
        "MCP_CLIENT_STATUS_UNIT_VALUES",
        TypeUse::Response,
    )?;
    type_graph.emit_response_shape(
        "haven_common::config::loader::SettingsSnapshot",
        "SETTINGS_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::memory::MemoryFactResponse",
        "MEMORY_FACT_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::memory::MemoryRecallItem",
        "MEMORY_RECALL_ITEM_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::RuntimeSessionListResponse",
        "RUNTIME_SESSION_LIST_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::session::SessionRecordDto",
        "SESSION_RECORD_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::session::SessionLineageResponse",
        "SESSION_LINEAGE_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::session::SessionResumeResponse",
        "SESSION_RESUME_RESPONSE_SHAPE",
    )?;
    type_graph.emit_response_shape(
        "haven_app_binary_lib::commands::settings::SessionPermissionGrant",
        "SESSION_PERMISSION_GRANT_RESPONSE_SHAPE",
    )?;

    // Associate every registered event channel with its Rust-serialized
    // payload type. Generator failure keeps the runtime field-shape registry
    // exhaustive when a channel is added or renamed.
    let event_payload_shapes = [
        ("session:lifecycle", Some("SessionLifecycleEvent")),
        ("recording:started", Some("RecordingEvent")),
        ("recording:stopped", Some("RecordingEvent")),
        ("recording:vad_status", Some("VadStatusEvent")),
        ("recording:error", Some("RecordingErrorEvent")),
        ("transcription:started", Some("TranscriptionStartedEvent")),
        ("transcription:result", Some("TranscriptionResultEvent")),
        ("transcription:error", Some("TranscriptionErrorEvent")),
        ("tool_run:created", Some("ToolRunEvent")),
        ("tool_run:updated", Some("ToolRunEvent")),
        ("tool_run:finished", Some("ToolRunEvent")),
        ("app:bootstrap", Some("AppBootstrapEvent")),
        ("tray:status_changed", Some("TrayStatusChangedEvent")),
        ("mute:changed", Some("MuteChangedEvent")),
        ("mcp:status_change", Some("McpStatusChangedEvent")),
        ("mcp:resource_changed", Some("McpResourceChangedEvent")),
        ("skills:status_change", Some("SkillsStatusChangedEvent")),
        ("interaction:requested", Some("InteractionRequestedEvent")),
        ("hotkey:conflict", Some("HotkeyConflictEvent")),
        ("hotkey:rebind", Some("HotkeyRebindEvent")),
        ("llm:config_changed", None),
        ("agent:thought", Some("AgentThoughtEvent")),
        ("agent:tool_call", Some("AgentToolCallEvent")),
        ("agent:observation", Some("AgentObservationEvent")),
        ("agent:thought_chunk", Some("AgentThoughtChunkEvent")),
        ("agent:reasoning_chunk", Some("AgentReasoningChunkEvent")),
        ("agent:tool_call_chunk", Some("AgentToolCallChunkEvent")),
        ("agent:stream_reset", Some("AgentStreamResetEvent")),
        ("agent:media_plan", Some("AgentMediaPlanEvent")),
        ("agent:web_search", Some("AgentWebSearchEvent")),
        ("agent:stream_stalled", Some("AgentStreamStalledEvent")),
        ("agent:supplement", Some("AgentSupplementEvent")),
        ("agent:compaction", Some("AgentCompactionEvent")),
        ("agent:usage", Some("AgentUsageEvent")),
        ("agent:tool_output", Some("AgentToolOutputEvent")),
        ("agent:working_document", Some("AgentWorkingDocumentEvent")),
        ("notification:show", Some("AgentNotificationEvent")),
    ];
    let declared_event_channels = event_channels
        .values()
        .flatten()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mapped_event_channels = event_payload_shapes
        .iter()
        .map(|(channel, _)| *channel)
        .collect::<HashSet<_>>();
    if declared_event_channels != mapped_event_channels {
        return Err(format!(
            "event payload shape registry does not match declared event channels (unmapped: {:?}; unknown: {:?})",
            declared_event_channels
                .difference(&mapped_event_channels)
                .collect::<Vec<_>>(),
            mapped_event_channels
                .difference(&declared_event_channels)
                .collect::<Vec<_>>(),
        ));
    }
    for (channel, type_name) in event_payload_shapes {
        if let Some(type_name) = type_name {
            let shape_name = event_shape_constant(channel);
            type_graph.emit_response_shape(
                &format!("haven_app_binary_lib::events::{type_name}"),
                &shape_name,
            )?;
        }
    }

    let mut output = String::from(
        "// Generated from Tauri commands and `crates/app-binary/src/events.rs` by `scripts/generate-ipc-contracts.ps1`.\n\
         // Do not edit by hand; `scripts/check-ipc-contracts.ps1` rejects drift.\n\n\
         // DTO declarations below are generated from Rust Serialize types.\n\n",
    );
    for (group, channels) in &event_channels {
        output.push_str(&format!("export const {group}_EVENT_NAMES = [\n"));
        for channel in channels {
            output.push_str(&format!("\t'{channel}',\n"));
        }
        output.push_str("] as const;\n\n");
    }
    output.push_str(&type_graph.declarations());
    output.push_str(&type_graph.runtime_shape_declarations());
    output.push_str("\nexport const TAURI_COMMAND_RESPONSE_SHAPES = {\n");
    for command in commands.values() {
        output.push_str(&format!(
            "\t{}: {},\n",
            command.name, command.response_shape
        ));
    }
    output.push_str("} as const;\n\nexport const IPC_EVENT_PAYLOAD_SHAPES = {\n");
    for (channel, type_name) in event_payload_shapes {
        let shape = type_name
            .map(|_| event_shape_constant(channel))
            .unwrap_or_else(|| "{ kind: 'null' }".to_string());
        output.push_str(&format!("\t'{channel}': {shape},\n"));
    }
    output.push_str("} as const;\n\nexport const IPC_EVENT_PAYLOAD_DTO_TYPES = {\n");
    for (channel, type_name) in event_payload_shapes {
        let type_name = type_name
            .map(|name| serde_json::to_string(name).unwrap())
            .unwrap_or_else(|| "null".to_string());
        output.push_str(&format!("\t'{channel}': {type_name},\n"));
    }
    output.push_str("} as const;\n");
    output.push_str(&format!(
        "\nexport const AGENT_METRICS_PHASE_COUNT = {METRICS_PHASE_COUNT} as const;\nexport const MAX_FACTS_IN_EXTRACTION_PROMPT = {MAX_FACTS_IN_EXTRACTION_PROMPT} as const;\n\n"
    ));
    output.push_str("\nexport interface TauriCommandMap {\n");
    for command in commands.values() {
        output.push_str(&format!(
            "\t{}: {{ request: {}; response: {} }};\n",
            command.name, command.request, command.response
        ));
    }
    output.push_str(
        "}\n\n\
         export type TauriCommandName = keyof TauriCommandMap;\n\
         export type TauriCommandRequest<K extends TauriCommandName> = TauriCommandMap[K]['request'];\n\
         export type TauriCommandResponse<K extends TauriCommandName> = TauriCommandMap[K]['response'];\n\
         export type TauriCommandRequestArgs<K extends TauriCommandName> = TauriCommandRequest<K> extends undefined\n\
         \t? [request?: undefined]\n\
         \t: {} extends TauriCommandRequest<K>\n\
         \t\t? [request?: TauriCommandRequest<K>]\n\
         \t\t: [request: TauriCommandRequest<K>];\n\n\
         export type TauriCommandInvoke = {\n\
             <K extends TauriCommandName>(\n\
             command: K,\n\
             ...args: TauriCommandRequestArgs<K>\n\
             ): Promise<TauriCommandResponse<K>>;\n\
         };\n",
    );
    output.push_str(
        "\nexport type CommandBoundary = 'read' | 'mutate' | 'execute';\n\n\
         export interface CommandContract {\n\
         \tboundary: CommandBoundary;\n\
         \tsecurity: string;\n\
         }\n\n\
         export const TAURI_COMMAND_CONTRACTS = {\n",
    );
    for command in commands.values() {
        let contract = &command_contracts[&command.name];
        let security = serde_json::to_string(&contract.security)
            .map_err(|error| format!("serialize command metadata for {}: {error}", command.name))?;
        output.push_str(&format!(
            "\t{}: {{ boundary: '{}', security: {} }},\n",
            command.name, contract.boundary, security
        ));
    }
    output.push_str("} as const satisfies Record<TauriCommandName, CommandContract>;\n");

    let mut command_table = String::from("| 命令 | 边界 | 安全不变量 |\n|---|---|---|\n");
    for command in commands.values() {
        let contract = &command_contracts[&command.name];
        let security = contract.security.replace('|', "\\|");
        command_table.push_str(&format!(
            "| `{}` | {} | {} |\n",
            command.name, contract.boundary, security
        ));
    }
    Ok((output, command_table))
}

fn command_security_contracts(
    path: &Path,
) -> Result<BTreeMap<String, CommandSecurityContract>, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let file =
        syn::parse_file(&source).map_err(|error| format!("parse {}: {error}", path.display()))?;
    let registry = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Const(item) if item.ident == "COMMAND_CONTRACTS" => Some(item),
            _ => None,
        })
        .ok_or_else(|| "COMMAND_CONTRACTS registry is missing".to_string())?;
    let Expr::Reference(reference) = registry.expr.as_ref() else {
        return Err("COMMAND_CONTRACTS must be a reference to a static array".into());
    };
    let Expr::Array(entries) = reference.expr.as_ref() else {
        return Err("COMMAND_CONTRACTS must be a reference to a static array".into());
    };

    let mut contracts = BTreeMap::new();
    for entry in &entries.elems {
        let Expr::Struct(contract) = entry else {
            return Err("COMMAND_CONTRACTS entries must be CommandContract struct literals".into());
        };
        let type_name = contract
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        if type_name.as_deref() != Some("CommandContract") {
            return Err("COMMAND_CONTRACTS entries must be CommandContract struct literals".into());
        }
        let mut name = None;
        let mut boundary = None;
        let mut security = None;
        for field in &contract.fields {
            let syn::Member::Named(field_name) = &field.member else {
                return Err("CommandContract fields must be named".into());
            };
            match field_name.to_string().as_str() {
                "name" => name = Some(string_literal(&field.expr, "command name")?),
                "boundary" => boundary = Some(boundary_name(&field.expr)?),
                "security" => security = Some(string_literal(&field.expr, "security metadata")?),
                other => return Err(format!("unknown CommandContract field {other}")),
            }
        }
        let name = name.ok_or_else(|| "CommandContract is missing name".to_string())?;
        let contract = CommandSecurityContract {
            boundary: boundary.ok_or_else(|| format!("{name} is missing boundary"))?,
            security: security.ok_or_else(|| format!("{name} is missing security metadata"))?,
        };
        if contract.security.trim().is_empty() {
            return Err(format!("{name} security metadata must not be empty"));
        }
        if contracts.insert(name.clone(), contract).is_some() {
            return Err(format!("command {name} has duplicate security metadata"));
        }
    }
    Ok(contracts)
}

fn string_literal(expression: &Expr, label: &str) -> Result<String, String> {
    let Expr::Lit(expression) = expression else {
        return Err(format!("{label} must be a string literal"));
    };
    let Lit::Str(value) = &expression.lit else {
        return Err(format!("{label} must be a string literal"));
    };
    Ok(value.value())
}

fn boundary_name(expression: &Expr) -> Result<&'static str, String> {
    let Expr::Path(expression) = expression else {
        return Err("command boundary must be a CommandBoundary variant".into());
    };
    match expression
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .as_deref()
    {
        Some("Read") => Ok("read"),
        Some("Mutate") => Ok("mutate"),
        Some("Execute") => Ok("execute"),
        _ => Err("command boundary must be Read, Mutate, or Execute".into()),
    }
}

fn replace_generated_command_table(source: &str, table: &str) -> Result<String, String> {
    let start = source
        .find(GENERATED_COMMAND_METADATA_START)
        .ok_or_else(|| format!("missing {GENERATED_COMMAND_METADATA_START} marker"))?;
    let end = source
        .find(GENERATED_COMMAND_METADATA_END)
        .ok_or_else(|| format!("missing {GENERATED_COMMAND_METADATA_END} marker"))?;
    if end <= start {
        return Err("generated command metadata markers are out of order".into());
    }
    let content_start = source[start..]
        .find('\n')
        .map(|offset| start + offset + 1)
        .ok_or_else(|| "generated command metadata start marker has no line ending".to_string())?;
    let mut output = String::with_capacity(source.len() + table.len());
    output.push_str(&source[..content_start]);
    output.push_str(table);
    output.push_str(&source[end..]);
    Ok(output)
}

fn event_channels(path: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    event_channels_from_source(&source)
        .map_err(|error| format!("parse event channels from {}: {error}", path.display()))
}

fn event_channels_from_source(source: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let file = syn::parse_file(source).map_err(|error| format!("parse event source: {error}"))?;
    let mut channels = BTreeMap::<String, Vec<String>>::new();
    let mut seen_channels = HashSet::new();

    for item in file.items {
        let Item::Const(item) = item else { continue };
        let constant_name = item.ident.to_string();
        if !constant_name.ends_with("_EVENT") {
            continue;
        }
        let Type::Reference(reference) = item.ty.as_ref() else {
            return Err(format!(
                "event channel {constant_name} must have type &'static str"
            ));
        };
        if !matches!(reference.elem.as_ref(), Type::Path(path) if path.path.is_ident("str")) {
            return Err(format!(
                "event channel {constant_name} must have type &'static str"
            ));
        }
        let syn::Expr::Lit(expression) = item.expr.as_ref() else {
            return Err(format!(
                "event channel {constant_name} must be a string literal"
            ));
        };
        let syn::Lit::Str(value) = &expression.lit else {
            return Err(format!(
                "event channel {constant_name} must be a string literal"
            ));
        };
        let channel = value.value();
        if !seen_channels.insert(channel.clone()) {
            return Err(format!(
                "event channel {channel:?} is declared more than once"
            ));
        }
        let group = event_group(&constant_name)?;
        channels.entry(group.to_string()).or_default().push(channel);
    }

    if channels.is_empty() {
        return Err("no event channel constants found".into());
    }
    Ok(channels)
}

fn event_group(constant_name: &str) -> Result<&'static str, String> {
    let group = if constant_name.starts_with("SESSION_") {
        "SESSION"
    } else if constant_name.starts_with("RECORDING_") || constant_name.starts_with("TRANSCRIPTION_")
    {
        "RECORDING"
    } else if constant_name.starts_with("TOOL_RUN_") {
        "TOOL_RUN"
    } else if constant_name.starts_with("AGENT_") || constant_name.starts_with("NOTIFICATION_") {
        "AGENT"
    } else if [
        "APP_",
        "TRAY_",
        "MUTE_",
        "MCP_",
        "SKILLS_",
        "INTERACTION_",
        "HOTKEY_",
        "LLM_",
    ]
    .iter()
    .any(|prefix| constant_name.starts_with(prefix))
    {
        "APP"
    } else {
        return Err(format!(
            "event channel constant {constant_name} has no frontend contract group"
        ));
    };
    Ok(group)
}

fn is_tauri_command(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "command")
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgumentCase {
    Camel,
    Snake,
}

fn argument_case(attributes: &[syn::Attribute]) -> Result<ArgumentCase, String> {
    let Some(command_attribute) = attributes.iter().find(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "command")
    }) else {
        return Err("command attribute missing".into());
    };
    let mut selected = None;
    let syn::Meta::List(_) = &command_attribute.meta else {
        return Ok(ArgumentCase::Camel);
    };
    command_attribute
        .parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                let value = meta.value()?.parse::<syn::LitStr>()?.value();
                selected = Some(match value.as_str() {
                    "camelCase" => ArgumentCase::Camel,
                    "snake_case" => ArgumentCase::Snake,
                    other => {
                        return Err(
                            meta.error(format!("unsupported Tauri rename_all value {other:?}"))
                        );
                    }
                });
            } else {
                return Err(meta.error("unsupported Tauri command attribute argument"));
            }
            Ok(())
        })
        .map_err(|error| format!("invalid Tauri command attribute: {error}"))?;
    Ok(selected.unwrap_or(ArgumentCase::Camel))
}

fn request_type(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
    argument_case: ArgumentCase,
    type_graph: &mut RustTypeGraph,
    context: &TypeContext,
) -> Result<String, String> {
    let mut fields = Vec::new();
    for input in inputs {
        let FnArg::Typed(argument) = input else {
            return Err("receiver arguments are not supported in a Tauri command".into());
        };
        if is_runtime_injected(&argument.ty) {
            continue;
        }
        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(format!(
                "unsupported command argument pattern: {}",
                argument.pat.to_token_stream()
            ));
        };
        let name = match argument_case {
            ArgumentCase::Camel => snake_to_camel(&pattern.ident.to_string()),
            ArgumentCase::Snake => pattern.ident.to_string(),
        };
        let mapped = type_graph.map_type(&argument.ty, TypeUse::Request, context)?;
        let optional = if mapped.optional { "?" } else { "" };
        fields.push(format!("{name}{optional}: {}", mapped.ts));
    }
    if fields.is_empty() {
        Ok("undefined".into())
    } else {
        Ok(format!("{{ {} }}", fields.join("; ")))
    }
}

fn response_type(
    output: &ReturnType,
    type_graph: &mut RustTypeGraph,
    context: &TypeContext,
) -> Result<String, String> {
    let success = response_success_type(output)?;
    Ok(type_graph
        .map_type(&success, TypeUse::Response, context)?
        .ts)
}

fn response_success_type(output: &ReturnType) -> Result<Type, String> {
    let ReturnType::Type(_, ty) = output else {
        return Ok(syn::parse_quote!(()));
    };
    let Type::Path(path) = ty.as_ref() else {
        return Err(format!(
            "unsupported handler response type: {}",
            ty.to_token_stream()
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err("handler response has an empty type path".into());
    };
    if segment.ident != "Result" {
        return Err(format!(
            "Tauri handler responses must use Result<T, String>; found {}",
            ty.to_token_stream()
        ));
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err("Result response is missing type arguments".into());
    };
    let mut args = arguments.args.iter();
    let Some(syn::GenericArgument::Type(success)) = args.next() else {
        return Err("Result response is missing success type".into());
    };
    let Some(syn::GenericArgument::Type(error)) = args.next() else {
        return Err("Result response is missing error type".into());
    };
    if !matches!(error, Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "String"))
    {
        return Err(format!(
            "Tauri command error must be String, found {}",
            error.to_token_stream()
        ));
    }
    Ok(success.clone())
}

fn event_shape_constant(channel: &str) -> String {
    format!(
        "{}_EVENT_PAYLOAD_SHAPE",
        channel.replace(':', "_").to_ascii_uppercase()
    )
}

fn is_runtime_injected(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    path.path.segments.last().is_some_and(|segment| {
        matches!(
            segment.ident.to_string().as_str(),
            "State" | "AppHandle" | "Window" | "WebviewWindow"
        )
    })
}

fn snake_to_camel(value: &str) -> String {
    let mut parts = value.split('_');
    let mut result = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.extend(first.to_uppercase());
            result.extend(chars);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_channel_constants_map_to_their_frontend_contract_owners() {
        assert_eq!(event_group("SESSION_LIFECYCLE_EVENT").unwrap(), "SESSION");
        assert_eq!(
            event_group("TRANSCRIPTION_RESULT_EVENT").unwrap(),
            "RECORDING"
        );
        assert_eq!(event_group("TOOL_RUN_UPDATED_EVENT").unwrap(), "TOOL_RUN");
        assert_eq!(event_group("NOTIFICATION_SHOW_EVENT").unwrap(), "AGENT");
        assert_eq!(event_group("HOTKEY_REBIND_EVENT").unwrap(), "APP");
    }

    #[test]
    fn event_channel_grouping_rejects_unowned_constants() {
        assert!(event_group("CUSTOM_EVENT").is_err());
    }

    #[test]
    fn event_channel_directory_rejects_duplicate_wire_names() {
        let source = r#"
            const APP_FIRST_EVENT: &str = "app:changed";
            const APP_SECOND_EVENT: &str = "app:changed";
        "#;
        assert!(
            event_channels_from_source(source)
                .unwrap_err()
                .contains("declared more than once")
        );
    }

    #[test]
    fn request_contract_comes_from_flat_handler_arguments_and_omits_tauri_state() {
        let source = r#"
            #[tauri::command]
            async fn update_title(state: State<'_, AppState>, session_id: String, pause: Option<bool>) -> Result<(), String> { todo!() }
        "#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert_eq!(
            request_type(
                &function.sig.inputs,
                ArgumentCase::Camel,
                &mut graph,
                &context
            )
            .unwrap(),
            "{ sessionId: string; pause?: boolean | null }"
        );
        assert_eq!(
            response_type(&function.sig.output, &mut graph, &context).unwrap(),
            "void"
        );
    }

    #[test]
    fn argument_case_respects_tauri_command_attribute() {
        let source = r#"#[tauri::command(rename_all = "snake_case")] fn change_item(item_id: String) -> Result<(), String> { todo!() }"#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let casing = argument_case(&function.attrs).unwrap();
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert_eq!(casing, ArgumentCase::Snake);
        assert_eq!(
            request_type(&function.sig.inputs, casing, &mut graph, &context).unwrap(),
            "{ item_id: string }"
        );
    }

    #[test]
    fn generator_rejects_unmapped_dynamic_and_non_string_result_types() {
        let source = r#"#[tauri::command] fn command(id: String) -> Result<Vec<u8>, anyhow::Error> { todo!() }"#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert!(
            response_type(&function.sig.output, &mut graph, &context)
                .unwrap_err()
                .contains("must be String")
        );
        assert_eq!(
            request_type(
                &function.sig.inputs,
                ArgumentCase::Camel,
                &mut graph,
                &context
            )
            .unwrap(),
            "{ id: string }"
        );
    }

    #[test]
    fn unknown_request_dto_fails_closed() {
        let source = r#"#[tauri::command] fn command(payload: UnknownDto) -> Result<(), String> { todo!() }"#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert!(
            request_type(
                &function.sig.inputs,
                ArgumentCase::Camel,
                &mut graph,
                &context
            )
            .unwrap_err()
            .contains("unsupported/unmapped Rust DTO")
        );
    }
}
