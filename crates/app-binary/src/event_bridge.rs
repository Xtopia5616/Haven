//! Agent event to Tauri IPC bridge.

use crate::events::*;
use crate::logging::sanitize_error_text;
use crate::notification::DesktopNotifications;
use haven_agent::{AgentEvent, AgentEventEmitter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;

pub(crate) struct TauriEmitter {
    pub(crate) handle: tauri::AppHandle,
    pub(crate) chunk_seq: AtomicU64,
    pub(crate) notifications: Arc<DesktopNotifications>,
}

/// Adapt one task lifecycle message from `haven-tools` to the public Tauri
/// contract. Tool payloads are deliberately not emitted directly: they are
/// internal status JSON and can grow fields without becoming UI API.
pub(crate) fn emit_tool_run_event(
    handle: &tauri::AppHandle,
    kind: ToolRunKind,
    event: &str,
    payload: &serde_json::Value,
) {
    let Some((channel, action)) = project_tool_run_event(kind, event, payload) else {
        return;
    };

    match action {
        Ok(action) => {
            if let Err(error) = handle.emit(channel, action) {
                tracing::warn!(tool_run_kind = ?kind, event, "failed to emit ToolRun lifecycle event: {error}");
            }
        }
        Err(error) => {
            tracing::warn!(tool_run_kind = ?kind, event, "dropping malformed ToolRun lifecycle payload: {error}");
        }
    }
}

pub(crate) fn project_tool_run_event(
    kind: ToolRunKind,
    event: &str,
    payload: &serde_json::Value,
) -> Option<(&'static str, Result<ToolRunEvent, String>)> {
    let projected = match (kind, event) {
        (ToolRunKind::Background, "tool_run:created") => (
            TOOL_RUN_CREATED_EVENT,
            ToolRunEvent::background_from_value(payload),
        ),
        (ToolRunKind::Background, "tool_run:updated") => (
            TOOL_RUN_UPDATED_EVENT,
            ToolRunEvent::background_from_value(payload),
        ),
        (ToolRunKind::Background, "tool_run:output") => (
            TOOL_RUN_OUTPUT_EVENT,
            ToolRunEvent::background_output_from_value(payload),
        ),
        (ToolRunKind::Background, "tool_run:finished") => (
            TOOL_RUN_FINISHED_EVENT,
            ToolRunEvent::background_from_value(payload),
        ),
        (ToolRunKind::Scheduled, "tool_run:created") => (
            TOOL_RUN_CREATED_EVENT,
            ToolRunEvent::scheduled_from_value(payload, false),
        ),
        (ToolRunKind::Scheduled, "tool_run:updated") => (
            TOOL_RUN_UPDATED_EVENT,
            ToolRunEvent::scheduled_from_value(payload, false),
        ),
        (ToolRunKind::Scheduled, "tool_run:finished") => (
            TOOL_RUN_FINISHED_EVENT,
            ToolRunEvent::scheduled_from_value(payload, false),
        ),
        (_, unexpected) => {
            tracing::warn!(tool_run_kind = ?kind, event = unexpected, "dropping unknown ToolRun lifecycle event");
            return None;
        }
    };
    Some(projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_agent::{ToolRunCompletionStatus, ToolRunNotificationSource};

    #[test]
    fn tool_run_output_event_projects_only_the_bounded_preview_fields() {
        let payload = serde_json::json!({
            "tool_run_id": "toolrun-output-preview",
            "status": "running",
            "source_step_id": "step-output-preview",
            "output": "bounded tail snapshot",
            "command": "echo token=private-command-value",
            "tool_args": { "token": "private-argument-value" },
            "log_path": "C:/private/tool-run.log",
            "stderr": "unbounded stderr value",
        });

        let (channel, projected) =
            project_tool_run_event(ToolRunKind::Background, "tool_run:output", &payload)
                .expect("background output event is registered");
        assert_eq!(channel, TOOL_RUN_OUTPUT_EVENT);
        let wire = serde_json::to_value(projected.unwrap()).unwrap();
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
        for private_value in [
            "private-command-value",
            "private-argument-value",
            "private/tool-run.log",
            "unbounded stderr value",
        ] {
            assert!(!serialized.contains(private_value));
        }
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
    fn terminal_primary_and_secondary_payloads_share_the_occurrence_id() {
        let cases = [
            (
                AgentEvent::SessionCompleted {
                    session_id: "ses-completed".into(),
                    title: "研究".into(),
                    reason: "用户主动结束会话".into(),
                },
                None,
            ),
            (
                AgentEvent::SessionError {
                    session_id: "ses-error".into(),
                    error: "网络请求超时".into(),
                },
                Some("研究".to_owned()),
            ),
        ];

        for (event, secondary_title) in cases {
            let occurrence_id = "occ-test";
            let primary = TauriEmitter::payload_with_chunk_seq(&event, None, Some(occurrence_id));
            let secondary =
                TauriEmitter::secondary_payload(&event, Some(occurrence_id), secondary_title)
                    .expect("terminal Agent events have a secondary lifecycle projection");

            assert_eq!(primary["occurrence_id"], occurrence_id);
            assert_eq!(secondary["occurrence_id"], occurrence_id);
        }
    }

    #[test]
    fn session_deletion_uses_the_existing_wire_contract() {
        let single = AgentEvent::SessionDeleted {
            session_id: Some("ses-deleted".into()),
        };
        assert_eq!(TauriEmitter::channel(&single), SESSION_DELETED_EVENT);
        assert_eq!(
            TauriEmitter::payload(&single, None),
            serde_json::json!({ "session_id": "ses-deleted" })
        );

        let all = AgentEvent::SessionDeleted { session_id: None };
        assert_eq!(
            TauriEmitter::payload(&all, None),
            serde_json::json!({ "session_id": null })
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
        let occurrence_id = match &event {
            AgentEvent::SessionCompleted { .. } | AgentEvent::SessionError { .. } => {
                Some(haven_common::types::new_id("occ"))
            }
            _ => None,
        };
        // Cache titles from create/rename/complete before any path that may
        // resolve a display title (SessionUpdated fill, toasts, secondary).
        self.notifications.remember_session_status(&event);
        let mut payload = Self::payload_with_chunk_seq(&event, chunk_seq, occurrence_id.as_deref());
        // Add a safe display title so in-app toast matches Windows (never raw
        // input).
        if let AgentEvent::SessionUpdated { session_id, .. } = &event {
            payload["title"] =
                serde_json::json!(self.notifications.session_display_title(session_id));
        }
        if let Err(error) = self.handle.emit(channel, payload) {
            tracing::warn!(
                channel,
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit agent event"
            );
        }
        self.emit_secondary_with_occurrence_id(&event, occurrence_id.as_deref());
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
            AgentEvent::SessionCreated(_) => SESSION_CREATED_EVENT,
            AgentEvent::SessionCompleted { .. } => SESSION_COMPLETED_EVENT,
            AgentEvent::SessionUpdated { .. } => SESSION_UPDATED_EVENT,
            AgentEvent::SessionError { .. } => SESSION_ERROR_EVENT,
            AgentEvent::SessionDeleted { .. } => SESSION_DELETED_EVENT,
            AgentEvent::Notification { .. } => NOTIFICATION_SHOW_EVENT,
            AgentEvent::ToolRunCompletionNotification { .. } => NOTIFICATION_SHOW_EVENT,
            AgentEvent::TitleUpdated { .. } => SESSION_TITLE_UPDATED_EVENT,
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
        occurrence_id: Option<&str>,
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
            AgentEvent::SessionCreated(session) => serialize(SessionLifecycleEvent {
                session_id: session.id.clone(),
                status: session.status,
                occurrence_id: None,
                waiting_reason: session.waiting_reason,
                title: session.title.clone(),
                reason: None,
            }),
            AgentEvent::SessionCompleted {
                session_id,
                title,
                reason,
            } => serialize(SessionLifecycleEvent {
                session_id: session_id.clone(),
                status: haven_common::SessionStatus::Completed,
                occurrence_id: occurrence_id.map(str::to_owned),
                waiting_reason: None,
                title: Some(title.clone()),
                reason: Some(sanitize_error_text(reason)),
            }),
            AgentEvent::SessionUpdated {
                session_id,
                status,
                waiting_reason,
                reason,
            } => serialize(SessionLifecycleEvent {
                session_id: session_id.clone(),
                status: *status,
                occurrence_id: None,
                waiting_reason: *waiting_reason,
                title: Some(String::new()),
                reason: reason.as_deref().map(sanitize_error_text),
            }),
            AgentEvent::SessionError { session_id, error } => serialize(SessionErrorEvent {
                session_id: session_id.clone(),
                error: sanitize_error_text(error),
                occurrence_id: occurrence_id.map(str::to_owned),
            }),
            AgentEvent::SessionDeleted { session_id } => serialize(SessionDeletedEvent {
                session_id: session_id.clone(),
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
                        "TauriEmitter emitting session:updated with paused status"
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

    /// `SessionCompleted` / `SessionError` 在 `session:updated` 上的副发。生命周期形状统一为
    /// `{session_id, status, title, reason, occurrence_id?}` —— `error` 只保留在主通道。
    fn emit_secondary_with_occurrence_id(&self, event: &AgentEvent, occurrence_id: Option<&str>) {
        let title = match event {
            AgentEvent::SessionError { session_id, .. } => {
                Some(self.notifications.session_display_title(session_id))
            }
            _ => None,
        };
        let Some(payload) = Self::secondary_payload(event, occurrence_id, title) else {
            return;
        };
        if let Err(error) = self.handle.emit(SESSION_UPDATED_EVENT, payload) {
            tracing::warn!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit secondary session lifecycle event"
            );
        }
    }

    fn secondary_payload(
        event: &AgentEvent,
        occurrence_id: Option<&str>,
        title: Option<String>,
    ) -> Option<serde_json::Value> {
        match event {
            AgentEvent::SessionCompleted {
                session_id,
                title,
                reason,
            } => Some(
                serde_json::to_value(SessionLifecycleEvent {
                    session_id: session_id.clone(),
                    status: haven_common::SessionStatus::Completed,
                    occurrence_id: occurrence_id.map(str::to_owned),
                    waiting_reason: None,
                    title: Some(title.clone()),
                    reason: Some(sanitize_error_text(reason)),
                })
                .unwrap_or_else(|error| {
                    tracing::error!(
                        error = %sanitize_error_text(&error.to_string()),
                        "failed to serialize session lifecycle event"
                    );
                    serde_json::Value::Null
                }),
            ),
            AgentEvent::SessionError { session_id, error } => Some(
                serde_json::to_value(SessionLifecycleEvent {
                    session_id: session_id.clone(),
                    status: haven_common::SessionStatus::Error,
                    occurrence_id: occurrence_id.map(str::to_owned),
                    waiting_reason: None,
                    title,
                    reason: Some(sanitize_error_text(error)),
                })
                .unwrap_or_else(|error| {
                    tracing::error!(
                        error = %sanitize_error_text(&error.to_string()),
                        "failed to serialize session lifecycle event"
                    );
                    serde_json::Value::Null
                }),
            ),
            _ => None,
        }
    }
}
