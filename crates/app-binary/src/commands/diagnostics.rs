//! Read-only local performance diagnostics.

use crate::app_state::AppState;
use std::sync::Arc;
use tauri::State;

/// Export the bounded, content-free ReAct performance snapshot for local
/// acceptance checks and diagnostics. Prompt text, tool arguments and
/// credentials are intentionally not part of this response.
#[tauri::command]
pub fn get_performance_metrics(
    state: State<'_, Arc<AppState>>,
) -> Result<haven_agent::MetricsSnapshot, String> {
    Ok(state.agent.react_metrics_snapshot())
}
