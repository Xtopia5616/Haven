//! Agent event to Tauri IPC bridge.

use crate::events::*;
use crate::notification::DesktopNotifications;
use haven_agent::{AgentEvent, AgentEventEmitter};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;

pub(crate) struct TauriEmitter {
    pub(crate) handle: tauri::AppHandle,
    pub(crate) chunk_seq: AtomicU64,
    pub(crate) notifications: DesktopNotifications,
}

/// Adapt one task lifecycle message from `haven-tools` to the public Tauri
/// contract. Tool payloads are deliberately not emitted directly: they are
/// internal status JSON and can grow fields without becoming UI API.
pub(crate) fn emit_action_event(
    handle: &tauri::AppHandle,
    kind: ActionKind,
    event: &str,
    payload: &serde_json::Value,
) {
    let Some((channel, action)) = project_action_event(kind, event, payload) else {
        return;
    };

    match action {
        Ok(action) => {
            if let Err(error) = handle.emit(channel, action) {
                tracing::warn!(action_kind = ?kind, event, "failed to emit action lifecycle event: {error}");
            }
        }
        Err(error) => {
            tracing::warn!(action_kind = ?kind, event, "dropping malformed action lifecycle payload: {error}");
        }
    }
}

pub(crate) fn project_action_event(
    kind: ActionKind,
    event: &str,
    payload: &serde_json::Value,
) -> Option<(&'static str, Result<ActionEvent, String>)> {
    let projected = match (kind, event) {
        (ActionKind::Background, "action:created") => (
            ACTION_CREATED_EVENT,
            ActionEvent::background_from_value(payload),
        ),
        (ActionKind::Background, "action:updated") => (
            ACTION_UPDATED_EVENT,
            ActionEvent::background_from_value(payload),
        ),
        (ActionKind::Background, "action:output") => (
            ACTION_OUTPUT_EVENT,
            ActionEvent::background_from_value(payload),
        ),
        (ActionKind::Background, "action:finished") => (
            ACTION_FINISHED_EVENT,
            ActionEvent::background_from_value(payload),
        ),
        (ActionKind::Scheduled, "action:created") => (
            ACTION_CREATED_EVENT,
            ActionEvent::scheduled_from_value(payload, false),
        ),
        (ActionKind::Scheduled, "action:updated") => (
            ACTION_UPDATED_EVENT,
            ActionEvent::scheduled_from_value(payload, true),
        ),
        (ActionKind::Scheduled, "action:finished") => (
            ACTION_FINISHED_EVENT,
            ActionEvent::scheduled_from_value(payload, false),
        ),
        (_, unexpected) => {
            tracing::warn!(action_kind = ?kind, event = unexpected, "dropping unknown action lifecycle event");
            return None;
        }
    };
    Some(projected)
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
        // Cache titles from create/rename/complete before any path that may
        // resolve a display title (SessionUpdated fill, toasts, secondary).
        self.notifications.remember_session_status(&event);
        let mut payload = Self::payload(&event, chunk_seq);
        // SessionUpdated wire historically sent title:""; fill a safe display
        // title so in-app toast matches Windows (never raw input).
        if let AgentEvent::SessionUpdated { session_id, .. } = &event {
            payload["title"] =
                serde_json::json!(self.notifications.session_display_title(session_id));
        }
        let _ = self.handle.emit(channel, payload);
        self.emit_secondary(&event);
        self.notifications.maybe_show_toast(&event);
    }
}

