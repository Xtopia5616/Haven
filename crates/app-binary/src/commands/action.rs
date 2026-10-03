use crate::app_state::AppState;
use crate::commands::log_err;
use crate::events::{ActionEvent, ActionKind};
use haven_common::ActionStatus;
use haven_memory::ActionRow;
use std::sync::Arc;
use tauri::State;

/// Board view of every task (background actions + pending scheduled actions), for
/// the UI's action panel. Mirrors the `action:created` / `action:updated`
/// / `action:finished` / `action:output` events so the panel can hydrate
/// on mount / navigation. Both action kinds use the same named action DTO and
/// stable `id` field; the tool implementation's `action_id` is not exposed.
///
/// Live tasks come from the unified in-memory board (with output preview); terminal
/// action rows that already aged out of the board's TTL are merged back in from
/// the persisted action table, so the panel keeps showing history (results
/// survive app restarts).
#[tauri::command]
pub async fn list_actions(state: State<'_, Arc<AppState>>) -> Result<Vec<ActionEvent>, String> {
    let live_rows = state.services.actions.board().await;
    let mut rows = Vec::with_capacity(live_rows.len());
    let mut live_ids = std::collections::HashSet::new();
    for row in live_rows {
        let event = ActionEvent::from(row);
        live_ids.insert(event.id.clone());
        rows.push(event);
    }
    let history = state
        .services
        .actions
        .list_persisted_actions(Some("background"))
        .await
        .map_err(|e| log_err("list_actions history", e))?;
    for a in history {
        if live_ids.contains(&a.id) {
            continue;
        }
        let preview = a
            .output
            .as_deref()
            .or(a.error.as_deref())
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        rows.push(action_event_from_row(
            a,
            ActionKind::Background,
            PersistedActionProjection::BoardHistory,
            Some(preview),
        ));
    }
    Ok(rows)
}

/// Cancel a running action from the UI (a background action or a pending
/// scheduled action, selected via `kind`). Returns false when the action does not
/// exist, is not cancellable, or its durable cancellation could not be committed.
#[tauri::command]
pub async fn cancel_action(
    state: State<'_, Arc<AppState>>,
    action_id: String,
    kind: ActionKind,
) -> Result<bool, String> {
    let cancelled = state
        .services
        .actions
        .cancel_for_kind(&action_id, kind.as_str())
        .await;
    if !cancelled {
        tracing::warn!("cancel_action: not found or not cancellable: {}", action_id);
    }
    Ok(cancelled)
}

/// Completed-scheduled-action history (and terminal action history past the in-memory TTL)
/// from the persisted action table, newest first, for the action panel's
/// history tab. Rows carry `kind` plus the full stored payload; scheduled-action rows
/// are limited to `limit` entries (default 50) so the panel cannot grow
/// unboundedly.
#[tauri::command]
pub async fn list_action_history(
    state: State<'_, Arc<AppState>>,
    kind: Option<ActionKind>,
    limit: Option<usize>,
) -> Result<Vec<ActionEvent>, String> {
    let limit = limit.unwrap_or(50).min(200);
    let rows = state
        .services
        .actions
        .list_persisted_actions(kind.map(ActionKind::as_str))
        .await
        .map_err(|e| log_err("list_action_history", e))?;
    let mut out = Vec::new();
    // Waiting scheduled actions are already exposed by `list_actions`; history
    // contains only terminal rows. Keep this filter at the app boundary so
    // the repository can remain a neutral persistence query.
    for a in rows
        .into_iter()
        .filter(|row| is_history_row(row.status))
        .take(limit)
    {
        let kind = match a.kind.as_str() {
            "background" => ActionKind::Background,
            "scheduled" => ActionKind::Scheduled,
            other => {
                return Err(log_err(
                    "list_action_history",
                    format!("unknown action kind '{other}'"),
                ));
            }
        };
        out.push(action_event_from_row(
            a,
            kind,
            PersistedActionProjection::History,
            None,
        ));
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum PersistedActionProjection {
    BoardHistory,
    History,
}

fn action_event_from_row(
    row: ActionRow,
    kind: ActionKind,
    projection: PersistedActionProjection,
    preview: Option<String>,
) -> ActionEvent {
    let (due_at, title, body, mode) = match projection {
        PersistedActionProjection::BoardHistory => (None, None, None, None),
        PersistedActionProjection::History => (
            row.due_at,
            (!row.title.is_empty()).then_some(row.title),
            row.body,
            row.mode,
        ),
    };
    ActionEvent {
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

fn is_history_row(status: ActionStatus) -> bool {
    status.is_terminal()
}

#[cfg(test)]
mod tests {
    use super::{action_event_from_row, is_history_row};
    use haven_common::ActionStatus;
    use haven_memory::ActionRow;

    #[test]
    fn pending_scheduled_rows_are_not_history() {
        assert!(!is_history_row(ActionStatus::Waiting));
        assert!(is_history_row(ActionStatus::Completed));
    }

    #[test]
    fn persisted_action_projection_uses_one_safe_row_mapper() {
        let row = ActionRow {
            id: "act-history".into(),
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
            status: ActionStatus::Completed,
            command: None,
            output: Some("result".into()),
            error: None,
            error_reason: None,
            log_path: Some("C:/private/action.log".into()),
            exit_code: Some(0),
            started_at: Some("started".into()),
            finished_at: Some("finished".into()),
            created_at: "created".into(),
        };
        let event = action_event_from_row(
            row.clone(),
            crate::events::ActionKind::Scheduled,
            super::PersistedActionProjection::BoardHistory,
            Some("result".into()),
        );

        assert_eq!(event.id, "act-history");
        assert_eq!(event.kind, crate::events::ActionKind::Scheduled);
        assert_eq!(event.status, Some(ActionStatus::Completed));
        assert_eq!(event.due_at, None);
        assert_eq!(event.title, None);
        assert_eq!(event.body, None);
        assert_eq!(event.mode, None);
        assert_eq!(event.preview.as_deref(), Some("result"));
        let serialized = serde_json::to_string(&event).unwrap();
        for internal_value in ["internal-args", "internal prompt", "private/action.log"] {
            assert!(!serialized.contains(internal_value));
        }

        let event = action_event_from_row(
            row,
            crate::events::ActionKind::Scheduled,
            super::PersistedActionProjection::History,
            None,
        );
        assert_eq!(event.id, "act-history");
        assert_eq!(event.kind, crate::events::ActionKind::Scheduled);
        assert_eq!(event.status, Some(ActionStatus::Completed));
        assert_eq!(event.due_at.as_deref(), Some("2026-09-27T10:00:00Z"));
        assert_eq!(event.title, None);
        assert_eq!(event.preview, None);
        let serialized = serde_json::to_string(&event).unwrap();
        for internal_value in ["internal-args", "internal prompt", "private/action.log"] {
            assert!(!serialized.contains(internal_value));
        }
    }

    #[test]
    fn background_rows_are_history_by_terminal_status() {
        assert!(is_history_row(ActionStatus::Failed));
        assert!(is_history_row(ActionStatus::Cancelled));
    }
}

/// Remove a persisted action row (terminal scheduled action or terminal action history)
/// by id. Returns false when no row matched.
#[tauri::command]
pub async fn delete_action(
    state: State<'_, Arc<AppState>>,
    action_id: String,
) -> Result<bool, String> {
    state
        .services
        .actions
        .delete_terminal(&action_id)
        .await
        .map_err(|e| log_err("delete_action", e))
}
