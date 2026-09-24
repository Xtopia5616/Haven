use crate::app_state::AppState;
use crate::commands::log_err;
use crate::config_runtime::{
    RuntimeConfigApplyPlan, RuntimeConfigTarget, apply_log_level_to_handles,
};
use crate::events::{HOTKEY_REBIND_EVENT, HotkeyRebindEvent};
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;

struct SettingsApplyContext {
    old_hotkey: String,
    snapshot: haven_common::config::ConfigSnapshot,
    change: haven_common::config::ConfigChanged,
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
        .config_service
        .settings()
        .map_err(|e| log_err("get_settings", e))
}

/// Cold-start progress for the titlebar status chip (`loading` | `ready`).
/// The frontend also listens for `app:bootstrap`; this command covers the
/// race where the UI mounts after the ready event already fired.
#[tauri::command]
pub async fn get_bootstrap_status(app: tauri::AppHandle) -> Result<String, String> {
    let state = app.state::<Arc<AppState>>();
    Ok(state.bootstrap_status().as_str().to_string())
}

#[tauri::command]
pub async fn update_settings(
    settings: haven_common::config::Settings,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    let mut last = t0;
    let mut tick = |name: &str| {
        let now = std::time::Instant::now();
        tracing::info!("update_settings: {name} += {:?}", now.duration_since(last));
        last = now;
    };
    let state = app.state::<Arc<AppState>>();
    let _apply_guard = state.config_apply_gate.lock().await;
    let Some(update) = apply_settings_edit(&state.config_service, &settings)
        .map_err(|e| log_err("update_settings", e))?
    else {
        return Ok(());
    };
    let SettingsApplyContext {
        old_hotkey,
        snapshot,
        change,
    } = update;
    let config = &snapshot.config;
    let plan = RuntimeConfigApplyPlan::from_change(&change);
    tracing::debug!(
        version = change.version,
        domains = ?change.domains,
        live = ?plan.live,
        restart_required = ?plan.restart_required,
        "configuration snapshot updated"
    );
    if !plan.restart_required.is_empty() {
        tracing::warn!(
            version = plan.version,
            targets = ?plan.restart_required,
            "configuration change requires a restart for some consumers"
        );
    }

    // Build every fallible router/media dependency before the first live
    // runtime update. A preparation failure leaves those consumers on their
    // previous generation while the durable config snapshot remains saved.
    let mut prepared_router = if plan.contains(RuntimeConfigTarget::LlmRouter) {
        Some(state.config_apply_gate.prepare_router_runtime(
            &state,
            &snapshot,
            "update_settings",
        )?)
    } else {
        None
    };
    tick("config apply");

    // Propagate audio config to running pipeline
    if plan.contains(RuntimeConfigTarget::InputPipeline) {
        state
            .pipeline
            .update_config(config.media.audio.clone())
            .await;
        state.agent.set_media_strategy(config.media.input_strategy);
        tick("pipeline.update_config");
    }

    // Propagate the default shell choice to the shell tool so the running
    // agent executes new commands in the selected shell.
    if plan.contains(RuntimeConfigTarget::Shell) {
        state.tools.set_default_shell(config.default_shell).await;
        tick("set_default_shell");
    }

    // Apply the new security boundary before any config reload can start a
    // connection. A single settings update may change both MCP definitions
    // and network policy; the stricter policy must win during that transition.
    if plan.contains(RuntimeConfigTarget::Security) {
        state.tools.apply_security(&config.security).await;
        tick("apply_security");
    }

    // Reload MCP servers from config
    if plan.contains(RuntimeConfigTarget::Mcp) {
        state.tools.load_mcp_from_config(&config.mcp_servers).await;
        tick("load_mcp_from_config");
        state
            .services
            .mcp
            .start_monitors(&config.mcp_discovery)
            .await;
        tick("mcp_manager.start_monitors");
    }

    if plan.contains(RuntimeConfigTarget::LlmRouter) {
        state
            .config_apply_gate
            .publish_router_runtime(
                &state,
                prepared_router
                    .take()
                    .expect("router target always has a prepared runtime"),
            )
            .await;
        tick("publish_router_runtime");
        crate::commands::emit_llm_config_changed(&app);
    }

    // Apply context limits only after the router and its dependent clients
    // have been prepared successfully, so a preparation error leaves every
    // context-limit consumer on its previous runtime state.
    if plan.contains(RuntimeConfigTarget::ContextLimits) {
        state.pipeline.set_limits(&config.context_limits);
        state
            .tools
            .set_context_limits(config.context_limits.clone())
            .await;
        state
            .agent
            .set_context_limits(config.context_limits.clone());
        tick("set_context_limits");
    }

    if plan.contains(RuntimeConfigTarget::SessionRuntime) {
        state.agent.set_max_steps(config.session.max_steps);
        state
            .agent
            .set_session_max_steps(config.session.session_max_steps);
        state
            .executor
            .set_max_concurrent(config.session.max_concurrent);
    }

    if plan.contains(RuntimeConfigTarget::ToolSettings) {
        state
            .services
            .authorization
            .set_tool_settings(config.tool_settings.clone())
            .await;
    }

    if plan.contains(RuntimeConfigTarget::Skills)
        && let Err(error) = state
            .services
            .skills
            .set_config(config.skills.root.clone(), config.skills.enabled.clone())
            .await
    {
        return Err(log_err("update_settings skills", error));
    }

    // Propagate log level to tracing subscriber (console + file)
    if plan.contains(RuntimeConfigTarget::Logging) {
        apply_log_level_to_handles(&state.log_filter_handles, &config.log.level)
            .map_err(|e| log_err("update_settings logging", e))?;
    }

    // Propagate hotkey mode change (always)
    use haven_common::types::HotkeyMode;
    if plan.contains(RuntimeConfigTarget::Hotkey) {
        state
            .shell
            .set_hold_mode(config.hotkey.mode == HotkeyMode::Hold)
            .await;
    }

    if plan.contains(RuntimeConfigTarget::Hotkey) && config.hotkey.key_binding != old_hotkey {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

        if let Some(old_shortcut) = haven_input::hotkey::KeyCombo::parse(&old_hotkey)
            .and_then(|c| crate::to_tauri_shortcut(&c))
            && let Err(e) = app.global_shortcut().unregister(old_shortcut)
        {
            return Err(log_err("update_settings unregister hotkey", e));
        }

        if let Some(new_shortcut) = haven_input::hotkey::KeyCombo::parse(&config.hotkey.key_binding)
            .and_then(|c| crate::to_tauri_shortcut(&c))
        {
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
                        config.hotkey.key_binding,
                    );
                }
                Err(e) => {
                    return Err(log_err("update_settings register hotkey", e));
                }
            }
        }

        if let Err(e) = app.emit(
            HOTKEY_REBIND_EVENT,
            HotkeyRebindEvent {
                old_binding: old_hotkey,
                new_binding: config.hotkey.key_binding.clone(),
            },
        ) {
            tracing::warn!(error = %e, "update_settings: hotkey rebind event emit failed");
        }
    }
    tick("hotkey section");
    tracing::info!("update_settings: TOTAL {:?}", t0.elapsed());
    Ok(())
}

