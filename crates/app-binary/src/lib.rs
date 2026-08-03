mod app_state;
mod commands;
mod desktop;
mod events;

use crate::desktop::TrayStatus;
use app_state::AppState;
use haven_agent::{AgentEvent, AgentEventEmitter};
use haven_common::config::LogConfig;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri_plugin_notification::NotificationExt;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::reload;

/// Initialize the tracing subscriber with console output and optional rolling file output.
/// A single reloadable filter is applied at the subscriber level so runtime
/// log-level changes affect both console and file output simultaneously.
fn init_tracing(
    log_cfg: &LogConfig,
) -> (
    Vec<reload::Handle<EnvFilter, Registry>>,
    Arc<std::sync::Mutex<LogConfig>>,
) {
    let level_str = log_cfg.level.as_str();

    // Single reloadable filter applied at the subscriber level — both
    // console and file layers inherit it, so updating the filter at
    // runtime changes both outputs.
    let (reloadable, handle) = reload::Layer::new(EnvFilter::new(format!("haven={}", level_str)));

    let subscriber = tracing_subscriber::registry().with(reloadable);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_line_number(true);

    let subscriber = subscriber.with(fmt_layer);

    let handles = vec![handle];

    if log_cfg.file_enabled {
        let log_path = log_cfg
            .file_path
            .clone()
            .unwrap_or_else(LogConfig::default_log_path);
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file_appender = tracing_appender::rolling::daily(
            log_path.parent().unwrap_or(std::path::Path::new(".")),
            log_path
                .file_stem()
                .unwrap_or(std::ffi::OsStr::new("haven")),
        );
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file_appender)
            .with_target(true)
            .with_line_number(true)
            .with_ansi(false);

        let subscriber = subscriber.with(file_layer);
        let _ = tracing::subscriber::set_global_default(subscriber);
    } else {
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    let log_config = Arc::new(std::sync::Mutex::new(log_cfg.clone()));
    (handles, log_config)
}

struct TauriEmitter {
    handle: tauri::AppHandle,
    chunk_seq: AtomicU64,
}

