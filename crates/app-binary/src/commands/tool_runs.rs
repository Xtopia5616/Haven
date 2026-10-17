use crate::app_state::AppState;
use crate::commands::{log_err, validate_command_id};
use crate::events::{ToolRunEvent, ToolRunKindDto};
use haven_tools::{ToolRunHistoryQuery, ToolRunKind};
use std::sync::Arc;
use tauri::State;

/// Board view of every task (background + pending scheduled ToolRuns), for
/// the UI's ToolRun panel. Mirrors the `tool_run:created` / `tool_run:updated`
/// / `tool_run:finished` / `tool_run:output` events so the panel can hydrate
/// on mount / navigation. Both ToolRun kinds use the same named DTO and
/// explicit `tool_run_id` field, matching the `tool_runs` entity.
///
/// Live tasks come from the unified in-memory board (with output preview); terminal
/// ToolRun rows that already aged out of the board's TTL are merged back in from
/// the persisted `tool_runs` table, so the panel keeps showing history (results
/// survive app restarts).
#[tauri::command]
pub async fn list_tool_runs(state: State<'_, Arc<AppState>>) -> Result<Vec<ToolRunEvent>, String> {
    let live_rows = state.runtime.services.tool_runs.board().await;
    let mut rows = Vec::with_capacity(live_rows.len());
    let mut live_ids = std::collections::HashSet::new();
    for row in live_rows {
        let event = ToolRunEvent::from(row);
        live_ids.insert(event.tool_run_id.clone());
        rows.push(event);
    }
    let history = state
        .runtime
        .services
        .tool_runs
        .list_board_history()
        .await
        .map_err(|e| log_err("list_tool_runs history", e))?;
    for tool_run in history {
        if live_ids.contains(&tool_run.tool_run_id) {
            continue;
        }
        rows.push(ToolRunEvent::from(tool_run));
    }
    Ok(rows)
}

/// Cancel a running ToolRun from the UI (a background ToolRun or a pending
/// scheduled ToolRun, selected via `kind`). Returns false when the ToolRun does not
/// exist, is not cancellable, or its durable cancellation could not be committed.
#[tauri::command]
pub async fn cancel_tool_run(
    state: State<'_, Arc<AppState>>,
    tool_run_id: String,
    kind: ToolRunKindDto,
) -> Result<bool, String> {
    validate_command_id(&tool_run_id, "tool_run_id", "toolrun")
        .map_err(|error| log_err("cancel_tool_run", error))?;
    let cancelled = state
        .runtime
        .services
        .tool_runs
        .cancel_for_kind(&tool_run_id, kind.into())
        .await;
    if !cancelled {
        tracing::warn!(
            "cancel_tool_run: not found or not cancellable: {}",
            tool_run_id
        );
    }
    Ok(cancelled)
}

/// Terminal ToolRun history (including scheduled history and background rows
/// past the in-memory TTL), newest first, for the ToolRun panel's history tab.
/// Tools owns the terminal filter, safe projection, and maximum page size.
#[tauri::command]
pub async fn list_tool_run_history(
    state: State<'_, Arc<AppState>>,
    kind: Option<ToolRunKindDto>,
    limit: Option<usize>,
    session_id: Option<String>,
) -> Result<Vec<ToolRunEvent>, String> {
    if let Some(session_id) = session_id.as_deref() {
        validate_command_id(session_id, "session_id", "ses")
            .map_err(|error| log_err("list_tool_run_history", error))?;
    }
    let limit = limit.unwrap_or(50);
    let kind = kind.map(ToolRunKind::from);
    state
        .runtime
        .services
        .tool_runs
        .list_history(ToolRunHistoryQuery::new(kind, session_id, limit))
        .await
        .map(|rows| rows.into_iter().map(ToolRunEvent::from).collect())
        .map_err(|e| log_err("list_tool_run_history", e))
}

/// Clear terminal task history while preserving waiting/running work and
/// completion results that have not yet been committed to their sessions.
#[tauri::command]
pub async fn clear_tool_run_history(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    state
        .runtime
        .services
        .tool_runs
        .clear_terminal_history()
        .await
        .map_err(|e| log_err("clear_tool_run_history", e))
}
