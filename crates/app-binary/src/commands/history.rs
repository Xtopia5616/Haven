use crate::app_state::AppState;
use crate::commands::log_err;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_history(
    state: State<'_, Arc<AppState>>,
    limit: i64,
    offset: i64,
) -> Result<Vec<haven_memory::repositories::sessions::Session>, String> {
    state
        .db
        .list_sessions(limit, offset)
        .map_err(|e| log_err("get_history", e))
}

#[tauri::command]
pub async fn count_history(state: State<'_, Arc<AppState>>) -> Result<i64, String> {
    state
        .db
        .count_sessions()
        .map_err(|e| log_err("count_history", e))
}

#[tauri::command]
pub async fn search_history_paginated(
    state: State<'_, Arc<AppState>>,
    query: String,
    limit: i64,
    offset: i64,
) -> Result<Vec<haven_memory::repositories::sessions::Session>, String> {
    state
        .db
        .search_sessions_paginated(&query, limit, offset)
        .map_err(|e| log_err("search_history_paginated", e))
}

#[tauri::command]
pub async fn count_history_search(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<i64, String> {
    state
        .db
        .count_sessions_search(&query)
        .map_err(|e| log_err("count_history_search", e))
}

#[tauri::command]
pub async fn search_history(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<haven_memory::repositories::sessions::Session>, String> {
    state
        .db
        .search_sessions(&query)
        .map_err(|e| log_err("search_history", e))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_history_filtered(
    state: State<'_, Arc<AppState>>,
    query: Option<String>,
    status: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<haven_memory::repositories::sessions::Session>, String> {
    state
        .db
        .search_sessions_filtered(
            query.as_deref(),
            status.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            limit.unwrap_or(50),
            offset.unwrap_or(0),
        )
        .map_err(|e| log_err("search_history_filtered", e))
}

#[tauri::command]
pub async fn export_history(
    state: State<'_, Arc<AppState>>,
    start_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
) -> Result<String, String> {
    let sessions = state
        .db
        .search_sessions_filtered(
            None,
            status.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            10000,
            0,
        )
        .map_err(|e| log_err("export_history", e))?;
    serde_json::to_string_pretty(&serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "count": sessions.len(),
        "sessions": sessions,
    }))
    .map_err(|e| log_err("export_history", e))
}
