//! Agent event to Tauri IPC bridge.

use crate::events::*;
use crate::notification::DesktopNotifications;
use haven_agent::{AgentEvent, AgentEventEmitter};
use haven_common::error::sanitize_error_text;
use haven_tools::ToolRunLifecycleEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;

pub(crate) struct TauriEmitter {
    pub(crate) handle: tauri::AppHandle,
    pub(crate) chunk_seq: AtomicU64,
    pub(crate) notifications: Arc<DesktopNotifications>,
}

pub(crate) struct ToolRunEventProjection {
    pub(crate) channel: &'static str,
    pub(crate) payload: ToolRunEvent,
}

struct AgentEventProjection {
    channel: &'static str,
    payload: serde_json::Value,
}

/// Adapt a typed Tools lifecycle event to the App-owned Tauri contract.
pub(crate) fn emit_tool_run_event(handle: &tauri::AppHandle, event: ToolRunLifecycleEvent) {
    let projection = project_tool_run_event(event);
    if let Err(_error) = handle.emit(projection.channel, projection.payload) {
        tracing::warn!(
            event = projection.channel,
            failure_kind = "tool_run_lifecycle_emit",
            "failed to emit ToolRun lifecycle event"
        );
    }
}

pub(crate) fn project_tool_run_event(event: ToolRunLifecycleEvent) -> ToolRunEventProjection {
    match event {
        ToolRunLifecycleEvent::Created(payload) => ToolRunEventProjection {
            channel: TOOL_RUN_CREATED_EVENT,
            payload: ToolRunEvent::from_lifecycle_payload(payload),
        },
        ToolRunLifecycleEvent::Updated(update) => ToolRunEventProjection {
            channel: TOOL_RUN_UPDATED_EVENT,
            payload: match update {
                haven_tools::ToolRunLifecycleUpdate::StateChanged(payload) => {
                    ToolRunEvent::from_lifecycle_payload(*payload)
                }
                haven_tools::ToolRunLifecycleUpdate::SessionAttached(payload) => {
                    ToolRunEvent::from_session_attached_payload(payload)
                }
            },
        },
        ToolRunLifecycleEvent::Finished(payload) => ToolRunEventProjection {
            channel: TOOL_RUN_FINISHED_EVENT,
            payload: ToolRunEvent::from_lifecycle_payload(payload),
        },
    }
}

#[async_trait::async_trait]
impl AgentEventEmitter for TauriEmitter {
    async fn emit(&self, event: AgentEvent) {
        Self::trace_event(&event);
        let chunk_seq = match &event {
            AgentEvent::ThoughtChunk { .. }
            | AgentEvent::ReasoningChunk { .. }
            | AgentEvent::ToolCallChunk { .. } => {
                Some(self.chunk_seq.fetch_add(1, Ordering::Relaxed))
            }
            _ => None,
        };
        // Cache titles before resolving labels for status and error events.
        self.notifications.remember_session_status(&event);
        let display_title = match &event {
            AgentEvent::SessionUpdated { session_id, .. }
            | AgentEvent::SessionError { session_id, .. } => {
                Some(self.notifications.session_display_title(session_id).await)
            }
            _ => None,
        };
        let projection = Self::project_with_chunk_seq(&event, chunk_seq, display_title);
        if let Err(error) = self.handle.emit(projection.channel, projection.payload) {
            tracing::warn!(
                channel = projection.channel,
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit agent event"
            );
        }
        self.notifications.maybe_show_toast(&event).await;
    }
}

