//! Tauri startup composition and lifecycle orchestration.

use crate::app_state::AppState;
use crate::autostart;
use crate::commands;
use crate::desktop::TrayStatus;
use crate::event_bridge::{TauriEmitter, emit_tool_run_event};
use crate::events::*;
use crate::handlers::{HavenInputEventHandler, HavenShellHandler, make_tray_icon};
use crate::logging::init_tracing;
use crate::notification::DesktopNotifications;
use crate::startup_error::{StartupFailure, should_show_startup_error, show_startup_error};
#[cfg(windows)]
use crate::startup_error::{WebviewFailure, WebviewFailureMonitor};
use haven_common::config::LogConfig;
use haven_common::error::sanitize_error_text;
use haven_input::{KeyCode, KeyCombo, KeyModifier};
use haven_memory::SessionStore;
use haven_tools::ToolRunLifecycleEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

macro_rules! log_ignored_result {
    ($context:expr, $result:expr) => {{
        if let Err(error) = $result {
            tracing::debug!(
                context = $context,
                error = %sanitize_error_text(&error.to_string()),
                "best-effort desktop operation failed"
            );
        }
    }};
}

pub(crate) fn project_interaction(
    request: &haven_agent::InteractionRequest,
    owner: haven_agent::InteractionOwner,
) -> InteractionRequestedEvent {
    let mut event = InteractionRequestedEvent {
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
        owner,
        kind: request.kind,
        status: request.status,
        options: Vec::new(),
        tool_name: None,
        risk_level: None,
        summary: None,
        permission_key: None,
        invocation_step_id: None,
        tool_index: None,
        tool_call_id: None,
        created_at: request.created_at.clone(),
        expires_at: request.expires_at.clone(),
    };
    match &request.details {
        haven_agent::InteractionDetails::Ask { options, .. } => {
            event.options = options.clone();
        }
        haven_agent::InteractionDetails::Confirm {
            step_id,
            tool_index,
            tool_call_id,
            tool_name,
            tool_input,
            risk_level,
            receipt,
            ..
        } => {
            event.tool_name = Some(tool_name.clone());
            event.risk_level = Some(*risk_level);
            event.summary = Some(haven_tools::permission_prompt_summary(
                tool_name, tool_input,
            ));
            event.permission_key = receipt
                .as_ref()
                .map(|receipt| receipt.capability.to_string());
            event.invocation_step_id = (!step_id.is_empty()).then(|| step_id.clone());
            event.tool_index = Some(*tool_index);
            event.tool_call_id = (!tool_call_id.is_empty()).then(|| tool_call_id.clone());
        }
        haven_agent::InteractionDetails::ScheduledConfirm {
            tool_name,
            tool_input,
            receipt,
            ..
        } => {
            event.tool_name = Some(tool_name.clone());
            event.summary = Some(haven_tools::permission_prompt_summary(
                tool_name, tool_input,
            ));
            event.permission_key = Some(receipt.capability.to_string());
        }
    }
    event
}

