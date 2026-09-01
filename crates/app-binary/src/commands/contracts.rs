//! The single registry for the Tauri command wire contract.
//!
//! Command arguments intentionally remain flat because Tauri maps the
//! renderer's camelCase object onto the Rust function arguments.  The
//! registry records the named request/response DTO at that boundary without
//! introducing a second `{ request: ... }` wire shape.  The strings are
//! documentation identifiers, not runtime JSON parsers; the Rust command
//! signatures and the frontend contract modules remain the executable side of
//! the contract.

/// Version of the public Tauri command directory.
pub const IPC_CONTRACT_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandBoundary {
    /// Read-only application state or diagnostics.
    Read,
    /// User-initiated state mutation.
    Mutate,
    /// A command that can execute a local, external, or otherwise privileged
    /// capability and therefore must retain the security gateway boundary.
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandContract {
    pub name: &'static str,
    /// Named DTO for the flat request fields, or `-` when there are no fields.
    pub request: &'static str,
    /// Named DTO, scalar, or `()` returned on success.
    pub response: &'static str,
    pub boundary: CommandBoundary,
    /// Short security invariant; detailed negative cases live in the tools
    /// security regression matrix.
    pub security: &'static str,
}

/// Every command passed to `tauri::generate_handler!` must have exactly one
/// entry here.  Keep this list in the same domain order as `commands/mod.rs`
/// and `lib.rs` so review can compare registration and documentation easily.
pub const COMMAND_CONTRACTS: &[CommandContract] = &[
    // action
    CommandContract {
        name: "list_actions",
        request: "-",
        response: "ActionEvent[]",
        boundary: CommandBoundary::Read,
        security: "projected task fields only",
    },
    CommandContract {
        name: "cancel_action",
        request: "CancelActionRequest",
        response: "bool",
        boundary: CommandBoundary::Mutate,
        security: "kind is enum; cancel only the selected task kind",
    },
    CommandContract {
        name: "list_action_history",
        request: "ListActionHistoryRequest",
        response: "ActionEvent[]",
        boundary: CommandBoundary::Read,
        security: "limit capped at 200; internal tool args excluded",
    },
    CommandContract {
        name: "delete_action",
        request: "DeleteActionRequest",
        response: "bool",
        boundary: CommandBoundary::Mutate,
        security: "delete one persisted task row by id",
    },
    // external
    CommandContract {
        name: "open_external",
        request: "OpenExternalRequest",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "http(s) or validated absolute local path only",
    },
    // history
    CommandContract {
        name: "get_history",
        request: "HistoryPageRequest",
        response: "Session[]",
        boundary: CommandBoundary::Read,
        security: "read-only session projection",
    },
    CommandContract {
        name: "count_history",
        request: "-",
        response: "i64",
        boundary: CommandBoundary::Read,
        security: "read-only aggregate",
    },
    CommandContract {
        name: "search_history_paginated",
        request: "HistorySearchPageRequest",
        response: "Session[]",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "count_history_search",
        request: "HistorySearchRequest",
        response: "i64",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "search_history",
        request: "HistorySearchRequest",
        response: "Session[]",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "search_history_filtered",
        request: "HistoryFilterRequest",
        response: "Session[]",
        boundary: CommandBoundary::Read,
        security: "bounded page and date-filtered projection",
    },
    CommandContract {
        name: "export_history",
        request: "HistoryExportRequest",
        response: "string",
        boundary: CommandBoundary::Read,
        security: "export contains persisted history only",
    },
    // log
    CommandContract {
        name: "get_log_info",
        request: "-",
        response: "LogInfo",
        boundary: CommandBoundary::Read,
        security: "path is optional; no environment details",
    },
    CommandContract {
        name: "read_log_tail",
        request: "ReadLogTailRequest",
        response: "LogTail",
        boundary: CommandBoundary::Read,
        security: "bounded tail; file logging must be enabled",
    },
    // mcp
    CommandContract {
        name: "list_mcp_tools",
        request: "-",
        response: "McpServerSnapshot[]",
        boundary: CommandBoundary::Read,
        security: "snapshot only; env values redacted; invocation remains gated",
    },
    CommandContract {
        name: "reconnect_mcp",
        request: "McpNameRequest",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "server name selects an existing configured client",
    },
    CommandContract {
        name: "refresh_mcp_servers",
        request: "-",
        response: "McpRefreshResult",
        boundary: CommandBoundary::Execute,
        security: "reconcile configured clients; no renderer command",
    },
    CommandContract {
        name: "mcp_tool_call",
        request: "McpToolCallRequest",
        response: "McpToolCallResponse",
        boundary: CommandBoundary::Execute,
        security: "MCP adapter name and args pass SafetyGateway",
    },
    CommandContract {
        name: "add_mcp_server",
        request: "McpServerConfig",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "shared self operation validates and persists config",
    },
    CommandContract {
        name: "update_mcp_server",
        request: "UpdateMcpServerRequest",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "shared self operation validates and reconnects safely",
    },
    CommandContract {
        name: "remove_mcp_server",
        request: "McpNameRequest",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "shared self operation removes client and config",
    },
    CommandContract {
        name: "toggle_mcp_server",
        request: "ToggleMcpServerRequest",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "shared self operation connects before enabling",
    },
    // memory
    CommandContract {
        name: "run_memory_maintenance",
        request: "-",
        response: "u64",
        boundary: CommandBoundary::Mutate,
        security: "maintenance path owns purge and embedding cleanup",
    },
    CommandContract {
        name: "recall_memory",
        request: "RecallMemoryRequest",
        response: "MemoryRecallItem[]",
        boundary: CommandBoundary::Read,
        security: "bounded and credential-filtered recall",
    },
    CommandContract {
        name: "list_facts",
        request: "ListFactsRequest",
        response: "Fact[]",
        boundary: CommandBoundary::Read,
        security: "read-only fact projection",
    },
    CommandContract {
        name: "add_fact",
        request: "AddFactRequest",
        response: "Fact",
        boundary: CommandBoundary::Mutate,
        security: "credential-like predicates and values rejected",
    },
    CommandContract {
        name: "delete_fact",
        request: "DeleteFactRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "delete one fact by id",
    },
    // model
    CommandContract {
        name: "get_api_key_status",
        request: "-",
        response: "ApiKeyStatus",
        boundary: CommandBoundary::Read,
        security: "boolean presence only; credentials excluded",
    },
    CommandContract {
        name: "check_llm_connection",
        request: "-",
        response: "string",
        boundary: CommandBoundary::Read,
        security: "status only; no provider payload",
    },
    CommandContract {
        name: "discover_models",
        request: "DiscoverModelsRequest",
        response: "ModelInfo[]",
        boundary: CommandBoundary::Execute,
        security: "http(s) endpoint plus stored-key host match",
    },
    CommandContract {
        name: "discover_all_models",
        request: "-",
        response: "Record<string, ModelInfo[]>",
        boundary: CommandBoundary::Execute,
        security: "only configured providers are queried",
    },
    CommandContract {
        name: "switch_model",
        request: "SwitchModelRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "role slot validated before config save",
    },
    CommandContract {
        name: "set_reasoning_effort",
        request: "SetReasoningEffortRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "role slot validated before config save",
    },
    CommandContract {
        name: "set_web_search",
        request: "SetWebSearchRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "provider capability checked before config save",
    },
    // recording
    CommandContract {
        name: "get_recording_state",
        request: "-",
        response: "RecordingState",
        boundary: CommandBoundary::Read,
        security: "state only; no device or provider detail",
    },
    CommandContract {
        name: "start_recording",
        request: "-",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "input pipeline owns capture lifecycle",
    },
    CommandContract {
        name: "stop_recording",
        request: "-",
        response: "string",
        boundary: CommandBoundary::Execute,
        security: "capture stops before asynchronous transcription",
    },
    CommandContract {
        name: "cancel_recording",
        request: "-",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "cancel clears the in-flight recording id",
    },
    CommandContract {
        name: "process_transcript",
        request: "ProcessTranscriptRequest",
        response: "ProcessResult",
        boundary: CommandBoundary::Execute,
        security: "attachment limits and file persistence are enforced",
    },
    // session
    CommandContract {
        name: "reopen_session",
        request: "SessionIdRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "session id selects persisted session",
    },
    CommandContract {
        name: "get_sessions",
        request: "-",
        response: "SessionListResponse",
        boundary: CommandBoundary::Read,
        security: "active session projection",
    },
    CommandContract {
        name: "end_session",
        request: "SessionIdRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "explicit user termination",
    },
    CommandContract {
        name: "resolve_confirmation",
        request: "ResolveConfirmationRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "effect/scope must match confirmation; deny wins",
    },
    CommandContract {
        name: "update_session_title",
        request: "UpdateSessionTitleRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "trimmed non-empty title only",
    },
    CommandContract {
        name: "delete_session",
        request: "SessionIdRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "delete by session id and release runtime state",
    },
    CommandContract {
        name: "clear_history",
        request: "-",
        response: "u64",
        boundary: CommandBoundary::Mutate,
        security: "clears persisted sessions and session trust",
    },
    CommandContract {
        name: "rollback_session",
        request: "RollbackSessionRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "event cursor and projection clock rollback",
    },
    CommandContract {
        name: "continue_session",
        request: "SessionIdRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "resume from saved error snapshot",
    },
    CommandContract {
        name: "get_session_for_resume",
        request: "SessionIdRequest",
        response: "SessionResumeResponse",
        boundary: CommandBoundary::Read,
        security: "session-scoped persisted projection",
    },
    CommandContract {
        name: "get_last_conversation",
        request: "-",
        response: "Option<SessionResumeResponse>",
        boundary: CommandBoundary::Read,
        security: "most recent persisted session only",
    },
    // settings
    CommandContract {
        name: "get_settings",
        request: "-",
        response: "Settings",
        boundary: CommandBoundary::Read,
        security: "config response masks credentials",
    },
    CommandContract {
        name: "get_bootstrap_status",
        request: "-",
        response: "string",
        boundary: CommandBoundary::Read,
        security: "status enum only",
    },
    CommandContract {
        name: "update_settings",
        request: "Settings",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "shared loader preserves masked secrets and tool sections",
    },
    CommandContract {
        name: "list_permissions",
        request: "-",
        response: "StoredPermission[]",
        boundary: CommandBoundary::Read,
        security: "permission keys/effects only",
    },
    CommandContract {
        name: "revoke_permission",
        request: "RevokePermissionRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "non-empty exact key; persisted atomically",
    },
    CommandContract {
        name: "check_shell_available",
        request: "CheckShellAvailableRequest",
        response: "ShellAvailability",
        boundary: CommandBoundary::Read,
        security: "availability boolean only",
    },
    CommandContract {
        name: "enable_autostart",
        request: "-",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "release build only",
    },
    CommandContract {
        name: "disable_autostart",
        request: "-",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "managed autostart entry only",
    },
    CommandContract {
        name: "is_autostart_enabled",
        request: "-",
        response: "bool",
        boundary: CommandBoundary::Read,
        security: "state boolean only",
    },
    // skills/tools
    CommandContract {
        name: "list_skills",
        request: "-",
        response: "SkillInfo[]",
        boundary: CommandBoundary::Read,
        security: "metadata projection",
    },
    CommandContract {
        name: "refresh_skills",
        request: "-",
        response: "()",
        boundary: CommandBoundary::Execute,
        security: "configured skills root scan",
    },
    CommandContract {
        name: "set_skill_enabled",
        request: "SetEnabledRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "shared self operation persists the toggle",
    },
    CommandContract {
        name: "set_tool_enabled",
        request: "SetEnabledRequest",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "shared self operation persists the toggle",
    },
    CommandContract {
        name: "open_skills_dir",
        request: "-",
        response: "string",
        boundary: CommandBoundary::Execute,
        security: "configured skills root only",
    },
    CommandContract {
        name: "execute_skill",
        request: "ExecuteSkillRequest",
        response: "SkillExecutionResponse",
        boundary: CommandBoundary::Execute,
        security: "qualified skill name and params pass SafetyGateway",
    },
    CommandContract {
        name: "get_tools",
        request: "-",
        response: "ToolListResponse",
        boundary: CommandBoundary::Read,
        security: "tool definition projection; schemas are dynamic extension data",
    },
    CommandContract {
        name: "reset_tool_circuits",
        request: "-",
        response: "()",
        boundary: CommandBoundary::Mutate,
        security: "clears local circuit state only",
    },
];

/// Responses whose outer shape was historically assembled as JSON but is now
/// a named DTO. The nested `Value` remains deliberate because MCP/skill output
/// is provider/tool-defined extension data rather than a stable Haven shape.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpToolCallResponse {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillExecutionResponse {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

/// Stable Tauri memory-recall item. It mirrors the typed `haven_memory` hit
/// while keeping the existing wire shape and field names.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryRecallItem {
    pub entity_id: String,
    pub text: String,
    pub score: f64,
    pub model: String,
}

