//! Tauri command handlers, split by domain:
//! `recording` / `session` / `tool_runs` / `tools` / `session_history` /
//! `model` / `mcp` / `skills` / `memory` / `settings` / `log` / `diagnostics`.
//!
//! Shared helpers (error conversion, router hot-swap, MCP connect, attachment
//! validation) live here. Admin command adaptation and AppCommand confirmation
//! lifecycle have dedicated private modules. `lib.rs` references command
//! submodule paths directly because `generate_handler!` resolves each command's
//! `__cmd__` symbol next to its definition module.

pub(crate) mod admin_adapter;
pub(crate) mod confirmation;
pub mod contracts;
pub mod diagnostics;
pub mod external;
pub mod log;
pub(crate) mod managed_media;
pub mod mcp;
pub mod memory;
pub mod model;
pub(crate) mod process;
pub mod recording;
pub mod session;
pub mod session_history;
pub mod settings;
pub mod skills;
pub mod tool_runs;
pub mod tools;
pub mod workflows;

use crate::events::LLM_CONFIG_CHANGED_EVENT;
use haven_common::error::sanitize_error_text;
use serde::Serialize;
use tauri::Emitter;

/// Event name for "the LlmRouter was rebuilt / model config changed". The
/// frontend +layout listens to this to re-probe LLM connectivity immediately
/// instead of waiting for the next backoff-scheduled probe (which may be up
/// to 120s away during a failure-streak).
/// Emit [`LLM_CONFIG_CHANGED_EVENT`] so the frontend probes immediately after
/// a router hot-swap (settings save, model switch, …). Best-effort: a missing
/// renderer must never fail the command.
pub(crate) fn emit_llm_config_changed(app: &tauri::AppHandle) {
    emit_event_logged(app, LLM_CONFIG_CHANGED_EVENT, (), "llm_config_changed");
}

/// Emit a frontend event without turning a completed backend operation into a
/// false command failure. Tauri emit failures are still observable and are
/// sanitized because event payloads often contain provider-controlled text.
pub(crate) fn emit_event_logged<T: Serialize + Clone>(
    app: &tauri::AppHandle,
    event: &str,
    payload: T,
    context: &str,
) {
    if let Err(error) = app.emit(event, payload) {
        tracing::debug!(
            event,
            context,
            error = %sanitize_error_text(&error.to_string()),
            "failed to emit frontend event"
        );
    }
}

/// Recording helpers shared with the shell hotkey path in `lib.rs`.
pub(crate) use recording::{
    RecordingStopCompletion, RecordingStopShellUpdate, begin_recording, emit_recording_error,
    emit_recording_started, emit_recording_stopped, finish_recording_stop,
    schedule_recording_transcription,
};

#[derive(Serialize)]
pub struct RuntimeSessionListResponse {
    pub sessions: Vec<RuntimeSessionDto>,
}

/// App-owned allowlist for the runtime session switcher. Keep Agent summary,
/// step inputs/outputs, and runtime bookkeeping inside the backend.
#[derive(Serialize)]
pub struct RuntimeSessionDto {
    pub session_id: String,
    pub input_text: String,
    pub title: Option<String>,
    pub status: haven_common::lifecycle::SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<haven_common::lifecycle::SessionWaitingReason>,
}

impl From<haven_agent::SessionInfo> for RuntimeSessionDto {
    fn from(session: haven_agent::SessionInfo) -> Self {
        Self {
            session_id: session.session_id,
            input_text: session.input,
            title: session.title,
            status: session.status,
            waiting_reason: session.waiting_reason,
        }
    }
}

/// Re-export: command error logging lives in `crate::logging` (conventions §1).
pub(crate) use crate::logging::log_err;
pub(crate) use crate::logging::log_external_err;
pub(crate) use crate::logging::log_storage_err;
#[cfg(test)]
use admin_adapter::{
    admin_operation_failure_kind, dispatch_authorized_admin_request, log_blocked_admin_request,
    parse_mcp_refresh_failed_names,
};
pub(crate) use admin_adapter::{
    app_command_authorization_request, authorize_admin_request, execute_admin_operation,
    finalize_admin_ui_operation, finalize_confirmed_admin_ui_operation,
};
pub(crate) use confirmation::queue_ui_confirmation;
#[cfg(test)]
use confirmation::redact_mcp_admin_confirmation_input;

