//! Tauri command handlers, split by domain:
//! `recording` / `session` / `action` / `history` / `model` / `mcp` /
//! `skills` / `memory` / `settings` / `log` / `diagnostics`.
//!
//! Shared helpers (error conversion, router hot-swap, MCP connect, attachment
//! validation) live here so every submodule stays thin. `lib.rs` references
//! the submodule paths directly (`commands::recording::start_recording`, …)
//! because `generate_handler!` resolves each command's `__cmd__` symbol next
//! to its definition module.

pub mod action;
pub mod contracts;
pub mod diagnostics;
pub mod external;
pub mod history;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod recording;
pub mod session;
pub mod settings;
pub mod skills;

use crate::app_state::{AppState, UiConfirmationAction, UiConfirmationPending};
use crate::events::{INTERACTION_REQUESTED_EVENT, LLM_CONFIG_CHANGED_EVENT};
use crate::logging::sanitize_error_text;
use serde::Serialize;
use std::future::Future;
use tauri::AppHandle;
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
    begin_recording_session, emit_recording_error, emit_recording_started, emit_recording_stopped,
    finalize_transcription, recording_reason_str,
};

#[derive(Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<haven_agent::SessionInfo>,
}

/// Re-export: command error logging lives in `crate::logging` (conventions §1).
pub(crate) use crate::logging::log_err;
pub(crate) use crate::logging::log_storage_err;

/// Execute one native admin request through the typed operation that owns its
/// capability. This helper is intentionally separate from authorization so a
/// queued UI confirmation can resume the exact same request.
pub(crate) async fn execute_admin_surface(
    state: &AppState,
    ctx: &str,
    request: haven_tools::AdminRequest,
) -> Result<haven_tools::ToolResult, String> {
    let admin_surfaces = state
        .tools
        .admin_surfaces()
        .await
        .ok_or_else(|| log_err(ctx, "admin surfaces are not wired"))?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = admin_surfaces
        .execute(request, cancel)
        .await
        .map_err(|e| log_err(ctx, e))?;
    if !result.success {
        return Err(log_err(
            ctx,
            result
                .error
                .unwrap_or_else(|| "admin operation failed".into()),
        ));
    }
    Ok(result)
}

/// Authorize one native typed admin request and execute it, or place the typed
/// request in the UI confirmation queue. The operation metadata is read from
/// the same typed implementation used by the provider adapter.
pub(crate) async fn authorize_admin_request(
    state: &AppState,
    app: &AppHandle,
    ctx: &str,
    request: haven_tools::AdminRequest,
) -> Result<haven_tools::ToolResult, String> {
    let admin_surfaces = state
        .tools
        .admin_surfaces()
        .await
        .ok_or_else(|| log_err(ctx, "admin surfaces are not wired"))?;
    let operation_name = request.model_operation_name();
    let metadata = admin_surfaces.metadata(&request);
    let tool_name = operation_name.to_string();
    let risk_level = metadata.risk_level;
    let input = request.input();
    let network_access = request.network_access();
    let policy = haven_tools::OperationPolicy::native(
        &tool_name,
        tool_name.clone().into(),
        risk_level,
        network_access,
    );
    let authorization_request =
        haven_tools::AuthorizationRequest::new(Some("ui"), &tool_name, input, policy);
    let decision = state
        .services
        .authorization
        .authorize(&authorization_request)
        .await;
    let execute_request = request.clone();
    dispatch_authorized_admin_request(
        decision,
        || execute_admin_surface(state, ctx, execute_request),
        |receipt| async move {
            queue_ui_confirmation(
                state,
                app,
                authorization_request,
                receipt,
                UiConfirmationAction::Admin {
                    request: Box::new(request),
                },
            )
            .await
        },
    )
    .await
}

async fn dispatch_authorized_admin_request<E, EFut, Q, QFut>(
    decision: haven_tools::AuthorizationDecision,
    execute: E,
    queue_confirmation: Q,
) -> Result<haven_tools::ToolResult, String>
where
    E: FnOnce() -> EFut,
    EFut: Future<Output = Result<haven_tools::ToolResult, String>>,
    Q: FnOnce(haven_tools::ConfirmationReceipt) -> QFut,
    QFut: Future<Output = Result<String, String>>,
{
    match decision {
        haven_tools::AuthorizationDecision::AutoApproved => execute().await,
        haven_tools::AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
            Err(queue_confirmation(receipt).await?)
        }
        haven_tools::AuthorizationDecision::Blocked { reason, .. } => Err(format!(
            "native admin operation blocked by security policy ({reason})"
        )),
    }
}