pub(crate) fn run() {
    // Load config early so we can initialize tracing with the right level
    let config_loader = match haven_common::config::ConfigLoader::load() {
        Ok(loader) => loader,
        Err(error) => {
            // Configuration may be unavailable before its logging settings
            // can be read. Use the default log file path so this failure is
            // still diagnosable for unattended autostart launches.
            let fallback_log_cfg = LogConfig::default();
            let _ = init_tracing(&fallback_log_cfg);
            let safe_error = sanitize_error_text(&error.to_string());
            tracing::error!(
                error = %safe_error,
                "Haven cannot start because configuration is unavailable"
            );
            if should_show_startup_error(autostart::is_autostart_launch()) {
                show_startup_error(StartupFailure::Configuration, Some(&safe_error));
            }
            std::process::exit(1);
        }
    };
    let log_cfg = config_loader.config().log.clone();

    // Initialize tracing subscriber (console + optional file output)
    let tracing_initialization = init_tracing(&log_cfg);

    // Set global panic hook to capture and log panics (M6-06)
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
            .map(|l| {
                let file = std::path::Path::new(l.file())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                format!("{file}:{}", l.line())
            })
            .unwrap_or_else(|| "?".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        let safe_msg = sanitize_error_text(&msg);
        tracing::error!(
            location = %location,
            panic = %safe_msg,
            backtrace = %backtrace,
            "PANIC"
        );
    }));

    // Build the window first, then finish AppState inside setup. That lets the
    // WebView start navigating while (or right as) backend init runs, instead
    // of serializing: AppState → then first window paint.
    let ui_assets = crate::ui_assets::ResourceUiAssets::default();
    let context = tauri::generate_context!(assets = ui_assets.clone());
    let ui_assets_for_setup = ui_assets.clone();
    let app_state_initialization_failed = Arc::new(AtomicBool::new(false));
    let setup_app_state_initialization_failed = Arc::clone(&app_state_initialization_failed);
    #[cfg(windows)]
    let webview_failure_monitor = Arc::new(WebviewFailureMonitor::default());
    #[cfg(windows)]
    let setup_webview_failure_monitor = Arc::clone(&webview_failure_monitor);
    #[cfg(windows)]
    let window_event_webview_failure_monitor = Arc::clone(&webview_failure_monitor);
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 自启触发的重复实例不弹出窗口：保持后台驻留，等待快捷键唤起。
            if args.iter().any(|a| a == autostart::AUTOSTART_ARG) {
                return;
            }
            let _ = app.get_webview_window("main").map(|w| {
                log_ignored_result!("single_instance.show", w.show());
                log_ignored_result!("single_instance.set_focus", w.set_focus());
            });
        }))
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                log_ignored_result!("window.close.hide", window.hide());
            }
            #[cfg(windows)]
            if matches!(event, tauri::WindowEvent::Focused(true)) {
                window_event_webview_failure_monitor
                    .on_window_focused(window.app_handle().clone());
            }
        })
        .setup(move |app| {
            if !tauri::is_dev() && !ui_assets_for_setup.is_ready() {
                return Err(std::io::Error::other(
                    "Haven UI resources are missing from the application bundle",
                )
                .into());
            }

            if !autostart::is_autostart_launch()
                && let Some(window) = app.get_webview_window("main")
            {
                log_ignored_result!("startup.main_window.show", window.show());
                log_ignored_result!("startup.main_window.set_focus", window.set_focus());
            }

            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main")
                && let Err(error) = crate::window_resize::install(&window)
            {
                tracing::error!(
                    error = %sanitize_error_text(&error.to_string()),
                    "failed to install the window aspect-ratio constraint; disabling manual resizing"
                );
                if let Err(fallback_error) = window.set_resizable(false) {
                    tracing::error!(
                        error = %sanitize_error_text(&fallback_error.to_string()),
                        "failed to disable window resizing after constraint installation failed"
                    );
                }
            }

            let handle = app.handle().clone();

            // Window from tauri.conf already exists here. Init backend now so
            // cold-start work no longer runs with zero visible window.
            let t_setup = std::time::Instant::now();
            let app_state = match init_app_state(
                tracing_initialization.filter_handles,
                tracing_initialization.effective_log_config,
                config_loader,
            ) {
                Ok(state) => state,
                Err(error) => {
                    setup_app_state_initialization_failed.store(true, Ordering::Release);
                    if should_show_startup_error(autostart::is_autostart_launch()) {
                        show_startup_error(
                            StartupFailure::ApplicationState,
                            Some(&sanitize_error_text(&error.to_string())),
                        );
                    }
                    return Err(error.into());
                }
            };
            app.manage(Arc::new(app_state));
            let notifications = Arc::new(DesktopNotifications::new(handle.clone()));
            app.manage(notifications.clone());

            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                install_webview_failure_handlers(
                    &window,
                    handle.clone(),
                    Arc::clone(&setup_webview_failure_monitor),
                );
            }

            tracing::info!(
                "setup AppState ready in {}ms",
                t_setup.elapsed().as_millis()
            );

            // 由任务计划程序（--autostart）启动时默认隐藏主窗口，驻留
            // 系统托盘；使用录音快捷键即可唤起窗口并开始录音。
            if autostart::is_autostart_launch()
                && let Some(w) = app.get_webview_window("main")
            {
                log_ignored_result!("autostart.hide", w.hide());
            }

            let state = app.state::<Arc<AppState>>();
            let shell = &state.runtime.shell;

            // Forward MCP status broadcasts to the webview. Startup connects
            // and health-monitor reconnects previously only updated the
            // internal channel — ToolsView / toasts never saw them until a
            // manual refresh.
            {
                let emit_handle = handle.clone();
                let mut rx = state.runtime.services.mcp.subscribe_status_events();
                state
                    .runtime
                    .spawn_with_child_token("mcp-status-forwarder", move |cancel| async move {
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            result = rx.recv() => match result {
                                Some(ev) => {
                                    log_ignored_result!(
                                        "event.mcp_status_changed",
                                        emit_handle.emit(
                                            MCP_STATUS_CHANGED_EVENT,
                                            McpStatusChangedEvent {
                                                name: ev.name,
                                                status: ev.status,
                                            },
                                        )
                                    );
                                }
                                None => break,
                            }
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
                let tools = state.runtime.tools.clone();
                state.runtime.spawn_with_child_token(
                    "skills-watcher",
                    move |cancel| async move {
                        tools
                            .run_skills_watcher(
                                std::time::Duration::from_secs(3),
                                cancel,
                                move || {
                                    log_ignored_result!(
                                        "event.skills_status_changed",
                                        emit_handle.emit(
                                            SKILLS_STATUS_CHANGED_EVENT,
                                            SkillsStatusChangedEvent {
                                                op: SkillsStatusOperation::AutoRefresh,
                                            },
                                        )
                                    );
                                },
                            )
                            .await;
                    },
                );
            }

            // Wire up the AgentEventEmitter to the app handle via an EventBus,
            // allowing multiple subscribers (frontend, log recorder, …).
            let bus = state.runtime.agent.install_event_bus();
            let emitter = Arc::new(TauriEmitter {
                handle: handle.clone(),
                chunk_seq: AtomicU64::new(0),
                notifications: notifications.clone(),
            });
            // Decouple the agent loops from the Tauri IPC subscriber chain:
            // emits become bounded-channel sends drained by a consumer session,
            // so a slow webview or toast notification can never stall agent
            // progress (previously every event was awaited end-to-end).
            let buffered = haven_agent::BufferedEmitter::new(1024, emitter);
            let session_error_emitter = buffered.clone();
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(bus.subscribe("tauri", buffered));
            });

            // Deferred cold-start work (MCP connect, skills scan, audio
            // prewarm) runs after the window exists so the UI can paint a
            // 加载中 chip instead of sitting on a black webview. The event bus
            // is installed first so a fresh session can be dispatched
            // immediately while catalog discovery continues in the background.
            {
                let emit_handle = handle.clone();
                state.spawn_background_init(move |payload: AppBootstrapEvent| {
                    log_ignored_result!(
                        "event.app_bootstrap",
                        emit_handle.emit(APP_BOOTSTRAP_EVENT, payload)
                    );
                });
            }

            // Project the typed ToolRun lifecycle stream into the App-owned
            // IPC DTO before it reaches the frontend.
            let tool_run_sink_handle = handle.clone();
            state.runtime.services.tool_runs.set_lifecycle_event_sink(Arc::new(
                move |event: ToolRunLifecycleEvent| {
                    emit_tool_run_event(&tool_run_sink_handle, event);
                },
            ));

            // Foreground tool live-output previews (`agent:tool_output`) so
            // shell (and future long-running tools) can expand the chat card
            // while still running.
            let tool_output_handle = handle.clone();
            state
                .runtime
                .services
                .live_output
                .set_event_sink(Arc::new(
                    move |event: String, payload: serde_json::Value| {
                        if event != AGENT_TOOL_OUTPUT_EVENT {
                            tracing::warn!(event, "dropping unknown live tool-output event");
                            return;
                        }
                        match serde_json::from_value::<AgentToolOutputEvent>(payload) {
                            Ok(projected) => {
                                log_ignored_result!(
                                    "event.agent_tool_output",
                                    tool_output_handle.emit(AGENT_TOOL_OUTPUT_EVENT, projected)
                                );
                            }
                            Err(_error) => {
                                tracing::warn!(
                                    event,
                                    failure_kind = "tool_output_decode",
                                    "dropping malformed live tool-output event"
                                );
                            }
                        }
                    },
                ));

            let cfg = state.runtime.config_service.snapshot()?.config;
            let is_hold = cfg.hotkey.mode == haven_common::types::HotkeyMode::Hold;
            let key_binding = cfg.hotkey.key_binding.clone();

            // The global-shortcut and tray callbacks run on plugin/main
            // threads that are outside the tokio runtime. ApplicationRuntime
            // stores the runtime handle, so these short async callbacks can
            // still be registered and cancelled without relying on
            // `Handle::current()` being available on the callback thread.

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
                    let runtime = state.runtime.clone();
                    match id {
                        "show" => {
                            let _ = app.get_webview_window("main").map(|w| {
                                log_ignored_result!("tray.show", w.show());
                                log_ignored_result!("tray.set_focus", w.set_focus());
                            });
                        }
                        "mute" => {
                            let shell = state.runtime.shell.clone();
                            runtime.spawn("tray-mute", async move {
                                    let shell_state = shell.state().await;
                                    shell.set_muted(!shell_state.is_muted).await;
                            });
                        }
                        "settings" => {
                            let _ = app.get_webview_window("main").map(|w| {
                                log_ignored_result!(
                                    "tray.settings.navigate",
                                    w.eval("window.location.href = '/?tab=settings'")
                                );
                                log_ignored_result!("tray.settings.show", w.show());
                                log_ignored_result!("tray.settings.set_focus", w.set_focus());
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
                            let is_visible = match w.is_visible() {
                                Ok(is_visible) => is_visible,
                                Err(error) => {
                                    tracing::debug!(
                                        error = %sanitize_error_text(&error.to_string()),
                                        "failed to read main window visibility"
                                    );
                                    false
                                }
                            };
                            if is_visible {
                                log_ignored_result!("tray.toggle.hide", w.hide());
                            } else {
                                log_ignored_result!("tray.toggle.show", w.show());
                                log_ignored_result!("tray.toggle.set_focus", w.set_focus());
                            }
                        });
                    }
                })
                .build(app)?;

            // Wire up shell handler (replaces former per-callback field assignments)
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                let shell_arc = state.runtime.shell.clone();
                let input_pipeline = state.runtime.input_pipeline.clone();
                let tray_ref = tray.clone();
                let handler = Arc::new(HavenShellHandler {
                    app_h: handle.clone(),
                    input_pipeline,
                    shell_arc: shell_arc.clone(),
                    tray: tray_ref,
                });
                shell_arc.set_handler(handler);

                // Wire up input event handler (VAD status + auto-stop)
                {
                    let app_h = handle.clone();
                    let shell_arc = state.runtime.shell.clone();
                    state
                        .runtime
                        .input_pipeline
                        .install_event_handler(Arc::new(HavenInputEventHandler { app_h, shell_arc }));
                }

                rt.block_on(shell.set_hold_mode(is_hold));

                // Consume the supervisor's typed event stream. Confirmation
                // details stay in the backend; the renderer receives only the
                // existing safe summary/permission projection.
                {
                    let app_h = handle.clone();
                    let st_arc = state.inner().clone();
                    let interaction_notifications = notifications.clone();
                    let session_error_emitter = session_error_emitter.clone();
                    state.runtime.spawn_with_child_token(
                        "session-event-forwarder",
                        move |cancel| async move {
                            let mut events = st_arc.runtime.executor.subscribe_events();
                            loop {
                                let event = tokio::select! {
                                    _ = cancel.cancelled() => return,
                                    result = events.recv() => match result {
                                        Ok(event) => event,
                                        Err(_) => return,
                                    }
                                };
                                match event {
                                    haven_agent::SessionSupervisorEvent::InteractionRequested { envelope } => {
                                        let haven_agent::InteractionEnvelope { owner, request } = *envelope;
                                        tracing::info!(
                                            session_id = ?request.session_id,
                                            request_id = %request.request_id,
                                            interaction_kind = ?request.kind,
                                            status = ?request.status,
                                            "interaction request lifecycle changed"
                                        );
                                        interaction_notifications
                                            .maybe_show_interaction_request(&request, &owner);
                                        log_ignored_result!(
                                            "event.interaction_requested",
                                            app_h.emit(
                                                INTERACTION_REQUESTED_EVENT,
                                                project_interaction(&request, owner),
                                            )
                                        );
                                    }
                                    haven_agent::SessionSupervisorEvent::SessionError {
                                        session_id,
                                        reason,
                                    } => {
                                        // Re-enter the shared bounded emitter so
                                        // terminal failure follows queued chunks
                                        // and committed Agent events in order.
                                        haven_agent::AgentEventEmitter::emit(
                                            session_error_emitter.as_ref(),
                                            haven_agent::AgentEvent::SessionError {
                                                session_id,
                                                error: reason,
                                            },
                                        )
                                        .await;
                                    }
                                    haven_agent::SessionSupervisorEvent::ScheduledConfirmOutcome {
                                        ..
                                    }
                                    | haven_agent::SessionSupervisorEvent::SessionResumed { .. }
                                    | haven_agent::SessionSupervisorEvent::SessionEndPaused { .. }
                                    | haven_agent::SessionSupervisorEvent::SessionCleanup { .. }
                                    | haven_agent::SessionSupervisorEvent::CascadeCompleted { .. } => {}
                                }
                            }
                        },
                    );
                }
            });

            // --------------------- Global hotkey ---------------------
            use tauri_plugin_global_shortcut::{
                Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
            };

            let shortcut = KeyCombo::parse(&key_binding)
                .and_then(|combo| to_tauri_shortcut(&combo))
                .unwrap_or_else(|| {
                    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
                });

            let _sc = shortcut;
            let result = handle
                .global_shortcut()
                .on_shortcut(shortcut, move |app, _sc, event| {
                    let state = app.state::<Arc<AppState>>();
                    if state.hotkey_capture_active.load(Ordering::Acquire) {
                        return;
                    }
                    let runtime = state.runtime.clone();
                    let shell = state.runtime.shell.clone();
                    let tools = state.runtime.tools.clone();
                    let hotkey_capture_active = state.hotkey_capture_active.clone();
                    let app_h = app.clone();
                    let pressed = event.state == ShortcutState::Pressed;
                    // `spawn` (unlike `block_on`) is safe from any thread, so a
                    // shortcut callback firing on the async runtime's own thread
                    // can't panic with "Cannot start a runtime from within a
                    // runtime".
                    runtime.spawn("global-hotkey", async move {
                        if hotkey_capture_active.load(Ordering::Acquire) {
                            return;
                        }
                        let shell_state = shell.state().await;
                        if shell_state.is_muted {
                            return;
                        }
                        // 快捷键唤起：先显示并聚焦前端窗口（含自启隐藏后的
                        // 后台场景），再开始/结束录音。
                        if pressed && let Some(w) = app_h.get_webview_window("main") {
                            log_ignored_result!("hotkey.show", w.show());
                            log_ignored_result!("hotkey.set_focus", w.set_focus());
                        }
                        // Voice ingress needs a transcription route, but this
                        // capability gate belongs to the media runtime rather
                        // than the capture pipeline.
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
                });

            match result {
                Ok(_) => {
                    tracing::info!("Hotkey registered: {}", key_binding);
                }
                Err(e) => {
                    tracing::warn!(
                        binding = %key_binding,
                        error = %sanitize_error_text(&e.to_string()),
                        "Hotkey conflict detected"
                    );
                    log_ignored_result!(
                        "event.hotkey_conflict",
                        handle.emit(
                            HOTKEY_CONFLICT_EVENT,
                            HotkeyConflictEvent {
                                binding: key_binding,
                                error: sanitize_error_text(&e.to_string()),
                            },
                        )
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
            commands::session::get_latest_session_for_resume,
            commands::session::list_runtime_sessions,
            commands::session::get_session_lineage,
            commands::tool_runs::list_tool_runs,
            commands::tool_runs::cancel_tool_run,
            commands::tool_runs::list_tool_run_history,
            commands::tool_runs::clear_tool_run_history,
            commands::session::end_session,
            commands::session::interrupt_session,
            commands::session::resolve_confirmation,
            commands::recording::set_hotkey_capture_active,
            commands::session_history::list_session_history,
            commands::session_history::count_session_history,
            commands::session_history::search_session_history_filtered,
            commands::session_history::count_session_history_search,
            commands::session::delete_session,
            commands::session::delete_all_sessions,
            commands::model::get_api_key_status,
            commands::model::check_llm_connection,
            commands::settings::get_bootstrap_status,
            commands::model::discover_models,
            commands::model::switch_model,
            commands::model::set_reasoning_effort,
            commands::model::set_web_search,
            commands::memory::run_memory_maintenance,
            commands::memory::recall_memory,
            commands::mcp::list_mcp_servers,
            commands::mcp::list_mcp_resources,
            commands::mcp::read_mcp_resource,
            commands::mcp::list_mcp_prompts,
            commands::mcp::get_mcp_prompt,
            commands::mcp::reconnect_mcp_server,
            commands::mcp::refresh_mcp_servers,
            commands::mcp::add_mcp_server,
            commands::mcp::update_mcp_server,
            commands::mcp::remove_mcp_server,
            commands::mcp::toggle_mcp_server,
            commands::skills::list_skills,
            commands::skills::refresh_skills,
            commands::skills::set_skill_enabled,
            commands::tools::set_tool_enabled,
            commands::skills::open_skills_dir,
            commands::external::open_external,
            commands::skills::execute_skill,
            commands::tools::list_builtin_tool_manifests,
            commands::tools::reset_tool_circuits,
            commands::tools::promote_sandbox_output,
            commands::tools::get_sandbox_output_review,
            commands::tools::discard_sandbox_output_review,
            commands::tools::pick_sandbox_output_destination,
            commands::memory::list_facts,
            commands::memory::add_fact,
            commands::memory::delete_fact,
            commands::memory::clear_facts,
            commands::settings::get_settings,
            commands::settings::pick_execution_workspace_directory,
            commands::settings::stage_provider_credential,
            commands::settings::stage_independent_search_credential,
            commands::settings::stage_ocr_credential,
            commands::settings::discard_staged_credentials,
            commands::settings::update_settings,
            commands::settings::list_session_permissions,
            commands::settings::revoke_permission,
            commands::settings::revoke_session_permission,
            commands::settings::reset_permissions,
            commands::settings::reset_session_permissions,
            commands::settings::check_shell_available,
            commands::settings::enable_autostart,
            commands::settings::disable_autostart,
            commands::settings::is_autostart_enabled,
            commands::session::get_session_for_resume,
            commands::session::rollback_session,
            commands::session::fork_session_from_message,
            commands::session::continue_session,
            commands::session::update_session_title,
            commands::log::get_log_info,
            commands::log::read_log_tail,
            commands::log::log_frontend_error,
            commands::diagnostics::get_performance_metrics,
        ])
        .build(context)
        .unwrap_or_else(|error| {
            if !app_state_initialization_failed.load(Ordering::Acquire) {
                let safe_error = sanitize_error_text(&error.to_string());
                tracing::error!(
                    error = %safe_error,
                    "error while building Haven app"
                );
                if should_show_startup_error(autostart::is_autostart_launch()) {
                    show_startup_error(StartupFailure::ApplicationBuild, Some(&safe_error));
                }
            }
            std::process::exit(1);
        })
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                tracing::info!("Haven app exit requested");
                let state = app_handle.state::<Arc<AppState>>();
                // Pause in-flight sessions so they survive a restart in a
                // resumable state. Without this, every still-`running` session
                // would be flipped to `error` at the next startup by
                // `finalize_orphaned_running_sessions` (which only intends to
                // catch crash leftovers).
                pause_running_sessions_on_exit(&state.runtime.session_store);
                state.runtime.teardown_blocking();
            }
        });
}

