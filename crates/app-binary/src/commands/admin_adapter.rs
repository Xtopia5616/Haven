use crate::app_state::AppState;
use crate::commands::confirmation::UiConfirmationAction;
use crate::commands::{emit_event_logged, log_err, log_external_err, queue_ui_confirmation};
use std::future::Future;
use tauri::AppHandle;

/// These MCP operation paths can return server-controlled connection detail.
/// Keep that detail in the sanitized caller result while logging a fixed category.
pub(super) fn admin_operation_failure_kind(
    request: &haven_tools::AdminRequest,
) -> Option<&'static str> {
    use haven_tools::{AdminRequest, McpOperationArgs, NativeMcpOperationArgs};

    match request {
        AdminRequest::Mcp(
            McpOperationArgs::McpConnect { .. }
            | McpOperationArgs::McpAdd { .. }
            | McpOperationArgs::McpUpdate { .. }
            | McpOperationArgs::McpToggle { .. }
            | McpOperationArgs::McpReload,
        )
        | AdminRequest::NativeMcp(
            NativeMcpOperationArgs::McpReconnect { .. } | NativeMcpOperationArgs::McpRefresh { .. },
        ) => Some("mcp_operation"),
        _ => None,
    }
}

fn log_admin_operation_error(
    ctx: &str,
    failure_kind: Option<&'static str>,
    error: impl std::fmt::Display,
) -> String {
    match failure_kind {
        Some(failure_kind) => log_external_err(ctx, failure_kind, error),
        None => log_err(ctx, error),
    }
}

/// Execute one native admin request through the typed operation that owns its
/// capability. This helper is intentionally separate from authorization so a
/// queued UI confirmation can resume the exact same request.
pub(crate) async fn execute_admin_operation(
    state: &AppState,
    ctx: &str,
    request: haven_tools::AdminRequest,
) -> Result<haven_tools::ToolResult, String> {
    let external_failure_kind = admin_operation_failure_kind(&request);
    let admin_operations = state
        .runtime
        .tools
        .admin_operations()
        .await
        .ok_or_else(|| log_err(ctx, "admin operations are not wired"))?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = admin_operations
        .execute(request, cancel)
        .await
        .map_err(|error| log_admin_operation_error(ctx, external_failure_kind, error))?;
    if !result.success {
        return Err(log_admin_operation_error(
            ctx,
            external_failure_kind,
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
    let admin_operations = state
        .runtime
        .tools
        .admin_operations()
        .await
        .ok_or_else(|| log_err(ctx, "admin operations are not wired"))?;
    let operation_name = request.model_operation_name();
    let tool_name = operation_name.to_string();
    let risk_level = admin_operations.risk_level(&request);
    let input = request.input();
    let network_access = request.network_access();
    let capability = haven_common::types::CapabilityScope::try_new(&tool_name)
        .map_err(|error| log_err(ctx, error))?;
    let policy =
        haven_tools::OperationPolicy::native(&tool_name, capability, risk_level, network_access);
    let authorization_request = app_command_authorization_request(&tool_name, input, policy);
    let decision = state
        .runtime
        .services
        .authorization
        .authorize(&authorization_request)
        .await;
    if let haven_tools::AuthorizationDecision::Blocked { reason, .. } = &decision {
        return Err(log_blocked_admin_request(ctx, reason));
    }
    let execute_request = request.clone();
    dispatch_authorized_admin_request(
        ctx,
        decision,
        || execute_admin_operation(state, ctx, execute_request),
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

pub(super) fn log_blocked_admin_request(ctx: &str, reason: &str) -> String {
    log_err(
        ctx,
        format!("native admin operation blocked by security policy ({reason})"),
    )
}

/// Build an authorization request for a direct app command. These operations
/// have no owning session, so they must not read session-scoped grants.
pub(crate) fn app_command_authorization_request(
    tool_name: impl Into<String>,
    input: serde_json::Value,
    policy: haven_tools::OperationPolicy,
) -> haven_tools::AuthorizationRequest {
    haven_tools::AuthorizationRequest::new(None, tool_name, input, policy)
}

pub(super) async fn dispatch_authorized_admin_request<E, EFut, Q, QFut>(
    ctx: &str,
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
        haven_tools::AuthorizationDecision::Blocked { reason, .. } => {
            Err(log_blocked_admin_request(ctx, &reason))
        }
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
                    op: crate::events::SkillsStatusOperation::Toggle,
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
                let connected = state.runtime.services.mcp.has_client(name).await;
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
            let status = state.runtime.services.mcp.client_status(name).await;
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

pub(super) fn parse_mcp_refresh_failed_names(
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
