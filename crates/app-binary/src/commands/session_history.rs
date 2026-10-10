use crate::app_state::AppState;
use crate::commands::log_err;
use crate::commands::session::SessionRecordDto;
use std::sync::Arc;
use tauri::State;

type StoredSession = haven_memory::Session;

fn session_record_rows(sessions: Vec<StoredSession>) -> Vec<SessionRecordDto> {
    sessions.into_iter().map(SessionRecordDto::from).collect()
}

#[tauri::command]
pub async fn list_session_history(
    state: State<'_, Arc<AppState>>,
    limit: i64,
    offset: i64,
) -> Result<Vec<SessionRecordDto>, String> {
    let sessions = state
        .runtime
        .session_store
        .list_session_history(limit, offset)
        .await
        .map_err(|e| log_err("list_session_history", e))?;
    Ok(session_record_rows(sessions))
}

#[tauri::command]
pub async fn count_session_history(state: State<'_, Arc<AppState>>) -> Result<i64, String> {
    state
        .runtime
        .session_store
        .count_session_history()
        .await
        .map_err(|e| log_err("count_session_history", e))
}

#[tauri::command]
pub async fn search_session_history_paginated(
    state: State<'_, Arc<AppState>>,
    query: String,
    limit: i64,
    offset: i64,
) -> Result<Vec<SessionRecordDto>, String> {
    let sessions = state
        .runtime
        .session_store
        .search_session_history_paginated(query, limit, offset)
        .await
        .map_err(|e| log_err("search_session_history_paginated", e))?;
    Ok(session_record_rows(sessions))
}

#[tauri::command]
pub async fn count_session_history_search(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<i64, String> {
    state
        .runtime
        .session_store
        .count_session_history_search(query)
        .await
        .map_err(|e| log_err("count_session_history_search", e))
}

#[tauri::command]
pub async fn search_session_history(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<SessionRecordDto>, String> {
    let sessions = state
        .runtime
        .session_store
        .search_session_history(query)
        .await
        .map_err(|e| log_err("search_session_history", e))?;
    Ok(session_record_rows(sessions))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_session_history_filtered(
    state: State<'_, Arc<AppState>>,
    query: Option<String>,
    status: Option<haven_memory::SessionHistoryStatusFilter>,
    start_date: Option<String>,
    end_date: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<SessionRecordDto>, String> {
    let sessions = state
        .runtime
        .session_store
        .search_session_history_filtered(haven_memory::SessionHistoryFilter {
            query,
            status,
            start_date,
            end_date,
            limit: limit.unwrap_or(50),
            offset: offset.unwrap_or(0),
        })
        .await
        .map_err(|e| log_err("search_session_history_filtered", e))?;
    Ok(session_record_rows(sessions))
}

#[tauri::command]
pub async fn export_session_history(
    state: State<'_, Arc<AppState>>,
    start_date: Option<String>,
    end_date: Option<String>,
    status: Option<haven_memory::SessionHistoryStatusFilter>,
) -> Result<String, String> {
    let sessions = state
        .runtime
        .session_store
        .search_session_history_filtered(haven_memory::SessionHistoryFilter {
            query: None,
            status,
            start_date,
            end_date,
            limit: 10000,
            offset: 0,
        })
        .await
        .map_err(|e| log_err("export_session_history", e))?;
    let sessions = session_record_rows(sessions);
    serde_json::to_string_pretty(&serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "count": sessions.len(),
        "sessions": sessions,
    }))
    .map_err(|e| log_err("export_session_history", e))
}