#[async_trait::async_trait]
impl AgentEventEmitter for TauriEmitter {
    async fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::Thought {
                task_id,
                thought,
                step_number,
                run_id,
            } => {
                tracing::debug!(
                    "TauriEmitter::on_thought: task={} step={} run={} len={}",
                    task_id,
                    step_number,
                    run_id,
                    thought.len()
                );
                let _ = self.handle.emit(
                    "agent:thought",
                    serde_json::json!({
                        "task_id": task_id,
                        "thought": thought,
                        "step_number": step_number,
                        "run_id": run_id,
                    }),
                );
            }
            AgentEvent::Action {
                task_id,
                tool_name,
                input,
                step_number,
                run_id,
                tool_call_id,
            } => {
                let silent = input
                    .get("silent")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                tracing::debug!(
                    "TauriEmitter::on_action: task={} tool={} step={} run={}",
                    task_id,
                    tool_name,
                    step_number,
                    run_id
                );
                let _ = self.handle.emit(
                    "agent:action",
                    serde_json::json!({
                        "task_id": task_id,
                        "tool_name": tool_name,
                        "input": input,
                        "step_number": step_number,
                        "run_id": run_id,
                        "silent": silent,
                        "tool_call_id": tool_call_id,
                    }),
                );
            }
            AgentEvent::Observation {
                task_id,
                observation,
                tool_name,
                step_number,
                run_id,
                silent,
                tool_call_id,
                ask_options,
            } => {
                tracing::debug!(
                    "TauriEmitter::on_observation: task={} tool={} step={} run={} silent={}",
                    task_id,
                    tool_name,
                    step_number,
                    run_id,
                    silent
                );
                let _ = self.handle.emit(
                    "agent:observation",
                    serde_json::json!({
                        "task_id": task_id,
                        "observation": observation,
                        "tool_name": tool_name,
                        "step_number": step_number,
                        "run_id": run_id,
                        "silent": silent,
                        "tool_call_id": tool_call_id,
                        "ask_options": ask_options,
                    }),
                );
            }
            AgentEvent::TaskCreated(task) => {
                tracing::info!(
                    "TauriEmitter::on_task_created: task_id={} status={}",
                    task.id,
                    task.status.as_str()
                );
                let _ = self.handle.emit(
                    "task:created",
                    serde_json::json!({
                        "task_id": task.id,
                        "status": task.status.as_str(),
                        "title": task.title,
                    }),
                );
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.task_created.windows)
                    .unwrap_or(false);
                if notify {
                    let display = if task.input.is_empty() {
                        task.id
                    } else {
                        task.input
                    };
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven")
                        .body(format!("New task: {}", display))
                        .show();
                }
            }
            AgentEvent::TaskCompleted { task_id, title } => {
                tracing::info!(
                    "TauriEmitter::on_task_completed: task={} title={}",
                    task_id,
                    title
                );
                let _ = self.handle.emit(
                    "task:completed",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "completed",
                        "title": title,
                    }),
                );
                let _ = self.handle.emit(
                    "task:updated",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "completed",
                        "title": title,
                    }),
                );
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.task_completed.windows)
                    .unwrap_or(true);
                if notify {
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven")
                        .body(format!("Task completed: {}", title))
                        .show();
                }
            }
            AgentEvent::TaskUpdated { task_id, status } => {
                tracing::info!(
                    "TauriEmitter::on_task_updated: task={} status={}",
                    task_id,
                    status
                );
                if status == "paused" {
                    tracing::warn!(
                        "TauriEmitter emitting task:updated with paused status for task {}",
                        task_id
                    );
                }
                let _ = self.handle.emit(
                    "task:updated",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": status,
                        "title": "",
                    }),
                );
            }
            AgentEvent::TaskError { task_id, error } => {
                let _ = self.handle.emit(
                    "task:error",
                    serde_json::json!({
                        "task_id": task_id,
                        "error": error,
                    }),
                );
                let _ = self.handle.emit(
                    "task:updated",
                    serde_json::json!({
                        "task_id": task_id,
                        "status": "error",
                        "error": error,
                        "title": "",
                    }),
                );
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.task_error.windows)
                    .unwrap_or(true);
                if notify {
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven - Error")
                        .body(format!("Task error: {}", error))
                        .show();
                }
            }
            AgentEvent::Notification {
                task_id,
                title,
                body,
            } => {
                tracing::info!(
                    "TauriEmitter::on_notification: task={} title={} body={}",
                    task_id,
                    title,
                    body
                );
                // In-app toast: the frontend shows it via addNotification.
                let _ = self.handle.emit(
                    "notification:show",
                    serde_json::json!({
                        "task_id": task_id,
                        "title": title,
                        "body": body,
                    }),
                );
                // Windows desktop notification. The `notify` tool is an
                // explicit agent request, so both channels are used by default.
                let _ = self
                    .handle
                    .notification()
                    .builder()
                    .title(if title.is_empty() { "Haven" } else { &title })
                    .body(body)
                    .show();
            }
            AgentEvent::TitleUpdated { task_id, title } => {
                let _ = self.handle.emit(
                    "task:title-updated",
                    serde_json::json!({
                        "task_id": task_id,
                        "title": title,
                    }),
                );
            }
            AgentEvent::BalancedModelActivated { task_id, reason } => {
                let _ = self.handle.emit(
                    "agent:balanced_model",
                    serde_json::json!({
                        "task_id": task_id,
                        "reason": reason,
                    }),
                );
            }
            AgentEvent::ThoughtChunk {
                task_id,
                delta,
                step_number,
                run_id,
            } => {
                let seq = self.chunk_seq.fetch_add(1, Ordering::Relaxed);
                let _ = self.handle.emit(
                    "agent:thought_chunk",
                    serde_json::json!({
                        "task_id": task_id,
                        "delta": delta,
                        "step_number": step_number,
                        "run_id": run_id,
                        "seq": seq,
                    }),
                );
            }
            AgentEvent::ReasoningChunk {
                task_id,
                delta,
                step_number,
                run_id,
            } => {
                let seq = self.chunk_seq.fetch_add(1, Ordering::Relaxed);
                let _ = self.handle.emit(
                    "agent:reasoning_chunk",
                    serde_json::json!({
                        "task_id": task_id,
                        "delta": delta,
                        "step_number": step_number,
                        "run_id": run_id,
                        "seq": seq,
                    }),
                );
            }
            AgentEvent::Supplement {
                task_id,
                additional_context,
                step_number,
                run_id,
            } => {
                let _ = self.handle.emit(
                    "agent:supplement",
                    serde_json::json!({
                        "task_id": task_id,
                        "additional_context": additional_context,
                        "step_number": step_number,
                        "run_id": run_id,
                    }),
                );
            }
            AgentEvent::Compaction {
                task_id,
                summary,
                tokens_before,
                tokens_after,
            } => {
                tracing::debug!(
                    "TauriEmitter::on_compaction: task={} tokens {}→{}",
                    task_id,
                    tokens_before,
                    tokens_after
                );
                let _ = self.handle.emit(
                    "agent:compaction",
                    serde_json::json!({
                        "task_id": task_id,
                        "summary": summary,
                        "tokens_before": tokens_before,
                        "tokens_after": tokens_after,
                    }),
                );
            }
            AgentEvent::Usage {
                task_id,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cost_usd,
                model,
                cumulative_prompt_tokens,
                cumulative_completion_tokens,
                cumulative_total_tokens,
                cumulative_cost_usd,
                context_window,
            } => {
                let _ = self.handle.emit(
                    "agent:usage",
                    serde_json::json!({
                        "task_id": task_id,
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": total_tokens,
                        "cost_usd": cost_usd,
                        "model": model,
                        "cumulative_prompt_tokens": cumulative_prompt_tokens,
                        "cumulative_completion_tokens": cumulative_completion_tokens,
                        "cumulative_total_tokens": cumulative_total_tokens,
                        "cumulative_cost_usd": cumulative_cost_usd,
                        "context_window": context_window,
                    }),
                );
            }
        }
    }
}