impl TauriEmitter {
    /// 单一事实来源：AgentEvent 变体 → 前端订阅的 channel 名。
    pub(crate) fn channel(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::Thought { .. } => AGENT_THOUGHT_EVENT,
            AgentEvent::Action { .. } => AGENT_ACTION_EVENT,
            AgentEvent::Observation { .. } => AGENT_OBSERVATION_EVENT,
            AgentEvent::SessionCreated(_) => SESSION_CREATED_EVENT,
            AgentEvent::SessionCompleted { .. } => SESSION_COMPLETED_EVENT,
            AgentEvent::SessionUpdated { .. } => SESSION_UPDATED_EVENT,
            AgentEvent::SessionError { .. } => SESSION_ERROR_EVENT,
            AgentEvent::Notification { .. } => NOTIFICATION_SHOW_EVENT,
            AgentEvent::TitleUpdated { .. } => SESSION_TITLE_UPDATED_EVENT,
            AgentEvent::ThoughtChunk { .. } => AGENT_THOUGHT_CHUNK_EVENT,
            AgentEvent::ReasoningChunk { .. } => AGENT_REASONING_CHUNK_EVENT,
            AgentEvent::StreamReset { .. } => AGENT_STREAM_RESET_EVENT,
            AgentEvent::WebSearch { .. } => AGENT_WEB_SEARCH_EVENT,
            AgentEvent::StreamStalled { .. } => AGENT_STREAM_STALLED_EVENT,
            AgentEvent::Supplement { .. } => AGENT_SUPPLEMENT_EVENT,
            AgentEvent::Compaction { .. } => AGENT_COMPACTION_EVENT,
            AgentEvent::Usage { .. } => AGENT_USAGE_EVENT,
        }
    }

    /// Construct the wire payload from an explicit DTO for every event. Dynamic
    /// `Value` fields remain only where they are part of an intentional
    /// extension point: tool input, web-search result, and usage diagnostics.
    pub(crate) fn payload(event: &AgentEvent, chunk_seq: Option<u64>) -> serde_json::Value {
        fn serialize<T: serde::Serialize>(payload: T) -> serde_json::Value {
            serde_json::to_value(payload).expect("Tauri event DTO is serializable")
        }

        match event {
            AgentEvent::Thought {
                session_id,
                thought,
                step_number,
                run_id,
                message_id,
            } => serialize(AgentThoughtEvent {
                session_id: session_id.clone(),
                thought: thought.clone(),
                step_number: *step_number,
                run_id: *run_id,
                message_id: message_id.clone(),
            }),
            AgentEvent::Action {
                session_id,
                tool_name,
                input,
                step_number,
                run_id,
                tool_call_id,
                action_index,
                step_id,
                suppress_streamed_thought,
            } => serialize(AgentActionEvent {
                session_id: session_id.clone(),
                tool_name: tool_name.clone(),
                input: input.clone(),
                step_number: *step_number,
                run_id: *run_id,
                tool_call_id: tool_call_id.clone(),
                action_index: *action_index,
                step_id: step_id.clone(),
                suppress_streamed_thought: *suppress_streamed_thought,
                silent: haven_tools::is_silent_action(tool_name, input),
            }),
            AgentEvent::Observation {
                session_id,
                observation,
                tool_name,
                step_number,
                run_id,
                silent,
                tool_call_id,
                action_index,
                ask_options,
                step_id,
            } => serialize(AgentObservationEvent {
                session_id: session_id.clone(),
                observation: observation.clone(),
                tool_name: tool_name.clone(),
                step_number: *step_number,
                run_id: *run_id,
                silent: *silent,
                tool_call_id: tool_call_id.clone(),
                action_index: *action_index,
                ask_options: ask_options.clone(),
                step_id: step_id.clone(),
            }),
            AgentEvent::SessionCreated(session) => serialize(SessionLifecycleEvent {
                session_id: session.id.clone(),
                status: session.status.as_str().to_string(),
                title: session.title.clone(),
            }),
            AgentEvent::SessionCompleted { session_id, title } => {
                serialize(SessionLifecycleEvent {
                    session_id: session_id.clone(),
                    status: "completed".into(),
                    title: Some(title.clone()),
                })
            }
            AgentEvent::SessionUpdated { session_id, status } => serialize(SessionLifecycleEvent {
                session_id: session_id.clone(),
                status: status.clone(),
                title: Some(String::new()),
            }),
            AgentEvent::SessionError { session_id, error } => serialize(SessionErrorEvent {
                session_id: session_id.clone(),
                error: error.clone(),
            }),
            AgentEvent::ThoughtChunk {
                session_id,
                delta,
                step_number,
                run_id,
                message_id,
            } => serialize(AgentThoughtChunkEvent {
                session_id: session_id.clone(),
                delta: delta.clone(),
                step_number: *step_number,
                run_id: *run_id,
                message_id: message_id.clone(),
                seq: chunk_seq.unwrap_or(0),
            }),
            AgentEvent::ReasoningChunk {
                session_id,
                delta,
                step_number,
                run_id,
                message_id,
            } => serialize(AgentReasoningChunkEvent {
                session_id: session_id.clone(),
                delta: delta.clone(),
                step_number: *step_number,
                run_id: *run_id,
                message_id: message_id.clone(),
                seq: chunk_seq.unwrap_or(0),
            }),
            AgentEvent::StreamReset {
                session_id,
                step_number,
                run_id,
                thought_message_id,
                reasoning_message_id,
            } => serialize(AgentStreamResetEvent {
                session_id: session_id.clone(),
                step_number: *step_number,
                run_id: *run_id,
                thought_message_id: thought_message_id.clone(),
                reasoning_message_id: reasoning_message_id.clone(),
            }),
            AgentEvent::WebSearch {
                session_id,
                phase,
                step_number,
                run_id,
                call_id,
                action,
                result,
            } => serialize(AgentWebSearchEvent {
                session_id: session_id.clone(),
                phase: phase.clone(),
                step_number: *step_number,
                run_id: *run_id,
                call_id: call_id.clone(),
                action: action.clone(),
                result: result.clone(),
            }),
            AgentEvent::StreamStalled { session_id } => serialize(AgentStreamStalledEvent {
                session_id: session_id.clone(),
            }),
            AgentEvent::Supplement {
                session_id,
                additional_context,
                step_number,
                run_id,
                inject_source,
            } => serialize(AgentSupplementEvent {
                session_id: session_id.clone(),
                additional_context: additional_context.clone(),
                step_number: *step_number,
                run_id: *run_id,
                inject_source: *inject_source,
            }),
            AgentEvent::Compaction {
                session_id,
                summary,
                tokens_before,
                tokens_after,
                degraded,
                episode_id,
            } => serialize(AgentCompactionEvent {
                session_id: session_id.clone(),
                summary: summary.clone(),
                tokens_before: *tokens_before,
                tokens_after: *tokens_after,
                degraded: *degraded,
                episode_id: episode_id.clone(),
            }),
            AgentEvent::TitleUpdated { session_id, title } => serialize(SessionTitleUpdatedEvent {
                session_id: session_id.clone(),
                title: title.clone(),
            }),
            AgentEvent::Notification {
                session_id,
                title,
                body,
            } => serialize(AgentNotificationEvent {
                session_id: session_id.clone(),
                title: title.clone(),
                body: body.clone(),
            }),
            AgentEvent::Usage {
                session_id,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_tokens,
                cache_creation_tokens,
                cache_miss_tokens,
                context_tokens,
                cache_exclusive,
                cache_accounting,
                cost_usd,
                model,
                cumulative_prompt_tokens,
                cumulative_completion_tokens,
                cumulative_total_tokens,
                cumulative_cached_tokens,
                cumulative_cache_creation_tokens,
                cumulative_cache_miss_tokens,
                cache_diagnostics,
                cumulative_cost_usd,
                context_window,
                step_number,
                duration_ms,
                role,
                has_cost,
            } => serialize(AgentUsageEvent {
                session_id: session_id.clone(),
                prompt_tokens: *prompt_tokens,
                completion_tokens: *completion_tokens,
                total_tokens: *total_tokens,
                cached_tokens: *cached_tokens,
                cache_creation_tokens: *cache_creation_tokens,
                cache_miss_tokens: *cache_miss_tokens,
                context_tokens: *context_tokens,
                cache_exclusive: *cache_exclusive,
                cache_accounting: cache_accounting.clone(),
                cost_usd: *cost_usd,
                model: model.clone(),
                cumulative_prompt_tokens: *cumulative_prompt_tokens,
                cumulative_completion_tokens: *cumulative_completion_tokens,
                cumulative_total_tokens: *cumulative_total_tokens,
                cumulative_cached_tokens: *cumulative_cached_tokens,
                cumulative_cache_creation_tokens: *cumulative_cache_creation_tokens,
                cumulative_cache_miss_tokens: *cumulative_cache_miss_tokens,
                cache_diagnostics: cache_diagnostics.clone(),
                cumulative_cost_usd: *cumulative_cost_usd,
                context_window: *context_window,
                step_number: *step_number,
                duration_ms: *duration_ms,
                role: role.clone(),
                has_cost: *has_cost,
            }),
        }
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
                if status == "paused" || status == "paused_awaiting_answer" {
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
            AgentEvent::SessionCompleted { session_id, title } => {
                serde_json::to_value(SessionLifecycleEvent {
                    session_id: session_id.clone(),
                    status: "completed".into(),
                    title: Some(title.clone()),
                })
                .expect("session lifecycle event is serializable")
            }
            AgentEvent::SessionError { session_id, .. } => {
                serde_json::to_value(SessionLifecycleEvent {
                    session_id: session_id.clone(),
                    status: "error".into(),
                    title: Some(self.notifications.session_display_title(session_id)),
                })
                .expect("session lifecycle event is serializable")
            }
            _ => return,
        };
        let _ = self.handle.emit(SESSION_UPDATED_EVENT, payload);
    }
}