/// Apply the app-shell side effects that normally follow a native admin
/// command after a queued confirmation resumes it. The structured admin
/// operation owns persistence and live state; this bridge owns catalog/event
/// refresh so a delayed confirmation updates the same UI surfaces as an
/// immediately approved command.
pub(crate) async fn finalize_admin_ui_operation(
    state: &AppState,
    app: &AppHandle,
    request: &haven_tools::AdminRequest,
) -> Result<(), String> {
    use haven_tools::{
        McpOperationArgs, NativeMcpOperationArgs, SkillsOperationArgs, ToolsOperationArgs,
    };

    match request {
        haven_tools::AdminRequest::Skills(
            SkillsOperationArgs::SkillEnable { .. } | SkillsOperationArgs::SkillDisable { .. },
        ) => {
            emit_event_logged(
                app,
                crate::events::SKILLS_STATUS_CHANGED_EVENT,
                crate::events::SkillsStatusChangedEvent {
                    op: "toggle".into(),
                },
                "resolve_ui_confirmation skill",
            );
        }
        haven_tools::AdminRequest::Tools(
            ToolsOperationArgs::ToolEnable { .. } | ToolsOperationArgs::ToolDisable { .. },
        ) => {}
        haven_tools::AdminRequest::Mcp(
            McpOperationArgs::McpAdd { .. }
            | McpOperationArgs::McpUpdate { .. }
            | McpOperationArgs::McpToggle { .. }
            | McpOperationArgs::McpConnect { .. },
        ) => {
            if let Some(name) = request.server_name() {
                let connected = state.services.mcp.get_client(name).await.is_some();
                crate::commands::mcp::emit_mcp_status(
                    app,
                    name.to_string(),
                    if connected {
                        haven_tools::McpClientStatus::Connected
                    } else {
                        haven_tools::McpClientStatus::Disconnected
                    },
                    "resolve_ui_confirmation mcp",
                );
            }
        }
        haven_tools::AdminRequest::Mcp(
            McpOperationArgs::McpDisconnect { .. } | McpOperationArgs::McpRemove { .. },
        ) => {
            if let Some(name) = request.server_name() {
                crate::commands::mcp::emit_mcp_status(
                    app,
                    name.to_string(),
                    haven_tools::McpClientStatus::Disconnected,
                    "resolve_ui_confirmation mcp",
                );
            }
        }
        haven_tools::AdminRequest::Mcp(McpOperationArgs::McpReload) => {}
        haven_tools::AdminRequest::NativeMcp(NativeMcpOperationArgs::McpReconnect {
            name, ..
        }) => {
            let status = match state.services.mcp.get_client(name).await {
                Some(client) => client.status().await,
                None => haven_tools::McpClientStatus::Disconnected,
            };
            crate::commands::mcp::emit_mcp_status(
                app,
                name.clone(),
                status,
                "resolve_ui_confirmation mcp reconnect",
            );
        }
        haven_tools::AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh { .. }) => {}
        _ => {}
    }
    Ok(())
}

const MCP_REFRESH_FAILURE_STATUS_SUMMARY: &str = "MCP 连接失败，请检查服务器状态或配置";

/// Publish result details that are only relevant after a queued UI
/// confirmation resumes an admin operation. Immediate refresh calls return
/// their result DTO to ToolsView and keep their existing result notification.
pub(crate) async fn finalize_confirmed_admin_ui_operation(
    state: &AppState,
    app: &AppHandle,
    request: &haven_tools::AdminRequest,
    result: &haven_tools::ToolResult,
) -> Result<(), String> {
    use haven_tools::{AdminRequest, NativeMcpOperationArgs};

    finalize_admin_ui_operation(state, app, request).await?;
    let AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh { plan }) = request else {
        return Ok(());
    };
    if !result.success {
        return Ok(());
    }
    for name in parse_mcp_refresh_failed_names(&result.output, plan) {
        crate::commands::mcp::emit_mcp_status(
            app,
            name,
            haven_tools::McpClientStatus::Offline {
                error: MCP_REFRESH_FAILURE_STATUS_SUMMARY.into(),
            },
            "resolve_ui_confirmation mcp refresh",
        );
    }
    Ok(())
}