/// Concrete `ShellHandler` wiring desktop hooks to the Tauri app handle,
/// input pipeline and tray icon. Replaces the former per-callback field
/// assignments on `DesktopShell`.
struct HavenShellHandler {
    app_h: tauri::AppHandle,
    pipeline: Arc<haven_input::InputPipeline>,
    shell_arc: Arc<desktop::DesktopShell>,
    tray: tauri::tray::TrayIcon,
}

#[async_trait::async_trait]
impl desktop::ShellHandler for HavenShellHandler {
    async fn on_recording_start(&self) {
        // Start the pipeline first: emitting `recording:started` before the
        // pipeline is actually recording would leave the UI stuck in the
        // recording state (and every stop attempt failing with "not
        // recording") if startup errors.
        if let Err(e) = self.pipeline.start_recording().await {
            tracing::warn!("pipeline start_recording failed: {e}");
            self.shell_arc.stop_recording().await;
            let _ = self.app_h.emit(
                "recording:error",
                serde_json::json!({
                    "session_id": uuid::Uuid::new_v4().to_string(),
                    "error": format!("录音启动失败，请检查麦克风/STT 配置: {e}"),
                }),
            );
            return;
        }
        let _ = self.app_h.emit(
            "recording:started",
            events::RecordingEvent {
                is_recording: true,
                session_id: Some(uuid::Uuid::new_v4().to_string()),
                reason: None,
                duration_ms: None,
            },
        );
    }

