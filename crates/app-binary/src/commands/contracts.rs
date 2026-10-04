//! The single registry for the Tauri command wire contract.
//!
//! Command arguments intentionally remain flat because Tauri maps the
//! renderer's camelCase object onto the Rust function arguments. This registry
//! stores only command names and reviewed boundary/security metadata; handler
//! signatures and Serde DTOs are the IPC shape authority.

/// Version of the public Tauri command directory.
pub const IPC_CONTRACT_VERSION: u16 = 1;
pub const EXPECTED_COMMAND_COUNT: usize = 79;

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
        boundary: CommandBoundary::Read,
        security: "projected task fields only",
    },
    CommandContract {
        name: "cancel_action",
        boundary: CommandBoundary::Mutate,
        security: "kind is enum; cancel only the selected task kind",
    },
    CommandContract {
        name: "list_action_history",
        boundary: CommandBoundary::Read,
        security: "optional session filter; limit capped at 200; internal tool args excluded",
    },
    CommandContract {
        name: "delete_action",
        boundary: CommandBoundary::Mutate,
        security: "delete one persisted task row by id",
    },
    // external
    CommandContract {
        name: "open_external",
        boundary: CommandBoundary::Execute,
        security: "http(s) or validated absolute local path only",
    },
    // history
    CommandContract {
        name: "get_history",
        boundary: CommandBoundary::Read,
        security: "read-only session projection",
    },
    CommandContract {
        name: "count_history",
        boundary: CommandBoundary::Read,
        security: "read-only aggregate",
    },
    CommandContract {
        name: "search_history_paginated",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "count_history_search",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "search_history",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "search_history_filtered",
        boundary: CommandBoundary::Read,
        security: "bounded page and date-filtered projection",
    },
    CommandContract {
        name: "export_history",
        boundary: CommandBoundary::Read,
        security: "export contains persisted history only",
    },
    // log
    CommandContract {
        name: "get_log_info",
        boundary: CommandBoundary::Read,
        security: "path is optional; no environment details",
    },
    CommandContract {
        name: "read_log_tail",
        boundary: CommandBoundary::Read,
        security: "bounded tail; file logging must be enabled",
    },
    CommandContract {
        name: "log_frontend_error",
        boundary: CommandBoundary::Mutate,
        security: "sanitized user-visible error mirrored into the backend log",
    },
    // diagnostics
    CommandContract {
        name: "get_performance_metrics",
        boundary: CommandBoundary::Read,
        security: "bounded content-free backend counters plus renderer stream counters",
    },
    // mcp
    CommandContract {
        name: "list_mcp_tools",
        boundary: CommandBoundary::Read,
        security: "snapshot only; env values redacted; invocation remains gated",
    },
    CommandContract {
        name: "reconnect_mcp",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; typed native operation reconnects one existing configured server after final version check",
    },
    CommandContract {
        name: "refresh_mcp_servers",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; one batch over persisted config diff and its affected targets; no renderer process arguments",
    },
    CommandContract {
        name: "mcp_tool_call",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; confirmation queues direct calls and errors are renderer-safe",
    },
    CommandContract {
        name: "add_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; typed native admin operation validates and persists config",
    },
    CommandContract {
        name: "update_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; typed native admin operation validates and reconnects safely",
    },
    CommandContract {
        name: "remove_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; typed native admin operation removes client and config",
    },
    CommandContract {
        name: "toggle_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; typed native admin operation connects before enabling",
    },
    // memory
    CommandContract {
        name: "run_memory_maintenance",
        boundary: CommandBoundary::Mutate,
        security: "maintenance path owns purge and embedding cleanup",
    },
    CommandContract {
        name: "recall_memory",
        boundary: CommandBoundary::Read,
        security: "bounded and credential-filtered recall",
    },
    CommandContract {
        name: "list_facts",
        boundary: CommandBoundary::Read,
        security: "read-only fact projection",
    },
    CommandContract {
        name: "add_fact",
        boundary: CommandBoundary::Mutate,
        security: "credential-like predicates and values rejected",
    },
    CommandContract {
        name: "delete_fact",
        boundary: CommandBoundary::Mutate,
        security: "delete one fact by id",
    },
    // model
    CommandContract {
        name: "get_api_key_status",
        boundary: CommandBoundary::Read,
        security: "boolean presence only; credentials excluded",
    },
    CommandContract {
        name: "check_llm_connection",
        boundary: CommandBoundary::Read,
        security: "status and non-sensitive reason only; no endpoint or provider payload",
    },
    CommandContract {
        name: "discover_models",
        boundary: CommandBoundary::Execute,
        security: "http(s) endpoint; typed auth scheme for an explicitly entered key; stored keys require a matching configured endpoint",
    },
    CommandContract {
        name: "discover_all_models",
        boundary: CommandBoundary::Execute,
        security: "only configured providers are queried",
    },
    CommandContract {
        name: "switch_model",
        boundary: CommandBoundary::Mutate,
        security: "role is a RequestKind; modelId is an assigned, capability-compatible ModelConfig id",
    },
    CommandContract {
        name: "set_reasoning_effort",
        boundary: CommandBoundary::Mutate,
        security: "model id or RequestKind selector validated before config save",
    },
    CommandContract {
        name: "set_web_search",
        boundary: CommandBoundary::Mutate,
        security: "provider capability checked before config save",
    },
    // recording
    CommandContract {
        name: "get_recording_state",
        boundary: CommandBoundary::Read,
        security: "state only; no device or provider detail",
    },
    CommandContract {
        name: "set_hotkey_capture_active",
        boundary: CommandBoundary::Mutate,
        security: "transient renderer key-capture state only; not persisted",
    },
    CommandContract {
        name: "start_recording",
        boundary: CommandBoundary::Execute,
        security: "input pipeline owns capture lifecycle",
    },
    CommandContract {
        name: "stop_recording",
        boundary: CommandBoundary::Execute,
        security: "capture stops before asynchronous transcription",
    },
    CommandContract {
        name: "cancel_recording",
        boundary: CommandBoundary::Execute,
        security: "cancel clears the in-flight recording id",
    },
    CommandContract {
        name: "process_transcript",
        boundary: CommandBoundary::Execute,
        security: "attachment limits and file persistence are enforced",
    },
    // session
    CommandContract {
        name: "reopen_session",
        boundary: CommandBoundary::Mutate,
        security: "session id selects persisted session",
    },
    CommandContract {
        name: "get_sessions",
        boundary: CommandBoundary::Read,
        security: "active session projection",
    },
    CommandContract {
        name: "get_session_lineage",
        boundary: CommandBoundary::Read,
        security: "parent and direct children of the selected session only",
    },
    CommandContract {
        name: "end_session",
        boundary: CommandBoundary::Mutate,
        security: "explicit user termination",
    },
    CommandContract {
        name: "interrupt_session",
        boundary: CommandBoundary::Mutate,
        security: "pauses the selected active session without deleting it",
    },
    CommandContract {
        name: "resolve_confirmation",
        boundary: CommandBoundary::Mutate,
        security: "owner/request id selects one registry; receipt, effect, scope, target and expiry are revalidated; retryable failures keep pending",
    },
    CommandContract {
        name: "update_session_title",
        boundary: CommandBoundary::Mutate,
        security: "trimmed non-empty title only",
    },
    CommandContract {
        name: "delete_session",
        boundary: CommandBoundary::Mutate,
        security: "delete by session id and release runtime state",
    },
    CommandContract {
        name: "clear_history",
        boundary: CommandBoundary::Mutate,
        security: "clears persisted sessions and session trust",
    },
    CommandContract {
        name: "rollback_session",
        boundary: CommandBoundary::Mutate,
        security: "event cursor and projection clock rollback",
    },
    CommandContract {
        name: "continue_session",
        boundary: CommandBoundary::Mutate,
        security: "resume from saved error snapshot",
    },
    CommandContract {
        name: "get_session_for_resume",
        boundary: CommandBoundary::Read,
        security: "session-scoped persisted projection",
    },
    CommandContract {
        name: "get_last_conversation",
        boundary: CommandBoundary::Read,
        security: "most recent persisted session only",
    },
    // settings
    CommandContract {
        name: "get_settings",
        boundary: CommandBoundary::Read,
        security: "config response redacts credentials and MCP environment values",
    },
    CommandContract {
        name: "stage_provider_credential",
        boundary: CommandBoundary::Mutate,
        security: "writes provider secret to secure storage and returns only an opaque reference",
    },
    CommandContract {
        name: "stage_ocr_credential",
        boundary: CommandBoundary::Mutate,
        security: "writes OCR secret to secure storage and returns only an opaque reference",
    },
    CommandContract {
        name: "discard_staged_credentials",
        boundary: CommandBoundary::Mutate,
        security: "deletes staged values not committed by a Settings save",
    },
    CommandContract {
        name: "get_bootstrap_status",
        boundary: CommandBoundary::Read,
        security: "status enum only",
    },
    CommandContract {
        name: "update_settings",
        boundary: CommandBoundary::Mutate,
        security: "rejects inline secrets; shared loader preserves credential refs, tool sections, and permission rules",
    },
    CommandContract {
        name: "list_permissions",
        boundary: CommandBoundary::Read,
        security: "permission keys/effects only",
    },
    CommandContract {
        name: "list_session_permissions",
        boundary: CommandBoundary::Read,
        security: "typed session grants with exact session, capability, target, and effect",
    },
    CommandContract {
        name: "revoke_permission",
        boundary: CommandBoundary::Mutate,
        security: "non-empty exact permanent key; session grants are retained",
    },
    CommandContract {
        name: "revoke_session_permission",
        boundary: CommandBoundary::Mutate,
        security: "non-empty session id and capability; removes one session grant",
    },
    CommandContract {
        name: "reset_permissions",
        boundary: CommandBoundary::Mutate,
        security: "clears permanent rules only; keeps session grants and selected default policy",
    },
    CommandContract {
        name: "reset_session_permissions",
        boundary: CommandBoundary::Mutate,
        security: "clears durable session grants only; keeps permanent rules",
    },
    CommandContract {
        name: "check_shell_available",
        boundary: CommandBoundary::Read,
        security: "availability boolean only",
    },
    CommandContract {
        name: "enable_autostart",
        boundary: CommandBoundary::Execute,
        security: "release build only",
    },
    CommandContract {
        name: "disable_autostart",
        boundary: CommandBoundary::Execute,
        security: "managed autostart entry only",
    },
    CommandContract {
        name: "is_autostart_enabled",
        boundary: CommandBoundary::Read,
        security: "state boolean only",
    },
    // skills/tools
    CommandContract {
        name: "list_skills",
        boundary: CommandBoundary::Read,
        security: "metadata projection",
    },
    CommandContract {
        name: "refresh_skills",
        boundary: CommandBoundary::Execute,
        security: "configured skills root scan",
    },
    CommandContract {
        name: "set_skill_enabled",
        boundary: CommandBoundary::Mutate,
        security: "AuthorizationEngine; typed native admin operation persists the toggle",
    },
    CommandContract {
        name: "set_tool_enabled",
        boundary: CommandBoundary::Mutate,
        security: "AuthorizationEngine; typed native admin operation persists the toggle",
    },
    CommandContract {
        name: "open_skills_dir",
        boundary: CommandBoundary::Execute,
        security: "configured skills root only",
    },
    CommandContract {
        name: "execute_skill",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationEngine; confirmation queues direct calls and errors are renderer-safe",
    },
    CommandContract {
        name: "get_tools",
        boundary: CommandBoundary::Read,
        security: "tool definition projection; schemas are dynamic extension data",
    },
    CommandContract {
        name: "reset_tool_circuits",
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolListResponse {
    pub tools: Vec<haven_common::tools::ToolManifest>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_registry_is_unique_and_covers_the_current_handler_set() {
        assert_eq!(IPC_CONTRACT_VERSION, 1);
        assert_eq!(COMMAND_CONTRACTS.len(), EXPECTED_COMMAND_COUNT);
        let names: HashSet<_> = COMMAND_CONTRACTS
            .iter()
            .map(|contract| contract.name)
            .collect();
        assert_eq!(names.len(), COMMAND_CONTRACTS.len());
        assert!(names.contains(&"mcp_tool_call"));
        assert!(names.contains(&"execute_skill"));
        assert!(names.contains(&"resolve_confirmation"));
        assert!(names.contains(&"reset_permissions"));
        assert!(names.contains(&"list_session_permissions"));
        assert!(names.contains(&"revoke_session_permission"));
        assert!(names.contains(&"reset_session_permissions"));
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
