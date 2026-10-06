//! Agent event to Tauri IPC bridge.

use crate::events::*;
use crate::logging::sanitize_error_text;
use crate::notification::DesktopNotifications;
use haven_agent::{AgentEvent, AgentEventEmitter};
use haven_tools::ToolRunLifecycleEvent;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;

pub(crate) struct TauriEmitter {
    pub(crate) handle: tauri::AppHandle,
    pub(crate) chunk_seq: AtomicU64,
    pub(crate) notifications: Arc<DesktopNotifications>,
}

/// Adapt a typed Tools lifecycle event to the App-owned Tauri contract.
pub(crate) fn emit_tool_run_event(handle: &tauri::AppHandle, event: ToolRunLifecycleEvent) {
    let (channel, action) = project_tool_run_event(event);
    if let Err(error) = handle.emit(channel, action) {
        tracing::warn!(
            event = channel,
            "failed to emit ToolRun lifecycle event: {error}"
        );
    }
}

pub(crate) fn project_tool_run_event(event: ToolRunLifecycleEvent) -> (&'static str, ToolRunEvent) {
    match event {
        ToolRunLifecycleEvent::Created(payload) => (
            TOOL_RUN_CREATED_EVENT,
            ToolRunEvent::from_lifecycle_payload(payload),
        ),
        ToolRunLifecycleEvent::Updated(update) => (
            TOOL_RUN_UPDATED_EVENT,
            match update {
                haven_tools::ToolRunLifecycleUpdate::StateChanged(payload) => {
                    ToolRunEvent::from_lifecycle_payload(*payload)
                }
                haven_tools::ToolRunLifecycleUpdate::SessionAttached(payload) => {
                    ToolRunEvent::from_session_attached_payload(payload)
                }
            },
        ),
        ToolRunLifecycleEvent::Output(payload) => (
            TOOL_RUN_OUTPUT_EVENT,
            ToolRunEvent::from_output_payload(payload),
        ),
        ToolRunLifecycleEvent::Finished(payload) => (
            TOOL_RUN_FINISHED_EVENT,
            ToolRunEvent::from_lifecycle_payload(payload),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_agent::{ToolRunCompletionStatus, ToolRunNotificationSource};

    #[test]
    fn tool_run_output_event_projects_only_the_bounded_preview_fields() {
        let event = ToolRunLifecycleEvent::Output(haven_tools::ToolRunOutputPayload {
            tool_run_id: "toolrun-output-preview".into(),
            source_step_id: Some("step-output-preview".into()),
            output: "bounded tail snapshot".into(),
        });
        let (channel, projected) = project_tool_run_event(event);
        assert_eq!(channel, TOOL_RUN_OUTPUT_EVENT);
        let wire = serde_json::to_value(projected).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "id": "toolrun-output-preview",
                "kind": "background",
                "status": "running",
                "source_step_id": "step-output-preview",
                "output": "bounded tail snapshot",
            })
        );
        let serialized = wire.to_string();
        assert!(!serialized.contains("tool_args"));
        assert!(!serialized.contains("log_path"));
    }

    #[test]
    fn tool_run_completion_notification_is_tagged_without_changing_generic_wire() {
        let generic = AgentEvent::Notification {
            session_id: Some("ses-generic".into()),
            title: "Notice".into(),
            body: "Generic notice".into(),
        };
        assert_eq!(TauriEmitter::channel(&generic), NOTIFICATION_SHOW_EVENT);
        assert_eq!(
            TauriEmitter::payload(&generic, None),
            serde_json::json!({
                "session_id": "ses-generic",
                "title": "Notice",
                "body": "Generic notice",
            })
        );

        let tool_run_completion = AgentEvent::ToolRunCompletionNotification {
            tool_run_kind: ToolRunNotificationSource::Scheduled,
            tool_run_id: "toolrun-scheduled".into(),
            session_id: None,
            tool_run_status: None,
            title: "任务完成".into(),
            body: "结果".into(),
        };
        assert_eq!(
            TauriEmitter::channel(&tool_run_completion),
            NOTIFICATION_SHOW_EVENT
        );
        assert_eq!(
            TauriEmitter::payload(&tool_run_completion, None),
            serde_json::json!({
                "title": "任务完成",
                "body": "结果",
                "notification_kind": "tool_run_completion",
                "tool_run_kind": "scheduled",
                "tool_run_id": "toolrun-scheduled",
            })
        );

        let background_completion = AgentEvent::ToolRunCompletionNotification {
            tool_run_kind: ToolRunNotificationSource::Background,
            tool_run_id: "toolrun-background".into(),
            session_id: Some("ses-owner".into()),
            tool_run_status: Some(ToolRunCompletionStatus::Failed),
            title: "后台任务失败".into(),
            body: "错误摘要".into(),
        };
        assert_eq!(
            TauriEmitter::payload(&background_completion, None),
            serde_json::json!({
                "session_id": "ses-owner",
                "title": "后台任务失败",
                "body": "错误摘要",
                "notification_kind": "tool_run_completion",
                "tool_run_kind": "background",
                "tool_run_id": "toolrun-background",
                "tool_run_status": "failed",
            })
        );
    }

    #[test]
    fn session_lifecycle_variants_share_one_channel_and_keep_terminal_details_typed() {
        let completed = AgentEvent::SessionCompleted {
            session_id: "ses-completed".into(),
            title: "研究".into(),
            reason: "用户主动结束会话".into(),
        };
        assert_eq!(TauriEmitter::channel(&completed), SESSION_LIFECYCLE_EVENT);
        assert_eq!(
            TauriEmitter::payload(&completed, None),
            serde_json::json!({
                "type": "completed",
                "session_id": "ses-completed",
                "title": "研究",
                "reason": "用户主动结束会话",
            })
        );

        let failed = AgentEvent::SessionError {
            session_id: "ses-error".into(),
            error: "网络请求超时".into(),
        };
        assert_eq!(TauriEmitter::channel(&failed), SESSION_LIFECYCLE_EVENT);
        assert_eq!(
            TauriEmitter::payload(&failed, None),
            serde_json::json!({
                "type": "error",
                "session_id": "ses-error",
                "title": "ses-error",
                "error": "网络请求超时",
            })
        );

        let single = AgentEvent::SessionDeleted {
            session_id: Some("ses-deleted".into()),
        };
        assert_eq!(TauriEmitter::channel(&single), SESSION_LIFECYCLE_EVENT);
        assert_eq!(
            TauriEmitter::payload(&single, None),
            serde_json::json!({ "type": "deleted", "session_id": "ses-deleted" })
        );

        let all = AgentEvent::SessionDeleted { session_id: None };
        assert_eq!(
            TauriEmitter::payload(&all, None),
            serde_json::json!({ "type": "deleted", "session_id": null })
        );
    }
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
        // Cache titles before resolving labels for status and error events.
        self.notifications.remember_session_status(&event);
        let display_title = match &event {
            AgentEvent::SessionUpdated { session_id, .. }
            | AgentEvent::SessionError { session_id, .. } => {
                Some(self.notifications.session_display_title(session_id))
            }
            _ => None,
        };
        let payload = Self::payload_with_chunk_seq(&event, chunk_seq, display_title);
        if let Err(error) = self.handle.emit(channel, payload) {
            tracing::warn!(
                channel,
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit agent event"
            );
        }
        self.notifications.maybe_show_toast(&event);
    }
}

