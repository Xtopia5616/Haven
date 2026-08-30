use crate::app_state::AppState;
use crate::commands::log_err;
use crate::events::{ActionEvent, ActionKind};
use std::sync::Arc;
use tauri::State;

/// Board view of every action (background actions + pending scheduled_actions), for
/// the UI's action panel. Mirrors the `action:created` / `action:updated`
/// / `action:finished` / `action:output` events so the panel can hydrate
/// on mount / navigation. Both action kinds use the same named action DTO and
/// stable `id` field; the tool implementation's `action_id` is not exposed.
///
/// Live actions come from the in-memory board (with output preview); terminal
/// action rows that already aged out of the board's TTL are merged back in from
/// the persisted action table, so the panel keeps showing history (results
/// survive app restarts).
#[tauri::command]
pub async fn list_actions(state: State<'_, Arc<AppState>>) -> Result<Vec<ActionEvent>, String> {
    let live_rows = state.tools.background_actions.board().await;
    let mut rows = Vec::with_capacity(live_rows.len());
    let mut live_ids = std::collections::HashSet::new();
    for row in &live_rows {
        let event = ActionEvent::background_from_value(row)
            .map_err(|error| log_err("list_actions background payload", error))?;
        live_ids.insert(event.id.clone());
        rows.push(event);
    }
    if let Ok(history) = state.db.list_actions(Some("background")) {
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
            rows.push(ActionEvent {
                id: a.id,
                kind: ActionKind::Background,
                status: a.status,
                session_id: a.session_id,
                started_at: a.started_at,
                finished_at: a.finished_at,
                due_at: None,
                title: None,
                body: None,
                mode: None,
                command: a.command,
                output: a.output,
                error: a.error,
                error_reason: a.error_reason,
                exit_code: a.exit_code,
                preview: Some(preview),
            });
        }
    }
    for row in state.tools.scheduled_actions.list().await {
        rows.push(
            ActionEvent::scheduled_from_value(&row, false)
                .map_err(|error| log_err("list_actions scheduled payload", error))?,
        );
    }
    Ok(rows)
}

/// Cancel a running action from the UI (a background action or a pending
/// scheduled action, selected via `kind`). Returns false when the action does not
/// exist or is not cancellable.
#[tauri::command]
pub async fn cancel_action(
    state: State<'_, Arc<AppState>>,
    action_id: String,
    kind: ActionKind,
) -> Result<bool, String> {
    let cancelled = if matches!(kind, ActionKind::Scheduled) {
        state.tools.scheduled_actions.cancel(&action_id).await
    } else {
        state.tools.background_actions.cancel(&action_id).await
    };
    if !cancelled {
        tracing::warn!("cancel_action: not found or not cancellable: {}", action_id);
    }
    Ok(cancelled)
}

/// Fired-scheduled-action history (and terminal action history past the in-memory TTL)
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
        .db
        .list_actions(kind.map(ActionKind::as_str))
        .map_err(|e| log_err("list_action_history", e))?;
    let mut out = Vec::new();
    // Pending scheduled actions are already exposed by `list_actions`; history
    // must contain only fired scheduled rows. Keep this filter at the app
    // boundary because the memory repository intentionally returns all rows.
    for a in rows
        .into_iter()
        .filter(|row| is_history_row(&row.kind, row.fired))
        .take(limit)
    {
        let kind = match a.kind.as_str() {
            "background" => ActionKind::Background,
            "scheduled" => ActionKind::Scheduled,
            other => {
                return Err(format!(
                    "list_action_history: unknown action kind '{other}'"
                ));
            }
        };
        out.push(ActionEvent {
            id: a.id,
            kind,
            status: a.status,
            session_id: a.session_id,
            started_at: a.started_at,
            finished_at: a.finished_at,
            due_at: a.due_at,
            title: (!a.title.is_empty()).then_some(a.title),
            body: a.body,
            mode: a.mode,
            command: a.command,
            output: a.output,
            error: a.error,
            error_reason: a.error_reason,
            exit_code: a.exit_code,
            preview: None,
        });
    }
    Ok(out)
}

fn is_history_row(kind: &str, fired: bool) -> bool {
    kind != ActionKind::Scheduled.as_str() || fired
}

#[cfg(test)]
mod tests {
    use super::is_history_row;

    #[test]
    fn pending_scheduled_rows_are_not_history() {
        assert!(!is_history_row("scheduled", false));
        assert!(is_history_row("scheduled", true));
    }

    #[test]
    fn background_rows_are_history_regardless_of_fired_flag() {
        assert!(is_history_row("background", false));
        assert!(is_history_row("background", true));
    }
}

/// Remove a persisted action row (fired scheduled_action or terminal action history)
/// by id. Returns false when no row matched.
#[tauri::command]
pub async fn delete_action(
    state: State<'_, Arc<AppState>>,
    action_id: String,
) -> Result<bool, String> {
    state
        .db
        .delete_action(&action_id)
        .map(|_| true)
        .map_err(|e| log_err("delete_action", e))
}