impl From<haven_memory::MemoryHit> for MemoryRecallItem {
    fn from(hit: haven_memory::MemoryHit) -> Self {
        Self {
            entity_id: hit.entity_id,
            text: hit.text,
            score: hit.score,
            model: hit.model,
        }
    }
}

/// The fixed part of a builtin tool listing. `input_schema` is intentionally
/// dynamic JSON because it is the tool/provider extension point.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolInfoResponse {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub risk_level: haven_common::types::RiskLevel,
    pub enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolListResponse {
    pub tools: Vec<ToolInfoResponse>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_registry_is_unique_and_covers_the_current_handler_set() {
        assert_eq!(IPC_CONTRACT_VERSION, 1);
        assert_eq!(COMMAND_CONTRACTS.len(), 67);
        let names: HashSet<_> = COMMAND_CONTRACTS
            .iter()
            .map(|contract| contract.name)
            .collect();
        assert_eq!(names.len(), COMMAND_CONTRACTS.len());
        assert!(names.contains(&"mcp_tool_call"));
        assert!(names.contains(&"execute_skill"));
        assert!(names.contains(&"resolve_confirmation"));
    }

    #[test]
    fn dynamic_execution_payloads_have_fixed_outer_shape() {
        let mcp = serde_json::to_value(McpToolCallResponse {
            success: true,
            output: serde_json::json!({"value": 1}),
            error: None,
        })
        .unwrap();
        assert_eq!(
            mcp,
            serde_json::json!({"success": true, "output": {"value": 1}, "error": null})
        );
        let skill = serde_json::to_value(SkillExecutionResponse {
            success: false,
            output: serde_json::json!(null),
            error: Some("blocked".into()),
        })
        .unwrap();
        assert_eq!(skill["error"], "blocked");
    }
}
