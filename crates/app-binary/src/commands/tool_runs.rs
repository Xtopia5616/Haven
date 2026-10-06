use crate::app_state::AppState;
use crate::commands::log_err;
use crate::events::{ToolRunEvent, ToolRunKind};
use haven_common::ToolRunStatus;
use haven_memory::ToolRunRow;
use std::sync::Arc;
use tauri::State;

/// Board view of every task (background + pending scheduled ToolRuns), for
/// the UI's ToolRun panel. Mirrors the `tool_run:created` / `tool_run:updated`
/// / `tool_run:finished` / `tool_run:output` events so the panel can hydrate
/// on mount / navigation. Both ToolRun kinds use the same named DTO and
/// stable `id` field; the tool implementation's `tool_run_id` is not exposed.
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
        live_ids.insert(event.id.clone());
        rows.push(event);
    }
    let history = state
        .runtime
        .services
        .tool_runs
        .list_persisted_tool_runs(Some("background"))
        .await
        .map_err(|e| log_err("list_tool_runs history", e))?;
    for tool_run in history {
        if live_ids.contains(&tool_run.id) {
            continue;
        }
        let preview = tool_run
            .output
            .as_deref()
            .or(tool_run.error.as_deref())
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        rows.push(tool_run_event_from_row(
            tool_run,
            ToolRunKind::Background,
            PersistedToolRunProjection::BoardHistory,
            Some(preview),
        ));
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
    kind: ToolRunKind,
) -> Result<bool, String> {
    let cancelled = state
        .runtime
        .services
        .tool_runs
        .cancel_for_kind(&tool_run_id, kind.as_str())
        .await;
    if !cancelled {
        tracing::warn!(
            "cancel_tool_run: not found or not cancellable: {}",
            tool_run_id
        );
    }
    Ok(cancelled)
}