#[cfg(windows)]
fn install_webview_failure_handlers(
    window: &tauri::WebviewWindow,
    app: tauri::AppHandle,
    monitor: Arc<WebviewFailureMonitor>,
) {
    use webview2_com::{NavigationCompletedEventHandler, ProcessFailedEventHandler};

    let callback_monitor = Arc::clone(&monitor);
    let callback_app = app.clone();
    let result = window.with_webview(move |platform| {
        let controller = platform.controller();
        let core = match unsafe { controller.CoreWebView2() } {
            Ok(core) => core,
            Err(error) => {
                tracing::error!(
                    error = %sanitize_error_text(&error.to_string()),
                    "failed to access the main WebView2 controller"
                );
                callback_monitor.report(&callback_app, WebviewFailure::Navigation);
                return;
            }
        };

        let navigation_monitor = Arc::clone(&callback_monitor);
        let navigation_app = callback_app.clone();
        let navigation_handler =
            NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut success = unsafe { std::mem::zeroed() };
                    if unsafe { args.IsSuccess(std::ptr::addr_of_mut!(success)) }.is_ok()
                        && !success.as_bool()
                    {
                        navigation_monitor.report(&navigation_app, WebviewFailure::Navigation);
                    }
                }
                Ok(())
            }));
        let mut navigation_token = 0;
        if let Err(error) =
            unsafe { core.add_NavigationCompleted(&navigation_handler, &mut navigation_token) }
        {
            tracing::error!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to observe WebView2 navigation failures"
            );
            callback_monitor.report(&callback_app, WebviewFailure::Navigation);
            return;
        }

        let process_monitor = Arc::clone(&callback_monitor);
        let process_app = callback_app.clone();
        let process_handler = ProcessFailedEventHandler::create(Box::new(move |_, _| {
            process_monitor.report(&process_app, WebviewFailure::Process);
            Ok(())
        }));
        let mut process_token = 0;
        if let Err(error) = unsafe { core.add_ProcessFailed(&process_handler, &mut process_token) }
        {
            tracing::error!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to observe WebView2 process failures"
            );
        }
    });
    if let Err(error) = result {
        tracing::error!(
            error = %sanitize_error_text(&error.to_string()),
            "failed to install WebView2 failure handlers"
        );
        monitor.report(&app, WebviewFailure::Navigation);
    }
}