    async fn on_recording_stop(&self) {
        // Same split as the `stop_recording` Tauri command: stop the audio
        // capture first and notify the UI, then run STT in the background.
        // Without this, VAD-triggered auto-stops would also keep the
        // "recording" overlay visible for the duration of the STT call.
        let result = self.pipeline.stop_capture().await;
        if let Ok(result) = result {
            let reason_str = match result.reason {
                haven_input::RecordingReason::Manual => "manual",
                haven_input::RecordingReason::Silence => "silence",
                haven_input::RecordingReason::MaxDuration => "max_duration",
                haven_input::RecordingReason::Cancel => "cancel",
            };
            let _ = self.app_h.emit(
                "recording:stopped",
                events::RecordingEvent {
                    is_recording: false,
                    session_id: None,
                    reason: Some(reason_str.to_string()),
                    duration_ms: Some(result.duration_ms),
                },
            );
            if matches!(
                result.reason,
                haven_input::RecordingReason::Silence | haven_input::RecordingReason::MaxDuration
            ) {
                self.shell_arc.reset_toggle_on_auto_stop().await;
            }

            // Same finalize path as the `stop_recording` Tauri command: run
            // STT, emit `transcription:result` / `transcription:error` and
            // auto-submit the transcript to the agent in the background.
            // Without this, hotkey / VAD-triggered stops silently dropped the
            // transcript — the text never reached the chat UI nor the agent.
            let state = self.app_h.state::<Arc<AppState>>();
            crate::commands::finalize_transcription(state.inner(), &self.app_h, result).await;
        }
    }

    fn on_tray_status(&self, status: TrayStatus) {
        let tooltip = match status {
            TrayStatus::Normal => "Haven",
            TrayStatus::Recording => "Haven - Recording",
            TrayStatus::Muted => "Haven - Muted",
            TrayStatus::Busy => "Haven - Busy",
        };
        let _ = self.tray.set_icon(Some(make_tray_icon(status)));
        let _ = self.app_h.emit(
            "tray:status_changed",
            serde_json::json!({
                "status": match status {
                    TrayStatus::Normal => "normal",
                    TrayStatus::Recording => "recording",
                    TrayStatus::Muted => "muted",
                    TrayStatus::Busy => "busy",
                },
                "tooltip": tooltip,
            }),
        );
    }

    fn on_mute_change(&self, muted: bool) {
        let _ = self
            .app_h
            .emit("mute:changed", serde_json::json!({ "muted": muted }));
    }
}

/// Concrete `InputHandler` wiring VAD status + auto-stop to the Tauri app
/// handle and the desktop shell. Replaces the former separate
/// `set_vad_status_callback` + `set_on_auto_stop` bindings on `InputPipeline`.
struct HavenInputHandler {
    app_h: tauri::AppHandle,
    shell_arc: Arc<desktop::DesktopShell>,
}

#[async_trait::async_trait]
impl haven_input::InputHandler for HavenInputHandler {
    fn on_vad_status(
        &self,
        signal: haven_input::vad::VadSignal,
        state: haven_input::vad::VadState,
    ) {
        let signal_str = match signal {
            haven_input::vad::VadSignal::None => "none",
            haven_input::vad::VadSignal::SpeechStart => "speech_start",
            haven_input::vad::VadSignal::SpeechEnd => "speech_end",
            haven_input::vad::VadSignal::AutoStop => "auto_stop",
        };
        let state_str = match state {
            haven_input::vad::VadState::Silent => "silent",
            haven_input::vad::VadState::Speech => "speech",
            haven_input::vad::VadState::SilenceAfterSpeech { .. } => "silence_after_speech",
        };
        let _ = self.app_h.emit(
            "recording:vad_status",
            events::VadStatusEvent {
                signal: signal_str.to_string(),
                state: state_str.to_string(),
            },
        );
    }

