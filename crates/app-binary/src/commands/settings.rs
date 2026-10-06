use crate::app_state::AppState;
use crate::commands::log_err;
use crate::config_runtime::{
    PreparedRouterRuntime, RouterRuntimePublishError, SettingsApplyOutcome, SettingsApplyPhase,
    SettingsRuntimeApplyCoordinator, apply_log_level_to_handles,
};
use crate::events::{HOTKEY_REBIND_EVENT, HotkeyRebindEvent};
use crate::runtime::ApplicationRuntime;
use serde::Serialize;
use std::future::Future;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;

struct SettingsApplyContext {
    old_hotkey: String,
    snapshot: haven_common::config::ConfigSnapshot,
    change: haven_common::config::ConfigChanged,
}

struct SettingsApplyTiming {
    started: std::time::Instant,
    last: std::sync::Mutex<std::time::Instant>,
}

fn spawn_settings_hotkey_task<F>(runtime: &ApplicationRuntime, task: F) -> bool
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime.spawn("global-hotkey", task)
}

impl SettingsApplyTiming {
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
    settings: &haven_common::config::Settings,
) -> anyhow::Result<Option<SettingsApplyContext>> {
    let update = config_service.edit(|config| {
        let old_hotkey = config.hotkey.key_binding.clone();
        config.apply_settings(settings);
        Ok(old_hotkey)
    })?;

    let Some(change) = update.change else {
        return Ok(None);
    };
    Ok(Some(SettingsApplyContext {
        old_hotkey: update.value,
        snapshot: update.snapshot,
        change,
    }))
}

#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<haven_common::config::Settings, String> {
    let state = app.state::<Arc<AppState>>();
    state
        .runtime
        .config_service
        .settings()
        .map_err(|e| log_err("get_settings", e))
}

/// Write a replacement provider key to secure storage first. The returned
/// value is an opaque `cred-*` reference; secret values never enter the
/// Settings update payload.
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

/// Write one OCR credential to secure storage and return its opaque reference.
#[tauri::command]
pub async fn stage_ocr_credential(
    state: State<'_, Arc<AppState>>,
    api_secret: bool,
    value: String,
) -> Result<String, String> {
    state
        .runtime
        .config_service
        .stage_ocr_credential(api_secret, &value)
        .map_err(|error| log_err("stage_ocr_credential", error))
}

/// Remove secure values staged by an unsaved Settings edit.
#[tauri::command]
pub async fn discard_staged_credentials(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .runtime
        .config_service
        .discard_staged_credentials()
        .map_err(|error| log_err("discard_staged_credentials", error))
}

fn validate_settings_payload(settings: &haven_common::config::Settings) -> anyhow::Result<()> {
    if settings
        .llm
        .providers
        .iter()
        .any(|provider| !provider.api_key.is_empty())
        || !settings.media.ocr.api_key.is_empty()
        || !settings.media.ocr.api_secret.is_empty()
    {
        anyhow::bail!(
            "credential values must be staged in secure storage before Settings can be saved"
        );
    }
    if settings
        .mcp_servers
        .iter()
        .any(|server| !server.env.is_empty())
    {
        anyhow::bail!("MCP environment values must be changed through the MCP settings commands");
    }
    for model in &settings.llm.models {
        if model.provider.trim().is_empty()
            || !settings
                .llm
                .providers
                .iter()
                .any(|provider| provider.name == model.provider)
        {
            anyhow::bail!("every configured model must reference a configured Provider");
        }
    }
    Ok(())
}

/// Cold-start progress for the titlebar status chip (`loading` | `ready`).
/// The frontend also listens for `app:bootstrap`; this command covers the
/// race where the UI mounts after the ready event already fired.
#[tauri::command]
pub async fn get_bootstrap_status(app: tauri::AppHandle) -> Result<String, String> {
    let state = app.state::<Arc<AppState>>();
    Ok(state.bootstrap_status().as_str().to_string())
}

