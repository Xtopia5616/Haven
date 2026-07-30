use crate::app_state::AppState;
use crate::events::*;
use haven_common::McpServerConfig;
use haven_common::types::RiskLevel;
use haven_input::RecordingReason;
use haven_llm::LlmRouter;
use haven_llm::{ModelInfo, ModelRegistry};
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::task_steps::TaskStep;
use haven_memory::repositories::tasks::Task;
use haven_tools::{ConfirmationResult, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::filter::EnvFilter;

#[derive(Serialize)]
pub struct TaskListResponse {
    pub tasks: Vec<haven_task::TaskInfo>,
}

#[derive(Serialize)]
pub struct ToolListResponse {
    pub tools: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct RecordingState {
    pub is_recording: bool,
    pub is_toggle: bool,
}

/// SkillInfo now sourced from `haven_tools::skills::SkillInfo` (M4-01).
pub use haven_tools::SkillInfo;

#[tauri::command]
pub async fn start_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if let Err(e) = state.pipeline.start_recording().await {
        let session_id = uuid::Uuid::new_v4().to_string();
        let _ = app.emit(
            "recording:error",
            serde_json::json!({
                "session_id": session_id,
                "error": format!("录音启动失败，请检查麦克风/STT 配置: {e}"),
            }),
        );
        return Err(e.to_string());
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    let _ = app.emit(
        "recording:started",
        RecordingEvent {
            is_recording: true,
            session_id: Some(session_id),
            reason: None,
            duration_ms: None,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn stop_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let result = state
        .pipeline
        .stop_recording()
        .await
        .map_err(|e| e.to_string())?;
    let reason_str = match result.reason {
        RecordingReason::Manual => "manual",
        RecordingReason::Silence => "silence",
        RecordingReason::MaxDuration => "max_duration",
        RecordingReason::Cancel => "cancel",
    };
    let _ = app.emit(
        "recording:stopped",
        RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: Some(reason_str.to_string()),
            duration_ms: Some(result.duration_ms),
        },
    );

    let session_id = uuid::Uuid::new_v4().to_string();
    match result.transcript {
        Some(text) => {
            let _ = app.emit(
                "transcription:result",
                TranscriptionResultEvent {
                    session_id: session_id.clone(),
                    text: text.clone(),
                    duration_ms: result.duration_ms,
                    confidence: None,
                },
            );
            // Auto-submit transcript to agent.
            // Find the most-recently-created running or pending task so
            // voice follow-ups supplement the active task instead of
            // creating a brand-new one every time.
            let active = {
                let tasks = state.executor.list_tasks().await;
                tasks
                    .iter()
                    .find(|t| {
                        let s = t.status.as_str();
                        s == "running" || s == "pending"
                    })
                    .map(|t| t.id.clone())
            };
            let _ = state
                .agent
                .process_input(&text, active.clone())
                .await
                .map_err(|e| e.to_string())?;
            Ok(text)
        }
        None => {
            let _ = app.emit(
                "transcription:error",
                TranscriptionErrorEvent {
                    session_id,
                    error: "转写失败，请检查 STT 服务配置".into(),
                },
            );
            Ok(String::new())
        }
    }
}

#[tauri::command]
pub async fn cancel_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    state
        .pipeline
        .cancel_recording()
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "recording:stopped",
        RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: Some("cancel".to_string()),
            duration_ms: None,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn process_transcript(
    state: State<'_, Arc<AppState>>,
    transcript: String,
    active_task_id: Option<String>,
) -> Result<Value, String> {
    tracing::debug!(
        "process_transcript called: text={:?} active_task_id={:?}",
        transcript,
        active_task_id
    );
    let result = state
        .agent
        .process_input(&transcript, active_task_id.clone())
        .await
        .map_err(|e| {
            tracing::error!("process_transcript error: {:?}", e);
            e.to_string()
        })?;
    tracing::debug!("process_transcript result: {:?}", result);
    Ok(serde_json::to_value(result).unwrap_or_default())
}

#[tauri::command]
pub async fn reopen_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    tracing::debug!("reopen_task called: task_id={}", task_id);
    state.agent.reopen_task(&task_id).await.map_err(|e| {
        tracing::error!("reopen_task error: {:?}", e);
        e.to_string()
    })?;
    tracing::debug!("reopen_task done");
    Ok(())
}

#[tauri::command]
pub async fn get_tasks(state: State<'_, Arc<AppState>>) -> Result<TaskListResponse, String> {
    let tasks = state.executor.list_tasks().await;
    Ok(TaskListResponse { tasks })
}

#[tauri::command]
pub async fn end_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let final_status = state
        .executor
        .end_task(&task_id)
        .await
        .map_err(|e| e.to_string())?;
    let title = state
        .executor
        .list_tasks()
        .await
        .into_iter()
        .find(|t| t.id == task_id)
        .map(|t| t.input)
        .or_else(|| {
            state
                .db
                .get_task(&task_id)
                .ok()
                .flatten()
                .map(|t| t.input_text)
        })
        .unwrap_or_default();
    if final_status == haven_task::TaskStatus::Error {
        let _ = app.emit(
            "task:updated",
            serde_json::json!({
                "task_id": task_id,
                "status": "error",
                "title": title,
            }),
        );
    } else {
        state.agent.emit_task_completed(&task_id, &title).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn resolve_confirmation(
    state: State<'_, Arc<AppState>>,
    step_id: String,
    confirmed: bool,
    trust_session: Option<bool>,
) -> Result<(), String> {
    state
        .executor
        .confirm_step(&step_id, confirmed)
        .map_err(|e| e.to_string())?;
    if trust_session.unwrap_or(false) && confirmed {
        // Trust this risk level for the session
        let tasks = state.executor.list_tasks().await;
        if let Some(task) = tasks
            .iter()
            .find(|t| t.steps.iter().any(|s| s.id == step_id))
            && let Some(step) = task.steps.iter().find(|s| s.id == step_id)
        {
            state
                .tools
                .safety_gateway
                .trust_risk_level(step.risk_level)
                .await;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn get_tools(state: State<'_, Arc<AppState>>) -> Result<ToolListResponse, String> {
    let tools = state.tools.registry.list_schemas().await;
    Ok(ToolListResponse { tools })
}

#[tauri::command]
pub async fn get_recording_state(
    state: State<'_, Arc<AppState>>,
) -> Result<RecordingState, String> {
    let shell_state = state.shell.get_state().await;
    Ok(RecordingState {
        is_recording: shell_state.is_recording,
        is_toggle: shell_state.is_recording_toggle,
    })
}

#[tauri::command]
pub async fn get_history(
    state: State<'_, Arc<AppState>>,
    limit: i64,
    offset: i64,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    let _ = state.db.finalize_stale_tasks(10);
    state
        .db
        .list_tasks(limit, offset)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn count_history(state: State<'_, Arc<AppState>>) -> Result<i64, String> {
    state.db.count_tasks().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_history_paginated(
    state: State<'_, Arc<AppState>>,
    query: String,
    limit: i64,
    offset: i64,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    state
        .db
        .search_tasks_paginated(&query, limit, offset)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn count_history_search(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<i64, String> {
    state
        .db
        .count_tasks_search(&query)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_mcp_tools(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<McpServerSnapshot>, String> {
    Ok(state.tools.mcp_manager.snapshot().await)
}

#[tauri::command]
pub async fn reconnect_mcp(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    state
        .tools
        .mcp_manager
        .reconnect(&name)
        .await
        .map_err(|e| e.to_string())?;
    // Restart health monitor for this client
    if let Some(client) = state.tools.mcp_manager.get_client(&name).await {
        let config = haven_common::config::ConfigLoader::load()
            .map_err(|e| e.to_string())?
            .config()
            .mcp_discovery
            .clone();
        let health_interval = std::time::Duration::from_secs(config.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(config.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(config.reconnect_max_ms);
        let max_retries = config.reconnect_max_retries;
        let status_tx = state.tools.mcp_manager.status_tx();
        client.spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            max_retries,
            status_tx,
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn mcp_tool_call(
    state: State<'_, Arc<AppState>>,
    client: String,
    tool: String,
    args: Value,
) -> Result<Value, String> {
    // Check SafetyGateway (MCP tools default to Medium risk)
    match state
        .tools
        .safety_gateway
        .check(
            &format!("mcp:{}:{}", client, tool),
            &args,
            RiskLevel::Medium,
        )
        .await
    {
        ConfirmationResult::AutoApproved => {}
        ConfirmationResult::RequiresConfirmation {
            tool_name,
            params,
            risk_level,
        } => {
            return Err(serde_json::to_string(&serde_json::json!({
                "requires_confirmation": true,
                "tool_name": tool_name,
                "params": params,
                "risk_level": risk_level,
            }))
            .map_err(|e| e.to_string())?);
        }
        ConfirmationResult::Blocked => {
            return Err("MCP tool call blocked by security policy".to_string());
        }
    }

    let cancel = CancellationToken::new();
    let result = state
        .tools
        .mcp_manager
        .call_tool(&client, &tool, args, cancel)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "success": result.success,
        "output": result.output,
        "error": result.error,
    }))
}

#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, Arc<AppState>>,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Persist to config
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| e.to_string())?;
    loader.config_mut().mcp_servers.push(config.clone());
    loader.save().map_err(|e| e.to_string())?;

    // Create client and connect
    let client = std::sync::Arc::new(haven_tools::McpClient::new(
        &config.name,
        &config.command,
        &config.args,
        &config.env,
    ));
    if config.enabled {
        client.connect().await.map_err(|e| e.to_string())?;
        // Start health monitor
        let discovery = loader.config().mcp_discovery.clone();
        let health_interval = std::time::Duration::from_secs(discovery.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(discovery.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
        let status_tx = state.tools.mcp_manager.status_tx();
        client.clone().spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            discovery.reconnect_max_retries,
            status_tx,
        );
    }
    state.tools.mcp_manager.add_client(client).await;
    // Keep the in-memory server_configs map in sync so `load_mcp` and the
    // MCP server index reflect the newly added server.
    state.tools.upsert_mcp_server_config(config.clone()).await;
    // Rebuild tool catalog so MCP tools appear in the Reasoner's tool list.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name: config.name,
            status: McpClientStatus::Connected,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn update_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| e.to_string())?;
    let servers = &mut loader.config_mut().mcp_servers;
    if let Some(existing) = servers.iter_mut().find(|s| s.name == name) {
        let config_changed = existing.command != config.command
            || existing.args != config.args
            || existing.env != config.env;
        *existing = config.clone();
        loader.save().map_err(|e| e.to_string())?;

        // If command/args/env changed, reconnect; if only enabled, toggle
        if config_changed {
            state.tools.mcp_manager.remove_client(&name).await;
            if config.enabled {
                let client = std::sync::Arc::new(haven_tools::McpClient::new(
                    &config.name,
                    &config.command,
                    &config.args,
                    &config.env,
                ));
                client.connect().await.map_err(|e| e.to_string())?;
                let discovery = loader.config().mcp_discovery.clone();
                let health_interval =
                    std::time::Duration::from_secs(discovery.health_interval_secs);
                let initial_backoff =
                    std::time::Duration::from_millis(discovery.reconnect_initial_ms);
                let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
                let status_tx = state.tools.mcp_manager.status_tx();
                client.clone().spawn_monitor(
                    health_interval,
                    initial_backoff,
                    max_backoff,
                    discovery.reconnect_max_retries,
                    status_tx,
                );
                state.tools.mcp_manager.add_client(client).await;
            }
        } else if config.enabled {
            // Toggle from disabled to enabled
            let client = std::sync::Arc::new(haven_tools::McpClient::new(
                &config.name,
                &config.command,
                &config.args,
                &config.env,
            ));
            client.connect().await.map_err(|e| e.to_string())?;
            let discovery = loader.config().mcp_discovery.clone();
            let health_interval = std::time::Duration::from_secs(discovery.health_interval_secs);
            let initial_backoff = std::time::Duration::from_millis(discovery.reconnect_initial_ms);
            let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
            let status_tx = state.tools.mcp_manager.status_tx();
            client.clone().spawn_monitor(
                health_interval,
                initial_backoff,
                max_backoff,
                discovery.reconnect_max_retries,
                status_tx,
            );
            state.tools.mcp_manager.add_client(client).await;
        } else {
            // Disabled: shutdown
            state.tools.mcp_manager.remove_client(&name).await;
        }
    } else {
        return Err(format!("MCP server '{}' not found", name));
    }

    // Keep server_configs in sync with the updated config.
    state.tools.upsert_mcp_server_config(config.clone()).await;

    // Rebuild tool catalog for consistency with add/remove/toggle commands.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name: config.name,
            status: if config.enabled {
                McpClientStatus::Connected
            } else {
                McpClientStatus::Disconnected
            },
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Remove from config
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| e.to_string())?;
    loader.config_mut().mcp_servers.retain(|s| s.name != name);
    loader.save().map_err(|e| e.to_string())?;

    // Shutdown and remove from manager
    state.tools.mcp_manager.remove_client(&name).await;
    state.tools.remove_mcp_server_config(&name).await;

    // Rebuild tool catalog so removed MCP tools disappear from the Reasoner.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name,
            status: McpClientStatus::Disconnected,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| e.to_string())?;
    let servers = &mut loader.config_mut().mcp_servers;
    if let Some(existing) = servers.iter_mut().find(|s| s.name == name) {
        existing.enabled = enabled;
        loader.save().map_err(|e| e.to_string())?;
    } else {
        return Err(format!("MCP server '{}' not found", name));
    }

    let config = loader
        .config()
        .mcp_servers
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| format!("MCP server '{}' not found", name))?;

    if enabled {
        // Reconnect
        let client = std::sync::Arc::new(haven_tools::McpClient::new(
            &config.name,
            &config.command,
            &config.args,
            &config.env,
        ));
        client.connect().await.map_err(|e| e.to_string())?;
        let discovery = loader.config().mcp_discovery.clone();
        let health_interval = std::time::Duration::from_secs(discovery.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(discovery.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
        let status_tx = state.tools.mcp_manager.status_tx();
        client.clone().spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            discovery.reconnect_max_retries,
            status_tx,
        );
        state.tools.mcp_manager.add_client(client).await;
    } else {
        state.tools.mcp_manager.remove_client(&name).await;
    }

    // Keep server_configs in sync (enabled flag changed).
    state.tools.upsert_mcp_server_config(config.clone()).await;

    // Rebuild tool catalog so the tool list reflects the toggle.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name,
            status: if enabled {
                McpClientStatus::Connected
            } else {
                McpClientStatus::Disconnected
            },
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn configure_mcp(
    state: State<'_, Arc<AppState>>,
    name: String,
    transport: String,
    config: String,
) -> Result<(), String> {
    let _ = state
        .db
        .save_mcp_server(&name, &transport, &config)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn list_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillInfo>, String> {
    Ok(state.tools.skills_engine.list().await)
}

#[tauri::command]
pub async fn refresh_skills(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Re-scan skills from the configured (or default) skills directory (M4-01).
    state
        .tools
        .skills_engine
        .refresh_from_disk()
        .await
        .map_err(|e| e.to_string())?;
    // Rebuild tool catalog so skills appear in the Reasoner's tool list.
    state.tools.rebuild_catalog().await;
    // Notify the frontend that the registry changed so views can refetch.
    let _ = app.emit(
        "skills:status_change",
        serde_json::json!({ "op": "refresh" }),
    );
    Ok(())
}

#[tauri::command]
pub async fn set_skill_enabled(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    state
        .tools
        .skills_engine
        .set_enabled(&name, enabled)
        .await
        .map_err(|e| e.to_string())?;
    // The engine's `set_enabled` already syncs its internal filter so the
    // toggle survives `refresh_from_disk`. Persist the filter to config.toml
    // so it also survives app restart.
    let filter = state.tools.skills_engine.enabled_filter().await;
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| e.to_string())?;
    loader.config_mut().skills.enabled = filter;
    loader.save().map_err(|e| e.to_string())?;

    // Rebuild tool catalog so the enable/disable takes effect in the Reasoner.
    state.tools.rebuild_catalog().await;

    Ok(())
}

#[tauri::command]
pub async fn open_skills_dir(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let root = state.tools.skills_engine.resolved_root().await;
    // Ensure the directory exists so the file manager opens something sensible
    // instead of erroring; users may have an empty skills root on first run.
    let _ = std::fs::create_dir_all(&root);
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(root.as_os_str())
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&root)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&root)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(root.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn execute_skill(
    state: State<'_, Arc<AppState>>,
    name: String,
    params: serde_json::Value,
    confirmed: Option<bool>,
) -> Result<serde_json::Value, String> {
    let skill_info = state
        .tools
        .skills_engine
        .get(&name)
        .await
        .ok_or_else(|| format!("skill '{}' not found", name))?;

    if !skill_info.enabled {
        return Err(format!("skill '{}' is not enabled", name));
    }

    let risk_level = haven_common::types::RiskLevel::Medium;
    if confirmed.unwrap_or(false) {
        state
            .tools
            .safety_gateway
            .trust_risk_level(risk_level)
            .await;
    } else {
        match state
            .tools
            .safety_gateway
            .check(&format!("skill:{}", name), &params, risk_level)
            .await
        {
            haven_tools::ConfirmationResult::AutoApproved => {}
            haven_tools::ConfirmationResult::RequiresConfirmation {
                tool_name,
                params,
                risk_level,
            } => {
                return Err(serde_json::to_string(&serde_json::json!({
                    "requires_confirmation": true,
                    "tool_name": tool_name,
                    "params": params,
                    "risk_level": risk_level,
                }))
                .map_err(|e| e.to_string())?);
            }
            haven_tools::ConfirmationResult::Blocked => {
                return Err("skill execution blocked by security policy".to_string());
            }
        }
    }

    let skill = state
        .tools
        .skills_engine
        .get_skill(&name)
        .await
        .ok_or_else(|| format!("skill '{}' not found", name))?;

    let cancel = tokio_util::sync::CancellationToken::new();
    let result = state
        .tools
        .skill_runner
        .read()
        .await
        .execute(&skill, &params, cancel)
        .await
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "success": result.success,
        "output": result.output,
        "error": result.error,
    }))
}

#[tauri::command]
pub async fn search_history(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    state.db.search_tasks(&query).map_err(|e| e.to_string())
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
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    let _ = state.db.finalize_stale_tasks(10);
    state
        .db
        .search_tasks_filtered(
            query.as_deref(),
            status.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            limit.unwrap_or(50),
            offset.unwrap_or(0),
        )
        .map_err(|e| e.to_string())
}

/// Manually update a task's display title.
#[tauri::command]
pub async fn update_task_title(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    task_id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Title cannot be empty".into());
    }
    state
        .db
        .update_task_title(&task_id, &title)
        .map_err(|e| e.to_string())?;
    state.executor.update_task_title(&task_id, &title).await;
    let _ = app.emit(
        "task:title-updated",
        serde_json::json!({
            "task_id": task_id,
            "title": title,
        }),
    );
    Ok(())
}

#[tauri::command]
pub async fn delete_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    state.db.delete_task(&task_id).map_err(|e| e.to_string())?;
    state.executor.remove_task(&task_id).await;
    Ok(())
}

#[tauri::command]
pub async fn clear_history(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let count = state
        .db
        .clear_tasks()
        .map(|n| n as u64)
        .map_err(|e| e.to_string())?;
    state.executor.clear_all_tasks().await;
    Ok(count)
}

#[tauri::command]
pub async fn get_api_key_status() -> Result<serde_json::Value, String> {
    let loader = haven_common::config::ConfigLoader::load().map_err(|e| e.to_string())?;
    let cfg = loader.config();
    Ok(serde_json::json!({
        "small_model": !cfg.llm.small_model.api_key.is_empty(),
        "default_model": !cfg.llm.default_model.api_key.is_empty(),
        "balanced_model": !cfg.llm.balanced_model.api_key.is_empty(),
    }))
}

/// §2.7: List available models from the built-in catalog
#[tauri::command]
pub async fn list_models(query: Option<String>) -> Result<Vec<ModelInfo>, String> {
    let reg = ModelRegistry::new();
    let results = match query {
        Some(q) if !q.is_empty() => {
            // Return owned ModelInfo from search
            let found = reg.search(&q);
            found.into_iter().cloned().collect()
        }
        _ => reg.all().into_iter().cloned().collect(),
    };
    Ok(results)
}

/// §2.7: Switch a model endpoint role to a different model.
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn switch_model(
    role: String,
    model_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();

    // Lock and update config
    let mut loader = {
        let guard = state.config_loader.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };
    {
        let cfg = loader.config_mut();
        let ep = match role.as_str() {
            "small_model" | "namer" => &mut cfg.llm.small_model,
            "default_model" | "reasoner" => &mut cfg.llm.default_model,
            "balanced_model" | "fallback" => &mut cfg.llm.balanced_model,
            _ => return Err(format!("unknown role: {}", role)),
        };
        ep.model_name = model_id;
    }
    loader.save().map_err(|e| e.to_string())?;

    // Replace the in-memory config_loader
    {
        let mut guard = state.config_loader.lock().map_err(|e| e.to_string())?;
        *guard = loader;
    }

    // Hot-swap the LlmRouter
    let config = {
        let guard = state.config_loader.lock().map_err(|e| e.to_string())?;
        guard.config().clone()
    };
    let new_router = Arc::new(LlmRouter::new(config.llm.clone()));
    state.agent.replace_router(new_router);

    Ok(())
}

#[tauri::command]
pub async fn export_history(
    state: State<'_, Arc<AppState>>,
    start_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
) -> Result<String, String> {
    let tasks = state
        .db
        .search_tasks_filtered(
            None,
            status.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            10000,
            0,
        )
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "count": tasks.len(),
        "tasks": tasks,
    }))
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_conversation_memory(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<Vec<haven_memory::repositories::messages::Message>, String> {
    state
        .db
        .get_session_messages(&session_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_conversation(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // Close the current session and create a new one
    let _ = state.db.close_active_session();
    let _ = state
        .db
        .get_or_create_active_session()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// M6-04: Fact management commands
#[tauri::command]
pub async fn list_facts(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<haven_memory::repositories::facts::Fact>, String> {
    state.db.list_facts().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_fact(
    state: State<'_, Arc<AppState>>,
    subject: String,
    predicate: String,
    object: String,
    tags: Option<Vec<String>>,
) -> Result<haven_memory::repositories::facts::Fact, String> {
    let tags_owned = tags.unwrap_or_default();
    let tags: Vec<&str> = tags_owned.iter().map(|s| s.as_str()).collect();
    state
        .db
        .insert_fact(&subject, &predicate, &object, "user", 1.0, &tags)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, Arc<AppState>>, fact_id: String) -> Result<(), String> {
    state.db.delete_fact(&fact_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_preference(
    state: State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<String>, String> {
    state.db.get_preference(&key).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_preferences(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<(String, String)>, String> {
    state.db.list_preferences().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_preference(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    state
        .db
        .set_preference(&key, &value)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_preference(state: State<'_, Arc<AppState>>, key: String) -> Result<(), String> {
    state.db.delete_preference(&key).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<haven_common::config::Settings, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = state.config_loader.lock().map_err(|e| e.to_string())?;
    let settings = cfg.settings();
    Ok(settings)
}

#[tauri::command]
pub async fn update_settings(
    settings: haven_common::config::Settings,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    let old_hotkey = {
        let cfg = state.config_loader.lock().map_err(|e| e.to_string())?;
        cfg.config().hotkey.key_binding.clone()
    };

    {
        let mut loader = state.config_loader.lock().map_err(|e| e.to_string())?;
        loader.apply_settings(&settings);
        loader.save().map_err(|e| e.to_string())?;
    }

    // Propagate audio config to running pipeline
    state.pipeline.update_config(settings.audio).await;

    // Propagate STT client change
    use haven_tools::stt::{LlmSttAdapter, McpSttClient};
    match settings.stt.provider.as_str() {
        "mcp" => {
            if let Some(ref server_name) = settings.stt.mcp_server {
                let client = McpSttClient::new(
                    state.tools.mcp_manager.clone(),
                    server_name,
                    settings.stt.timeout_secs,
                );
                state.pipeline.set_stt_client(Box::new(client)).await;
            }
        }
        "llm" => {
            state.pipeline.set_stt_client(Box::new(LlmSttAdapter)).await;
        }
        _ => {}
    }

    // Reload MCP servers from config
    let (mcp_servers, mcp_discovery, task_max_steps, llm_config, min_risk_level) = {
        let cfg = state.config_loader.lock().map_err(|e| e.to_string())?;
        let config = cfg.config();
        (
            config.mcp_servers.clone(),
            config.mcp_discovery.clone(),
            config.task.max_steps,
            config.llm.clone(),
            config.security.min_risk_level,
        )
    };
    state.tools.load_mcp_from_config(&mcp_servers).await;
    state.tools.mcp_manager.start_monitors(&mcp_discovery).await;
    state.tools.rebuild_catalog().await;
    state.agent.set_max_steps(task_max_steps);
    let new_router = Arc::new(LlmRouter::new(llm_config));
    state.agent.replace_router(new_router);
    state
        .tools
        .safety_gateway
        .set_min_risk_level(min_risk_level)
        .await;

    // Propagate log level to tracing subscriber (console + file)
    let level = settings.log.level.as_str();
    for handle in &state.log_filter_handles {
        let _ = handle.modify(|filter| {
            *filter = EnvFilter::new(format!("haven={}", level));
        });
    }

    // Propagate hotkey mode change (always)
    use haven_common::types::HotkeyMode;
    state
        .shell
        .set_hold_mode(settings.hotkey.mode == HotkeyMode::Hold)
        .await;

    if settings.hotkey.key_binding != old_hotkey {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

        if let Some(old_shortcut) = crate::parse_shortcut(&old_hotkey) {
            let _ = app.global_shortcut().unregister(old_shortcut);
        }

        if let Some(new_shortcut) = crate::parse_shortcut(&settings.hotkey.key_binding) {
            let result =
                app.global_shortcut()
                    .on_shortcut(new_shortcut, move |_app, _sc, event| {
                        let state = _app.state::<Arc<AppState>>();
                        let shell = &state.shell;
                        tokio::task::block_in_place(|| {
                            let rt = tokio::runtime::Handle::current();
                            let shell_state = rt.block_on(shell.get_state());
                            if shell_state.is_muted {
                                return;
                            }
                            if shell_state.hold_mode {
                                if event.state == ShortcutState::Pressed {
                                    rt.block_on(shell.hold_press());
                                } else {
                                    rt.block_on(shell.hold_release());
                                }
                            } else {
                                if event.state == ShortcutState::Pressed {
                                    rt.block_on(shell.toggle_recording());
                                }
                            }
                        });
                    });

            match result {
                Ok(()) => {
                    tracing::info!(
                        "Hotkey rebound: {} -> {}",
                        old_hotkey,
                        settings.hotkey.key_binding,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Hotkey rebind conflict: {} - {}",
                        settings.hotkey.key_binding,
                        e,
                    );
                }
            }
        }

        let _ = app.emit(
            "hotkey:rebind",
            serde_json::json!({
                "old_binding": old_hotkey,
                "new_binding": settings.hotkey.key_binding,
            }),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn enable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    // Debug builds load from devUrl (localhost:4721) — autostart would
    // launch the binary without the Vite dev server, showing a blank/
    // connection-error page.  Only release builds embed the frontend
    // and can be safely autostarted.
    if cfg!(debug_assertions) {
        return Err("自动启动仅支持生产版本（cargo tauri build）。开发模式下请手动运行。".into());
    }
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().enable().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn disable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().disable().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn is_autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct TaskReviewResponse {
    pub task: Task,
    pub messages: Vec<Message>,
    pub steps: Vec<TaskStep>,
}

#[tauri::command]
pub async fn get_task_for_review(
    state: State<'_, Arc<AppState>>,
    task_id: String,
) -> Result<TaskReviewResponse, String> {
    let task = state
        .db
        .get_task(&task_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Task not found: {}", task_id))?;

    let messages = if let Some(ref session_id) = task.session_id {
        if !session_id.is_empty() {
            state
                .db
                .get_session_messages(session_id)
                .map_err(|e| e.to_string())?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let steps = state
        .db
        .get_task_steps(&task_id)
        .map_err(|e| e.to_string())?;

    Ok(TaskReviewResponse {
        task,
        messages,
        steps,
    })
}

/// Roll back a task to a specific branch point. The task is rewound to
/// the saved state at that step. When `pause` is true the task is set to
/// Paused (user wants to edit the message before re-sending); otherwise it
/// is set to Pending for immediate re-execution.
#[tauri::command]
pub async fn rollback_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    target_step: u32,
    pause: Option<bool>,
) -> Result<(), String> {
    state
        .agent
        .rollback_task(&task_id, target_step, pause.unwrap_or(false))
        .await
        .map_err(|e| e.to_string())
}

/// Branch a task from a specific step into a new conversation. Copies all
/// messages up to that step into a new child session and creates a new Paused
/// task seeded with the branch point state. Returns the new task id.
#[tauri::command]
pub async fn branch_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    target_step: u32,
) -> Result<String, String> {
    state
        .agent
        .branch_task(&task_id, target_step)
        .await
        .map_err(|e| e.to_string())
}

/// Resume a task that errored mid-step. Removes partial output persisted on
/// error and sets the task to Pending so the dispatcher retries the failed
/// step from the saved snapshot.
#[tauri::command]
pub async fn continue_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
) -> Result<(), String> {
    state
        .agent
        .continue_task(&task_id)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_list_response_serde() {
        let resp = TaskListResponse { tasks: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"tasks":[]}"#);
    }

    #[test]
    fn test_tool_list_response_serde() {
        let tools = vec![serde_json::json!({"name": "file", "description": "File operations"})];
        let resp = ToolListResponse { tools };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("file"));
        assert!(json.contains("File operations"));
    }

    #[test]
    fn test_recording_state_default() {
        let state = RecordingState {
            is_recording: false,
            is_toggle: false,
        };
        assert!(!state.is_recording);
        assert!(!state.is_toggle);
    }

    #[test]
    fn test_recording_state_serde() {
        let state = RecordingState {
            is_recording: true,
            is_toggle: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"is_recording\":true"));
        assert!(json.contains("\"is_toggle\":true"));
    }
}