impl TauriEmitter {
    /// Test convenience that reads the channel from the paired projection.
    #[cfg(test)]
    pub(crate) fn channel(event: &AgentEvent) -> &'static str {
        Self::project_with_chunk_seq(event, None, None).channel
    }

    /// Construct the wire payload from an explicit DTO for every event. Dynamic
    /// `Value` fields remain only where they are part of an intentional
    /// extension point: tool input, web-search result, and usage diagnostics.
    #[cfg(test)]
    pub(crate) fn payload(event: &AgentEvent, chunk_seq: Option<u64>) -> serde_json::Value {
        Self::project_with_chunk_seq(event, chunk_seq, None).payload
    }

    fn project_with_chunk_seq(
        event: &AgentEvent,
        chunk_seq: Option<u64>,
        display_title: Option<String>,
    ) -> AgentEventProjection {
        fn serialize<T: serde::Serialize>(payload: T) -> serde_json::Value {
            match serde_json::to_value(payload) {
                Ok(value) => value,
                Err(error) => {
                    tracing::error!(
                        error = %sanitize_error_text(&error.to_string()),
                        "failed to serialize Tauri event DTO"
                    );
                    serde_json::Value::Null
                }
            }
        }

        fn project(channel: &'static str, payload: serde_json::Value) -> AgentEventProjection {
            AgentEventProjection { channel, payload }
        }

        match event {
            AgentEvent::Thought {
                session_id,
                thought,
                step_number,
                run_id,
                message_id,
                event_sequence,
            } => project(
                AGENT_THOUGHT_EVENT,
                serialize(AgentThoughtEvent {
                    session_id: session_id.clone(),
                    thought: thought.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    message_id: message_id.clone(),
                    event_sequence: *event_sequence,
                }),
            ),
            AgentEvent::ToolCall {
                session_id,
                tool_name,
                input,
                step_number,
                run_id,
                tool_call_id,
                tool_index,
                step_id,
                suppress_streamed_thought,
                event_sequence: durable_event_sequence,
            } => project(
                AGENT_TOOL_CALL_EVENT,
                serialize(AgentToolCallEvent {
                    session_id: session_id.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    tool_call_id: tool_call_id.clone(),
                    tool_index: *tool_index,
                    step_id: step_id.clone(),
                    suppress_streamed_thought: *suppress_streamed_thought,
                    silent: haven_tools::is_silent_tool_call(tool_name, input),
                    event_sequence: *durable_event_sequence,
                }),
            ),
            AgentEvent::Observation {
                session_id,
                observation,
                tool_name,
                step_number,
                run_id,
                silent,
                tool_call_id,
                tool_index,
                ask_options,
                step_id,
                idempotency,
                operation_scope,
                renderer,
                result,
                event_sequence: durable_event_sequence,
            } => project(
                AGENT_OBSERVATION_EVENT,
                serialize(AgentObservationEvent {
                    session_id: session_id.clone(),
                    observation: observation.clone(),
                    tool_name: tool_name.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    silent: *silent,
                    tool_call_id: tool_call_id.clone(),
                    tool_index: *tool_index,
                    ask_options: ask_options.clone(),
                    step_id: step_id.clone(),
                    idempotency: *idempotency,
                    operation_scope: *operation_scope,
                    renderer: renderer.clone(),
                    result: result.clone(),
                    event_sequence: *durable_event_sequence,
                }),
            ),
            AgentEvent::SessionCreated(session) => project(
                SESSION_LIFECYCLE_EVENT,
                serialize(SessionLifecycleEvent::Created {
                    session_id: session.session_id.clone(),
                    status: session.status,
                    waiting_reason: session.waiting_reason,
                    title: session.title.clone(),
                }),
            ),
            AgentEvent::SessionCompleted {
                session_id,
                title,
                reason,
            } => project(
                SESSION_LIFECYCLE_EVENT,
                serialize(SessionLifecycleEvent::Completed {
                    session_id: session_id.clone(),
                    title: title.clone(),
                    reason: sanitize_error_text(reason),
                }),
            ),
            AgentEvent::SessionUpdated {
                session_id,
                status,
                waiting_reason,
                reason,
            } => {
                let title = display_title.clone().unwrap_or_else(|| session_id.clone());
                match status {
                    haven_common::lifecycle::SessionStatus::Completed => project(
                        SESSION_LIFECYCLE_EVENT,
                        serialize(SessionLifecycleEvent::Completed {
                            session_id: session_id.clone(),
                            title,
                            reason: reason
                                .as_deref()
                                .map(sanitize_error_text)
                                .unwrap_or_else(|| "会话已完成。".to_string()),
                        }),
                    ),
                    haven_common::lifecycle::SessionStatus::Error => project(
                        SESSION_LIFECYCLE_EVENT,
                        serialize(SessionLifecycleEvent::Error {
                            session_id: session_id.clone(),
                            title,
                            error: reason
                                .as_deref()
                                .map(sanitize_error_text)
                                .unwrap_or_else(|| "会话已停止，但未收到错误详情。".to_string()),
                        }),
                    ),
                    haven_common::lifecycle::SessionStatus::Pending => project(
                        SESSION_LIFECYCLE_EVENT,
                        serialize(SessionLifecycleEvent::Updated {
                            session_id: session_id.clone(),
                            status: SessionUpdateStatus::Pending,
                            waiting_reason: None,
                            title,
                            reason: reason.as_deref().map(sanitize_error_text),
                        }),
                    ),
                    haven_common::lifecycle::SessionStatus::Running => project(
                        SESSION_LIFECYCLE_EVENT,
                        serialize(SessionLifecycleEvent::Updated {
                            session_id: session_id.clone(),
                            status: SessionUpdateStatus::Running,
                            waiting_reason: None,
                            title,
                            reason: reason.as_deref().map(sanitize_error_text),
                        }),
                    ),
                    haven_common::lifecycle::SessionStatus::Paused => project(
                        SESSION_LIFECYCLE_EVENT,
                        serialize(SessionLifecycleEvent::Updated {
                            session_id: session_id.clone(),
                            status: SessionUpdateStatus::Paused,
                            waiting_reason: *waiting_reason,
                            title,
                            reason: reason.as_deref().map(sanitize_error_text),
                        }),
                    ),
                }
            }
            AgentEvent::SessionError { session_id, error } => project(
                SESSION_LIFECYCLE_EVENT,
                serialize(SessionLifecycleEvent::Error {
                    session_id: session_id.clone(),
                    title: display_title.clone().unwrap_or_else(|| session_id.clone()),
                    error: sanitize_error_text(error),
                }),
            ),
            AgentEvent::SessionDeleted { session_id } => project(
                SESSION_LIFECYCLE_EVENT,
                serialize(SessionLifecycleEvent::Deleted {
                    session_id: session_id.clone(),
                }),
            ),
            AgentEvent::ThoughtChunk {
                session_id,
                delta,
                step_number,
                run_id,
                message_id,
            } => project(
                AGENT_THOUGHT_CHUNK_EVENT,
                serialize(AgentThoughtChunkEvent {
                    session_id: session_id.clone(),
                    delta: delta.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    message_id: message_id.clone(),
                    seq: chunk_seq.unwrap_or(0),
                }),
            ),
            AgentEvent::ReasoningChunk {
                session_id,
                delta,
                step_number,
                run_id,
                message_id,
            } => project(
                AGENT_REASONING_CHUNK_EVENT,
                serialize(AgentReasoningChunkEvent {
                    session_id: session_id.clone(),
                    delta: delta.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    message_id: message_id.clone(),
                    seq: chunk_seq.unwrap_or(0),
                }),
            ),
            AgentEvent::ToolCallChunk {
                session_id,
                preview_id,
                tool_name,
                arguments,
                arguments_truncated,
                step_number,
                run_id,
                tool_index,
            } => project(
                AGENT_TOOL_CALL_CHUNK_EVENT,
                serialize(AgentToolCallChunkEvent {
                    session_id: session_id.clone(),
                    preview_id: preview_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments: arguments.clone(),
                    arguments_truncated: *arguments_truncated,
                    step_number: *step_number,
                    run_id: *run_id,
                    tool_index: *tool_index,
                    seq: chunk_seq.unwrap_or(0),
                }),
            ),
            AgentEvent::StreamReset {
                session_id,
                step_number,
                run_id,
                thought_message_id,
                reasoning_message_id,
            } => project(
                AGENT_STREAM_RESET_EVENT,
                serialize(AgentStreamResetEvent {
                    session_id: session_id.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    thought_message_id: thought_message_id.clone(),
                    reasoning_message_id: reasoning_message_id.clone(),
                }),
            ),
            AgentEvent::MediaPlan {
                session_id,
                step_number,
                run_id,
                request_kind,
                strategy,
                projections,
                notices,
                event_sequence: durable_event_sequence,
            } => project(
                AGENT_MEDIA_PLAN_EVENT,
                serialize(AgentMediaPlanEvent {
                    session_id: session_id.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    request_kind: *request_kind,
                    strategy: *strategy,
                    projections: projections.clone(),
                    notices: notices.clone(),
                    event_sequence: *durable_event_sequence,
                }),
            ),
            AgentEvent::WebSearch {
                session_id,
                phase,
                step_number,
                run_id,
                call_id,
                call_index,
                action,
                result,
            } => project(
                AGENT_WEB_SEARCH_EVENT,
                serialize(AgentWebSearchEvent {
                    session_id: session_id.clone(),
                    phase: *phase,
                    step_number: *step_number,
                    run_id: *run_id,
                    call_id: call_id.clone(),
                    call_index: *call_index,
                    action: action.clone(),
                    result: result.clone(),
                }),
            ),
            AgentEvent::StreamStalled { session_id } => project(
                AGENT_STREAM_STALLED_EVENT,
                serialize(AgentStreamStalledEvent {
                    session_id: session_id.clone(),
                }),
            ),
            AgentEvent::Supplement {
                session_id,
                additional_context,
                step_number,
                run_id,
                message_id,
                supplement_id,
                inject_source,
                event_sequence: durable_event_sequence,
            } => project(
                AGENT_SUPPLEMENT_EVENT,
                serialize(AgentSupplementEvent {
                    session_id: session_id.clone(),
                    additional_context: additional_context.clone(),
                    step_number: *step_number,
                    run_id: *run_id,
                    message_id: message_id.clone(),
                    supplement_id: supplement_id.clone(),
                    inject_source: *inject_source,
                    event_sequence: *durable_event_sequence,
                }),
            ),
            AgentEvent::Compaction {
                session_id,
                summary,
                tokens_before,
                tokens_after,
                degraded,
                episode_id,
                event_sequence: durable_event_sequence,
            } => project(
                AGENT_COMPACTION_EVENT,
                serialize(AgentCompactionEvent {
                    session_id: session_id.clone(),
                    summary: summary.clone(),
                    tokens_before: *tokens_before,
                    tokens_after: *tokens_after,
                    degraded: *degraded,
                    episode_id: episode_id.clone(),
                    event_sequence: *durable_event_sequence,
                }),
            ),
            AgentEvent::TitleUpdated { session_id, title } => project(
                SESSION_LIFECYCLE_EVENT,
                serialize(SessionLifecycleEvent::TitleUpdated {
                    session_id: session_id.clone(),
                    title: title.clone(),
                }),
            ),
            AgentEvent::Notification {
                session_id,
                title,
                body,
            } => project(
                NOTIFICATION_SHOW_EVENT,
                serialize(AgentNotificationEvent {
                    session_id: session_id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                    notification_kind: None,
                    tool_run_kind: None,
                    tool_run_id: None,
                    tool_run_status: None,
                }),
            ),
            AgentEvent::ToolRunCompletionNotification {
                tool_run_kind,
                tool_run_id,
                session_id,
                tool_run_status,
                title,
                body,
            } => project(
                NOTIFICATION_SHOW_EVENT,
                serialize(AgentNotificationEvent {
                    session_id: session_id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                    notification_kind: Some(AgentNotificationKind::ToolRunCompletion),
                    tool_run_kind: Some(match tool_run_kind {
                        haven_agent::ToolRunNotificationSource::Background => {
                            ToolRunKindDto::Background
                        }
                    }),
                    tool_run_id: Some(tool_run_id.clone()),
                    tool_run_status: tool_run_status.map(|status| match status {
                        haven_agent::ToolRunCompletionStatus::Completed => {
                            ToolRunCompletionStatusDto::Completed
                        }
                        haven_agent::ToolRunCompletionStatus::Failed => {
                            ToolRunCompletionStatusDto::Failed
                        }
                    }),
                }),
            ),
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
                request_kind,
                call_kind,
                has_cost,
            } => project(
                AGENT_USAGE_EVENT,
                serialize(AgentUsageEvent {
                    session_id: session_id.clone(),
                    prompt_tokens: *prompt_tokens,
                    completion_tokens: *completion_tokens,
                    total_tokens: *total_tokens,
                    cached_tokens: *cached_tokens,
                    cache_creation_tokens: *cache_creation_tokens,
                    cache_miss_tokens: *cache_miss_tokens,
                    context_tokens: *context_tokens,
                    cache_exclusive: *cache_exclusive,
                    cache_accounting: *cache_accounting,
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
                    request_kind: *request_kind,
                    call_kind: *call_kind,
                    has_cost: *has_cost,
                }),
            ),
        }
    }

    /// 保留原有按变体区分的 tracing 日志（语义不变）。
    fn trace_event(event: &AgentEvent) {
        match event {
            AgentEvent::Thought {
                session_id,
                thought,
                step_number,
                run_id,
                ..
            } => {
                tracing::debug!(
                    session_id = %session_id,
                    step_number = %step_number,
                    run_id = %run_id,
                    thought_bytes = thought.len(),
                    "TauriEmitter::on_thought"
                );
            }
            AgentEvent::ToolCall {
                session_id,
                step_number,
                run_id,
                ..
            } => {
                tracing::debug!(
                    session_id = %session_id,
                    step_number = %step_number,
                    run_id = %run_id,
                    "TauriEmitter::on_action"
                );
            }
            AgentEvent::Observation {
                session_id,
                step_number,
                run_id,
                silent,
                ..
            } => {
                tracing::debug!(
                    session_id = %session_id,
                    step_number = %step_number,
                    run_id = %run_id,
                    silent = *silent,
                    "TauriEmitter::on_observation"
                );
            }
            AgentEvent::ToolCallChunk {
                session_id,
                arguments,
                step_number,
                run_id,
                ..
            } => {
                tracing::trace!(
                    session_id = %session_id,
                    step_number = %step_number,
                    run_id = %run_id,
                    arguments_bytes = arguments.len(),
                    "TauriEmitter::on_tool_call_chunk"
                );
            }
            AgentEvent::SessionCreated(session) => {
                tracing::info!(
                    session_id = %session.session_id,
                    status = %session.status.as_str(),
                    "TauriEmitter::on_session_created"
                );
            }
            AgentEvent::SessionCompleted {
                session_id,
                title,
                reason,
            } => {
                tracing::info!(
                    session_id = %session_id,
                    title_chars = title.chars().count(),
                    reason_chars = reason.chars().count(),
                    "TauriEmitter::on_session_completed"
                );
            }
            AgentEvent::SessionDeleted { session_id } => {
                tracing::info!(
                    session_id = session_id.as_deref().unwrap_or("*"),
                    "TauriEmitter::on_session_deleted"
                );
            }
            AgentEvent::SessionUpdated {
                session_id, status, ..
            } => {
                tracing::info!(
                    session_id = %session_id,
                    status = status.as_str(),
                    "TauriEmitter::on_session_updated"
                );
                if *status == haven_common::lifecycle::SessionStatus::Paused {
                    tracing::warn!(
                        session_id = %session_id,
                        "TauriEmitter emitting session:lifecycle with paused status"
                    );
                }
            }
            AgentEvent::Notification {
                session_id,
                title,
                body,
            }
            | AgentEvent::ToolRunCompletionNotification {
                session_id,
                title,
                body,
                ..
            } => {
                tracing::info!(
                    session_id = ?session_id,
                    title_chars = title.chars().count(),
                    body_chars = body.chars().count(),
                    "TauriEmitter::on_notification"
                );
            }
            AgentEvent::Compaction {
                session_id,
                tokens_before,
                tokens_after,
                ..
            } => {
                tracing::debug!(
                    session_id = %session_id,
                    tokens_before,
                    tokens_after,
                    "TauriEmitter::on_compaction"
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_agent::{ToolRunCompletionStatus, ToolRunNotificationSource};

    #[test]
    fn agent_event_logs_redact_tool_names_and_label_text_lengths() {
        use std::io::{self, Write};
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct BufferWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for BufferWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for BufferWriter {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let private_tool_name = "private_dynamic_tool_name";
        let session_id = "ses-55555555555555555555555555555555";
        let events = [
            AgentEvent::Thought {
                session_id: session_id.into(),
                thought: "é".into(),
                step_number: 1,
                run_id: 2,
                message_id: "step-55555555555555555555555555555555".into(),
                event_sequence: None,
            },
            AgentEvent::ToolCall {
                session_id: session_id.into(),
                tool_name: private_tool_name.into(),
                input: serde_json::json!({"private_key": "private_argument_value"}),
                step_number: 1,
                run_id: 2,
                tool_call_id: None,
                tool_index: 0,
                step_id: "step-55555555555555555555555555555555".into(),
                suppress_streamed_thought: false,
                event_sequence: None,
            },
            AgentEvent::Observation {
                session_id: session_id.into(),
                observation: "private observation".into(),
                tool_name: private_tool_name.into(),
                step_number: 1,
                run_id: 2,
                silent: false,
                tool_call_id: None,
                tool_index: 0,
                ask_options: Vec::new(),
                step_id: "step-55555555555555555555555555555555".into(),
                idempotency: haven_common::tools::OperationIdempotency::Idempotent,
                operation_scope: haven_common::tools::ToolOperationScope::Session,
                renderer: "files".into(),
                result: haven_common::tools::ToolResultEnvelope::from_parts(
                    haven_common::tools::ToolExecutionOutcome::Succeeded,
                    None,
                    haven_common::tools::ToolRetryability::Unknown,
                    haven_common::tools::OperationIdempotency::Unknown,
                ),
                event_sequence: None,
            },
            AgentEvent::ToolCallChunk {
                session_id: session_id.into(),
                preview_id: "preview-55555555555555555555555555555555".into(),
                tool_name: private_tool_name.into(),
                arguments: "é".into(),
                arguments_truncated: false,
                step_number: 1,
                run_id: 2,
                tool_index: 0,
            },
            AgentEvent::SessionCompleted {
                session_id: session_id.into(),
                title: "标题".into(),
                reason: "结束".into(),
            },
            AgentEvent::Notification {
                session_id: Some(session_id.into()),
                title: "通知".into(),
                body: "正文".into(),
            },
        ];
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(BufferWriter(output.clone()))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            for event in &events {
                TauriEmitter::trace_event(event);
            }
        });
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();

        assert!(!logs.contains(private_tool_name), "captured logs: {logs}");
        assert!(!logs.contains("private_key"), "captured logs: {logs}");
        assert!(
            !logs.contains("private_argument_value"),
            "captured logs: {logs}"
        );
        assert!(logs.contains("thought_bytes=2"), "captured logs: {logs}");
        assert!(logs.contains("arguments_bytes=2"), "captured logs: {logs}");
        assert!(logs.contains("title_chars=2"), "captured logs: {logs}");
        assert!(logs.contains("reason_chars=2"), "captured logs: {logs}");
        assert!(logs.contains("body_chars=2"), "captured logs: {logs}");
    }

    #[test]
    fn tool_run_completion_notification_is_tagged_without_changing_generic_wire() {
        let generic = AgentEvent::Notification {
            session_id: Some("ses-b10266431b94e07821b946c973a97783".into()),
            title: "Notice".into(),
            body: "Generic notice".into(),
        };
        assert_eq!(TauriEmitter::channel(&generic), NOTIFICATION_SHOW_EVENT);
        assert_eq!(
            TauriEmitter::payload(&generic, None),
            serde_json::json!({
                "session_id": "ses-b10266431b94e07821b946c973a97783",
                "title": "Notice",
                "body": "Generic notice",
            })
        );

        let background_completion = AgentEvent::ToolRunCompletionNotification {
            tool_run_kind: ToolRunNotificationSource::Background,
            tool_run_id: "toolrun-20d3e20a073cc2b99ef530b375956cc7".into(),
            session_id: Some("ses-b6745a2e56b364791ed6b2ff6441cfe7".into()),
            tool_run_status: Some(ToolRunCompletionStatus::Failed),
            title: "后台任务失败".into(),
            body: "错误摘要".into(),
        };
        assert_eq!(
            TauriEmitter::payload(&background_completion, None),
            serde_json::json!({
                "session_id": "ses-b6745a2e56b364791ed6b2ff6441cfe7",
                "title": "后台任务失败",
                "body": "错误摘要",
                "notification_kind": "tool_run_completion",
                "tool_run_kind": "background",
                "tool_run_id": "toolrun-20d3e20a073cc2b99ef530b375956cc7",
                "tool_run_status": "failed",
            })
        );

        let completed_completion = AgentEvent::ToolRunCompletionNotification {
            tool_run_kind: ToolRunNotificationSource::Background,
            tool_run_id: "toolrun-144f268e0cbe6f7fbd820649652ff45f".into(),
            session_id: Some("ses-b6745a2e56b364791ed6b2ff6441cfe7".into()),
            tool_run_status: Some(ToolRunCompletionStatus::Completed),
            title: "后台任务已完成".into(),
            body: "完成".into(),
        };
        assert_eq!(
            TauriEmitter::payload(&completed_completion, None)["tool_run_status"],
            "completed"
        );
    }

    #[test]
    fn agent_observation_uses_the_result_envelope_as_its_only_outcome_source() {
        let event = AgentEvent::Observation {
            session_id: "ses-7ae290dbf0b4468c9d2c401be649850b".into(),
            observation: "operation result".into(),
            tool_name: "files.read".into(),
            step_number: 1,
            run_id: 1,
            silent: false,
            tool_call_id: Some("call-1".into()),
            tool_index: 0,
            ask_options: Vec::new(),
            step_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
            idempotency: haven_common::tools::OperationIdempotency::Idempotent,
            operation_scope: haven_common::tools::ToolOperationScope::Session,
            renderer: "files".into(),
            result: haven_common::tools::ToolResultEnvelope::from_parts(
                haven_common::tools::ToolExecutionOutcome::Failed,
                None,
                haven_common::tools::ToolRetryability::Unknown,
                haven_common::tools::OperationIdempotency::Unknown,
            ),
            event_sequence: Some(1),
        };

        let payload = TauriEmitter::payload(&event, None);
        assert_eq!(payload["result"]["outcome"], "failed");
        assert!(payload.get("outcome").is_none());
    }

    #[test]
    fn session_lifecycle_variants_share_one_channel_and_keep_terminal_details_typed() {
        let completed = AgentEvent::SessionCompleted {
            session_id: "ses-89564312af0e67cec57339c7df9c23b8".into(),
            title: "研究".into(),
            reason: "用户主动结束会话".into(),
        };
        assert_eq!(TauriEmitter::channel(&completed), SESSION_LIFECYCLE_EVENT);
        assert_eq!(
            TauriEmitter::payload(&completed, None),
            serde_json::json!({
                "type": "completed",
                "session_id": "ses-89564312af0e67cec57339c7df9c23b8",
                "title": "研究",
                "reason": "用户主动结束会话",
            })
        );

        let failed = AgentEvent::SessionError {
            session_id: "ses-68074e35ff3d611936f197b42ed3c8c2".into(),
            error: "网络请求超时".into(),
        };
        assert_eq!(TauriEmitter::channel(&failed), SESSION_LIFECYCLE_EVENT);
        assert_eq!(
            TauriEmitter::payload(&failed, None),
            serde_json::json!({
                "type": "error",
                "session_id": "ses-68074e35ff3d611936f197b42ed3c8c2",
                "title": "ses-68074e35ff3d611936f197b42ed3c8c2",
                "error": "网络请求超时",
            })
        );

        let single = AgentEvent::SessionDeleted {
            session_id: Some("ses-0c0bb384d521e3c3c4d2e3e7bbc6a979".into()),
        };
        assert_eq!(TauriEmitter::channel(&single), SESSION_LIFECYCLE_EVENT);
        assert_eq!(
            TauriEmitter::payload(&single, None),
            serde_json::json!({ "type": "deleted", "session_id": "ses-0c0bb384d521e3c3c4d2e3e7bbc6a979" })
        );

        let all = AgentEvent::SessionDeleted { session_id: None };
        assert_eq!(
            TauriEmitter::payload(&all, None),
            serde_json::json!({ "type": "deleted", "session_id": null })
        );
    }
}
