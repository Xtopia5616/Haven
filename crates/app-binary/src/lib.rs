mod app_state;
mod autostart;
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
        if let Some(parent) = log_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("failed to create log directory {}: {}", parent.display(), e);
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
        self.trace_event(&event);
        let channel = Self::channel(&event);
        let chunk_seq = match &event {
            AgentEvent::ThoughtChunk { .. } | AgentEvent::ReasoningChunk { .. } => {
                Some(self.chunk_seq.fetch_add(1, Ordering::Relaxed))
            }
            _ => None,
        };
        let payload = Self::payload(&event, chunk_seq);
        let _ = self.handle.emit(channel, payload);
        self.emit_secondary(&event);
        self.maybe_show_toast(&event);
    }
}

impl TauriEmitter {
    /// 单一事实来源：AgentEvent 变体 → 前端订阅的 channel 名。
    fn channel(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::Thought { .. } => "agent:thought",
            AgentEvent::Action { .. } => "agent:action",
            AgentEvent::Observation { .. } => "agent:observation",
            AgentEvent::SessionCreated(_) => "session:created",
            AgentEvent::SessionCompleted { .. } => "session:completed",
            AgentEvent::SessionUpdated { .. } => "session:updated",
            AgentEvent::SessionError { .. } => "session:error",
            AgentEvent::Notification { .. } => "notification:show",
            AgentEvent::TitleUpdated { .. } => "session:title-updated",
            AgentEvent::BalancedModelActivated { .. } => "agent:balanced_model",
            AgentEvent::ThoughtChunk { .. } => "agent:thought_chunk",
            AgentEvent::ReasoningChunk { .. } => "agent:reasoning_chunk",
            AgentEvent::WebSearch { .. } => "agent:web_search",
            AgentEvent::StreamStalled { .. } => "agent:stream_stalled",
            AgentEvent::Supplement { .. } => "agent:supplement",
            AgentEvent::Compaction { .. } => "agent:compaction",
            AgentEvent::Usage { .. } => "agent:usage",
        }
    }

    /// 剥掉 serde 枚举 tag（`{"Thought": {...}}` → `{...}`），适用于除特例外的
    /// 所有变体。
    fn variant_payload(event: &AgentEvent) -> serde_json::Value {
        let v = serde_json::to_value(event).expect("AgentEvent is serializable");
        v.as_object()
            .expect("serialized AgentEvent is a map")
            .values()
            .next()
            .expect("serialized AgentEvent has exactly one variant")
            .clone()
    }

    /// 构造 wire 载荷。`variant_payload` 之外的五个特例在构造时覆盖：
    /// - `SessionCreated` 投影为 `{session_id, status, title}`，不泄漏 SessionInfo 内部的
    ///   `id` / `input` / `summary` 等字段
    /// - `SessionCompleted` 补 `status: "completed"`（变体本身没有该字段）
    /// - `SessionUpdated` 补 `title: ""`（wire 上始终带 title 键）
    /// - `Action` 额外派生 `silent`
    /// - `ThoughtChunk` / `ReasoningChunk` 插入单调递增的 `seq`（调用方传入已
    ///   自增的值，本函数保持纯函数化以便单测）
    fn payload(event: &AgentEvent, chunk_seq: Option<u64>) -> serde_json::Value {
        let mut payload = match event {
            AgentEvent::SessionCreated(session) => {
                return serde_json::json!({
                    "session_id": session.id,
                    "status": session.status.as_str(),
                    "title": session.title,
                });
            }
            AgentEvent::SessionCompleted { session_id, title } => {
                return serde_json::json!({
                    "session_id": session_id,
                    "status": "completed",
                    "title": title,
                });
            }
            AgentEvent::SessionUpdated { session_id, status } => {
                return serde_json::json!({
                    "session_id": session_id,
                    "status": status,
                    "title": "",
                });
            }
            _ => Self::variant_payload(event),
        };
        match event {
            AgentEvent::Action {
                tool_name, input, ..
            } => {
                payload["silent"] =
                    serde_json::json!(haven_tools::is_silent_action(tool_name, input));
            }
            AgentEvent::ThoughtChunk { .. } | AgentEvent::ReasoningChunk { .. } => {
                payload["seq"] = serde_json::json!(chunk_seq.unwrap_or(0));
            }
            _ => {}
        }
        payload
    }

    /// 保留原有按变体区分的 tracing 日志（语义不变）。
    fn trace_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::Thought {
                session_id,
                thought,
                step_number,
                run_id,
                ..
            } => {
                tracing::debug!(
                    "TauriEmitter::on_thought: session={} step={} run={} len={}",
                    session_id,
                    step_number,
                    run_id,
                    thought.len()
                );
            }
            AgentEvent::Action {
                session_id,
                tool_name,
                step_number,
                run_id,
                ..
            } => {
                tracing::debug!(
                    "TauriEmitter::on_action: session={} tool={} step={} run={}",
                    session_id,
                    tool_name,
                    step_number,
                    run_id
                );
            }
            AgentEvent::Observation {
                session_id,
                tool_name,
                step_number,
                run_id,
                silent,
                ..
            } => {
                tracing::debug!(
                    "TauriEmitter::on_observation: session={} tool={} step={} run={} silent={}",
                    session_id,
                    tool_name,
                    step_number,
                    run_id,
                    silent
                );
            }
            AgentEvent::SessionCreated(session) => {
                tracing::info!(
                    "TauriEmitter::on_session_created: session_id={} status={}",
                    session.id,
                    session.status.as_str()
                );
            }
            AgentEvent::SessionCompleted { session_id, title } => {
                tracing::info!(
                    "TauriEmitter::on_session_completed: session={} title={}",
                    session_id,
                    title
                );
            }
            AgentEvent::SessionUpdated { session_id, status } => {
                tracing::info!(
                    "TauriEmitter::on_session_updated: session={} status={}",
                    session_id,
                    status
                );
                if status == "paused" {
                    tracing::warn!(
                        "TauriEmitter emitting session:updated with paused status for session {}",
                        session_id
                    );
                }
            }
            AgentEvent::Notification {
                session_id,
                title,
                body,
            } => {
                tracing::info!(
                    "TauriEmitter::on_notification: session={} title={} body={}",
                    session_id,
                    title,
                    body
                );
            }
            AgentEvent::Compaction {
                session_id,
                tokens_before,
                tokens_after,
                ..
            } => {
                tracing::debug!(
                    "TauriEmitter::on_compaction: session={} tokens {}→{}",
                    session_id,
                    tokens_before,
                    tokens_after
                );
            }
            _ => {}
        }
    }

    /// `SessionCompleted` / `SessionError` 在 `session:updated` 上的副发。三条形状统一为
    /// `{session_id, status, title}` —— `error` 字段只保留在 `session:error` 主通道。
    fn emit_secondary(&self, event: &AgentEvent) {
        let payload = match event {
            AgentEvent::SessionCompleted { session_id, title } => serde_json::json!({
                "session_id": session_id,
                "status": "completed",
                "title": title,
            }),
            AgentEvent::SessionError { session_id, .. } => serde_json::json!({
                "session_id": session_id,
                "status": "error",
                "title": "",
            }),
            _ => return,
        };
        let _ = self.handle.emit("session:updated", payload);
    }

    /// 四个通知变体的 Windows 桌面通知（其余变体直接返回）。
    fn maybe_show_toast(&self, event: &AgentEvent) {
        match event {
            AgentEvent::SessionCreated(session) => {
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.session_created.windows)
                    .unwrap_or(false);
                if notify {
                    let display = if session.input.is_empty() {
                        &session.id
                    } else {
                        &session.input
                    };
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven")
                        .body(format!("New session: {}", display))
                        .show();
                }
            }
            AgentEvent::SessionCompleted {
                session_id: _,
                title,
            } => {
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.session_completed.windows)
                    .unwrap_or(true);
                if notify {
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven")
                        .body(format!("Session completed: {}", title))
                        .show();
                }
            }
            AgentEvent::SessionError {
                session_id: _,
                error,
            } => {
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.session_error.windows)
                    .unwrap_or(true);
                if notify {
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven - Error")
                        .body(format!("Session error: {}", error))
                        .show();
                }
            }
            AgentEvent::SessionUpdated { session_id, status } if status == "pending" => {
                // A transition to Pending after a Paused/Error state is a
                // resume (continue flow, ask answer, action-completion wake).
                // Surface it as a Windows toast when enabled so the user
                // knows the session is running again without checking the app.
                let notify = self
                    .handle
                    .state::<Arc<AppState>>()
                    .config_loader
                    .lock()
                    .map(|c| c.config().notification.session_resumed.windows)
                    .unwrap_or(false);
                if notify {
                    let display = self
                        .handle
                        .state::<Arc<AppState>>()
                        .db
                        .get_session(session_id)
                        .ok()
                        .flatten()
                        .and_then(|t| {
                            t.title
                                .filter(|s| !s.is_empty())
                                .or_else(|| (!t.input_text.is_empty()).then_some(t.input_text))
                        })
                        .unwrap_or_else(|| session_id.clone());
                    let _ = self
                        .handle
                        .notification()
                        .builder()
                        .title("Haven")
                        .body(format!("Session resumed: {}", display))
                        .show();
                }
            }
            AgentEvent::Notification {
                session_id: _,
                title,
                body,
            } => {
                // In-app toast: the frontend shows it via addNotification.
                // Windows desktop notification. The `notify` tool is an
                // explicit agent request, so both channels are used by default.
                let _ = self
                    .handle
                    .notification()
                    .builder()
                    .title(if title.is_empty() { "Haven" } else { title })
                    .body(body)
                    .show();
            }
            _ => {}
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
            crate::commands::emit_recording_error(
                &self.app_h,
                format!("录音启动失败，请检查麦克风/STT 配置: {e}"),
            );
            return;
        }
        let state = self.app_h.state::<Arc<AppState>>();
        let session_id = crate::commands::begin_recording_session(&state);
        crate::commands::emit_recording_started(&self.app_h, &session_id);
    }

    async fn on_recording_stop(&self) {
        // Same split as the `stop_recording` Tauri command: stop the audio
        // capture first and notify the UI, then run STT in the background.
        // Without this, VAD-triggered auto-stops would also keep the
        // "recording" overlay visible for the duration of the STT call.
        let result = self.pipeline.stop_capture().await;
        if let Ok(result) = result {
            crate::commands::emit_recording_stopped(
                &self.app_h,
                crate::commands::recording_reason_str(result.reason),
                Some(result.duration_ms),
            );
            if matches!(
                result.reason,
                haven_input::RecordingReason::Silence | haven_input::RecordingReason::MaxDuration
            ) {
                self.shell_arc.reset_toggle_on_auto_stop().await;
            }

            // Same finalize path as the `stop_recording` Tauri command: run
            // STT and emit `transcription:result` / `transcription:error`.
            // The frontend then submits the transcript through
            // `process_transcript` like a typed message, so voice input
            // continues the open conversation. Without this, hotkey / VAD-
            // triggered stops silently dropped the transcript — the text
            // never reached the chat UI nor the agent.
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
                state.spawn_background_init(move |event, payload| {
                    let _ = emit_handle.emit(event, payload);
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
                                let _ = emit_handle.emit("mcp:status_change", &ev);
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
                            "skills:status_change",
                            serde_json::json!({ "op": "auto_refresh" }),
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

            // Forward action lifecycle to the frontend (`action:created`
            // / `action:updated` / `action:output` / `action:finished`)
            // so the action panel stays live while sessions run in the
            // background. Emits are fire-and-forget like every other Tauri
            // event. Actions (`action_id` payloads) and scheduled_actions (`id` payloads)
            // share this sink.
            let action_sink_handle = handle.clone();
            state.tools.background_actions.set_event_sink(Arc::new(
                move |event: String, payload: serde_json::Value| {
                    let _ = action_sink_handle.emit(&event, payload);
                },
            ));

            // Same for scheduled_actions, so the pending list in the action panel
            // stays live and fired scheduled_actions can be acknowledged.
            let reminder_sink_handle = handle.clone();
            state.tools.scheduled_actions.set_event_sink(Arc::new(
                move |event: String, payload: serde_json::Value| {
                    let _ = reminder_sink_handle.emit(&event, payload);
                },
            ));

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
                                  risk_level: haven_common::types::RiskLevel| {
                                let _ = app_h.emit("confirm:requested", serde_json::json!({
                                    "step_id": step_id,
                                    "tool_name": tool_name,
                                    "risk_level": risk_level,
                                    "session_id": session_id,
                                }));
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
                                    "session:error",
                                    serde_json::json!({
                                        "session_id": session_id,
                                        "error": reason,
                                    }),
                                );
                                let _ = app_h.emit(
                                    "session:updated",
                                    serde_json::json!({
                                        "session_id": session_id,
                                        "status": "error",
                                        "title": "",
                                    }),
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
                        "hotkey:conflict",
                        serde_json::json!({
                            "binding": key_binding,
                            "error": e.to_string(),
                        }),
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
            commands::session::resolve_confirmation,
            commands::skills::get_tools,
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
            commands::model::list_models,
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
            commands::settings::check_shell_available,
            commands::history::export_history,
            commands::settings::enable_autostart,
            commands::settings::disable_autostart,
            commands::settings::is_autostart_enabled,
            commands::session::get_session_for_review,
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
fn to_tauri_shortcut(
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

#[cfg(test)]
mod tests {
    use super::*;
    use haven_agent::{SessionInfo, SessionStatus};
    use serde_json::json;

    /// Parse a binding through the unified `haven-input` hotkey parser and
    /// convert to the Tauri shortcut type (the production startup path).
    fn parse_shortcut(binding: &str) -> Option<tauri_plugin_global_shortcut::Shortcut> {
        haven_input::hotkey::KeyCombo::parse(binding).and_then(|combo| to_tauri_shortcut(&combo))
    }

    fn test_session_info() -> SessionInfo {
        SessionInfo {
            id: "ses-1".into(),
            input: "my input".into(),
            summary: "my summary".into(),
            title: Some("My Title".into()),
            status: SessionStatus::Running,
            steps: vec![],
            supplement_queue: vec![],
            steering_queue: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn channel_maps_every_variant_to_expected_channel() {
        let cases: Vec<(AgentEvent, &str)> = vec![
            (
                AgentEvent::Thought {
                    session_id: "t".into(),
                    thought: "x".into(),
                    step_number: 1,
                    run_id: 1,
                    message_id: "msg-1".into(),
                },
                "agent:thought",
            ),
            (
                AgentEvent::Action {
                    session_id: "t".into(),
                    tool_name: "read_file".into(),
                    input: json!({}),
                    step_number: 1,
                    run_id: 1,
                    tool_call_id: None,
                    step_id: "step-1".into(),
                },
                "agent:action",
            ),
            (
                AgentEvent::Observation {
                    session_id: "t".into(),
                    observation: "o".into(),
                    tool_name: "read_file".into(),
                    step_number: 1,
                    run_id: 1,
                    silent: false,
                    tool_call_id: None,
                    ask_options: vec![],
                    step_id: "step-1".into(),
                },
                "agent:observation",
            ),
            (
                AgentEvent::SessionCreated(test_session_info()),
                "session:created",
            ),
            (
                AgentEvent::SessionCompleted {
                    session_id: "t".into(),
                    title: "x".into(),
                },
                "session:completed",
            ),
            (
                AgentEvent::SessionUpdated {
                    session_id: "t".into(),
                    status: "paused".into(),
                },
                "session:updated",
            ),
            (
                AgentEvent::SessionError {
                    session_id: "t".into(),
                    error: "e".into(),
                },
                "session:error",
            ),
            (
                AgentEvent::Notification {
                    session_id: "t".into(),
                    title: "x".into(),
                    body: "y".into(),
                },
                "notification:show",
            ),
            (
                AgentEvent::TitleUpdated {
                    session_id: "t".into(),
                    title: "x".into(),
                },
                "session:title-updated",
            ),
            (
                AgentEvent::BalancedModelActivated {
                    session_id: "t".into(),
                    reason: "r".into(),
                },
                "agent:balanced_model",
            ),
            (
                AgentEvent::ThoughtChunk {
                    session_id: "t".into(),
                    delta: "d".into(),
                    step_number: 1,
                    run_id: 1,
                    message_id: "msg-1".into(),
                },
                "agent:thought_chunk",
            ),
            (
                AgentEvent::ReasoningChunk {
                    session_id: "t".into(),
                    delta: "d".into(),
                    step_number: 1,
                    run_id: 1,
                    message_id: "msg-2".into(),
                },
                "agent:reasoning_chunk",
            ),
            (
                AgentEvent::WebSearch {
                    session_id: "t".into(),
                    phase: "searching".into(),
                    step_number: 1,
                    run_id: 1,
                },
                "agent:web_search",
            ),
            (
                AgentEvent::StreamStalled {
                    session_id: "t".into(),
                },
                "agent:stream_stalled",
            ),
            (
                AgentEvent::Supplement {
                    session_id: "t".into(),
                    additional_context: "c".into(),
                    step_number: 1,
                    run_id: 1,
                },
                "agent:supplement",
            ),
            (
                AgentEvent::Compaction {
                    session_id: "t".into(),
                    summary: "s".into(),
                    tokens_before: 1,
                    tokens_after: 2,
                },
                "agent:compaction",
            ),
            (
                AgentEvent::Usage {
                    session_id: "t".into(),
                    prompt_tokens: 1,
                    completion_tokens: 2,
                    total_tokens: 3,
                    cost_usd: None,
                    model: None,
                    cumulative_prompt_tokens: 1,
                    cumulative_completion_tokens: 2,
                    cumulative_total_tokens: 3,
                    cumulative_cost_usd: None,
                    context_window: None,
                },
                "agent:usage",
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(
                TauriEmitter::channel(&event),
                expected,
                "channel mismatch for {:?}",
                event
            );
        }
    }

    #[test]
    fn variant_payload_strips_enum_tag() {
        let event = AgentEvent::Thought {
            session_id: "t1".into(),
            thought: "hello".into(),
            step_number: 2,
            run_id: 7,
            message_id: "msg-1".into(),
        };
        assert_eq!(
            TauriEmitter::variant_payload(&event),
            json!({
                "session_id": "t1",
                "thought": "hello",
                "step_number": 2,
                "run_id": 7,
                "message_id": "msg-1",
            })
        );
    }

    #[test]
    fn payload_adds_silent_to_action() {
        let event = AgentEvent::Action {
            session_id: "t".into(),
            tool_name: "read_file".into(),
            input: json!({"silent": true, "path": "/tmp/x"}),
            step_number: 1,
            run_id: 1,
            tool_call_id: Some("call-1".into()),
            step_id: "step-1".into(),
        };
        let payload = TauriEmitter::payload(&event, None);
        assert_eq!(payload["silent"], json!(true));
        assert_eq!(payload["tool_name"], json!("read_file"));
        assert_eq!(payload["tool_call_id"], json!("call-1"));
        assert_eq!(payload["step_id"], json!("step-1"));
    }

    #[test]
    fn payload_never_silences_ask() {
        let event = AgentEvent::Action {
            session_id: "t".into(),
            tool_name: "ask".into(),
            input: json!({"silent": true}),
            step_number: 1,
            run_id: 1,
            tool_call_id: None,
            step_id: "step-1".into(),
        };
        let payload = TauriEmitter::payload(&event, None);
        assert_eq!(payload["silent"], json!(false));
    }

    #[test]
    fn payload_injects_seq_for_chunk_variants() {
        let thought = AgentEvent::ThoughtChunk {
            session_id: "t".into(),
            delta: "d".into(),
            step_number: 1,
            run_id: 1,
            message_id: "msg-1".into(),
        };
        let payload = TauriEmitter::payload(&thought, Some(42));
        assert_eq!(payload["seq"], json!(42));
        assert_eq!(payload["delta"], json!("d"));
        assert_eq!(payload["message_id"], json!("msg-1"));

        let reasoning = AgentEvent::ReasoningChunk {
            session_id: "t".into(),
            delta: "d".into(),
            step_number: 1,
            run_id: 1,
            message_id: "msg-2".into(),
        };
        let payload = TauriEmitter::payload(&reasoning, Some(43));
        assert_eq!(payload["seq"], json!(43));
        assert_eq!(payload["message_id"], json!("msg-2"));
    }

    #[test]
    fn payload_projects_session_created_without_leaking_internal_fields() {
        let event = AgentEvent::SessionCreated(test_session_info());
        let payload = TauriEmitter::payload(&event, None);
        assert_eq!(
            payload,
            json!({
                "session_id": "ses-1",
                "status": "running",
                "title": "My Title",
            })
        );
        assert!(payload.get("id").is_none(), "must not leak SessionInfo.id");
        assert!(
            payload.get("input").is_none(),
            "must not leak SessionInfo.input"
        );
        assert!(
            payload.get("summary").is_none(),
            "must not leak SessionInfo.summary"
        );
    }

    #[test]
    fn payload_preserves_session_completed_and_updated_wire_shape() {
        let completed = AgentEvent::SessionCompleted {
            session_id: "t".into(),
            title: "X".into(),
        };
        let payload = TauriEmitter::payload(&completed, None);
        assert_eq!(
            payload,
            json!({"session_id": "t", "status": "completed", "title": "X"})
        );

        let updated = AgentEvent::SessionUpdated {
            session_id: "t".into(),
            status: "paused".into(),
        };
        let payload = TauriEmitter::payload(&updated, None);
        assert_eq!(
            payload,
            json!({"session_id": "t", "status": "paused", "title": ""})
        );
    }

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
        let dir = haven_common::config::ConfigLoader::data_dir();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.eq_ignore_ascii_case("haven"),
            "expected 'haven' got '{}'",
            name
        );
    }

    #[test]
    fn test_app_data_dir_on_windows_uses_appdata() {
        #[cfg(target_os = "windows")]
        {
            let dir = haven_common::config::ConfigLoader::data_dir();
            let s = dir.to_string_lossy();
            assert!(
                s.contains("AppData\\Roaming") || s.contains("APPDATA"),
                "expected AppData path, got: {}",
                s
            );
        }
    }
}