/// Check whether a shell is available on this machine. The settings UI uses
/// this to warn when the user picks PowerShell 7 (`pwsh`) without having it
/// installed — `cmd` and the built-in `powershell` are always present.
#[tauri::command]
pub async fn list_permissions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<haven_common::config::StoredPermission>, String> {
    Ok(state.services.authorization.list_permanent().await)
}

#[tauri::command]
pub async fn revoke_permission(state: State<'_, Arc<AppState>>, key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("permission key cannot be empty".into());
    }
    state
        .config_service
        .edit(|config| {
            config
                .security
                .permissions
                .retain(|permission| permission.key != key);
            Ok(())
        })
        .map_err(|e| log_err("revoke_permission", e))?;
    state.services.authorization.revoke_permanent(&key).await;
    Ok(())
}

/// Remove all user-created permission rules and restore the selected default
/// policy. This intentionally does not change the policy mode itself.
#[tauri::command]
pub async fn reset_permissions(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .config_service
        .edit(|config| {
            config.security.permissions.clear();
            Ok(())
        })
        .map_err(|e| log_err("reset_permissions", e))?;
    state.services.authorization.clear_permanent().await;
    Ok(())
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
        return Err("自动启动仅支持生产版本（cargo tauri build）。开发模式下请手动运行。".into());
    }
    crate::autostart::enable()
}

#[tauri::command]
pub async fn disable_autostart() -> Result<(), String> {
    crate::autostart::disable()
}

#[tauri::command]
pub async fn is_autostart_enabled() -> Result<bool, String> {
    crate::autostart::is_enabled()
}

#[cfg(test)]
mod tests {
    use super::{ShellAvailability, apply_settings_edit};
    use haven_common::config::{
        AppConfig, ConfigLoader, ConfigService, Settings, StoredPermission,
    };
    use haven_common::types::PermissionEffect;

    fn config_service_with_config(config: AppConfig) -> (ConfigService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        *loader.config_mut() = config;
        loader.save().unwrap();
        (ConfigService::new(loader), dir)
    }

    #[test]
    fn shell_availability_has_a_named_stable_wire_shape() {
        assert_eq!(
            serde_json::to_value(ShellAvailability { available: true }).unwrap(),
            serde_json::json!({"available": true})
        );
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
    fn no_op_settings_edit_returns_no_runtime_apply_context() {
        let (service, _dir) = config_service_with_config(AppConfig::default());
        let settings = service.settings().unwrap();
        let receiver = service.subscribe().unwrap();

        let update = apply_settings_edit(&service, &settings).unwrap();

        assert!(update.is_none());
        assert!(receiver.try_recv().is_err());
        assert_eq!(service.snapshot().unwrap().version, 0);
    }
}