/// Reject non-canonical Haven entity IDs at the Tauri boundary before they
/// reach stores, actors, or side effects. The shared Common helper owns the
/// format; callers supply the entity prefix without its separator.
pub(crate) fn validate_command_id(id: &str, field_name: &str, prefix: &str) -> Result<(), String> {
    if haven_common::types::is_canonical_id(id, prefix) {
        Ok(())
    } else {
        Err(format!(
            "{field_name} must be a canonical {prefix}-{{uuid32}} ID"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        admin_operation_failure_kind, app_command_authorization_request,
        dispatch_authorized_admin_request, log_blocked_admin_request,
        parse_mcp_refresh_failed_names, redact_mcp_admin_confirmation_input, validate_command_id,
    };
    use haven_common::tools::NetworkAccess;
    use haven_common::types::{
        CapabilityScope, NetworkPolicy, PermissionEffect, PermissionScope, RiskLevel, SandboxMode,
    };
    use haven_tools::{
        AdminRequest, AuthorizationDecision, AuthorizationReasonCode, ConfirmationReceipt,
        McpOperationArgs, McpRefreshAction, McpRefreshPlan, OperationPolicy, ToolResult,
        ToolsFacade,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn command_id_validation_uses_the_canonical_entity_format() {
        assert!(
            validate_command_id("ses-0123456789abcdef0123456789abcdef", "session_id", "ses")
                .is_ok()
        );
        assert_eq!(
            validate_command_id("ses-0123", "session_id", "ses"),
            Err("session_id must be a canonical ses-{uuid32} ID".into())
        );
        assert_eq!(
            validate_command_id("ses-0123456789ABCDEF0123456789abcdef", "session_id", "ses"),
            Err("session_id must be a canonical ses-{uuid32} ID".into())
        );
    }

    fn receipt() -> ConfirmationReceipt {
        ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: CapabilityScope::try_new("haven.mcp.mcp_refresh").unwrap(),
            canonical_input_hash: "test-hash".into(),
            effective_risk: RiskLevel::Medium,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        }
    }

    #[tokio::test]
    async fn direct_app_command_authorization_has_no_session_grant_owner() {
        let tool_name = "mcp__test__write";
        let capability = CapabilityScope::try_new(tool_name).unwrap();
        let request = app_command_authorization_request(
            tool_name,
            json!({}),
            OperationPolicy::native(
                tool_name,
                capability.clone(),
                RiskLevel::High,
                NetworkAccess::Opaque,
            ),
        );
        assert_eq!(request.session_id, None);

        let authorization = ToolsFacade::new().share_services().authorization;
        authorization
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Ask)
            .await;
        authorization
            .grant(
                Some("ui"),
                capability,
                PermissionEffect::Allow,
                PermissionScope::Session,
            )
            .await;
        assert!(matches!(
            authorization.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    fn refresh_plan() -> McpRefreshPlan {
        McpRefreshPlan::new_for_test(
            3,
            vec![
                ("new-server".into(), McpRefreshAction::Connect),
                ("changed-server".into(), McpRefreshAction::Reconnect),
                ("removed-server".into(), McpRefreshAction::Disconnect),
            ],
        )
    }

    #[test]
    fn confirmed_refresh_failure_parser_extracts_authorized_failed_targets() {
        let names = parse_mcp_refresh_failed_names(
            &serde_json::json!({"failed": ["new-server", "changed-server"]}),
            &refresh_plan(),
        );
        assert_eq!(names, vec!["new-server", "changed-server"]);
    }

    #[test]
    fn confirmed_refresh_failure_parser_handles_empty_results() {
        let plan = refresh_plan();
        assert!(
            parse_mcp_refresh_failed_names(&serde_json::json!({"failed": []}), &plan).is_empty()
        );
        assert!(parse_mcp_refresh_failed_names(&serde_json::json!({}), &plan).is_empty());
    }

    #[test]
    fn confirmed_refresh_failure_parser_ignores_malformed_and_unauthorized_values() {
        let names = parse_mcp_refresh_failed_names(
            &serde_json::json!({"failed": ["new-server", "removed-server", "unknown", "", 7, null]}),
            &refresh_plan(),
        );
        assert_eq!(names, vec!["new-server"]);
        assert!(
            parse_mcp_refresh_failed_names(
                &serde_json::json!({"failed": "new-server"}),
                &refresh_plan()
            )
            .is_empty()
        );
        assert!(
            parse_mcp_refresh_failed_names(&serde_json::Value::Null, &refresh_plan()).is_empty()
        );
    }

    #[test]
    fn mcp_confirmation_input_redacts_values_but_keeps_names() {
        let input = serde_json::json!({
            "operation": "mcp_add",
            "name": "server",
            "env": ["TOKEN=secret-value", "EMPTY=", "INHERITED"]
        });
        let redacted = redact_mcp_admin_confirmation_input("haven.mcp.mcp_add", input);
        assert_eq!(
            redacted["env"],
            serde_json::json!(["TOKEN=<redacted>", "EMPTY=<redacted>", "INHERITED"])
        );
        assert!(!redacted.to_string().contains("secret-value"));
    }

    #[test]
    fn non_mcp_confirmation_input_is_unchanged() {
        let input = serde_json::json!({"env": ["TOKEN=secret-value"]});
        assert_eq!(
            redact_mcp_admin_confirmation_input("haven.skills.run", input.clone()),
            input
        );
    }

    #[tokio::test]
    async fn pending_or_blocked_admin_authorization_never_runs_connection_effects() {
        for decision in [
            AuthorizationDecision::RequiresConfirmation {
                capability: CapabilityScope::try_new("haven.mcp.mcp_refresh").unwrap(),
                risk_level: RiskLevel::Medium,
                receipt: receipt(),
                reason_code: AuthorizationReasonCode::SensitiveData,
            },
            AuthorizationDecision::Blocked {
                reason: "network disabled".into(),
                reason_code: AuthorizationReasonCode::NetworkPolicy,
            },
        ] {
            let should_queue = matches!(
                &decision,
                AuthorizationDecision::RequiresConfirmation { .. }
            );
            let executions = Arc::new(AtomicUsize::new(0));
            let queues = Arc::new(AtomicUsize::new(0));
            let executions_in_task = executions.clone();
            let queues_in_task = queues.clone();
            let result = dispatch_authorized_admin_request(
                "test_admin",
                decision,
                move || async move {
                    executions_in_task.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolResult::ok(serde_json::json!({"connected": true})))
                },
                move |_receipt| async move {
                    queues_in_task.fetch_add(1, Ordering::SeqCst);
                    Ok("confirmation pending".into())
                },
            )
            .await;

            assert!(result.is_err());
            assert_eq!(executions.load(Ordering::SeqCst), 0);
            if should_queue {
                assert_eq!(queues.load(Ordering::SeqCst), 1);
            } else {
                assert_eq!(queues.load(Ordering::SeqCst), 0);
            }
        }
    }

    #[test]
    fn blocked_admin_command_errors_use_the_sanitized_command_boundary() {
        let message = log_blocked_admin_request(
            "mcp_command",
            "network policy denied request; api_key=secret-value",
        );
        assert!(message.contains("network policy denied request"));
        assert!(!message.contains("secret-value"));
    }

    #[test]
    fn mcp_connection_admin_failures_use_external_error_logging() {
        let update = AdminRequest::Mcp(McpOperationArgs::McpUpdate {
            name: "server".into(),
            exposure: None,
            transport: None,
            stdio_execution: None,
            command: None,
            url: None,
            args: None,
            env: None,
            cwd: None,
            enabled: None,
        });
        assert_eq!(admin_operation_failure_kind(&update), Some("mcp_operation"));

        let list = AdminRequest::Mcp(McpOperationArgs::McpList);
        assert_eq!(admin_operation_failure_kind(&list), None);
    }
}
