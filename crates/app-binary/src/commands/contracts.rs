//! The single registry for the Tauri command wire contract.
//!
//! Command arguments intentionally remain flat because Tauri maps the
//! renderer's camelCase object onto the Rust function arguments. This registry
//! stores only command names and reviewed boundary/security metadata; handler
//! signatures and Serde DTOs are the IPC shape authority.

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandBoundary {
    /// Read-only application state or diagnostics.
    Read,
    /// User-initiated state mutation.
    Mutate,
    /// A command that can execute a local, external, or otherwise privileged
    /// capability and therefore must retain the security gateway boundary.
    Execute,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandContract {
    pub(crate) name: &'static str,
    pub(crate) boundary: CommandBoundary,
    /// Short security invariant; detailed negative cases live in the tools
    /// security regression matrix.
    pub(crate) security: &'static str,
}

/// Every command passed to `tauri::generate_handler!` must have exactly one
/// entry here.  Keep this list in the same domain order as `commands/mod.rs`
/// and `lib.rs` so review can compare registration and documentation easily.
#[cfg(test)]
pub(crate) const COMMAND_CONTRACTS: &[CommandContract] = &[
    // tool_run
    CommandContract {
        name: "list_tool_runs",
        boundary: CommandBoundary::Read,
        security: "projected task fields only",
    },
    CommandContract {
        name: "cancel_tool_run",
        boundary: CommandBoundary::Mutate,
        security: "only a canonical toolrun ID can cancel background tool execution",
    },
    CommandContract {
        name: "list_tool_run_history",
        boundary: CommandBoundary::Read,
        security: "optional session filter; limit capped at 200; internal tool args excluded",
    },
    CommandContract {
        name: "clear_tool_run_history",
        boundary: CommandBoundary::Mutate,
        security: "delete terminal task history; preserve live work and undelivered results",
    },
    CommandContract {
        name: "list_schedules",
        boundary: CommandBoundary::Read,
        security: "safe projection excludes dynamic arguments and continuation prompt",
    },
    CommandContract {
        name: "cancel_schedule",
        boundary: CommandBoundary::Mutate,
        security: "canonical schedule and owner-session IDs; owner-scoped cancellation",
    },
    // external
    CommandContract {
        name: "open_external",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; queues AppCommand confirmation; http(s) or validated absolute local path only",
    },
    // session_history
    CommandContract {
        name: "list_session_history",
        boundary: CommandBoundary::Read,
        security: "read-only session projection",
    },
    CommandContract {
        name: "count_session_history",
        boundary: CommandBoundary::Read,
        security: "read-only aggregate",
    },
    CommandContract {
        name: "count_session_history_search",
        boundary: CommandBoundary::Read,
        security: "parameterized read-only search",
    },
    CommandContract {
        name: "search_session_history_filtered",
        boundary: CommandBoundary::Read,
        security: "bounded page and date-filtered projection",
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
        security: "fixed renderer failure category only; error text and payload are not mirrored",
    },
    // diagnostics
    CommandContract {
        name: "get_performance_metrics",
        boundary: CommandBoundary::Read,
        security: "bounded content-free backend counters plus renderer stream counters",
    },
    // mcp
    CommandContract {
        name: "list_mcp_servers",
        boundary: CommandBoundary::Read,
        security: "snapshot only; env values redacted; invocation remains gated",
    },
    CommandContract {
        name: "list_mcp_resources",
        boundary: CommandBoundary::Execute,
        security: "explicit host discovery; enabled live target binding; bounded opaque resource metadata",
    },
    CommandContract {
        name: "list_mcp_resource_templates",
        boundary: CommandBoundary::Execute,
        security: "user-driven template discovery; enabled live target binding; bounded opaque URI templates",
    },
    CommandContract {
        name: "read_mcp_resource",
        boundary: CommandBoundary::Execute,
        security: "explicit UI selection; enabled live target binding; opaque URI; bounded untrusted content",
    },
    CommandContract {
        name: "subscribe_mcp_resource",
        boundary: CommandBoundary::Execute,
        security: "explicit UI selection from current resource list; enabled live target binding; bounded acknowledged subscription",
    },
    CommandContract {
        name: "unsubscribe_mcp_resource",
        boundary: CommandBoundary::Execute,
        security: "explicit UI cancellation; enabled live target binding; cancels only the selected resource stream",
    },
    CommandContract {
        name: "respond_mcp_feature_input",
        boundary: CommandBoundary::Execute,
        security: "explicit user response to a bounded form request; enabled live target binding; schema-validated answers",
    },
    CommandContract {
        name: "cancel_mcp_feature_input",
        boundary: CommandBoundary::Execute,
        security: "explicit UI cancellation of one pending selected resource or prompt interaction",
    },
    CommandContract {
        name: "list_mcp_prompts",
        boundary: CommandBoundary::Execute,
        security: "user-driven prompt discovery; enabled live target binding; no model callable surface",
    },
    CommandContract {
        name: "get_mcp_prompt",
        boundary: CommandBoundary::Execute,
        security: "explicit user-selected prompt and declared arguments; enabled live target binding; bounded untrusted messages",
    },
    CommandContract {
        name: "reconnect_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; typed native operation reconnects one existing configured server after final version check",
    },
    CommandContract {
        name: "refresh_mcp_servers",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; one batch over persisted config diff and its affected targets; no renderer process arguments",
    },
    CommandContract {
        name: "add_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; typed native admin operation validates and persists config",
    },
    CommandContract {
        name: "update_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; typed native admin operation validates and reconnects safely",
    },
    CommandContract {
        name: "remove_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; typed native admin operation removes client and config",
    },
    CommandContract {
        name: "toggle_mcp_server",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; typed native admin operation connects before enabling",
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
    CommandContract {
        name: "clear_facts",
        boundary: CommandBoundary::Mutate,
        security: "delete all saved long-term facts and invalidate derived caches",
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
        name: "switch_model",
        boundary: CommandBoundary::Mutate,
        security: "requestKind selects the route; modelConfigId is an assigned, capability-compatible Haven ModelConfig identity, not a Provider service model ID",
    },
    CommandContract {
        name: "set_reasoning_effort",
        boundary: CommandBoundary::Mutate,
        security: "request kind selects the assigned model; validated before config save",
    },
    CommandContract {
        name: "set_web_search",
        boundary: CommandBoundary::Mutate,
        security: "request kind selects the assigned model; provider capability checked before config save",
    },
    // recording
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
        name: "submit_transcript",
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
        name: "list_runtime_sessions",
        boundary: CommandBoundary::Read,
        security: "resident nonterminal runtime sessions only",
    },
    CommandContract {
        name: "get_session_lineage",
        boundary: CommandBoundary::Read,
        security: "parent and direct child sessions of the selected session only",
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
        name: "delete_all_sessions",
        boundary: CommandBoundary::Mutate,
        security: "clears persisted sessions and session trust",
    },
    CommandContract {
        name: "rollback_session",
        boundary: CommandBoundary::Mutate,
        security: "event cursor and projection clock rollback",
    },
    CommandContract {
        name: "fork_session_from_message",
        boundary: CommandBoundary::Mutate,
        security: "canonical parent and user message ids; creates an independent re-keyed transcript prefix",
    },
    CommandContract {
        name: "continue_session",
        boundary: CommandBoundary::Mutate,
        security: "replay durable session_events and truncate partial projection",
    },
    CommandContract {
        name: "get_session_for_resume",
        boundary: CommandBoundary::Read,
        security: "session-scoped persisted projection",
    },
    CommandContract {
        name: "get_latest_session_for_resume",
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
        name: "pick_execution_workspace_directory",
        boundary: CommandBoundary::Read,
        security: "user-selected local directory candidate only; validates but grants no file access",
    },
    CommandContract {
        name: "stage_provider_credential",
        boundary: CommandBoundary::Mutate,
        security: "writes provider secret to secure storage and returns only an opaque reference",
    },
    CommandContract {
        name: "start_chatgpt_plan_login",
        boundary: CommandBoundary::Execute,
        security: "starts a state- and PKCE-bound loopback OAuth callback and returns only the official authorization URL",
    },
    CommandContract {
        name: "complete_chatgpt_plan_login",
        boundary: CommandBoundary::Mutate,
        security: "validates OAuth state, issuer, audience, nonce and signature; stores token and granted-scope state only in secure storage",
    },
    CommandContract {
        name: "cancel_chatgpt_plan_login",
        boundary: CommandBoundary::Mutate,
        security: "cancels one in-memory OAuth attempt and aborts its loopback callback listener",
    },
    CommandContract {
        name: "discard_staged_chatgpt_plan_credential",
        boundary: CommandBoundary::Mutate,
        security: "deletes only the exact uncommitted ChatGPT plan credential reference abandoned by the settings draft",
    },
    CommandContract {
        name: "reassign_staged_chatgpt_plan_credential",
        boundary: CommandBoundary::Mutate,
        security: "moves one exact staged ChatGPT credential reference to another ChatGPT plan provider name without exposing its secret",
    },
    CommandContract {
        name: "stage_independent_search_credential",
        boundary: CommandBoundary::Mutate,
        security: "writes a model's Alibaba AI Search key to secure storage and returns only an opaque reference",
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
    // skills
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
        security: "AuthorizationPort; typed native admin operation persists the toggle",
    },
    CommandContract {
        name: "open_skills_dir",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; queues AppCommand confirmation; configured skills root only",
    },
    CommandContract {
        name: "execute_skill",
        boundary: CommandBoundary::Execute,
        security: "AuthorizationPort; confirmation queues direct calls and errors are renderer-safe",
    },
    // tools
    CommandContract {
        name: "list_builtin_tool_manifests",
        boundary: CommandBoundary::Read,
        security: "builtin manifest projection; schemas are dynamic extension data",
    },
    CommandContract {
        name: "set_tool_enabled",
        boundary: CommandBoundary::Mutate,
        security: "AuthorizationPort; typed native admin operation persists the toggle",
    },
    CommandContract {
        name: "reset_tool_circuits",
        boundary: CommandBoundary::Mutate,
        security: "clears local circuit state only",
    },
    CommandContract {
        name: "promote_sandbox_output",
        boundary: CommandBoundary::Execute,
        security: "Windows only; owner-held change index; exact session request; required one-time confirmation; current Files capability and live execution proof",
    },
    CommandContract {
        name: "get_sandbox_output_review",
        boundary: CommandBoundary::Read,
        security: "canonical session/step pair; only completed Shell steps; bounded owner manifest without file content or absolute source paths",
    },
    CommandContract {
        name: "discard_sandbox_output_review",
        boundary: CommandBoundary::Mutate,
        security: "canonical session/step pair; only the owning Shell review can be discarded",
    },
    CommandContract {
        name: "pick_sandbox_output_destination",
        boundary: CommandBoundary::Read,
        security: "native save picker; returns a user-selected candidate only and grants no filesystem access",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_registry_is_unique() {
        let names: HashSet<_> = COMMAND_CONTRACTS
            .iter()
            .map(|contract| contract.name)
            .collect();
        assert_eq!(names.len(), COMMAND_CONTRACTS.len());
        assert!(names.contains(&"execute_skill"));
        assert!(names.contains(&"resolve_confirmation"));
        assert!(names.contains(&"reset_permissions"));
        assert!(names.contains(&"list_session_permissions"));
        assert!(names.contains(&"revoke_session_permission"));
        assert!(names.contains(&"reset_session_permissions"));
        assert!(names.contains(&"get_sandbox_output_review"));
        assert!(names.contains(&"discard_sandbox_output_review"));
        assert!(names.contains(&"pick_sandbox_output_destination"));
        assert!(names.contains(&"promote_sandbox_output"));
    }
}
