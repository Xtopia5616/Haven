//! Tauri startup composition and lifecycle orchestration.

use crate::app_state::AppState;
use crate::autostart;
use crate::commands;
use crate::desktop::TrayStatus;
use crate::event_bridge::{TauriEmitter, emit_action_event};
use crate::events::*;
use crate::handlers::{HavenInputHandler, HavenShellHandler, make_tray_icon};
use crate::logging::init_tracing;
use crate::notification::DesktopNotifications;
use haven_common::config::LogConfig;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

pub(crate) fn run() {
    // Keep the versioned command directory live in the application binary as
    // well as in CI/docs. A drift in the source registry is a startup error,
    // not a silently stale contract inventory.
    debug_assert_eq!(commands::contracts::IPC_CONTRACT_VERSION, 1);
    debug_assert_eq!(commands::contracts::COMMAND_CONTRACTS.len(), 68);

    // Load config early so we can initialize tracing with the right level
    let config_loader = haven_common::config::ConfigLoader::load().unwrap_or_else(|_| {
        haven_common::config::ConfigLoader::load_from(
            &haven_common::config::ConfigLoader::default_path(),
        )
        .unwrap()
    });
    let log_cfg = config_loader.config().log.clone();

    // Initialize tracing subscriber (console + optional file output)
    let (filter_handles, log_config) = init_tracing(&log_cfg);

    // Set global panic hook to capture and log panics (M6-06)
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let msg = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("PANIC at {}: {}\n{}", location, msg, backtrace);
        prev_hook(panic_info);
    }));

    // Build the window first, then finish AppState inside setup. That lets the
    // WebView start navigating while (or right as) backend init runs, instead
    // of serializing: AppState → then first window paint.
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 自启触发的重复实例不弹出窗口：保持后台驻留，等待快捷键唤起。
            if args.iter().any(|a| a == autostart::AUTOSTART_ARG) {
                return;
            }
            let _ = app.get_webview_window("main").map(|w| {
                let _ = w.show();
                let _ = w.set_focus();
            });
        }))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();

            // Window from tauri.conf already exists here. Init backend now so
            // cold-start work no longer runs with zero visible window.
            let t_setup = std::time::Instant::now();
            let app_state = init_app_state(filter_handles, log_config, config_loader);
            app.manage(Arc::new(app_state));
            tracing::info!(
                "setup AppState ready in {}ms",
                t_setup.elapsed().as_millis()
            );

            // 由任务计划程序（--autostart）启动时默认隐藏主窗口，驻留
            // 系统托盘；使用录音快捷键即可唤起窗口并开始录音。
            if autostart::is_autostart_launch()
                && let Some(w) = app.get_webview_window("main")
            {
                let _ = w.hide();
            }

            let state = app.state::<Arc<AppState>>();
            let shell = &state.shell;

            // Deferred cold-start work (MCP connect, skills scan, audio
            // prewarm) runs after the window exists so the UI can paint a
            // 加载中 chip instead of sitting on a black webview.
            {
                let emit_handle = handle.clone();
                state.spawn_background_init(move |payload: AppBootstrapEvent| {
                    let _ = emit_handle.emit(APP_BOOTSTRAP_EVENT, payload);
                });
            }

            // Forward MCP status broadcasts to the webview. Startup connects
            // and health-monitor reconnects previously only updated the
            // internal channel — ToolsView / toasts never saw them until a
            // manual refresh.
            {
                let emit_handle = handle.clone();
                let mut rx = state.tools.mcp_manager.subscribe();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(ev) => {
                                let _ = emit_handle.emit(
                                    MCP_STATUS_CHANGED_EVENT,
                                    McpStatusChangedEvent {
                                        name: ev.name,
                                        status: ev.status,
                                    },
                                );
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }

            // Auto-refresh skills when the skills folder changes on disk
            // (and once at startup, so a UI that opened before the initial
            // scan finished still catches up). Newly added / modified /
            // removed SKILL.md files are picked up without a manual Refresh.
            {
                let emit_handle = handle.clone();
                state.tools.clone().spawn_skills_watcher(
                    std::time::Duration::from_secs(3),
                    move || {
                        let _ = emit_handle.emit(
                            SKILLS_STATUS_CHANGED_EVENT,
                            SkillsStatusChangedEvent {
                                op: "auto_refresh".into(),
                            },
                        );
                    },
                );
            }

            // Wire up the AgentEventEmitter to the app handle via an EventBus,
            // allowing multiple subscribers (frontend, log recorder, …).
            let bus = state.agent.install_event_bus();
            let emitter = Arc::new(TauriEmitter {
                handle: handle.clone(),
                chunk_seq: AtomicU64::new(0),
                notifications: DesktopNotifications::new(handle.clone()),
            });
            // Decouple the agent loops from the Tauri IPC subscriber chain:
            // emits become bounded-channel sends drained by a consumer session,
            // so a slow webview or toast notification can never stall agent
            // progress (previously every event was awaited end-to-end).
            let buffered = haven_agent::BufferedEmitter::new(1024, emitter);
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(bus.subscribe("tauri", buffered));
            });

            // Project tool-internal lifecycle JSON into the explicit action IPC
            // DTO before it reaches the frontend.  Background and scheduled
            // actions use one stable `id` field and never expose dynamic tool
            // args, continuation prompts, or output-log paths.
            let action_sink_handle = handle.clone();
            state.tools.background_actions.set_event_sink(Arc::new(
                move |event: String, payload: serde_json::Value| {
                    emit_action_event(
                        &action_sink_handle,
                        ActionKind::Background,
                        &event,
                        &payload,
                    );
                },
            ));

            // Same for scheduled_actions, so the pending list in the action panel
            // stays live and fired scheduled_actions can be acknowledged.
            let reminder_sink_handle = handle.clone();
            state.tools.scheduled_actions.set_event_sink(Arc::new(
                move |event: String, payload: serde_json::Value| {
                    emit_action_event(
                        &reminder_sink_handle,
                        ActionKind::Scheduled,
                        &event,
                        &payload,
                    );
                },
            ));

            // Foreground tool live-output previews (`agent:tool_output`) so
            // shell (and future long-running tools) can expand the chat card
            // while still running.
            let tool_output_handle = handle.clone();
            state.tools.live_outputs.set_event_sink(Arc::new(
                move |event: String, payload: serde_json::Value| {
                    if event != AGENT_TOOL_OUTPUT_EVENT {
                        tracing::warn!(event, "dropping unknown live tool-output event");
                        return;
                    }
                    match serde_json::from_value::<AgentToolOutputEvent>(payload) {
                        Ok(projected) => {
                            let _ = tool_output_handle.emit(AGENT_TOOL_OUTPUT_EVENT, projected);
                        }
                        Err(error) => {
                            tracing::warn!("dropping malformed live tool-output event: {error}");
                        }
                    }
                },
            ));

            let cfg = state.config_service.snapshot().unwrap().config;
            let is_hold = cfg.hotkey.mode == haven_common::types::HotkeyMode::Hold;
            let key_binding = cfg.hotkey.key_binding.clone();

            // The global-shortcut and tray callbacks run on plugin/main
            // threads that are outside the tokio runtime, where
            // `Handle::current()` panics ("there is no reactor running").
            // All callbacks therefore dispatch work through
            // `tauri::async_runtime::spawn`, which is safe from any thread
            // (unlike `Handle::block_on`, which panics with "Cannot start a
            // runtime from within a runtime" when the callback fires on the
            // async runtime's own thread).

            // --------------------- System tray (build first) ---------------------
            let show = MenuItemBuilder::with_id("show", "Show Window").build(app)?;
            let mute = MenuItemBuilder::with_id("mute", "Mute").build(app)?;
            let settings = MenuItemBuilder::with_id("settings", "Settings").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&show, &mute, &settings, &quit])
                .build()?;

            let tray = TrayIconBuilder::new()
                .icon(make_tray_icon(TrayStatus::Normal))
                .menu(&menu)
                .tooltip("Haven")
                .on_menu_event(move |app, _event| {
                    let id = _event.id().as_ref();
                    let state = app.state::<Arc<AppState>>();
                    match id {
                        "show" => {
                            let _ = app.get_webview_window("main").map(|w| {
                                let _ = w.show();
                                let _ = w.set_focus();
                            });
                        }
                        "mute" => {
                            let shell = state.shell.clone();
                            tauri::async_runtime::spawn(async move {
                                let shell_state = shell.get_state().await;
                                shell.set_muted(!shell_state.is_muted).await;
                            });
                        }
                        "settings" => {
                            let _ = app.get_webview_window("main").map(|w| {
                                let _ = w.eval("window.location.href = '/settings'");
                                let _ = w.show();
                                let _ = w.set_focus();
                            });
                        }
                        "quit" => {
                            tracing::info!("Quit selected from system tray");
                            // Graceful exit instead of `std::process::exit`:
                            // `app.exit(0)` lets RunEvent::Exit run the cleanup
                            // (pause running sessions, close the active session)
                            // and keeps the process exit code 0 so `tauri dev`
                            // treats it as a normal exit rather than an abrupt
                            // termination that can leave the dev session and
                            // terminal running.
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        let _ = app.get_webview_window("main").map(|w| {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        });
                    }
                })
                .build(app)?;

            // Wire up shell handler (replaces former per-callback field assignments)
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                let shell_arc = state.shell.clone();
                let pipeline = state.pipeline.clone();
                let tray_ref = tray.clone();
                let handler = Arc::new(HavenShellHandler {
                    app_h: handle.clone(),
                    pipeline,
                    shell_arc: shell_arc.clone(),
                    tray: tray_ref,
                });
                shell_arc.set_handler(handler);

                // Wire up unified input handler (VAD status + auto-stop)
                {
                    let app_h = handle.clone();
                    let shell_arc = state.shell.clone();
                    state
                        .pipeline
                        .set_handler(Arc::new(HavenInputHandler { app_h, shell_arc }));
                }

                rt.block_on(shell.set_hold_mode(is_hold));

                // Wire up confirm callback
                {
                    let app_h = handle.clone();
                    let st_arc = state.inner().clone();
                    rt.block_on(async {
                        st_arc.executor.on_confirm_request.set(Arc::new(
                            move |step_id: haven_common::types::ConfirmId,
                                  session_id: String,
                                  tool_name: String,
                                  risk_level: haven_common::types::RiskLevel,
                                  params: serde_json::Value,
                                  invocation_step_id: Option<String>,
                                  action_index: u32,
                                  tool_call_id: Option<String>| {
                                let permission_key =
                                    haven_common::types::permission_key(&tool_name, &params);
                                let _ = app_h.emit(
                                    CONFIRM_REQUESTED_EVENT,
                                    ConfirmationRequestedEvent {
                                        step_id,
                                        invocation_step_id,
                                        action_index,
                                        tool_call_id,
                                        tool_name,
                                        risk_level,
                                        session_id,
                                        params,
                                        permission_key,
                                    },
                                );
                            },
                        ));
                    });
                }

                // Wire up the terminal-failure callback: the dispatcher's
                // panic/abort path marks the session Error without going through
                // the ReAct loop's event emission, so the UI would never learn
                // about the transition (stuck busy chip, stale session list).
                // Emit both channels in the same shapes the loop uses.
                {
                    let app_h = handle.clone();
                    let st_arc = state.inner().clone();
                    rt.block_on(async {
                        st_arc.executor.on_session_error.set(Arc::new(
                            move |session_id: String, reason: String| {
                                let _ = app_h.emit(
                                    SESSION_ERROR_EVENT,
                                    SessionErrorEvent {
                                        session_id: session_id.clone(),
                                        error: reason,
                                    },
                                );
                                let _ = app_h.emit(
                                    SESSION_UPDATED_EVENT,
                                    SessionLifecycleEvent {
                                        session_id,
                                        status: "error".into(),
                                        title: Some(String::new()),
                                    },
                                );
                            },
                        ));
                    });
                }
            });

            // --------------------- Global hotkey ---------------------
            use tauri_plugin_global_shortcut::{
                Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
            };

            let shortcut = haven_input::hotkey::KeyCombo::parse(&key_binding)
                .and_then(|combo| to_tauri_shortcut(&combo))
                .unwrap_or_else(|| {
                    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
                });

            let _sc = shortcut;
            let result = handle
                .global_shortcut()
                .on_shortcut(shortcut, move |app, _sc, event| {
                    let state = app.state::<Arc<AppState>>();
                    let shell = state.shell.clone();
                    let pipeline = state.pipeline.clone();
                    let app_h = app.clone();
                    let pressed = event.state == ShortcutState::Pressed;
                    // `spawn` (unlike `block_on`) is safe from any thread, so a
                    // shortcut callback firing on the async runtime's own thread
                    // can't panic with "Cannot start a runtime from within a
                    // runtime".
                    tauri::async_runtime::spawn(async move {
                        let shell_state = shell.get_state().await;
                        if shell_state.is_muted {
                            return;
                        }
                        // 快捷键唤起：先显示并聚焦前端窗口（含自启隐藏后的
                        // 后台场景），再开始/结束录音。
                        if pressed && let Some(w) = app_h.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                        // 未配置录音（STT 不可用）时，快捷键仅唤醒窗口，不尝试
                        // 开始录音，避免无意义的录音错误提示。
                        if !pipeline.recording_configured().await {
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
                });

            match result {
                Ok(_) => {
                    tracing::info!("Hotkey registered: {}", key_binding);
                }
                Err(e) => {
                    tracing::warn!("Hotkey conflict detected: {} - {}", key_binding, e);
                    let _ = handle.emit(
                        HOTKEY_CONFLICT_EVENT,
                        HotkeyConflictEvent {
                            binding: key_binding,
                            error: e.to_string(),
                        },
                    );
                }
            }

            tracing::info!("Haven Tauri app initialized");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::recording::start_recording,
            commands::recording::stop_recording,
            commands::recording::cancel_recording,
            commands::recording::process_transcript,
            commands::session::reopen_session,
            commands::session::get_last_conversation,
            commands::session::get_sessions,
            commands::action::list_actions,
            commands::action::cancel_action,
            commands::action::list_action_history,
            commands::action::delete_action,
            commands::session::end_session,
            commands::session::interrupt_session,
            commands::session::resolve_confirmation,
            commands::skills::get_tools,
            commands::skills::reset_tool_circuits,
            commands::recording::get_recording_state,
            commands::history::get_history,
            commands::history::count_history,
            commands::history::search_history,
            commands::history::search_history_filtered,
            commands::history::search_history_paginated,
            commands::history::count_history_search,
            commands::session::delete_session,
            commands::session::clear_history,
            commands::model::get_api_key_status,
            commands::model::check_llm_connection,
            commands::settings::get_bootstrap_status,
            commands::model::discover_models,
            commands::model::discover_all_models,
            commands::model::switch_model,
            commands::model::set_reasoning_effort,
            commands::model::set_web_search,
            commands::memory::run_memory_maintenance,
            commands::memory::recall_memory,
            commands::mcp::list_mcp_tools,
            commands::mcp::reconnect_mcp,
            commands::mcp::refresh_mcp_servers,
            commands::mcp::mcp_tool_call,
            commands::mcp::add_mcp_server,
            commands::mcp::update_mcp_server,
            commands::mcp::remove_mcp_server,
            commands::mcp::toggle_mcp_server,
            commands::skills::list_skills,
            commands::skills::refresh_skills,
            commands::skills::set_skill_enabled,
            commands::skills::set_tool_enabled,
            commands::skills::open_skills_dir,
            commands::external::open_external,
            commands::skills::execute_skill,
            commands::memory::list_facts,
            commands::memory::add_fact,
            commands::memory::delete_fact,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::list_permissions,
            commands::settings::revoke_permission,
            commands::settings::check_shell_available,
            commands::history::export_history,
            commands::settings::enable_autostart,
            commands::settings::disable_autostart,
            commands::settings::is_autostart_enabled,
            commands::session::get_session_for_resume,
            commands::session::rollback_session,
            commands::session::continue_session,
            commands::session::update_session_title,
            commands::log::get_log_info,
            commands::log::read_log_tail,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Haven app")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                tracing::info!("Haven app exit requested");
                let state = app_handle.state::<Arc<AppState>>();
                // Pause in-flight sessions so they survive a restart in a
                // resumable state. Without this, every still-`running` session
                // would be flipped to `error` at the next startup by
                // `finalize_orphaned_running_sessions` (which only intends to
                // catch crash leftovers).
                if let Ok(n) = state.db.pause_running_sessions()
                    && n > 0
                {
                    tracing::info!("paused {} running session(s) on exit", n);
                }
            }
        });
}

