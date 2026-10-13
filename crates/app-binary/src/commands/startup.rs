use crate::startup::StartupCoordinator;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// Acknowledge that the first hydrated app surface has painted.
///
/// This releases the native startup surface and reveals the main window. It
/// has no effect on persistent app state.
#[tauri::command]
pub async fn report_startup_ui_ready(app: AppHandle) -> Result<(), String> {
    let startup = app.state::<Arc<StartupCoordinator>>();
    startup
        .report_ui_ready(&app)
        .map_err(|error| crate::commands::log_err("report_startup_ui_ready", error))
}