    async fn on_auto_stop(&self) {
        self.shell_arc.stop_recording().await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
        tracing::error!("PANIC at {}: {}", location, msg);
        prev_hook(panic_info);
    }));

    let app_state = init_app_state(filter_handles, log_config, config_loader);

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
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
        .manage(Arc::new(app_state))
        .setup(|app| {
            let handle = app.handle().clone();

            let state = app.state::<Arc<AppState>>();
            let shell = &state.shell;

            // Wire up the AgentEventEmitter to the app handle via an EventBus,
            // allowing multiple subscribers (frontend, log recorder, …).
            let bus = state.agent.install_event_bus();
            let emitter = Arc::new(TauriEmitter {
                handle: handle.clone(),
                chunk_seq: AtomicU64::new(0),
            });
            tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(bus.subscribe("tauri", emitter));
            });

            let cfg = state.config_loader.lock().unwrap();
            let is_hold = cfg.config().hotkey.mode == haven_common::types::HotkeyMode::Hold;
            let key_binding = cfg.config().hotkey.key_binding.clone();

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
            let menu = MenuBuilder::new(app).items(&[&show, &mute, &settings, &quit]).build()?;

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
                            // (pause running tasks, close the active session)
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
                rt.block_on(shell_arc.set_handler(handler));

                // Wire up unified input handler (VAD status + auto-stop)
                {
                    let app_h = handle.clone();
                    let shell_arc = state.shell.clone();
                    rt.block_on(async {
                        state.pipeline.set_handler(Arc::new(HavenInputHandler {
                            app_h,
                            shell_arc,
                        }));
                    });
                }

                rt.block_on(shell.set_hold_mode(is_hold));

                // Wire up confirm callback
                {
                    let app_h = handle.clone();
                    let st_arc = state.inner().clone();
                    let st_arc2 = st_arc.clone();
                    rt.block_on(async {
                        *st_arc.executor.on_confirm_request.lock().await = Some(Box::new(move |step_id: String, tool_name: String, risk_level: haven_common::types::RiskLevel| {
                            let task_id = tokio::task::block_in_place(|| {
                                let rt = tokio::runtime::Handle::current();
                                rt.block_on(async {
                                    let tasks = st_arc2.executor.list_tasks().await;
                                    tasks.iter()
                                        .find(|t| t.steps.iter().any(|s| s.id == step_id))
                                        .map(|t| t.id.clone())
                                        .unwrap_or_default()
                                })
                            });
                            let _ = app_h.emit("confirm:requested", serde_json::json!({
                                "step_id": step_id,
                                "tool_name": tool_name,
                                "risk_level": risk_level,
                                "task_id": task_id,
                            }));
                        }));
                    });
                }
            });

            // --------------------- Global hotkey ---------------------
            use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

            let shortcut = parse_shortcut(&key_binding).unwrap_or_else(|| {
                Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
            });

            let _sc = shortcut;
            let result = handle.global_shortcut().on_shortcut(shortcut, move |app, _sc, event| {
                let state = app.state::<Arc<AppState>>();
                let shell = state.shell.clone();
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
                    let _ = handle.emit("hotkey:conflict", serde_json::json!({
                        "binding": key_binding,
                        "error": e.to_string(),
                    }));
                }
            }

            tracing::info!("Haven Tauri app initialized");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::cancel_recording,
            commands::process_transcript,
            commands::reopen_task,
            commands::get_last_conversation,
            commands::get_tasks,
            commands::end_task,
            commands::resolve_confirmation,
            commands::get_tools,
            commands::get_recording_state,
            commands::get_history,
            commands::count_history,
            commands::search_history,
            commands::search_history_filtered,
            commands::search_history_paginated,
            commands::count_history_search,
            commands::delete_task,
            commands::clear_history,
            commands::get_api_key_status,
            commands::check_llm_connection,
            commands::list_models,
            commands::discover_models,
            commands::switch_model,
            commands::set_reasoning_effort,
            commands::list_mcp_tools,
            commands::reconnect_mcp,
            commands::mcp_tool_call,
            commands::add_mcp_server,
            commands::update_mcp_server,
            commands::remove_mcp_server,
            commands::toggle_mcp_server,
            commands::configure_mcp,
            commands::list_skills,
            commands::refresh_skills,
            commands::set_skill_enabled,
            commands::open_skills_dir,
            commands::execute_skill,
            commands::list_facts,
            commands::add_fact,
            commands::delete_fact,
            commands::get_preference,
            commands::list_preferences,
            commands::update_preference,
            commands::delete_preference,
            commands::get_settings,
            commands::update_settings,
            commands::export_history,
            commands::enable_autostart,
            commands::disable_autostart,
            commands::is_autostart_enabled,
            commands::get_task_for_review,
            commands::rollback_task,
            commands::branch_task,
            commands::continue_task,
            commands::update_task_title,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Haven app")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                tracing::info!("Haven app exit requested");
                let state = app_handle.state::<Arc<AppState>>();
                // Pause in-flight tasks so they survive a restart in a
                // resumable state. Without this, every still-`running` task
                // would be flipped to `error` at the next startup by
                // `finalize_orphaned_running_tasks` (which only intends to
                // catch crash leftovers).
                if let Ok(n) = state.db.pause_running_tasks()
                    && n > 0
                {
                    tracing::info!("paused {} running task(s) on exit", n);
                }
            }
        });
}

