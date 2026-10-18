//! User-managed Workflow definitions.

use crate::app_state::AppState;
use crate::commands::{log_err, validate_command_id};
use haven_memory::{CreateWorkflowSchedule, WorkflowDefinitionRow, WorkflowScheduleRow};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::State;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinitionDto {
    pub workflow_id: String,
    pub name: String,
    pub description: String,
    pub revision: u32,
    pub definition: Value,
    pub created_at: String,
    pub updated_at: String,
}

impl TryFrom<WorkflowDefinitionRow> for WorkflowDefinitionDto {
    type Error = serde_json::Error;

    fn try_from(row: WorkflowDefinitionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            workflow_id: row.workflow_id,
            name: row.name,
            description: row.description,
            revision: row.revision,
            definition: serde_json::from_str(&row.definition_json)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowScheduleDto {
    pub schedule_id: String,
    pub name: String,
    pub workflow_definition_id: Option<String>,
    pub workflow_revision: u32,
    pub session_id: String,
    pub interval_seconds: u64,
    pub anchor_at: String,
    pub next_due_at: String,
    pub enabled: bool,
    pub active_tool_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<WorkflowScheduleRow> for WorkflowScheduleDto {
    fn from(row: WorkflowScheduleRow) -> Self {
        Self {
            schedule_id: row.schedule_id,
            name: row.name,
            workflow_definition_id: row.workflow_definition_id,
            workflow_revision: row.workflow_revision,
            session_id: row.session_id,
            interval_seconds: row.interval_seconds,
            anchor_at: row.anchor_at,
            next_due_at: row.next_due_at,
            enabled: row.enabled,
            active_tool_run_id: row.active_tool_run_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[tauri::command]
pub async fn list_workflow_definitions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<WorkflowDefinitionDto>, String> {
    state
        .workflow_definitions
        .list()
        .await
        .map_err(|error| log_err("list_workflow_definitions", error))?
        .into_iter()
        .map(|row| {
            WorkflowDefinitionDto::try_from(row)
                .map_err(|error| log_err("list_workflow_definitions decode", error))
        })
        .collect()
}

/// Create a Workflow when `workflow_id` is absent, or write a new revision
/// when it is present. Definitions contain data only; they never store grants.
#[tauri::command]
pub async fn save_workflow_definition(
    state: State<'_, Arc<AppState>>,
    workflow_id: Option<String>,
    expected_revision: Option<u32>,
    name: String,
    description: String,
    definition: Value,
) -> Result<WorkflowDefinitionDto, String> {
    match (workflow_id.as_deref(), expected_revision) {
        (Some(id), Some(revision)) if revision > 0 => {
            validate_command_id(id, "workflow_id", "wfdef")
                .map_err(|error| log_err("save_workflow_definition", error))?;
        }
        (None, None) => {}
        _ => return Err("Workflow revision changed; reload before saving".into()),
    }
    let definition_json = serde_json::to_string(&definition)
        .map_err(|error| log_err("save_workflow_definition encode", error))?;
    let row = match workflow_id {
        Some(workflow_id) => state
            .workflow_definitions
            .update(
                workflow_id,
                expected_revision.expect("validated update revision"),
                name,
                description,
                definition_json,
            )
            .await
            .map_err(|error| log_err("save_workflow_definition update", error))?
            .ok_or_else(|| {
                "Workflow definition changed or no longer exists; reload before saving".to_owned()
            })?,
        None => state
            .workflow_definitions
            .create(name, description, definition_json)
            .await
            .map_err(|error| log_err("save_workflow_definition create", error))?,
    };
    WorkflowDefinitionDto::try_from(row)
        .map_err(|error| log_err("save_workflow_definition decode", error))
}

#[tauri::command]
pub async fn delete_workflow_definition(
    state: State<'_, Arc<AppState>>,
    workflow_id: String,
) -> Result<bool, String> {
    validate_command_id(&workflow_id, "workflow_id", "wfdef")
        .map_err(|error| log_err("delete_workflow_definition", error))?;
    state
        .workflow_definitions
        .delete(workflow_id)
        .await
        .map_err(|error| log_err("delete_workflow_definition", error))
}

#[tauri::command]
pub async fn run_workflow_definition(
    state: State<'_, Arc<AppState>>,
    workflow_id: String,
    session_id: String,
) -> Result<String, String> {
    validate_command_id(&workflow_id, "workflow_id", "wfdef")
        .map_err(|error| log_err("run_workflow_definition", error))?;
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("run_workflow_definition", error))?;
    if !state.runtime.executor.session_is_live(&session_id).await {
        return Err("Workflow requires an active runtime session".into());
    }
    let row = state
        .workflow_definitions
        .get(workflow_id.clone())
        .await
        .map_err(|error| log_err("run_workflow_definition lookup", error))?
        .ok_or_else(|| "Workflow definition no longer exists".to_owned())?;
    let definition: Value = serde_json::from_str(&row.definition_json)
        .map_err(|error| log_err("run_workflow_definition decode", error))?;
    state
        .runtime
        .services
        .tool_runs
        .start_workflow_run(
            &session_id,
            &workflow_id,
            row.revision,
            &row.name,
            definition,
        )
        .await
        .map_err(|error| log_err("run_workflow_definition", error))
}

#[tauri::command]
pub async fn retry_workflow_run(
    state: State<'_, Arc<AppState>>,
    tool_run_id: String,
) -> Result<Option<String>, String> {
    validate_command_id(&tool_run_id, "tool_run_id", "toolrun")
        .map_err(|error| log_err("retry_workflow_run", error))?;
    state
        .runtime
        .services
        .tool_runs
        .retry_workflow_run(&tool_run_id)
        .await
        .map_err(|error| log_err("retry_workflow_run", error))
}

#[tauri::command]
pub async fn list_workflow_schedules(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<WorkflowScheduleDto>, String> {
    state
        .workflow_definitions
        .list_schedules()
        .await
        .map(|rows| rows.into_iter().map(WorkflowScheduleDto::from).collect())
        .map_err(|error| log_err("list_workflow_schedules", error))
}

#[tauri::command]
pub async fn create_workflow_schedule(
    state: State<'_, Arc<AppState>>,
    workflow_id: String,
    session_id: String,
    interval_seconds: u64,
    anchor_at: String,
) -> Result<WorkflowScheduleDto, String> {
    validate_command_id(&workflow_id, "workflow_id", "wfdef")
        .map_err(|error| log_err("create_workflow_schedule", error))?;
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("create_workflow_schedule", error))?;
    if !state.runtime.executor.session_is_live(&session_id).await {
        return Err("Workflow schedule requires an active runtime session".into());
    }
    let definition = state
        .workflow_definitions
        .get(workflow_id.clone())
        .await
        .map_err(|error| log_err("create_workflow_schedule lookup", error))?
        .ok_or_else(|| "Workflow definition no longer exists".to_owned())?;
    state
        .workflow_definitions
        .create_schedule(CreateWorkflowSchedule {
            name: definition.name,
            workflow_definition_id: Some(workflow_id),
            workflow_revision: definition.revision,
            definition_json: definition.definition_json,
            session_id,
            interval_seconds,
            anchor_at,
        })
        .await
        .map(WorkflowScheduleDto::from)
        .map_err(|error| log_err("create_workflow_schedule", error))
}

#[tauri::command]
pub async fn set_workflow_schedule_enabled(
    state: State<'_, Arc<AppState>>,
    schedule_id: String,
    enabled: bool,
) -> Result<bool, String> {
    validate_command_id(&schedule_id, "schedule_id", "wfsched")
        .map_err(|error| log_err("set_workflow_schedule_enabled", error))?;
    state
        .workflow_definitions
        .set_schedule_enabled(schedule_id, enabled)
        .await
        .map_err(|error| log_err("set_workflow_schedule_enabled", error))
}

#[tauri::command]
pub async fn delete_workflow_schedule(
    state: State<'_, Arc<AppState>>,
    schedule_id: String,
) -> Result<bool, String> {
    validate_command_id(&schedule_id, "schedule_id", "wfsched")
        .map_err(|error| log_err("delete_workflow_schedule", error))?;
    state
        .workflow_definitions
        .delete_schedule(schedule_id)
        .await
        .map_err(|error| log_err("delete_workflow_schedule", error))
}