impl TauriEmitter {
    /// 单一事实来源：AgentEvent 变体 → 前端订阅的 channel 名。
    pub(crate) fn channel(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::Thought { .. } => AGENT_THOUGHT_EVENT,
            AgentEvent::ToolCall { .. } => AGENT_TOOL_CALL_EVENT,
            AgentEvent::Observation { .. } => AGENT_OBSERVATION_EVENT,
            AgentEvent::SessionCreated(_)
            | AgentEvent::SessionCompleted { .. }
            | AgentEvent::SessionUpdated { .. }
            | AgentEvent::SessionError { .. }
            | AgentEvent::SessionDeleted { .. }
            | AgentEvent::TitleUpdated { .. } => SESSION_LIFECYCLE_EVENT,
            AgentEvent::Notification { .. } => NOTIFICATION_SHOW_EVENT,
            AgentEvent::ToolRunCompletionNotification { .. } => NOTIFICATION_SHOW_EVENT,
            AgentEvent::ThoughtChunk { .. } => AGENT_THOUGHT_CHUNK_EVENT,
            AgentEvent::ReasoningChunk { .. } => AGENT_REASONING_CHUNK_EVENT,
            AgentEvent::StreamReset { .. } => AGENT_STREAM_RESET_EVENT,
            AgentEvent::MediaPlan { .. } => AGENT_MEDIA_PLAN_EVENT,
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
    #[cfg(test)]
    pub(crate) fn payload(event: &AgentEvent, chunk_seq: Option<u64>) -> serde_json::Value {
        Self::payload_with_chunk_seq(event, chunk_seq, None)
    }

    fn payload_with_chunk_seq(
        event: &AgentEvent,
        chunk_seq: Option<u64>,
        display_title: Option<String>,
    ) -> serde_json::Value {
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

        match event {
            AgentEvent::Thought {
                session_id,
                thought,
                step_number,
                run_id,
                message_id,
                event_seq,
            } => serialize(AgentThoughtEvent {
                session_id: session_id.clone(),
                thought: thought.clone(),
                step_number: *step_number,
                run_id: *run_id,
                message_id: message_id.clone(),
                event_seq: *event_seq,
            }),
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
                event_seq: durable_event_seq,
            } => serialize(AgentToolCallEvent {
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
                event_seq: *durable_event_seq,
            }),
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
                outcome,
                idempotency,
                operation_scope,
                renderer,
                result,
                event_seq: durable_event_seq,
            } => serialize(AgentObservationEvent {
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
                outcome: outcome.clone(),
                idempotency: idempotency.clone(),
                operation_scope: operation_scope.clone(),
                renderer: renderer.clone(),
                result: result.clone(),
                event_seq: *durable_event_seq,
            }),
            AgentEvent::SessionCreated(session) => serialize(SessionLifecycleEvent::Created {
                session_id: session.id.clone(),
                status: session.status,
                waiting_reason: session.waiting_reason,
                title: session.title.clone(),
            }),
            AgentEvent::SessionCompleted {
                session_id,
                title,
                reason,
            } => serialize(SessionLifecycleEvent::Completed {
                session_id: session_id.clone(),
                title: title.clone(),
                reason: sanitize_error_text(reason),
            }),
            AgentEvent::SessionUpdated {
                session_id,
                status,
                waiting_reason,
                reason,
            } => {
                let title = display_title.clone().unwrap_or_else(|| session_id.clone());
                match status {
                    haven_common::SessionStatus::Completed => {
                        serialize(SessionLifecycleEvent::Completed {
                            session_id: session_id.clone(),
                            title,
                            reason: reason
                                .as_deref()
                                .map(sanitize_error_text)
                                .unwrap_or_else(|| "会话已完成。".to_string()),
                        })
                    }
                    haven_common::SessionStatus::Error => serialize(SessionLifecycleEvent::Error {
                        session_id: session_id.clone(),
                        title,
                        error: reason
                            .as_deref()
                            .map(sanitize_error_text)
                            .unwrap_or_else(|| "会话已停止，但未收到错误详情。".to_string()),
                    }),
                    haven_common::SessionStatus::Pending => {
                        serialize(SessionLifecycleEvent::Updated {
                            session_id: session_id.clone(),
                            status: SessionUpdateStatus::Pending,
                            waiting_reason: None,
                            title,
                            reason: reason.as_deref().map(sanitize_error_text),
                        })
                    }
                    haven_common::SessionStatus::Running => {
                        serialize(SessionLifecycleEvent::Updated {
                            session_id: session_id.clone(),
                            status: SessionUpdateStatus::Running,
                            waiting_reason: None,
                            title,
                            reason: reason.as_deref().map(sanitize_error_text),
                        })
                    }
                    haven_common::SessionStatus::Paused => {
                        serialize(SessionLifecycleEvent::Updated {
                            session_id: session_id.clone(),
                            status: SessionUpdateStatus::Paused,
                            waiting_reason: *waiting_reason,
                            title,
                            reason: reason.as_deref().map(sanitize_error_text),
                        })
                    }
                }
            }
            AgentEvent::SessionError { session_id, error } => {
                serialize(SessionLifecycleEvent::Error {
                    session_id: session_id.clone(),
                    title: display_title.clone().unwrap_or_else(|| session_id.clone()),
                    error: sanitize_error_text(error),
                })
            }
            AgentEvent::SessionDeleted { session_id } => {
                serialize(SessionLifecycleEvent::Deleted {
                    session_id: session_id.clone(),
                })
            }
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
            AgentEvent::MediaPlan {
                session_id,
                step_number,
                run_id,
                role,
                strategy,
                projections,
                notices,
                event_seq: durable_event_seq,
            } => serialize(AgentMediaPlanEvent {
                session_id: session_id.clone(),
                step_number: *step_number,
                run_id: *run_id,
                role: *role,
                strategy: *strategy,
                projections: projections.clone(),
                notices: notices.clone(),
                event_seq: *durable_event_seq,
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
                message_id,
                supplement_id,
                inject_source,
                event_seq: durable_event_seq,
            } => serialize(AgentSupplementEvent {
                session_id: session_id.clone(),
                additional_context: additional_context.clone(),
                step_number: *step_number,
                run_id: *run_id,
                message_id: message_id.clone(),
                supplement_id: supplement_id.clone(),
                inject_source: *inject_source,
                event_seq: *durable_event_seq,
            }),
            AgentEvent::Compaction {
                session_id,
                summary,
                tokens_before,
                tokens_after,
                degraded,
                episode_id,
                event_seq: durable_event_seq,
            } => serialize(AgentCompactionEvent {
                session_id: session_id.clone(),
                summary: summary.clone(),
                tokens_before: *tokens_before,
                tokens_after: *tokens_after,
                degraded: *degraded,
                episode_id: episode_id.clone(),
                event_seq: *durable_event_seq,
            }),
            AgentEvent::TitleUpdated { session_id, title } => {
                serialize(SessionLifecycleEvent::TitleUpdated {
                    session_id: session_id.clone(),
                    title: title.clone(),
                })
            }
            AgentEvent::Notification {
                session_id,
                title,
                body,
            } => serialize(AgentNotificationEvent {
                session_id: session_id.clone(),
                title: title.clone(),
                body: body.clone(),
                notification_kind: None,
                tool_run_kind: None,
                tool_run_id: None,
                tool_run_status: None,
            }),
            AgentEvent::ToolRunCompletionNotification {
                tool_run_kind,
                tool_run_id,
                session_id,
                tool_run_status,
                title,
                body,
            } => serialize(AgentNotificationEvent {
                session_id: session_id.clone(),
                title: title.clone(),
                body: body.clone(),
                notification_kind: Some(AgentNotificationKind::ToolRunCompletion),
                tool_run_kind: Some(tool_run_kind.as_str().to_string()),
                tool_run_id: Some(tool_run_id.clone()),
                tool_run_status: tool_run_status.map(|status| status.as_str().to_string()),
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
                call_kind,
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
                role: *role,
                call_kind: call_kind.clone(),
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
                    session_id = %session_id,
                    step_number = %step_number,
                    run_id = %run_id,
                    thought_len = thought.len(),
                    "TauriEmitter::on_thought"
                );
            }
            AgentEvent::ToolCall {
                session_id,
                tool_name,
                step_number,
                run_id,
                ..
            } => {
                tracing::debug!(
                    session_id = %session_id,
                    tool_name = %tool_name,
                    step_number = %step_number,
                    run_id = %run_id,
                    "TauriEmitter::on_action"
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
                    session_id = %session_id,
                    tool_name = %tool_name,
                    step_number = %step_number,
                    run_id = %run_id,
                    silent = *silent,
                    "TauriEmitter::on_observation"
                );
            }
            AgentEvent::SessionCreated(session) => {
                tracing::info!(
                    session_id = %session.id,
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
                    title_len = title.chars().count(),
                    reason_len = reason.chars().count(),
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
                if *status == haven_common::SessionStatus::Paused {
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
                    title_len = title.chars().count(),
                    body_len = body.chars().count(),
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