fn parse_mcp_refresh_failed_names(
    output: &serde_json::Value,
    plan: &haven_tools::McpRefreshPlan,
) -> Vec<String> {
    use haven_tools::McpRefreshAction;
    use std::collections::HashSet;

    let Some(failed_names) = output.get("failed").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let authorized_names: HashSet<&str> = plan
        .targets
        .iter()
        .filter(|target| {
            matches!(
                target.action,
                McpRefreshAction::Connect | McpRefreshAction::Reconnect
            )
        })
        .map(|target| target.name.as_str())
        .collect();
    let mut seen = HashSet::new();
    failed_names
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|name| {
            !name.trim().is_empty() && authorized_names.contains(name) && seen.insert(*name)
        })
        .map(str::to_owned)
        .collect()
}

/// Register a renderer-triggered MCP/skill invocation as the same canonical
/// confirmation interaction used by agent and scheduled confirmations. The
/// app-level store retains the typed execution payload needed after resolve;
/// only the renderer-safe projection crosses the Tauri boundary.
pub(crate) async fn queue_ui_confirmation(
    state: &AppState,
    app: &AppHandle,
    authorization_request: haven_tools::AuthorizationRequest,
    receipt: haven_tools::ConfirmationReceipt,
    action: UiConfirmationAction,
) -> Result<String, String> {
    let tool_name = authorization_request.tool_name.clone();
    let display_input =
        redact_mcp_admin_confirmation_input(&tool_name, authorization_request.input.clone());
    let summary = haven_tools::permission_prompt_summary(&tool_name, &display_input);
    let permission_key = authorization_request.policy.capability.to_string();
    let risk_level = receipt.effective_risk;
    let request = haven_agent::InteractionRequest::ui_confirm(
        tool_name,
        display_input,
        summary.clone(),
        receipt.clone(),
    );
    let request_id = request.id.clone();
    state.ui_confirmations.lock().await.insert(
        request_id.to_string(),
        UiConfirmationPending {
            request: request.clone(),
            session_id: "ui".into(),
            authorization_request,
            summary: summary.clone(),
            receipt: receipt.clone(),
            action,
        },
    );
    if let Err(error) = app.emit(
        INTERACTION_REQUESTED_EVENT,
        crate::bootstrap::project_interaction(&request),
    ) {
        state
            .ui_confirmations
            .lock()
            .await
            .remove(&request_id.to_string());
        return Err(log_err("queue_ui_confirmation", error));
    }
    serde_json::to_string(&serde_json::json!({
        "requires_confirmation": true,
        "request_id": request_id,
        "summary": summary,
        "permission_key": permission_key,
        "risk_level": risk_level,
    }))
    .map_err(|error| log_err("queue_ui_confirmation", error))
}

/// MCP environment values stay in the typed pending action so the approved
/// operation can execute, but confirmation summaries and interaction payloads
/// expose only variable names. Keep this at the renderer boundary so the
/// authorization receipt still binds the original input internally.
fn redact_mcp_admin_confirmation_input(
    tool_name: &str,
    mut input: serde_json::Value,
) -> serde_json::Value {
    if !matches!(tool_name, "haven.mcp.mcp_add" | "haven.mcp.mcp_update") {
        return input;
    }
    let Some(environment) = input
        .get_mut("env")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return input;
    };
    for entry in environment {
        let Some(raw) = entry.as_str() else {
            continue;
        };
        let Some((name, _)) = raw.split_once('=') else {
            continue;
        };
        *entry = serde_json::Value::String(format!("{name}=<redacted>"));
    }
    input
}

#[cfg(test)]
mod tests {
    use super::{
        dispatch_authorized_admin_request, parse_mcp_refresh_failed_names,
        redact_mcp_admin_confirmation_input,
    };
    use haven_common::types::{CapabilityScope, RiskLevel, new_id};
    use haven_tools::{
        AuthorizationDecision, AuthorizationReasonCode, ConfirmationReceipt, McpRefreshAction,
        McpRefreshPlan, McpRefreshTarget, ToolResult,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn receipt() -> ConfirmationReceipt {
        ConfirmationReceipt {
            confirmation_id: new_id("conf").into(),
            capability: CapabilityScope::try_new("haven.mcp.mcp_refresh").unwrap(),
            canonical_input_hash: "test-hash".into(),
            effective_risk: RiskLevel::Medium,
            policy_revision: 1,
            expires_at: u64::MAX,
        }
    }

    fn refresh_plan() -> McpRefreshPlan {
        McpRefreshPlan {
            config_version: 3,
            targets: vec![
                McpRefreshTarget {
                    name: "new-server".into(),
                    action: McpRefreshAction::Connect,
                },
                McpRefreshTarget {
                    name: "changed-server".into(),
                    action: McpRefreshAction::Reconnect,
                },
                McpRefreshTarget {
                    name: "removed-server".into(),
                    action: McpRefreshAction::Disconnect,
                },
            ],
        }
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
}
