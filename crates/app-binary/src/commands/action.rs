use crate::app_state::AppState;
use crate::commands::log_err;
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::State;

/// Board view of every action (background actions + pending scheduled_actions), for
/// the UI's action panel. Mirrors the `action:created` / `action:updated`
/// / `action:finished` / `action:output` events so the panel can hydrate
/// on mount / navigation. Action rows carry `kind: "background"` (plus `action_id`),
/// scheduled-action rows `kind: "scheduled"` (plus `id`).
///
/// Live actions come from the in-memory board (with output preview); terminal
/// action rows that already aged out of the board's TTL are merged back in from
/// the persisted action table, so the panel keeps showing history (results
/// survive app restarts).
#[tauri::command]
pub async fn list_actions(state: State<'_, Arc<AppState>>) -> Result<Vec<Value>, String> {
    let mut rows = state.tools.background_actions.board().await;
    for row in &mut rows {
        row["kind"] = json!("background");
    }
    let mut live_ids = std::collections::HashSet::new();
    for row in &rows {
        if let Some(id) = row.get("action_id").and_then(|v| v.as_str()) {
            live_ids.insert(id.to_string());
        }
    }
    if let Ok(history) = state.db.list_actions(Some("background")) {
        for a in history {
            if live_ids.contains(&a.id) {
                continue;
            }
            let mut row = json!({
                "kind": "background",
                "action_id": a.id,
                "status": a.status,
                "started_at": a.started_at,
                "finished_at": a.finished_at,
                "command": a.command,
            });
            if let Some(tid) = &a.session_id {
                row["session_id"] = json!(tid);
            }
            if let Some(code) = a.exit_code {
                row["exit_code"] = json!(code);
            }
            if let Some(p) = &a.log_path {
                row["log_path"] = json!(p);
            }
            let preview = a
                .output
                .as_deref()
                .or(a.error.as_deref())
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>();
            row["output"] = json!(a.output);
            if let Some(e) = &a.error {
                row["error"] = json!(e);
            }
            if let Some(r) = &a.error_reason {
                row["error_reason"] = json!(r);
            }
            row["preview"] = json!(preview);
            rows.push(row);
        }
    }
    let mut reminder_rows = state.tools.scheduled_actions.list().await;
    for row in &mut reminder_rows {
        row["kind"] = json!("scheduled");
    }
    rows.extend(reminder_rows);
    Ok(rows)
}

/// Cancel a running action from the UI (a background action or a pending
/// scheduled action, selected via `kind`). Returns false when the action does not
/// exist or is not cancellable.
#[tauri::command]
pub async fn cancel_action(
    state: State<'_, Arc<AppState>>,
    action_id: String,
    kind: String,
) -> Result<bool, String> {
    let cancelled = if kind == "scheduled" {
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
    kind: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    let limit = limit.unwrap_or(50).min(200);
    let rows = state
        .db
        .list_actions(kind.as_deref())
        .map_err(|e| log_err("list_action_history", e))?;
    let mut out = Vec::new();
    for a in rows.into_iter().take(limit) {
        let mut row = json!({
            "kind": a.kind,
            "id": a.id,
            "fired": a.fired,
        });
        if let Some(t) = &a.due_at {
            row["due_at"] = json!(t);
        }
        if let Some(t) = &a.started_at {
            row["started_at"] = json!(t);
        }
        if let Some(t) = &a.finished_at {
            row["finished_at"] = json!(t);
        }
        if let Some(s) = &a.status {
            row["status"] = json!(s);
        }
        row["title"] = json!(&a.title);
        if let Some(b) = &a.body {
            row["body"] = json!(b);
        }
        if let Some(m) = &a.mode {
            row["mode"] = json!(m);
        }
        if let Some(tid) = &a.session_id {
            row["session_id"] = json!(tid);
        }
        if let Some(c) = &a.command {
            row["command"] = json!(c);
        }
        if let Some(o) = &a.output {
            row["output"] = json!(o);
        }
        if let Some(e) = &a.error_reason {
            row["error_reason"] = json!(e);
        }
        if let Some(p) = &a.log_path {
            row["log_path"] = json!(p);
        }
        if let Some(code) = a.exit_code {
            row["exit_code"] = json!(code);
        }
        out.push(row);
    }
    Ok(out)
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
