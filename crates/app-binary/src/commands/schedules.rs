use crate::app_state::AppState;
use crate::commands::{log_err, validate_command_id};
use haven_common::schedule::ScheduleMode;
use haven_memory::{SchedulePlanRow, ScheduleType};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// Safe renderer projection of an enabled Schedule plan. Tool arguments and
/// continuation prompts stay private to the schedule executor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchedulePlanDto {
    pub schedule_id: String,
    pub owner_session_id: String,
    pub schedule_type: ScheduleType,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    pub due_at: Option<String>,
    pub watch_tool_run_id: Option<String>,
    pub interval_seconds: Option<u64>,
    pub anchor_at: Option<String>,
    pub next_due_at: Option<String>,
    pub active_schedule_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<SchedulePlanRow> for SchedulePlanDto {
    fn from(row: SchedulePlanRow) -> Self {
        Self {
            schedule_id: row.schedule_id,
            owner_session_id: row.owner_session_id,
            schedule_type: row.schedule_type,
            title: row.title,
            body: row.body,
            mode: row.mode,
            due_at: row.due_at,
            watch_tool_run_id: row.watch_tool_run_id,
            interval_seconds: row.interval_seconds,
            anchor_at: row.anchor_at,
            next_due_at: row.next_due_at,
            active_schedule_run_id: row.active_schedule_run_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// List enabled Schedule plans for the task center.
#[tauri::command]
pub async fn list_schedules(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<SchedulePlanDto>, String> {
    state
        .runtime
        .services
        .schedule_management
        .list_enabled()
        .await
        .map(|rows| rows.into_iter().map(SchedulePlanDto::from).collect())
        .map_err(|error| log_err("list_schedules", error))
}

/// Cancel an enabled Schedule plan after validating both durable identities.
#[tauri::command]
pub async fn cancel_schedule(
    state: State<'_, Arc<AppState>>,
    schedule_id: String,
    session_id: String,
) -> Result<bool, String> {
    validate_command_id(&schedule_id, "schedule_id", "sched")
        .map_err(|error| log_err("cancel_schedule", error))?;
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("cancel_schedule", error))?;
    state
        .runtime
        .services
        .schedule_management
        .cancel(&schedule_id, &session_id)
        .await
        .map_err(|error| log_err("cancel_schedule", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_renderer_projection_omits_private_execution_inputs() {
        let dto = SchedulePlanDto::from(SchedulePlanRow {
            schedule_id: "sched-00000000000000000000000000000001".into(),
            owner_session_id: "ses-00000000000000000000000000000001".into(),
            schedule_type: ScheduleType::Once,
            title: "Check later".into(),
            body: "Review the result".into(),
            mode: ScheduleMode::Continue,
            due_at: Some("2026-10-20T12:00:00.000Z".into()),
            watch_tool_run_id: None,
            tool_name: Some("notify".into()),
            tool_args: Some(serde_json::json!({"token":"private"})),
            prompt: Some("private continuation prompt".into()),
            interval_seconds: None,
            anchor_at: None,
            next_due_at: None,
            enabled: true,
            active_schedule_run_id: None,
            created_at: "2026-10-19T12:00:00.000Z".into(),
            updated_at: "2026-10-19T12:00:00.000Z".into(),
        });
        let value = serde_json::to_value(dto).unwrap();
        assert!(value.get("tool_args").is_none());
        assert!(value.get("prompt").is_none());
        assert_eq!(
            value["schedule_id"],
            "sched-00000000000000000000000000000001"
        );
    }
}
