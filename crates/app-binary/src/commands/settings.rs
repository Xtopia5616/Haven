use crate::app_state::AppState;
use crate::commands::hot_swap_router;
use crate::commands::log_err;
use haven_llm::LlmRouter;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tracing_subscriber::filter::EnvFilter;

#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<haven_common::config::Settings, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = state
        .config_loader
        .lock()
        .map_err(|e| log_err("get_settings", e))?;
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
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_settings", e))?;
        cfg.config().hotkey.key_binding.clone()
    };

    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_settings", e))?;
        loader.apply_settings(&settings);
        // The settings form does not manage MCP servers / skills / tool
        // settings: those are mutated by dedicated commands (add/update/
        // remove/toggle MCP, skill ops) that write config.toml directly via a
        // fresh `ConfigLoader::load()`, leaving the shared in-memory loader
        // stale. Restore the authoritative on-disk copies before saving so a
        // settings save can never wipe configured servers/skills.
        let disk = haven_common::config::ConfigLoader::load()
            .map_err(|e| log_err("update_settings", e))?;
        let cfg = loader.config_mut();
        cfg.mcp_servers = disk.config().mcp_servers.clone();
        cfg.mcp_discovery = disk.config().mcp_discovery.clone();
        cfg.skills = disk.config().skills.clone();
        cfg.skills_exec = disk.config().skills_exec.clone();
        cfg.tool_settings = disk.config().tool_settings.clone();
        loader.save().map_err(|e| log_err("update_settings", e))?;
    }

    // Propagate audio config to running pipeline
    state.pipeline.update_config(settings.audio).await;

    // Propagate the default shell choice to the shell tool so the running
    // agent executes new commands in the selected shell.
    state.tools.set_default_shell(settings.default_shell).await;

    // Reload MCP servers from config
    let (
        mcp_servers,
        mcp_discovery,
        session_max_steps,
        session_max_concurrent,
        llm_config,
        max_response_tokens,
        reasoning_echo_max_chars,
        min_risk_level,
    ) = {
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_settings", e))?;
        let config = cfg.config();
        (
            config.mcp_servers.clone(),
            config.mcp_discovery.clone(),
            config.session.max_steps,
            config.session.max_concurrent,
            config.llm.clone(),
            config.context_limits.max_response_tokens,
            config.context_limits.reasoning_echo_max_chars,
            config.security.min_risk_level,
        )
    };
    state.tools.load_mcp_from_config(&mcp_servers).await;
    state.tools.mcp_manager.start_monitors(&mcp_discovery).await;
    let new_router = Arc::new(LlmRouter::new(
        llm_config
            .with_response_cap(max_response_tokens)
            .with_reasoning_echo_cap(reasoning_echo_max_chars),
    ));
    hot_swap_router(&state, new_router).await?;
    state.agent.set_max_steps(session_max_steps);
    state.executor.set_max_concurrent(session_max_concurrent);
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

        if let Some(old_shortcut) = crate::parse_shortcut(&old_hotkey)
            && let Err(e) = app.global_shortcut().unregister(old_shortcut)
        {
            tracing::warn!("failed to unregister old hotkey {}: {}", old_hotkey, e);
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

/// Check whether a shell is available on this machine. The settings UI uses
/// this to warn when the user picks PowerShell 7 (`pwsh`) without having it
/// installed — `cmd` and the built-in `powershell` are always present.
#[tauri::command]
pub async fn check_shell_available(shell: String) -> Result<serde_json::Value, String> {
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
    Ok(serde_json::json!({ "available": available }))
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