fn parse_shortcut(binding: &str) -> Option<tauri_plugin_global_shortcut::Shortcut> {
    use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};
    let parts: Vec<&str> = binding.split('+').collect();
    let mut modifiers = Modifiers::empty();
    let mut key = "";
    for part in &parts {
        match *part {
            "Ctrl" | "Control" => modifiers |= Modifiers::CONTROL,
            "Shift" => modifiers |= Modifiers::SHIFT,
            "Alt" => modifiers |= Modifiers::ALT,
            "Super" | "Win" | "Cmd" => modifiers |= Modifiers::SUPER,
            _ => key = part,
        }
    }
    let code = match key.to_lowercase().as_str() {
        "space" => Code::Space,
        "enter" => Code::Enter,
        "escape" => Code::Escape,
        "tab" => Code::Tab,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        _ => return None,
    };
    Some(Shortcut::new(Some(modifiers), code))
}

fn make_tray_icon(status: TrayStatus) -> tauri::image::Image<'static> {
    let (r, g, b) = match status {
        TrayStatus::Normal => (60, 100, 200),
        TrayStatus::Recording => (220, 50, 50),
        TrayStatus::Muted => (120, 120, 120),
        TrayStatus::Busy => (220, 140, 40),
    };
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for _ in 0..(32 * 32) {
        rgba.push(r);
        rgba.push(g);
        rgba.push(b);
        rgba.push(255);
    }
    tauri::image::Image::new_owned(rgba, 32, 32)
}

fn init_app_state(
    filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    _log_config: Arc<std::sync::Mutex<LogConfig>>,
    config_loader: haven_common::config::ConfigLoader,
) -> AppState {
    let db_path = app_data_dir().join("haven.db");
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let fh = filter_handles.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(AppState::new(&db_path, fh, config_loader))
    })
    .unwrap_or_else(|e| {
        tracing::error!("failed to initialize application state: {}", e);
        let db = Arc::new(haven_memory::Database::open(&db_path).unwrap_or_else(|_| {
            std::process::exit(1);
        }));
        let tools = Arc::new(haven_tools::ToolsManager::new());
        let executor = Arc::new(haven_task::TaskExecutor::new(db.clone(), tools.clone(), 3));
        let router = Arc::new(haven_llm::LlmRouter::new(
            haven_common::config::LlmConfig::default(),
        ));
        let agent = Arc::new(haven_agent::AgentLayer::new(
            db.clone(),
            executor.clone(),
            router.clone(),
            30,
            50,
            8000,
        ));
        let pipeline = Arc::new(haven_input::InputPipeline::new());
        let shell = Arc::new(crate::desktop::DesktopShell::new());
        agent.clone().start();
        let config_loader_arc = Arc::new(std::sync::Mutex::new(
            haven_common::config::ConfigLoader::load().unwrap_or_else(|_| {
                haven_common::config::ConfigLoader::load_from(
                    &haven_common::config::ConfigLoader::default_path(),
                )
                .unwrap()
            }),
        ));
        AppState {
            db,
            tools,
            executor,
            agent,
            pipeline,
            shell,
            log_filter_handles: filter_handles,
            config_loader: config_loader_arc,
        }
    })
}