/// Executes one planned phase against its existing runtime owner. The typed
/// coordinator controls ordering; this callback keeps Router preparation ahead
/// of live updates and security ahead of MCP config reloads.
async fn execute_settings_apply_phase(
    phase: SettingsApplyPhase,
    state: Arc<AppState>,
    app: tauri::AppHandle,
    snapshot: Arc<haven_common::config::ConfigSnapshot>,
    old_hotkey: Arc<String>,
    prepared_router: Arc<std::sync::Mutex<Option<PreparedRouterRuntime>>>,
    timing: Arc<SettingsApplyTiming>,
) -> SettingsApplyOutcome {
    let config = &snapshot.config;
    match phase {
        SettingsApplyPhase::RouterPrepare => match state
            .runtime
            .config_runtime_coordinator
            .prepare_router_runtime(&state, &snapshot, "update_settings")
        {
            Ok(prepared) => {
                *prepared_router
                    .lock()
                    .expect("prepared router mutex poisoned") = Some(prepared);
                timing.tick("config apply");
                SettingsApplyOutcome::applied()
            }
            Err(error) => SettingsApplyOutcome::failed_already_rendered("update_settings", error),
        },
        SettingsApplyPhase::InputPipeline => {
            state
                .runtime
                .pipeline
                .update_config(config.media.audio.clone())
                .await;
            timing.tick("pipeline.update_config");
            match state
                .runtime
                .agent
                .set_media_strategy(config.media.input_strategy)
            {
                Ok(()) => SettingsApplyOutcome::applied(),
                Err(error) => SettingsApplyOutcome::failed("update_settings media strategy", error),
            }
        }
        SettingsApplyPhase::Shell => {
            let result = state
                .runtime
                .tools
                .set_default_shell(config.default_shell)
                .await;
            timing.tick("set_default_shell");
            match result {
                Ok(_) => SettingsApplyOutcome::applied(),
                Err(error) => {
                    SettingsApplyOutcome::failed_catalog_rebuild("update_settings shell", error)
                }
            }
        }
        SettingsApplyPhase::Security => {
            state.runtime.tools.apply_security(&config.security).await;
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
                    SettingsApplyOutcome::applied()
                }
                Err(error) => SettingsApplyOutcome::failed(
                    "update_settings restore session authorization grants",
                    error,
                ),
            }
        }
        SettingsApplyPhase::McpConfig => {
            state
                .runtime
                .tools
                .load_mcp_from_config(&config.mcp_servers)
                .await;
            timing.tick("load_mcp_from_config");
            SettingsApplyOutcome::applied()
        }
        SettingsApplyPhase::McpMonitors => {
            state
                .runtime
                .services
                .mcp
                .start_monitors(&config.mcp_discovery)
                .await;
            timing.tick("mcp_manager.start_monitors");
            SettingsApplyOutcome::applied()
        }
        SettingsApplyPhase::RouterPublish => {
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
                    SettingsApplyOutcome::applied()
                }
                Err(RouterRuntimePublishError::ToolCatalog(error)) => {
                    crate::commands::emit_llm_config_changed(&app);
                    SettingsApplyOutcome::failed_catalog_rebuild_after_router_publish(
                        "update_settings router publish",
                        error,
                    )
                }
                Err(error @ RouterRuntimePublishError::Agent(_)) => {
                    SettingsApplyOutcome::failed("update_settings router publish", error)
                }
            }
        }
        SettingsApplyPhase::ContextLimits => {
            state.runtime.pipeline.set_limits(&config.context_limits);
            let result = state
                .runtime
                .tools
                .set_context_limits(config.context_limits.clone())
                .await;
            timing.tick("set_context_limits");
            match result {
                Err(error) => SettingsApplyOutcome::failed_catalog_rebuild(
                    "update_settings context limits",
                    error,
                ),
                Ok(_) => match state
                    .runtime
                    .agent
                    .set_context_limits(config.context_limits.clone())
                {
                    Ok(()) => SettingsApplyOutcome::applied(),
                    Err(error) => {
                        SettingsApplyOutcome::failed("update_settings agent context limits", error)
                    }
                },
            }
        }
        SettingsApplyPhase::SessionRuntime => {
            if let Err(error) = state.runtime.agent.set_max_steps(config.session.max_steps) {
                return SettingsApplyOutcome::failed("update_settings max steps", error);
            }
            if let Err(error) = state
                .runtime
                .agent
                .set_session_max_steps(config.session.session_max_steps)
            {
                return SettingsApplyOutcome::failed("update_settings session max steps", error);
            }
            state
                .runtime
                .executor
                .set_max_concurrent(config.session.max_concurrent);
            SettingsApplyOutcome::applied()
        }
        SettingsApplyPhase::ToolSettings => {
            let result = state
                .runtime
                .tools
                .set_tool_settings(config.tool_settings.clone())
                .await;
            match result {
                Ok(_) => SettingsApplyOutcome::applied(),
                Err(error) => SettingsApplyOutcome::failed_catalog_rebuild(
                    "update_settings tool settings",
                    error,
                ),
            }
        }
        SettingsApplyPhase::Skills => match state
            .runtime
            .services
            .skills
            .set_config(config.skills.root.clone(), config.skills.enabled.clone())
            .await
        {
            Ok(()) => SettingsApplyOutcome::applied(),
            Err(error) => SettingsApplyOutcome::failed("update_settings skills", error),
        },
        SettingsApplyPhase::Logging => {
            match apply_log_level_to_handles(&state.runtime.log_filter_handles, &config.log.level) {
                Ok(()) => SettingsApplyOutcome::applied(),
                Err(error) => SettingsApplyOutcome::failed("update_settings logging", error),
            }
        }
        SettingsApplyPhase::HotkeyMode => {
            use haven_common::types::HotkeyMode;
            state
                .runtime
                .shell
                .set_hold_mode(config.hotkey.mode == HotkeyMode::Hold)
                .await;
            SettingsApplyOutcome::applied()
        }
        SettingsApplyPhase::HotkeyUnregister => {
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            if let Some(old_shortcut) = haven_input::hotkey::KeyCombo::parse(&old_hotkey)
                .and_then(|combo| crate::to_tauri_shortcut(&combo))
                && let Err(error) = app.global_shortcut().unregister(old_shortcut)
            {
                return SettingsApplyOutcome::failed("update_settings unregister hotkey", error);
            }
            SettingsApplyOutcome::applied()
        }
        SettingsApplyPhase::HotkeyRegister => {
            use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
            if let Some(new_shortcut) =
                haven_input::hotkey::KeyCombo::parse(&config.hotkey.key_binding)
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
                                        error = %crate::logging::sanitize_error_text(&error.to_string()),
                                        "failed to show main window for global hotkey"
                                    );
                                }
                                if let Err(error) = window.set_focus() {
                                    tracing::debug!(
                                        error = %crate::logging::sanitize_error_text(&error.to_string()),
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
                        return SettingsApplyOutcome::failed(
                            "update_settings register hotkey",
                            error,
                        );
                    }
                }
            }
            SettingsApplyOutcome::applied()
        }
        SettingsApplyPhase::HotkeyRebindEvent => match app.emit(
            HOTKEY_REBIND_EVENT,
            HotkeyRebindEvent {
                old_binding: (*old_hotkey).clone(),
                new_binding: config.hotkey.key_binding.clone(),
            },
        ) {
            Ok(()) => SettingsApplyOutcome::applied(),
            Err(error) => {
                SettingsApplyOutcome::warning("update_settings hotkey rebind event", error)
            }
        },
    }
}

