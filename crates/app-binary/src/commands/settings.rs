use crate::app_state::{AppState, BootstrapStatus};
use crate::chatgpt_plan_login::{LoginStart, serialize_credential};
use crate::commands::{log_err, validate_command_id};
use crate::config_runtime::{
    PreparedRouterRuntime, RouterRuntimePublishError, SettingsRuntimeApplyCoordinator,
    SettingsRuntimeApplyOutcome, SettingsRuntimeApplyPhase, apply_log_level_to_handles,
};
use crate::events::{HOTKEY_REBIND_EVENT, HotkeyRebindEvent};
use crate::runtime::ApplicationRuntime;
use haven_common::types::{PermissionEffect, PermissionTarget, ShellChoice};
use serde::Serialize;
use std::future::Future;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;

struct SettingsRuntimeApplyContext {
    old_hotkey: String,
    snapshot: haven_common::config::ConfigSnapshot,
    change: haven_common::config::ConfigChanged,
}

#[derive(Clone)]
struct SettingsRuntimePhaseContext {
    command_name: &'static str,
    state: Arc<AppState>,
    app: tauri::AppHandle,
    snapshot: Arc<haven_common::config::ConfigSnapshot>,
    old_hotkey: Arc<String>,
    prepared_router: Arc<std::sync::Mutex<Option<PreparedRouterRuntime>>>,
    timing: Arc<SettingsRuntimeApplyTiming>,
}

struct SettingsRuntimeApplyTiming {
    started: std::time::Instant,
    last: std::sync::Mutex<std::time::Instant>,
}

fn spawn_settings_hotkey_task<F>(runtime: &ApplicationRuntime, task: F) -> bool
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime.spawn("global-hotkey", task)
}

impl SettingsRuntimeApplyTiming {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last: std::sync::Mutex::new(now),
        }
    }

    fn tick(&self, name: &str) {
        let now = std::time::Instant::now();
        let mut last = self.last.lock().expect("settings timing mutex poisoned");
        tracing::info!("update_settings: {name} += {:?}", now.duration_since(*last));
        *last = now;
    }

    fn log_total(&self) {
        tracing::info!("update_settings: TOTAL {:?}", self.started.elapsed());
    }
}

fn apply_settings_edit(
    config_service: &haven_common::config::ConfigService,
    settings: &haven_common::config::SettingsPatch,
) -> anyhow::Result<Option<SettingsRuntimeApplyContext>> {
    let update = config_service.edit(|config| {
        let old_hotkey = config.hotkey.key_binding.clone();
        validate_settings_payload(settings, config)?;
        config.apply_settings_patch(settings);
        Ok(old_hotkey)
    })?;

    let Some(change) = update.change else {
        return Ok(None);
    };
    Ok(Some(SettingsRuntimeApplyContext {
        old_hotkey: update.value,
        snapshot: update.snapshot,
        change,
    }))
}

#[tauri::command]
pub async fn get_settings(
    app: tauri::AppHandle,
) -> Result<haven_common::config::SettingsSnapshot, String> {
    let state = app.state::<Arc<AppState>>();
    state
        .runtime
        .config_service
        .settings()
        .map_err(|e| log_err("get_settings", e))
}

#[derive(Serialize)]
pub struct LifecycleHooksSettingsResponse {
    pub hooks: haven_common::config::LifecycleHooksConfig,
    pub trusted_hook_workspaces: Vec<String>,
    pub resource_policy: haven_common::config::ResourcePolicy,
    pub config_version: u64,
    pub diagnostics: Vec<haven_common::hooks::LifecycleHookEventDiagnostics>,
}

#[tauri::command]
pub async fn get_lifecycle_hooks_settings(
    state: State<'_, Arc<AppState>>,
) -> Result<LifecycleHooksSettingsResponse, String> {
    let snapshot = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|error| log_err("get_lifecycle_hooks_settings", error))?;
    let config = &snapshot.config;
    let resource_policy = state.runtime.tools.lifecycle_hook_resource_policy().await;
    Ok(LifecycleHooksSettingsResponse {
        hooks: config.hooks.clone(),
        trusted_hook_workspaces: config
            .trusted_hook_workspaces
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        resource_policy,
        config_version: snapshot.version,
        diagnostics: state.runtime.tools.lifecycle_hook_diagnostics(),
    })
}

#[tauri::command]
pub async fn save_lifecycle_hooks(
    state: State<'_, Arc<AppState>>,
    hooks: haven_common::config::LifecycleHooksConfig,
) -> Result<(), String> {
    state
        .runtime
        .config_service
        .apply_patch(haven_common::config::ConfigPatch::LifecycleHooks(hooks))
        .map(|_| ())
        .map_err(|error| log_err("save_lifecycle_hooks", error))
}