fn app_data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:".into());
            format!("{}\\AppData\\Roaming", home)
        });
        std::path::PathBuf::from(base).join("Haven")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        std::path::PathBuf::from(home).join(".local/share/haven")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_shortcut_ctrl_shift_space() {
        let s = parse_shortcut("Ctrl+Shift+Space");
        assert!(s.is_some());
    }

    #[test]
    fn test_parse_shortcut_single_key() {
        let s = parse_shortcut("a");
        assert!(s.is_some());
    }

    #[test]
    fn test_parse_shortcut_alt_tab() {
        let s = parse_shortcut("Alt+Tab");
        assert!(s.is_some());
    }

    #[test]
    fn test_parse_shortcut_control_alias() {
        let s = parse_shortcut("Control+C");
        assert!(s.is_some());
    }

    #[test]
    fn test_parse_shortcut_super_modifier() {
        let s = parse_shortcut("Super+Space");
        assert!(s.is_some());
        let s = parse_shortcut("Win+E");
        assert!(s.is_some());
    }

    #[test]
    fn test_parse_shortcut_invalid_key() {
        let s = parse_shortcut("Ctrl+InvalidKey");
        assert!(s.is_none());
    }

    #[test]
    fn test_parse_shortcut_empty() {
        let s = parse_shortcut("");
        assert!(s.is_none());
    }

    #[test]
    fn test_parse_shortcut_numeric_keys() {
        let s = parse_shortcut("Ctrl+F1");
        assert!(s.is_some());
        let s = parse_shortcut("Shift+F12");
        assert!(s.is_some());
    }

    #[test]
    fn test_parse_shortcut_letter_keys() {
        for ch in 'a'..='z' {
            let binding = format!("Ctrl+{}", ch);
            assert!(parse_shortcut(&binding).is_some(), "failed for {}", binding);
        }
    }

    #[test]
    fn test_make_tray_icon_creates_valid_image() {
        let img = make_tray_icon(TrayStatus::Normal);
        assert_eq!(img.width(), 32);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn test_make_tray_icon_all_statuses_have_correct_size() {
        for status in &[
            TrayStatus::Normal,
            TrayStatus::Recording,
            TrayStatus::Muted,
            TrayStatus::Busy,
        ] {
            let img = make_tray_icon(*status);
            assert_eq!(img.width(), 32, "failed for {:?}", status);
            assert_eq!(img.height(), 32, "failed for {:?}", status);
        }
    }

    #[test]
    fn test_init_tracing_creates_handle() {
        let cfg = LogConfig::default();
        let (_handles, _log_cfg) = init_tracing(&cfg);
        let cfg_ref = _log_cfg.lock().unwrap();
        assert_eq!(cfg_ref.level.as_str(), "info");
    }

    #[test]
    fn test_init_tracing_with_file_enabled() {
        let cfg = LogConfig {
            file_enabled: true,
            file_path: Some(std::env::temp_dir().join("haven_test_log")),
            ..Default::default()
        };
        let (_handles, _log_cfg) = init_tracing(&cfg);
    }

    #[test]
    fn test_app_data_dir_contains_haven() {
        let dir = app_data_dir();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.eq_ignore_ascii_case("haven"),
            "expected 'Haven' got '{}'",
            name
        );
    }

    #[test]
    fn test_app_data_dir_on_windows_uses_appdata() {
        #[cfg(target_os = "windows")]
        {
            let dir = app_data_dir();
            let s = dir.to_string_lossy();
            assert!(
                s.contains("AppData\\Roaming") || s.contains("APPDATA"),
                "expected AppData path, got: {}",
                s
            );
        }
    }
}