/// Completed-scheduled ToolRun history (and terminal ToolRun history past the in-memory TTL)
/// from the persisted `tool_runs` table, newest first, for the ToolRun panel's
/// history tab. Rows carry `kind` plus the full stored payload; scheduled ToolRun rows
/// are limited to `limit` entries (default 50) so the panel cannot grow
/// unboundedly.
#[tauri::command]
pub async fn list_tool_run_history(
    state: State<'_, Arc<AppState>>,
    kind: Option<ToolRunKind>,
    limit: Option<usize>,
    session_id: Option<String>,
) -> Result<Vec<ToolRunEvent>, String> {
    let limit = limit.unwrap_or(50).min(200);
    let rows = match session_id.as_deref() {
        Some(session_id) => {
            state
                .runtime
                .services
                .tool_runs
                .list_persisted_tool_runs_for_session(session_id, kind.map(ToolRunKind::as_str))
                .await
        }
        None => {
            state
                .runtime
                .services
                .tool_runs
                .list_persisted_tool_runs(kind.map(ToolRunKind::as_str))
                .await
        }
    }
    .map_err(|e| log_err("list_tool_run_history", e))?;
    let mut out = Vec::new();
    // Waiting scheduled ToolRuns are already exposed by `list_tool_runs`; history
    // contains only terminal rows. Keep this filter at the app boundary so
    // the repository can remain a neutral persistence query.
    for a in rows
        .into_iter()
        .filter(|row| is_history_row(row.status))
        .take(limit)
    {
        let kind = match a.kind.as_str() {
            "background" => ToolRunKind::Background,
            "scheduled" => ToolRunKind::Scheduled,
            other => {
                return Err(log_err(
                    "list_tool_run_history",
                    format!("unknown ToolRun kind '{other}'"),
                ));
            }
        };
        out.push(tool_run_event_from_row(
            a,
            kind,
            PersistedToolRunProjection::History,
            None,
        ));
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum PersistedToolRunProjection {
    BoardHistory,
    History,
}

fn tool_run_event_from_row(
    row: ToolRunRow,
    kind: ToolRunKind,
    projection: PersistedToolRunProjection,
    preview: Option<String>,
) -> ToolRunEvent {
    let (due_at, title, body, mode) = match projection {
        PersistedToolRunProjection::BoardHistory => (None, None, None, None),
        PersistedToolRunProjection::History => (
            row.due_at,
            (!row.title.is_empty()).then_some(row.title),
            row.body,
            row.mode,
        ),
    };
    ToolRunEvent {
        id: row.id,
        kind,
        status: Some(row.status),
        session_id: row.session_id,
        source_step_id: row.source_step_id,
        started_at: row.started_at,
        finished_at: row.finished_at,
        due_at,
        title,
        body,
        mode,
        command: row.command,
        output: row.output,
        error: row.error,
        error_reason: row.error_reason,
        exit_code: row.exit_code,
        preview,
    }
}

fn is_history_row(status: ToolRunStatus) -> bool {
    status.is_terminal()
}

#[cfg(test)]
mod tests {
    use super::{is_history_row, tool_run_event_from_row};
    use haven_common::ToolRunStatus;
    use haven_memory::ToolRunRow;

    #[test]
    fn pending_scheduled_rows_are_not_history() {
        assert!(!is_history_row(ToolRunStatus::Waiting));
        assert!(is_history_row(ToolRunStatus::Completed));
    }

    #[test]
    fn persisted_tool_run_projection_uses_one_safe_row_mapper() {
        let row = ToolRunRow {
            id: "toolrun-history".into(),
            kind: "scheduled".into(),
            due_at: Some("2026-09-27T10:00:00Z".into()),
            title: String::new(),
            body: Some("reminder body".into()),
            mode: Some("continue".into()),
            session_id: Some("ses-owner".into()),
            source_step_id: None,
            tool_name: Some("notify".into()),
            tool_args: Some(r#"{"token":"internal-args"}"#.into()),
            prompt: Some("internal prompt".into()),
            status: ToolRunStatus::Completed,
            command: None,
            output: Some("result".into()),
            error: None,
            error_reason: None,
            log_path: Some("C:/private/tool-run.log".into()),
            exit_code: Some(0),
            started_at: Some("started".into()),
            finished_at: Some("finished".into()),
            created_at: "created".into(),
        };
        let event = tool_run_event_from_row(
            row.clone(),
            crate::events::ToolRunKind::Scheduled,
            super::PersistedToolRunProjection::BoardHistory,
            Some("result".into()),
        );

        assert_eq!(event.id, "toolrun-history");
        assert_eq!(event.kind, crate::events::ToolRunKind::Scheduled);
        assert_eq!(event.status, Some(ToolRunStatus::Completed));
        assert_eq!(event.due_at, None);
        assert_eq!(event.title, None);
        assert_eq!(event.body, None);
        assert_eq!(event.mode, None);
        assert_eq!(event.preview.as_deref(), Some("result"));
        let serialized = serde_json::to_string(&event).unwrap();
        for internal_value in ["internal-args", "internal prompt", "private/tool-run.log"] {
            assert!(!serialized.contains(internal_value));
        }

        let event = tool_run_event_from_row(
            row,
            crate::events::ToolRunKind::Scheduled,
            super::PersistedToolRunProjection::History,
            None,
        );
        assert_eq!(event.id, "toolrun-history");
        assert_eq!(event.kind, crate::events::ToolRunKind::Scheduled);
        assert_eq!(event.status, Some(ToolRunStatus::Completed));
        assert_eq!(event.due_at.as_deref(), Some("2026-09-27T10:00:00Z"));
        assert_eq!(event.title, None);
        assert_eq!(event.preview, None);
        let serialized = serde_json::to_string(&event).unwrap();
        for internal_value in ["internal-args", "internal prompt", "private/tool-run.log"] {
            assert!(!serialized.contains(internal_value));
        }
    }

    #[test]
    fn background_rows_are_history_by_terminal_status() {
        assert!(is_history_row(ToolRunStatus::Failed));
        assert!(is_history_row(ToolRunStatus::Cancelled));
    }
}

/// Remove a persisted ToolRun row (terminal scheduled ToolRun or terminal ToolRun history)
/// by id. Returns false when no row matched.
#[tauri::command]
pub async fn delete_tool_run(
    state: State<'_, Arc<AppState>>,
    tool_run_id: String,
) -> Result<bool, String> {
    state
        .runtime
        .services
        .tool_runs
        .delete_terminal(&tool_run_id)
        .await
        .map_err(|e| log_err("delete_tool_run", e))
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