/// Trust a project hook root selected through the native folder picker.
#[tauri::command]
pub async fn trust_hook_workspace(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .pick_folder(move |selection| {
            let _ = sender.send(selection);
        });
    let Some(selection) = receiver
        .await
        .map_err(|_| log_err("trust_hook_workspace", "directory picker closed"))?
    else {
        return Ok(None);
    };
    let selected_path = selection
        .into_path()
        .map_err(|_| log_err("trust_hook_workspace", "invalid local directory"))?;
    let canonical_path = std::fs::canonicalize(&selected_path)
        .map_err(|_| log_err("trust_hook_workspace", "selected directory is unavailable"))?;
    let canonical_path = haven_common::path::normalize_absolute_local_path(&canonical_path)
        .filter(|path| path.is_dir())
        .ok_or_else(|| {
            log_err(
                "trust_hook_workspace",
                "selected path is not a local directory",
            )
        })?;
    state
        .runtime
        .config_service
        .apply_patch(haven_common::config::ConfigPatch::TrustHookWorkspace(
            canonical_path.clone(),
        ))
        .map_err(|error| log_err("trust_hook_workspace", error))?;
    Ok(Some(canonical_path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn untrust_hook_workspace(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<(), String> {
    state
        .runtime
        .config_service
        .apply_patch(haven_common::config::ConfigPatch::UntrustHookWorkspace(
            std::path::PathBuf::from(path),
        ))
        .map(|_| ())
        .map_err(|error| log_err("untrust_hook_workspace", error))
}

/// Let the user choose a candidate execution root. The selected path is
/// validated by Tools and returned to the SettingsSnapshot draft; this command does
/// not persist it or grant renderer file access.
#[tauri::command]
pub async fn pick_execution_workspace_directory(
    window: tauri::WebviewWindow,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .pick_folder(move |selection| {
            let _ = sender.send(selection);
        });

    let Some(selection) = receiver.await.map_err(|_| {
        log_err(
            "pick_execution_workspace_directory",
            "directory picker closed",
        )
    })?
    else {
        return Ok(None);
    };
    let selected_path = selection.into_path().map_err(|_| {
        log_err(
            "pick_execution_workspace_directory",
            "invalid local directory",
        )
    })?;
    let normalized_path = state
        .runtime
        .tools
        .validate_execution_workspace(&selected_path)
        .map_err(|error| log_err("pick_execution_workspace_directory", error))?;
    let path = normalized_path.to_str().ok_or_else(|| {
        log_err(
            "pick_execution_workspace_directory",
            "invalid local directory",
        )
    })?;
    Ok(Some(path.to_string()))
}

/// Write a replacement provider key to secure storage first. The returned
/// value is an opaque `cred-*` reference; secret values never enter the
/// SettingsPatch payload.
#[tauri::command]
pub async fn stage_provider_credential(
    state: State<'_, Arc<AppState>>,
    provider_name: String,
    api_key: String,
) -> Result<String, String> {
    state
        .runtime
        .config_service
        .stage_provider_credential(&provider_name, &api_key)
        .map_err(|error| log_err("stage_provider_credential", error))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptPlanLoginComplete {
    credential_ref: String,
    account_email: Option<String>,
    plan_usage_enabled: bool,
}

/// Start the official Sign in with ChatGPT OAuth flow on a local loopback
/// callback. The renderer opens the returned URL through `open_external`.
#[tauri::command]
pub async fn start_chatgpt_plan_login(
    state: State<'_, Arc<AppState>>,
    provider_name: String,
) -> Result<LoginStart, String> {
    state
        .chatgpt_plan_login
        .start(
            &provider_name,
            &state.runtime.config_service,
            &state.runtime.chatgpt_plan_auth,
        )
        .await
        .map_err(|error| log_err("start_chatgpt_plan_login", error))
}

/// Complete OAuth after the browser returns to Haven's loopback listener.
/// Only the credential reference and validated account email cross IPC.
#[tauri::command]
pub async fn complete_chatgpt_plan_login(
    state: State<'_, Arc<AppState>>,
    login_id: String,
    provider_name: String,
) -> Result<ChatGptPlanLoginComplete, String> {
    let result = state
        .chatgpt_plan_login
        .complete(&login_id, &provider_name)
        .await
        .map_err(|error| log_err("complete_chatgpt_plan_login", error))?;
    let serialized = serialize_credential(&result.credential)
        .map_err(|error| log_err("complete_chatgpt_plan_login", error))?;
    let credential_ref = state
        .runtime
        .config_service
        .stage_chatgpt_plan_credential(&provider_name, &serialized)
        .map_err(|error| log_err("complete_chatgpt_plan_login", error))?;
    Ok(ChatGptPlanLoginComplete {
        credential_ref,
        account_email: result.account_email,
        plan_usage_enabled: result.plan_usage_enabled,
    })
}

/// Cancel one in-flight ChatGPT login and close its loopback callback listener.
#[tauri::command]
pub async fn cancel_chatgpt_plan_login(
    state: State<'_, Arc<AppState>>,
    login_id: String,
) -> Result<(), String> {
    state.chatgpt_plan_login.cancel(&login_id).await;
    Ok(())
}

/// Remove one ChatGPT credential staged by a login result the settings draft abandoned.
#[tauri::command]
pub async fn discard_staged_chatgpt_plan_credential(
    state: State<'_, Arc<AppState>>,
    credential_ref: String,
) -> Result<(), String> {
    state
        .runtime
        .config_service
        .discard_staged_chatgpt_plan_credential(&credential_ref)
        .map_err(|error| log_err("discard_staged_chatgpt_plan_credential", error))
}

/// Reassign one staged ChatGPT credential to the provider name in the current Settings draft.
#[tauri::command]
pub async fn reassign_staged_chatgpt_plan_credential(
    state: State<'_, Arc<AppState>>,
    credential_ref: String,
    provider_name: String,
) -> Result<(), String> {
    state
        .runtime
        .config_service
        .reassign_staged_chatgpt_plan_credential(&credential_ref, &provider_name)
        .map_err(|error| log_err("reassign_staged_chatgpt_plan_credential", error))
}

/// Store a replacement Alibaba AI Search key securely for one model config.
#[tauri::command]
pub async fn stage_independent_search_credential(
    state: State<'_, Arc<AppState>>,
    model_id: String,
    api_key: String,
) -> Result<String, String> {
    state
        .runtime
        .config_service
        .stage_independent_search_credential(&model_id, &api_key)
        .map_err(|error| log_err("stage_independent_search_credential", error))
}

/// Write one OCR credential to secure storage and return its opaque reference.
#[tauri::command]
pub async fn stage_ocr_credential(
    state: State<'_, Arc<AppState>>,
    field: haven_common::config::OcrCredentialField,
    value: String,
) -> Result<String, String> {
    state
        .runtime
        .config_service
        .stage_ocr_credential(field, &value)
        .map_err(|error| log_err("stage_ocr_credential", error))
}

/// Remove secure values staged by an unsaved SettingsPatch edit.
#[tauri::command]
pub async fn discard_staged_credentials(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .runtime
        .config_service
        .discard_staged_credentials()
        .map_err(|error| log_err("discard_staged_credentials", error))
}

fn validate_settings_payload(
    settings: &haven_common::config::SettingsPatch,
    current: &haven_common::config::AppConfig,
) -> anyhow::Result<()> {
    if settings.llm.as_ref().is_some_and(|llm| {
        llm.providers
            .iter()
            .any(|provider| !provider.api_key.is_empty())
    }) || settings
        .media
        .as_ref()
        .is_some_and(|media| !media.ocr.api_key.is_empty() || !media.ocr.api_secret.is_empty())
    {
        anyhow::bail!(
            "credential values must be staged in secure storage before Settings can be saved"
        );
    }
    let mut candidate = current.clone();
    let mut validation_patch = settings.clone();
    validation_patch.llm = None;
    candidate.apply_settings_patch(&validation_patch);
    if let Some(llm) = &settings.llm {
        candidate.llm = llm.clone();
    }
    let snapshot = haven_common::config::SettingsSnapshot::from(&candidate);
    haven_common::config::validate_settings_contract(&snapshot)
}

/// Cold-start progress for the titlebar status chip (`loading` | `ready`).
/// The frontend also listens for `app:bootstrap`; this command covers the
/// race where the UI mounts after the ready event already fired.
#[tauri::command]
pub async fn get_bootstrap_status(app: tauri::AppHandle) -> Result<BootstrapStatus, String> {
    let state = app.state::<Arc<AppState>>();
    Ok(state.bootstrap_status())
}

/// Executes one planned phase against its existing runtime owner. The typed
/// coordinator controls ordering; this callback keeps Router preparation ahead
/// of live updates and security ahead of MCP config reloads.
async fn execute_settings_apply_phase(
    phase: SettingsRuntimeApplyPhase,
    context: SettingsRuntimePhaseContext,
) -> SettingsRuntimeApplyOutcome {
    let SettingsRuntimePhaseContext {
        command_name,
        state,
        app,
        snapshot,
        old_hotkey,
        prepared_router,
        timing,
    } = context;
    let config = &snapshot.config;
    match phase {
        SettingsRuntimeApplyPhase::RouterPrepare => match state
            .runtime
            .config_runtime_coordinator
            .prepare_router_runtime(&state, &snapshot, command_name)
        {
            Ok(prepared) => {
                *prepared_router
                    .lock()
                    .expect("prepared router mutex poisoned") = Some(prepared);
                timing.tick("config apply");
                SettingsRuntimeApplyOutcome::applied()
            }
            Err(error) => SettingsRuntimeApplyOutcome::failed_already_rendered(command_name, error),
        },
        SettingsRuntimeApplyPhase::InputPipeline => {
            state
                .runtime
                .input_pipeline
                .update_audio_config(config.media.audio.clone())
                .await;
            timing.tick("input_pipeline.update_audio_config");
            match state
                .runtime
                .agent
                .set_media_strategy(config.media.input_strategy)
            {
                Ok(()) => SettingsRuntimeApplyOutcome::applied(),
                Err(error) => SettingsRuntimeApplyOutcome::failed(command_name, error),
            }
        }
        SettingsRuntimeApplyPhase::Shell => {
            let result = state
                .runtime
                .tools
                .set_default_shell(config.default_shell)
                .await;
            timing.tick("set_default_shell");
            match result {
                Ok(_) => SettingsRuntimeApplyOutcome::applied(),
                Err(error) => {
                    SettingsRuntimeApplyOutcome::failed_catalog_rebuild(command_name, error)
                }
            }
        }
        SettingsRuntimeApplyPhase::Security => {
            if let Err(error) = state.runtime.tools.apply_security(&config.security).await {
                return SettingsRuntimeApplyOutcome::failed(command_name, error);
            }
            match state
                .runtime
                .executor
                .restore_session_authorization_grants()
                .await
            {
                Ok(_) => {
                    state
                        .last_fully_applied_security_config_version
                        .store(snapshot.version, std::sync::atomic::Ordering::Release);
                    timing.tick("apply_security");
                    SettingsRuntimeApplyOutcome::applied()
                }
                Err(error) => SettingsRuntimeApplyOutcome::failed(command_name, error),
            }
        }
        SettingsRuntimeApplyPhase::McpConfig => {
            state
                .runtime
                .tools
                .load_mcp_from_config(&config.mcp_servers)
                .await;
            timing.tick("load_mcp_from_config");
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::McpMonitors => {
            state
                .runtime
                .services
                .mcp
                .start_monitors(&config.mcp_discovery)
                .await;
            timing.tick("mcp_runtime.start_monitors");
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::RouterPublish => {
            let prepared = prepared_router
                .lock()
                .expect("prepared router mutex poisoned")
                .take()
                .expect("router target always has a prepared runtime");
            let result = state
                .runtime
                .config_runtime_coordinator
                .publish_router_runtime(&state, prepared)
                .await;
            timing.tick("publish_router_runtime");
            match result {
                Ok(()) => {
                    crate::commands::emit_llm_config_changed(&app);
                    SettingsRuntimeApplyOutcome::applied()
                }
                Err(RouterRuntimePublishError::ToolCatalog { source: error, .. }) => {
                    crate::commands::emit_llm_config_changed(&app);
                    SettingsRuntimeApplyOutcome::failed_catalog_rebuild_after_router_publish(
                        command_name,
                        error,
                    )
                }
                Err(error @ RouterRuntimePublishError::ModelMedia(_)) => {
                    SettingsRuntimeApplyOutcome::failed(command_name, error)
                }
            }
        }
        SettingsRuntimeApplyPhase::ContextLimits => {
            if let Err(error) = state
                .runtime
                .input_pipeline
                .set_ring_buffer_capacity_secs(config.context_limits.input_ring_buffer_secs)
            {
                return SettingsRuntimeApplyOutcome::failed(command_name, error);
            }
            let result = state
                .runtime
                .tools
                .set_context_limits(config.context_limits.clone())
                .await;
            timing.tick("set_context_limits");
            match result {
                Err(error) => {
                    SettingsRuntimeApplyOutcome::failed_catalog_rebuild(command_name, error)
                }
                Ok(_) => match state
                    .runtime
                    .agent
                    .set_context_limits(config.context_limits.clone())
                {
                    Ok(()) => SettingsRuntimeApplyOutcome::applied(),
                    Err(error) => SettingsRuntimeApplyOutcome::failed(command_name, error),
                },
            }
        }
        SettingsRuntimeApplyPhase::FactInference => {
            state
                .runtime
                .agent
                .set_fact_inference_enabled(config.memory.fact_inference_enabled);
            timing.tick("set_fact_inference_enabled");
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::SessionRuntime => {
            if let Err(error) = state
                .runtime
                .agent
                .set_max_steps_per_run(config.session.max_steps_per_run)
            {
                return SettingsRuntimeApplyOutcome::failed(command_name, error);
            }
            if let Err(error) = state
                .runtime
                .agent
                .set_max_steps_per_session(config.session.max_steps_per_session)
            {
                return SettingsRuntimeApplyOutcome::failed(command_name, error);
            }
            if let Err(error) = state
                .runtime
                .executor
                .set_max_concurrent(config.session.max_concurrent)
            {
                return SettingsRuntimeApplyOutcome::failed(command_name, error);
            }
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::ToolSettings => {
            let result = state
                .runtime
                .tools
                .set_tool_settings(config.tool_settings.clone())
                .await;
            match result {
                Ok(_) => SettingsRuntimeApplyOutcome::applied(),
                Err(error) => {
                    SettingsRuntimeApplyOutcome::failed_catalog_rebuild(command_name, error)
                }
            }
        }
        SettingsRuntimeApplyPhase::Skills => match state
            .runtime
            .services
            .skills
            .set_config(config.skills.root.clone(), config.skills.enabled.clone())
            .await
        {
            Ok(()) => SettingsRuntimeApplyOutcome::applied(),
            Err(error) => SettingsRuntimeApplyOutcome::failed(command_name, error),
        },
        SettingsRuntimeApplyPhase::Logging => {
            match apply_log_level_to_handles(&state.runtime.log_filter_handles, &config.log.level) {
                Ok(()) => SettingsRuntimeApplyOutcome::applied(),
                Err(error) => SettingsRuntimeApplyOutcome::failed(command_name, error),
            }
        }
        SettingsRuntimeApplyPhase::HotkeyMode => {
            use haven_common::types::HotkeyMode;
            state
                .runtime
                .shell
                .set_hold_mode(config.hotkey.mode == HotkeyMode::Hold)
                .await;
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::HotkeyUnregister => {
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            if let Some(old_shortcut) = haven_input::KeyCombo::parse(&old_hotkey)
                .and_then(|combo| crate::to_tauri_shortcut(&combo))
                && let Err(error) = app.global_shortcut().unregister(old_shortcut)
            {
                return SettingsRuntimeApplyOutcome::failed(command_name, error);
            }
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::HotkeyRegister => {
            use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
            if let Some(new_shortcut) = haven_input::KeyCombo::parse(&config.hotkey.key_binding)
                .and_then(|combo| crate::to_tauri_shortcut(&combo))
            {
                match app.global_shortcut().on_shortcut(
                    new_shortcut,
                    move |_app, _shortcut, event| {
                        let state = _app.state::<Arc<AppState>>();
                        if state
                            .hotkey_capture_active
                            .load(std::sync::atomic::Ordering::Acquire)
                        {
                            return;
                        }
                        let runtime = state.runtime.clone();
                        let shell = state.runtime.shell.clone();
                        let tools = state.runtime.tools.clone();
                        let hotkey_capture_active = state.hotkey_capture_active.clone();
                        let app_h = _app.clone();
                        let pressed = event.state == ShortcutState::Pressed;
                        let accepted = spawn_settings_hotkey_task(&runtime, async move {
                            if hotkey_capture_active
                                .load(std::sync::atomic::Ordering::Acquire)
                            {
                                return;
                            }
                            let shell_state = shell.state().await;
                            if shell_state.is_muted {
                                return;
                            }

                            if pressed && let Some(window) = app_h.get_webview_window("main") {
                                if let Err(error) = window.show() {
                                    tracing::debug!(
                                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                                        "failed to show main window for global hotkey"
                                    );
                                }
                                if let Err(error) = window.set_focus() {
                                    tracing::debug!(
                                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                                        "failed to focus main window for global hotkey"
                                    );
                                }
                            }
                            if !tools.transcription_available().await {
                                return;
                            }
                            if shell_state.hold_mode {
                                if pressed {
                                    shell.hold_press().await;
                                } else {
                                    shell.hold_release().await;
                                }
                            } else if pressed {
                                shell.toggle_recording().await;
                            }
                        });
                        if !accepted {
                            tracing::debug!("dropping global hotkey event after app shutdown");
                        }
                    },
                ) {
                    Ok(()) => tracing::info!(
                        "Hotkey rebound: {} -> {}",
                        old_hotkey,
                        config.hotkey.key_binding,
                    ),
                    Err(error) => {
                        return SettingsRuntimeApplyOutcome::failed(
                            command_name,
                            error,
                        );
                    }
                }
            }
            SettingsRuntimeApplyOutcome::applied()
        }
        SettingsRuntimeApplyPhase::HotkeyRebindEvent => match app.emit(
            HOTKEY_REBIND_EVENT,
            HotkeyRebindEvent {
                old_binding: (*old_hotkey).clone(),
                new_binding: config.hotkey.key_binding.clone(),
            },
        ) {
            Ok(()) => SettingsRuntimeApplyOutcome::applied(),
            Err(error) => SettingsRuntimeApplyOutcome::warning(command_name, error),
        },
    }
}

async fn execute_settings_apply_plan(
    state: Arc<AppState>,
    app: tauri::AppHandle,
    snapshot: haven_common::config::ConfigSnapshot,
    old_hotkey: String,
    command_name: &'static str,
    plan: crate::config_runtime::SettingsRuntimeApplyPlan,
    timing: Arc<SettingsRuntimeApplyTiming>,
) -> (SettingsRuntimeApplyCoordinator, Result<(), String>) {
    let snapshot = Arc::new(snapshot);
    let old_hotkey = Arc::new(old_hotkey);
    let prepared_router: Arc<std::sync::Mutex<Option<PreparedRouterRuntime>>> =
        Arc::new(std::sync::Mutex::new(None));
    let apply_context = SettingsRuntimePhaseContext {
        command_name,
        state,
        app,
        snapshot,
        old_hotkey,
        prepared_router,
        timing,
    };
    let mut apply = SettingsRuntimeApplyCoordinator::from_plan(plan);
    let result = apply
        .apply(move |phase| execute_settings_apply_phase(phase, apply_context.clone()))
        .await;
    (apply, result)
}

#[tauri::command]
pub async fn update_settings(
    settings: haven_common::config::SettingsPatch,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let timing = Arc::new(SettingsRuntimeApplyTiming::new());
    let state = app.state::<Arc<AppState>>();
    let state = Arc::clone(&*state);
    let _apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    if let Some(security) = settings.security.as_ref() {
        for roots in [
            &security.resource_policy.files.write_roots,
            &security.resource_policy.files.path_roots,
        ] {
            state
                .runtime
                .tools
                .validate_resource_roots(roots)
                .map_err(|error| log_err("update_settings resource policy roots", error))?;
        }
    }
    let current_execution_root = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|error| log_err("update_settings execution workspace", error))?
        .config
        .security
        .execution_root;
    if let Some(security) = settings.security.as_ref()
        && security.execution_root != current_execution_root
        && let Some(root) = security.execution_root.as_deref()
    {
        state
            .runtime
            .tools
            .validate_execution_workspace(root)
            .map_err(|error| log_err("update_settings execution workspace", error))?;
    }
    let Some(update) = apply_settings_edit(&state.runtime.config_service, &settings)
        .map_err(|error| log_err("update_settings", error))?
    else {
        return Ok(());
    };
    let SettingsRuntimeApplyContext {
        old_hotkey,
        snapshot,
        change,
    } = update;
    let plan = crate::config_runtime::SettingsRuntimeApplyPlan::from_change(
        &change,
        &snapshot,
        &old_hotkey,
    );
    let (version, live_targets, restart_required_targets, has_router_prepare) = {
        (
            plan.config_version,
            plan.live_targets.clone(),
            plan.restart_required_targets.clone(),
            plan.phases()
                .contains(&SettingsRuntimeApplyPhase::RouterPrepare),
        )
    };
    tracing::debug!(
        version = change.version,
        domains = ?change.domains,
        live = ?live_targets,
        restart_required = ?restart_required_targets,
        "configuration snapshot updated"
    );
    if !restart_required_targets.is_empty() {
        tracing::warn!(
            version,
            targets = ?restart_required_targets,
            "configuration change requires a restart for some consumers"
        );
    }

    if !has_router_prepare {
        timing.tick("config apply");
    }
    state
        .runtime
        .config_runtime_coordinator
        .begin_settings_apply(&plan, &old_hotkey);
    let (apply, result) = execute_settings_apply_plan(
        state.clone(),
        app.clone(),
        snapshot,
        old_hotkey,
        "update_settings",
        plan.clone(),
        timing.clone(),
    )
    .await;
    let failure = apply.failed_phase().zip(result.as_ref().err().cloned());
    state
        .runtime
        .config_runtime_coordinator
        .finish_settings_apply(&plan, apply.completed_phases(), failure);

    if let Err(error) = result {
        let phase = apply.failed_phase_name().unwrap_or("unknown");
        let security_runtime = apply.security_runtime_disposition();
        let security_base_version = state
            .last_fully_applied_security_config_version
            .load(std::sync::atomic::Ordering::Acquire);
        let error = crate::config_runtime::partial_config_apply_error(
            error,
            version,
            phase,
            security_runtime,
            security_base_version,
        );
        return Err(log_err("update_settings", error));
    }

    timing.tick("hotkey section");
    timing.log_total();
    Ok(())
}

#[tauri::command]
pub async fn get_settings_runtime_apply_status(
    app: tauri::AppHandle,
) -> Result<Vec<crate::config_runtime::SettingsRuntimeTargetStatus>, String> {
    let state = app.state::<Arc<AppState>>();
    let _apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    Ok(state
        .runtime
        .config_runtime_coordinator
        .settings_apply_statuses())
}

#[tauri::command]
pub async fn reapply_saved_settings(
    app: tauri::AppHandle,
) -> Result<Vec<crate::config_runtime::SettingsRuntimeTargetStatus>, String> {
    let state = app.state::<Arc<AppState>>();
    let state = Arc::clone(&*state);
    let _apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let snapshot = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|error| log_err("reapply_saved_settings", error))?;
    let Some((plan, old_hotkey)) = state
        .runtime
        .config_runtime_coordinator
        .prepare_settings_reapply(&snapshot)
    else {
        return Ok(state
            .runtime
            .config_runtime_coordinator
            .settings_apply_statuses());
    };

    if plan
        .live_targets
        .contains(&crate::config_runtime::RuntimeConfigTarget::Security)
    {
        let config = &snapshot.config;
        for roots in [
            &config.security.resource_policy.files.write_roots,
            &config.security.resource_policy.files.path_roots,
        ] {
            if let Err(error) = state.runtime.tools.validate_resource_roots(roots) {
                let error = log_err("reapply_saved_settings resource policy roots", error);
                state
                    .runtime
                    .config_runtime_coordinator
                    .finish_settings_apply(
                        &plan,
                        &[],
                        Some((SettingsRuntimeApplyPhase::Security, error.clone())),
                    );
                return Err(error);
            }
        }
        if let Some(root) = config.security.execution_root.as_deref()
            && let Err(error) = state.runtime.tools.validate_execution_workspace(root)
        {
            let error = log_err("reapply_saved_settings execution workspace", error);
            state
                .runtime
                .config_runtime_coordinator
                .finish_settings_apply(
                    &plan,
                    &[],
                    Some((SettingsRuntimeApplyPhase::Security, error.clone())),
                );
            return Err(error);
        }
    }

    let version = snapshot.version;
    let (apply, result) = execute_settings_apply_plan(
        state.clone(),
        app,
        snapshot,
        old_hotkey,
        "reapply_saved_settings",
        plan.clone(),
        Arc::new(SettingsRuntimeApplyTiming::new()),
    )
    .await;
    let failure = apply.failed_phase().zip(result.as_ref().err().cloned());
    state
        .runtime
        .config_runtime_coordinator
        .finish_settings_apply(&plan, apply.completed_phases(), failure);
    if let Err(error) = result {
        let phase = apply.failed_phase_name().unwrap_or("unknown");
        let security_runtime = apply.security_runtime_disposition();
        let security_base_version = state
            .last_fully_applied_security_config_version
            .load(std::sync::atomic::Ordering::Acquire);
        let error = crate::config_runtime::partial_config_apply_error(
            error,
            version,
            phase,
            security_runtime,
            security_base_version,
        );
        return Err(log_err("reapply_saved_settings", error));
    }
    Ok(state
        .runtime
        .config_runtime_coordinator
        .settings_apply_statuses())
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPermissionGrant {
    pub session_id: String,
    pub session_title: Option<String>,
    pub capability: String,
    pub target: PermissionTarget,
    pub effect: PermissionEffect,
    pub resource_scope: Vec<haven_common::types::PermissionResourceScope>,
}

#[tauri::command]
pub async fn list_session_permissions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<SessionPermissionGrant>, String> {
    state
        .runtime
        .session_store
        .list_session_authorization_grants()
        .await
        .map(|grants| {
            grants
                .into_iter()
                .map(|stored| SessionPermissionGrant {
                    session_id: stored.session_id,
                    session_title: stored.session_title,
                    capability: stored.grant.capability.to_string(),
                    target: stored.grant.target,
                    effect: stored.grant.effect,
                    resource_scope: stored.grant.resource_scope,
                })
                .collect()
        })
        .map_err(|error| log_err("list_session_permissions", error))
}

#[tauri::command]
pub async fn revoke_permission(state: State<'_, Arc<AppState>>, key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(log_err(
            "revoke_permission",
            "permission key cannot be empty",
        ));
    }
    let _config_apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let previous = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|error| log_err("revoke_permission", error))?
        .config
        .security
        .permissions
        .into_iter()
        .find(|permission| permission.key == key);
    state
        .runtime
        .services
        .authorization
        .revoke_permanent(&key)
        .await;
    let edit = state.runtime.config_service.edit(|config| {
        config
            .security
            .permissions
            .retain(|permission| permission.key != key);
        Ok(())
    });
    if let Err(error) = edit {
        if let Some(permission) = previous {
            state
                .runtime
                .services
                .authorization
                .grant(
                    None,
                    haven_common::types::CapabilityScope::try_new(&permission.key)
                        .map_err(|error| log_err("revoke_permission", error))?,
                    permission.effect,
                    haven_common::types::PermissionScope::Always,
                    permission.resource_scope.clone(),
                )
                .await;
        }
        return Err(log_err("revoke_permission", error));
    }
    Ok(())
}

/// Remove all permanent permission rules and restore the selected default
/// policy. Session-scoped decisions and the policy mode remain unchanged.
#[tauri::command]
pub async fn reset_permissions(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let _config_apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let previous = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|error| log_err("reset_permissions", error))?
        .config
        .security
        .permissions;
    state.runtime.services.authorization.clear_permanent().await;
    if let Err(error) = state.runtime.config_service.edit(|config| {
        config.security.permissions.clear();
        Ok(())
    }) {
        for permission in previous {
            state
                .runtime
                .services
                .authorization
                .grant(
                    None,
                    if let Ok(capability) =
                        haven_common::types::CapabilityScope::try_new(&permission.key)
                    {
                        capability
                    } else {
                        continue;
                    },
                    permission.effect,
                    haven_common::types::PermissionScope::Always,
                    permission.resource_scope.clone(),
                )
                .await;
        }
        return Err(log_err("reset_permissions", error));
    }
    Ok(())
}

#[tauri::command]
pub async fn revoke_session_permission(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    capability: String,
) -> Result<(), String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("revoke_session_permission", error))?;
    let _config_apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let capability = capability.trim().to_string();
    let capability = haven_common::types::CapabilityScope::try_new(capability)
        .map_err(|error| log_err("revoke_session_permission", error))?;
    let existing = state
        .runtime
        .session_store
        .session_authorization_grants(&session_id)
        .await
        .map_err(|error| log_err("revoke_session_permission", error))?
        .into_iter()
        .find(|grant| grant.capability == capability);
    let Some(existing) = existing else {
        return Ok(());
    };
    state
        .runtime
        .services
        .authorization
        .revoke_session_grant(&session_id, &capability)
        .await;
    if let Err(error) = state
        .runtime
        .session_store
        .revoke_session_authorization_grant(&session_id, capability.clone())
        .await
    {
        state
            .runtime
            .services
            .authorization
            .grant(
                Some(&session_id),
                existing.capability,
                existing.effect,
                existing.scope,
                existing.resource_scope,
            )
            .await;
        return Err(log_err("revoke_session_permission", error));
    }
    Ok(())
}

#[tauri::command]
pub async fn reset_session_permissions(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let _config_apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let previous = state
        .runtime
        .session_store
        .list_session_authorization_grants()
        .await
        .map_err(|error| log_err("reset_session_permissions", error))?;
    state.runtime.services.authorization.clear_all_trust().await;
    let removed = match state
        .runtime
        .session_store
        .clear_session_authorization_grants()
        .await
    {
        Ok(removed) => removed,
        Err(error) => {
            for stored in previous {
                state
                    .runtime
                    .services
                    .authorization
                    .grant(
                        Some(&stored.session_id),
                        stored.grant.capability,
                        stored.grant.effect,
                        stored.grant.scope,
                        stored.grant.resource_scope,
                    )
                    .await;
            }
            return Err(log_err("reset_session_permissions", error));
        }
    };
    Ok(removed)
}

#[tauri::command]
pub async fn check_shell_available(shell: ShellChoice) -> Result<ShellAvailability, String> {
    #[cfg(windows)]
    let available = match shell {
        ShellChoice::Cmd | ShellChoice::Powershell => true,
        ShellChoice::Pwsh => shell_on_path("pwsh.exe"),
    };
    #[cfg(not(windows))]
    let available = match shell {
        ShellChoice::Cmd | ShellChoice::Powershell => true,
        ShellChoice::Pwsh => shell_on_path("pwsh"),
    };
    Ok(ShellAvailability { available })
}

#[derive(Debug, serde::Serialize)]
pub struct ShellAvailability {
    pub available: bool,
}

/// True when `name` resolves to an executable on PATH.
fn shell_on_path(name: &str) -> bool {
    #[cfg(windows)]
    let probe = command_output(std::process::Command::new("where.exe").arg(name));
    #[cfg(not(windows))]
    let probe = command_output(std::process::Command::new("which").arg(name));
    probe.map(|o| o.status.success()).unwrap_or(false)
}

fn command_output(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    let child = {
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
        command.spawn()?
    };
    child.wait_with_output()
}

#[tauri::command]
pub async fn enable_autostart() -> Result<(), String> {
    // Debug builds load from devUrl (localhost:4721) — autostart would
    // launch the binary without the Vite dev server, showing a blank/
    // connection-error page.  Only release builds embed the frontend
    // and can be safely autostarted.
    if cfg!(debug_assertions) {
        return Err(log_err(
            "enable_autostart",
            "自动启动仅支持生产版本（cargo tauri build）。开发模式下请手动运行。",
        ));
    }
    crate::autostart::enable().map_err(|error| log_err("enable_autostart", error))
}

#[tauri::command]
pub async fn disable_autostart() -> Result<(), String> {
    crate::autostart::disable().map_err(|error| log_err("disable_autostart", error))
}

#[tauri::command]
pub async fn is_autostart_enabled() -> Result<bool, String> {
    crate::autostart::is_enabled().map_err(|error| log_err("is_autostart_enabled", error))
}

#[cfg(test)]
mod tests {
    use super::{
        SessionPermissionGrant, ShellAvailability, apply_settings_edit, spawn_settings_hotkey_task,
        validate_settings_payload,
    };
    use crate::app_state::AppState;
    use crate::config_runtime::{
        SettingsRuntimeApplyCoordinator, SettingsRuntimeApplyOutcome, SettingsRuntimeApplyPhase,
    };
    use haven_common::config::{
        AppConfig, ConfigLoader, ConfigService, InMemoryCredentialStore, LogLevel, SettingsPatch,
        SettingsSnapshot, StoredPermission,
    };
    use haven_common::types::{PermissionEffect, PermissionTarget, ShellChoice};

    fn config_service_with_config(config: AppConfig) -> (ConfigService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let loader = ConfigLoader::load_from(&path).unwrap();
        let service = ConfigService::new_with_credential_store(
            loader,
            std::sync::Arc::new(InMemoryCredentialStore::default()),
        )
        .unwrap();
        service
            .edit(move |current| {
                *current = config;
                Ok(())
            })
            .unwrap();
        (service, dir)
    }

    #[tokio::test]
    async fn settings_hotkey_task_can_be_submitted_from_callback_thread() {
        let dir = tempfile::tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let state = AppState::new_for_test(&dir.path().join("test.db"), vec![], loader, dir.path())
            .await
            .unwrap();
        let runtime = state.runtime.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let accepted = std::thread::spawn(move || {
            spawn_settings_hotkey_task(&runtime, async move {
                let _ = tx.send(());
            })
        })
        .join()
        .unwrap();

        assert!(accepted);
        tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .expect("hotkey task should run on the app runtime")
            .expect("hotkey task should complete");
        state.runtime.shutdown().await;
    }

    #[test]
    fn shell_availability_has_a_named_stable_wire_shape() {
        assert_eq!(
            serde_json::to_value(ShellAvailability { available: true }).unwrap(),
            serde_json::json!({"available": true})
        );
    }

    #[test]
    fn shell_availability_input_uses_the_configured_shell_vocabulary() {
        for (wire, shell) in [
            ("powershell", ShellChoice::Powershell),
            ("cmd", ShellChoice::Cmd),
            ("pwsh", ShellChoice::Pwsh),
        ] {
            assert_eq!(
                serde_json::from_str::<ShellChoice>(&format!("\"{wire}\"")).unwrap(),
                shell
            );
            assert_eq!(shell.as_str(), wire);
        }

        for wire in ["", "bash", "fish", "CMD"] {
            assert!(serde_json::from_str::<ShellChoice>(&format!("\"{wire}\"")).is_err());
        }
    }

    #[test]
    fn session_permission_grant_serializes_typed_target_and_effect_as_existing_strings() {
        let grant = SessionPermissionGrant {
            session_id: "ses-00000000000000000000000000000000".into(),
            session_title: Some("session title".into()),
            capability: "files.read".into(),
            target: PermissionTarget::Group,
            effect: PermissionEffect::Deny,
            resource_scope: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(grant).unwrap(),
            serde_json::json!({
                "session_id": "ses-00000000000000000000000000000000",
                "session_title": "session title",
                "capability": "files.read",
                "target": "group",
                "effect": "deny",
                "resource_scope": []
            })
        );
    }

    #[test]
    fn settings_boundary_rejects_inline_provider_ocr_and_mcp_values() {
        for payload in [
            serde_json::json!({
                "llm": { "providers": [{ "name": "primary", "api_key": "provider-secret-marker", "media_capabilities": { "stt": false, "tts": false, "image_gen": false } }] }
            }),
            serde_json::json!({
                "media": { "ocr": { "api_key": "ocr-key-marker", "api_secret": "ocr-secret-marker" } }
            }),
        ] {
            let settings: SettingsPatch = serde_json::from_value(payload).unwrap();
            let error = validate_settings_payload(&settings, &AppConfig::default()).unwrap_err();
            for marker in [
                "provider-secret-marker",
                "ocr-key-marker",
                "ocr-secret-marker",
                "mcp-secret-marker",
            ] {
                assert!(!error.to_string().contains(marker));
            }
        }
        assert!(
            serde_json::from_value::<SettingsPatch>(serde_json::json!({
                "mcp_servers": [{ "name": "server", "env": ["TOKEN=mcp-secret-marker"] }]
            }))
            .is_err()
        );
    }

    #[test]
    fn settings_boundary_rejects_unsafe_and_invalid_numeric_values() {
        let mut snapshot = SettingsSnapshot::default();
        snapshot.context_limits.max_upload_total_bytes = 9_007_199_254_740_992;
        let settings = SettingsPatch::from(&snapshot);
        assert!(validate_settings_payload(&settings, &AppConfig::default()).is_err());

        snapshot.context_limits.max_upload_total_bytes = 512 * 1024 * 1024;
        snapshot.media.audio.vad_threshold = f32::INFINITY;
        let settings = SettingsPatch::from(&snapshot);
        assert!(validate_settings_payload(&settings, &AppConfig::default()).is_err());
    }

    #[test]
    fn settings_boundary_requires_each_model_to_reference_a_configured_provider() {
        for llm in [
            serde_json::json!({
                "models": [{ "id": "assistant", "provider_name": "", "model": "model-a" }]
            }),
            serde_json::json!({
                "providers": [{ "name": "primary", "media_capabilities": { "stt": false, "tts": false, "image_gen": false } }],
                "models": [{ "id": "assistant", "provider_name": "removed", "model": "model-a" }]
            }),
        ] {
            let settings: SettingsPatch =
                serde_json::from_value(serde_json::json!({ "llm": llm })).unwrap();
            assert_eq!(
                validate_settings_payload(&settings, &AppConfig::default())
                    .unwrap_err()
                    .to_string(),
                "llm.models[0].provider_name must reference a configured Provider"
            );
        }

        let settings: SettingsPatch = serde_json::from_value(serde_json::json!({
            "llm": {
                "providers": [{ "name": "primary", "media_capabilities": { "stt": false, "tts": false, "image_gen": false } }],
                "models": [{ "id": "assistant", "provider_name": "primary", "model": "model-a" }]
            }
        }))
        .unwrap();
        validate_settings_payload(&settings, &AppConfig::default()).unwrap();
    }

    #[test]
    fn settings_edit_captures_hotkey_and_preserves_live_security_state() {
        let mut live_config = AppConfig::default();
        let mut stale_settings = SettingsSnapshot::from(&live_config);
        live_config.hotkey.key_binding = "Ctrl+Alt+O".into();
        let live_permission = StoredPermission {
            key: "files.read".into(),
            effect: PermissionEffect::Allow,
            resource_scope: vec![haven_common::types::PermissionResourceScope::LocalPath {
                path: std::path::PathBuf::from("C:\\authorized\\file.txt"),
                recursive: false,
                access: haven_common::types::LocalPathAccess::Read,
            }],
        };
        live_config.security.permissions = vec![live_permission.clone()];
        live_config.security.encrypt_sensitive = false;
        stale_settings.hotkey.key_binding = "Ctrl+Alt+N".into();
        let (service, _dir) = config_service_with_config(live_config);

        let update = apply_settings_edit(&service, &SettingsPatch::from(&stale_settings))
            .unwrap()
            .unwrap();

        assert_eq!(update.old_hotkey, "Ctrl+Alt+O");
        assert_eq!(update.snapshot.config.hotkey.key_binding, "Ctrl+Alt+N");
        assert_eq!(
            update.snapshot.config.security.permissions,
            vec![live_permission]
        );
        assert!(!update.snapshot.config.security.encrypt_sensitive);
    }

    #[test]
    fn no_op_settings_edit_does_not_start_runtime_apply() {
        let (service, _dir) = config_service_with_config(AppConfig::default());
        let initial_version = service.snapshot().unwrap().version;
        let settings = service.settings().unwrap();
        let receiver = service.subscribe().unwrap();

        let update = apply_settings_edit(&service, &SettingsPatch::from(&settings)).unwrap();

        assert!(update.is_none());
        assert!(receiver.try_recv().is_err());
        assert_eq!(service.snapshot().unwrap().version, initial_version);
    }

    #[tokio::test]
    async fn failed_settings_apply_keeps_persisted_config_and_identical_edit_skips_retry() {
        use std::sync::{Arc, Mutex};

        let (service, _dir) = config_service_with_config(AppConfig::default());
        let initial_version = service.snapshot().unwrap().version;
        let mut settings = service.settings().unwrap();
        settings.log.level = LogLevel::Debug;
        let changes = service.subscribe().unwrap();
        let patch = SettingsPatch::from(&settings);
        let update = apply_settings_edit(&service, &patch)
            .unwrap()
            .expect("log level change should persist");
        let mut coordinator = SettingsRuntimeApplyCoordinator::new(
            &update.change,
            &update.snapshot,
            &update.old_hotkey,
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_callback = seen.clone();

        let result = coordinator
            .apply(move |phase| {
                let seen = seen_by_callback.clone();
                async move {
                    seen.lock().unwrap().push(phase);
                    if phase == SettingsRuntimeApplyPhase::Logging {
                        SettingsRuntimeApplyOutcome::failed("settings_apply_test", "logging failed")
                    } else {
                        SettingsRuntimeApplyOutcome::applied()
                    }
                }
            })
            .await;

        assert_eq!(result, Err("logging failed".into()));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![SettingsRuntimeApplyPhase::Logging]
        );
        let snapshot = service.snapshot().unwrap();
        let applied_version = initial_version + 1;
        assert_eq!(snapshot.version, applied_version);
        assert_eq!(snapshot.config.log.level, LogLevel::Debug);
        let persisted = ConfigLoader::load_from(&service.path().unwrap()).unwrap();
        assert_eq!(persisted.config().log.level, LogLevel::Debug);
        assert_eq!(changes.try_recv().unwrap().version, applied_version);

        assert!(apply_settings_edit(&service, &patch).unwrap().is_none());
        assert!(changes.try_recv().is_err());
    }
}