#[tauri::command]
pub async fn update_settings(
    settings: haven_common::config::Settings,
    app: tauri::AppHandle,
) -> Result<(), String> {
    validate_settings_payload(&settings).map_err(|error| log_err("update_settings", error))?;
    let timing = Arc::new(SettingsApplyTiming::new());
    let state = app.state::<Arc<AppState>>();
    let state = Arc::clone(&*state);
    let _apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let Some(update) = apply_settings_edit(&state.runtime.config_service, &settings)
        .map_err(|error| log_err("update_settings", error))?
    else {
        return Ok(());
    };
    let SettingsApplyContext {
        old_hotkey,
        snapshot,
        change,
    } = update;
    let mut apply = SettingsRuntimeApplyCoordinator::new(&change, &snapshot, &old_hotkey);
    let (version, live_targets, restart_required_targets, has_router_prepare) = {
        let plan = apply.plan();
        (
            plan.config_version,
            plan.live_targets.clone(),
            plan.restart_required_targets.clone(),
            plan.phases().contains(&SettingsApplyPhase::RouterPrepare),
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

    let snapshot = Arc::new(snapshot);
    let old_hotkey = Arc::new(old_hotkey);
    let prepared_router: Arc<std::sync::Mutex<Option<PreparedRouterRuntime>>> =
        Arc::new(std::sync::Mutex::new(None));
    let apply_state = state.clone();
    let apply_app = app.clone();
    let apply_snapshot = snapshot.clone();
    let apply_old_hotkey = old_hotkey.clone();
    let apply_prepared_router = prepared_router.clone();
    let apply_timing = timing.clone();

    if let Err(error) = apply
        .apply(move |phase| {
            execute_settings_apply_phase(
                phase,
                apply_state.clone(),
                apply_app.clone(),
                apply_snapshot.clone(),
                apply_old_hotkey.clone(),
                apply_prepared_router.clone(),
                apply_timing.clone(),
            )
        })
        .await
    {
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

/// Check whether a shell is available on this machine. The settings UI uses
/// this to warn when the user picks PowerShell 7 (`pwsh`) without having it
/// installed — `cmd` and the built-in `powershell` are always present.
#[tauri::command]
pub async fn list_permissions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<haven_common::config::StoredPermission>, String> {
    Ok(state.runtime.services.authorization.list_permanent().await)
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPermissionGrant {
    pub session_id: String,
    pub session_title: Option<String>,
    pub capability: String,
    pub target: String,
    pub effect: &'static str,
}

#[tauri::command]
pub async fn list_session_permissions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<SessionPermissionGrant>, String> {
    state
        .runtime
        .session_store
        .all_session_authorization_grants()
        .await
        .map(|grants| {
            grants
                .into_iter()
                .map(|stored| SessionPermissionGrant {
                    session_id: stored.session_id,
                    session_title: stored.session_title,
                    capability: stored.grant.capability.to_string(),
                    target: stored.grant.target.as_str().to_string(),
                    effect: match stored.grant.effect {
                        haven_common::types::PermissionEffect::Allow => "allow",
                        haven_common::types::PermissionEffect::Deny => "deny",
                    },
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
                    permission.key,
                    permission.effect,
                    haven_common::types::PermissionScope::Always,
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
                    permission.key,
                    permission.effect,
                    haven_common::types::PermissionScope::Always,
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
    let _config_apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    let session_id = session_id.trim().to_string();
    let capability = capability.trim().to_string();
    if session_id.is_empty() {
        return Err(log_err(
            "revoke_session_permission",
            "session id cannot be empty",
        ));
    }
    if capability.is_empty() {
        return Err(log_err(
            "revoke_session_permission",
            "permission capability cannot be empty",
        ));
    }
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
        .all_session_authorization_grants()
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
                    )
                    .await;
            }
            return Err(log_err("reset_session_permissions", error));
        }
    };
    Ok(removed)
}

#[tauri::command]
pub async fn check_shell_available(shell: String) -> Result<ShellAvailability, String> {
    #[cfg(windows)]
    let available = match shell.as_str() {
        "cmd" | "powershell" => true,
        "pwsh" => shell_on_path("pwsh.exe"),
        _ => true,
    };
    #[cfg(not(windows))]
    let available = match shell.as_str() {
        "pwsh" => shell_on_path("pwsh"),
        _ => true,
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
    let probe = std::process::Command::new("where.exe").arg(name).output();
    #[cfg(not(windows))]
    let probe = std::process::Command::new("which").arg(name).output();
    probe.map(|o| o.status.success()).unwrap_or(false)
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
        ShellAvailability, apply_settings_edit, spawn_settings_hotkey_task,
        validate_settings_payload,
    };
    use crate::app_state::AppState;
    use crate::config_runtime::{
        SettingsApplyOutcome, SettingsApplyPhase, SettingsRuntimeApplyCoordinator,
    };
    use haven_common::config::{
        AppConfig, ConfigLoader, ConfigService, InMemoryCredentialStore, LogLevel, Settings,
        StoredPermission,
    };
    use haven_common::types::PermissionEffect;

    fn config_service_with_config(config: AppConfig) -> (ConfigService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        *loader.config_mut() = config;
        loader.save().unwrap();
        (
            ConfigService::new_with_credential_store(
                loader,
                std::sync::Arc::new(InMemoryCredentialStore::default()),
            )
            .unwrap(),
            dir,
        )
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
    fn settings_boundary_rejects_inline_provider_ocr_and_mcp_values() {
        for payload in [
            serde_json::json!({
                "llm": { "providers": [{ "name": "primary", "api_key": "provider-secret-marker" }] }
            }),
            serde_json::json!({
                "media": { "ocr": { "api_key": "ocr-key-marker", "api_secret": "ocr-secret-marker" } }
            }),
            serde_json::json!({
                "mcp_servers": [{ "name": "server", "env": ["TOKEN=mcp-secret-marker"] }]
            }),
        ] {
            let settings: Settings = serde_json::from_value(payload).unwrap();
            let error = validate_settings_payload(&settings).unwrap_err();
            for marker in [
                "provider-secret-marker",
                "ocr-key-marker",
                "ocr-secret-marker",
                "mcp-secret-marker",
            ] {
                assert!(!error.to_string().contains(marker));
            }
        }
    }

    #[test]
    fn settings_boundary_requires_each_model_to_reference_a_configured_provider() {
        for llm in [
            serde_json::json!({
                "models": [{ "id": "assistant", "provider": "", "model": "model-a" }]
            }),
            serde_json::json!({
                "providers": [{ "name": "primary" }],
                "models": [{ "id": "assistant", "provider": "removed", "model": "model-a" }]
            }),
        ] {
            let settings: Settings =
                serde_json::from_value(serde_json::json!({ "llm": llm })).unwrap();
            assert_eq!(
                validate_settings_payload(&settings)
                    .unwrap_err()
                    .to_string(),
                "every configured model must reference a configured Provider"
            );
        }

        let settings: Settings = serde_json::from_value(serde_json::json!({
            "llm": {
                "providers": [{ "name": "primary" }],
                "models": [{ "id": "assistant", "provider": "primary", "model": "model-a" }]
            }
        }))
        .unwrap();
        validate_settings_payload(&settings).unwrap();
    }

    #[test]
    fn settings_edit_captures_hotkey_and_preserves_live_security_state() {
        let mut live_config = AppConfig::default();
        let mut stale_settings = Settings::from(&live_config);
        live_config.hotkey.key_binding = "Ctrl+Alt+O".into();
        let live_permission = StoredPermission {
            key: "files.read".into(),
            effect: PermissionEffect::Allow,
        };
        live_config.security.permissions = vec![live_permission.clone()];
        live_config.security.encrypt_sensitive = false;
        stale_settings.hotkey.key_binding = "Ctrl+Alt+N".into();
        let (service, _dir) = config_service_with_config(live_config);

        let update = apply_settings_edit(&service, &stale_settings)
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
        let settings = service.settings().unwrap();
        let receiver = service.subscribe().unwrap();

        let update = apply_settings_edit(&service, &settings).unwrap();

        assert!(update.is_none());
        assert!(receiver.try_recv().is_err());
        assert_eq!(service.snapshot().unwrap().version, 0);
    }

    #[tokio::test]
    async fn failed_settings_apply_keeps_persisted_config_and_identical_edit_skips_retry() {
        use std::sync::{Arc, Mutex};

        let (service, _dir) = config_service_with_config(AppConfig::default());
        let mut settings = service.settings().unwrap();
        settings.log.level = LogLevel::Debug;
        let changes = service.subscribe().unwrap();
        let update = apply_settings_edit(&service, &settings)
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
                    if phase == SettingsApplyPhase::Logging {
                        SettingsApplyOutcome::failed("settings_apply_test", "logging failed")
                    } else {
                        SettingsApplyOutcome::applied()
                    }
                }
            })
            .await;

        assert_eq!(result, Err("logging failed".into()));
        assert_eq!(*seen.lock().unwrap(), vec![SettingsApplyPhase::Logging]);
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.config.log.level, LogLevel::Debug);
        let persisted = ConfigLoader::load_from(&service.path().unwrap()).unwrap();
        assert_eq!(persisted.config().log.level, LogLevel::Debug);
        assert_eq!(changes.try_recv().unwrap().version, 1);

        assert!(apply_settings_edit(&service, &settings).unwrap().is_none());
        assert!(changes.try_recv().is_err());
    }
}