/// Convert a neutral [`haven_input::hotkey::KeyCombo`] into the Tauri
/// global-shortcut type. Parsing/validation already happened in
/// `haven-input`; this is the only place Tauri shortcut types are built.
pub(crate) fn to_tauri_shortcut(
    combo: &haven_input::hotkey::KeyCombo,
) -> Option<tauri_plugin_global_shortcut::Shortcut> {
    use haven_input::hotkey::{ALT, CTRL, KeyCode, SHIFT, SUPER};
    use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

    const LETTERS: [Code; 26] = [
        Code::KeyA,
        Code::KeyB,
        Code::KeyC,
        Code::KeyD,
        Code::KeyE,
        Code::KeyF,
        Code::KeyG,
        Code::KeyH,
        Code::KeyI,
        Code::KeyJ,
        Code::KeyK,
        Code::KeyL,
        Code::KeyM,
        Code::KeyN,
        Code::KeyO,
        Code::KeyP,
        Code::KeyQ,
        Code::KeyR,
        Code::KeyS,
        Code::KeyT,
        Code::KeyU,
        Code::KeyV,
        Code::KeyW,
        Code::KeyX,
        Code::KeyY,
        Code::KeyZ,
    ];
    const FUNCTIONS: [Code; 12] = [
        Code::F1,
        Code::F2,
        Code::F3,
        Code::F4,
        Code::F5,
        Code::F6,
        Code::F7,
        Code::F8,
        Code::F9,
        Code::F10,
        Code::F11,
        Code::F12,
    ];

    let mut modifiers = Modifiers::empty();
    if combo.has(CTRL) {
        modifiers |= Modifiers::CONTROL;
    }
    if combo.has(SHIFT) {
        modifiers |= Modifiers::SHIFT;
    }
    if combo.has(ALT) {
        modifiers |= Modifiers::ALT;
    }
    if combo.has(SUPER) {
        modifiers |= Modifiers::SUPER;
    }
    let code = match combo.key() {
        KeyCode::Space => Code::Space,
        KeyCode::Enter => Code::Enter,
        KeyCode::Escape => Code::Escape,
        KeyCode::Tab => Code::Tab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::CapsLock => Code::CapsLock,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::ArrowLeft => Code::ArrowLeft,
        KeyCode::ArrowRight => Code::ArrowRight,
        KeyCode::ArrowUp => Code::ArrowUp,
        KeyCode::ArrowDown => Code::ArrowDown,
        KeyCode::Key(c) => LETTERS[(c - b'a') as usize],
        KeyCode::Digit(d) => {
            let digits: [Code; 10] = [
                Code::Digit0,
                Code::Digit1,
                Code::Digit2,
                Code::Digit3,
                Code::Digit4,
                Code::Digit5,
                Code::Digit6,
                Code::Digit7,
                Code::Digit8,
                Code::Digit9,
            ];
            digits[(d - b'0') as usize]
        }
        KeyCode::F(n) => FUNCTIONS[(n - 1) as usize],
    };
    Some(Shortcut::new(Some(modifiers), code))
}

fn init_app_state(
    filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    _log_config: Arc<std::sync::Mutex<LogConfig>>,
    config_loader: haven_common::config::ConfigLoader,
) -> AppState {
    let db_path = haven_common::config::ConfigLoader::data_dir().join("haven.db");
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let fh = filter_handles.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(AppState::new(&db_path, fh, config_loader))
    })
    .unwrap_or_else(|e| {
        // No degraded fallback: a failed backend is not usable, so exit with
        // a clear error (e.g. an old-version haven.db rejected by the schema
        // check tells the user to delete the file and rebuild).
        tracing::error!("failed to initialize application state: {}", e);
        std::process::exit(1);
    })
}