fn pause_running_sessions_on_exit(session_store: &SessionStore) {
    match session_store.pause_running_sessions() {
        Ok(n) if n > 0 => {
            tracing::info!("paused {} running session(s) on exit", n);
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to pause running sessions on exit"
            );
        }
    }
}

/// Convert a neutral [`KeyCombo`] into the Tauri
/// global-shortcut type. Parsing/validation already happened in
/// `haven-input`; this is the only place Tauri shortcut types are built.
pub(crate) fn to_tauri_shortcut(
    combo: &KeyCombo,
) -> Option<tauri_plugin_global_shortcut::Shortcut> {
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
    if combo.has_modifier(KeyModifier::Control) {
        modifiers |= Modifiers::CONTROL;
    }
    if combo.has_modifier(KeyModifier::Shift) {
        modifiers |= Modifiers::SHIFT;
    }
    if combo.has_modifier(KeyModifier::Alt) {
        modifiers |= Modifiers::ALT;
    }
    if combo.has_modifier(KeyModifier::Super) {
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
    log_config: Arc<std::sync::Mutex<LogConfig>>,
    config_loader: haven_common::config::ConfigLoader,
) -> anyhow::Result<AppState> {
    let file_logging_enabled = log_config
        .lock()
        .map(|config| config.file_enabled)
        .unwrap_or(false);
    let db_path = haven_common::config::ConfigLoader::data_dir().join("haven.db");
    if let Some(parent) = db_path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            error = %sanitize_error_text(&error.to_string()),
            "failed to create application data directory"
        );
    }
    let fh = filter_handles.clone();
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(AppState::new(
            &db_path,
            fh,
            config_loader,
            file_logging_enabled,
        ))
    });
    if let Err(error) = &result {
        tracing::error!(
            error = %sanitize_error_text(&error.to_string()),
            "failed to initialize application state"
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{pause_running_sessions_on_exit, project_interaction};
    use haven_common::lifecycle::SessionStatus;
    use haven_memory::{Database, SessionStore};
    use std::sync::Arc;

    #[test]
    fn interaction_projection_keeps_typed_lifecycle_values_on_the_existing_wire() {
        let request = haven_agent::InteractionRequest::ask(
            "ses-e47db875d9bb819c9b621fed98ffe82b",
            Vec::new(),
            vec!["step-8035d06884c9e501700b994a5f4d4f55".into()],
        );
        let event = project_interaction(
            &request,
            haven_agent::InteractionOwner::Session {
                session_id: "ses-e47db875d9bb819c9b621fed98ffe82b".into(),
            },
        );

        let wire = serde_json::to_value(event).unwrap();
        assert_eq!(wire["kind"], "ask");
        assert_eq!(wire["status"], "pending");
        assert_eq!(
            wire["owner"],
            serde_json::json!({
                "kind": "session",
                "session_id": "ses-e47db875d9bb819c9b621fed98ffe82b",
            })
        );
    }

    #[test]
    fn exit_pause_helper_uses_session_store_to_pause_running_sessions() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("in-flight session").unwrap();
        db.update_session_status(&session.session_id, SessionStatus::Running)
            .unwrap();
        let session_store = SessionStore::new(db.clone());

        pause_running_sessions_on_exit(&session_store);

        assert_eq!(
            db.get_session(&session.session_id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
    }
}
