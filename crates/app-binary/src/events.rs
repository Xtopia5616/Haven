use haven_common::{SessionStatus, SessionWaitingReason, ToolRunStatus};
use haven_tools::{ToolRunLifecyclePayload, ToolRunOutputPayload, ToolRunView};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The single authoritative session lifecycle channel.
pub(crate) const SESSION_LIFECYCLE_EVENT: &str = "session:lifecycle";

/// Stable recording and transcription event channels exposed by the Tauri
/// boundary. Keep these names beside their DTOs so every producer shares one
/// public wire directory.
pub(crate) const RECORDING_STARTED_EVENT: &str = "recording:started";
pub(crate) const RECORDING_STOPPED_EVENT: &str = "recording:stopped";
pub(crate) const RECORDING_VAD_STATUS_EVENT: &str = "recording:vad_status";
pub(crate) const RECORDING_ERROR_EVENT: &str = "recording:error";
pub(crate) const TRANSCRIPTION_STARTED_EVENT: &str = "transcription:started";
pub(crate) const TRANSCRIPTION_RESULT_EVENT: &str = "transcription:result";
pub(crate) const TRANSCRIPTION_ERROR_EVENT: &str = "transcription:error";

/// Stable ToolRun event channels exposed by the Tauri boundary.
///
/// The tool crate deliberately owns execution, but it does not own the IPC
/// contract.  Keep the public channel names and their projected payload here,
/// alongside the session contract, so internal tool status JSON cannot become
/// an accidental frontend API.
pub(crate) const TOOL_RUN_CREATED_EVENT: &str = "tool_run:created";
pub(crate) const TOOL_RUN_UPDATED_EVENT: &str = "tool_run:updated";
pub(crate) const TOOL_RUN_OUTPUT_EVENT: &str = "tool_run:output";
pub(crate) const TOOL_RUN_FINISHED_EVENT: &str = "tool_run:finished";

/// Stable channels for the remaining app-shell events. Their producers may
/// live in different crates, but the Tauri wire names and DTOs belong here.
pub(crate) const APP_BOOTSTRAP_EVENT: &str = "app:bootstrap";
pub(crate) const TRAY_STATUS_CHANGED_EVENT: &str = "tray:status_changed";
pub(crate) const MUTE_CHANGED_EVENT: &str = "mute:changed";
pub(crate) const MCP_STATUS_CHANGED_EVENT: &str = "mcp:status_change";
pub(crate) const SKILLS_STATUS_CHANGED_EVENT: &str = "skills:status_change";
pub(crate) const INTERACTION_REQUESTED_EVENT: &str = "interaction:requested";
pub(crate) const HOTKEY_CONFLICT_EVENT: &str = "hotkey:conflict";
pub(crate) const HOTKEY_REBIND_EVENT: &str = "hotkey:rebind";
pub(crate) const LLM_CONFIG_CHANGED_EVENT: &str = "llm:config_changed";

/// Agent event channels that are not session lifecycle events.
pub(crate) const AGENT_THOUGHT_EVENT: &str = "agent:thought";
pub(crate) const AGENT_TOOL_CALL_EVENT: &str = "agent:tool_call";
pub(crate) const AGENT_OBSERVATION_EVENT: &str = "agent:observation";
pub(crate) const AGENT_THOUGHT_CHUNK_EVENT: &str = "agent:thought_chunk";
pub(crate) const AGENT_REASONING_CHUNK_EVENT: &str = "agent:reasoning_chunk";
pub(crate) const AGENT_STREAM_RESET_EVENT: &str = "agent:stream_reset";
pub(crate) const AGENT_MEDIA_PLAN_EVENT: &str = "agent:media_plan";
pub(crate) const AGENT_WEB_SEARCH_EVENT: &str = "agent:web_search";
pub(crate) const AGENT_STREAM_STALLED_EVENT: &str = "agent:stream_stalled";
pub(crate) const AGENT_SUPPLEMENT_EVENT: &str = "agent:supplement";
pub(crate) const AGENT_COMPACTION_EVENT: &str = "agent:compaction";
pub(crate) const AGENT_USAGE_EVENT: &str = "agent:usage";
pub(crate) const AGENT_TOOL_OUTPUT_EVENT: &str = "agent:tool_output";
pub(crate) const NOTIFICATION_SHOW_EVENT: &str = "notification:show";

/// App-owned wire DTO category. Keep this separate from `haven_tools::ToolRunKind`:
/// the values currently match, but IPC serialization and field policy belong
/// to App and must not change when Tools changes its runtime model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRunKindDto {
    Background,
    Scheduled,
}

/// The minimal ToolRun record displayed by the task panel and history.
///
/// This intentionally excludes internal dynamic tool parameters, continuation
/// prompts, and local output-log paths.  Those are execution details rather
/// than a stable UI contract and may contain sensitive values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolRunEvent {
    pub id: String,
    pub kind: ToolRunKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolRunStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

impl ToolRunEvent {
    pub(crate) fn from_lifecycle_payload(payload: ToolRunLifecyclePayload) -> Self {
        let status = payload.state.status();
        let started_at = payload.state.started_at().map(str::to_owned);
        let finished_at = payload.state.finished_at().map(str::to_owned);
        Self {
            id: payload.tool_run_id,
            kind: match payload.kind {
                haven_tools::ToolRunKind::Background => ToolRunKindDto::Background,
                haven_tools::ToolRunKind::Scheduled => ToolRunKindDto::Scheduled,
            },
            status: Some(status),
            session_id: payload.session_id,
            source_step_id: payload.source_step_id,
            started_at,
            finished_at,
            due_at: payload.due_at,
            title: payload.title,
            body: payload.body,
            mode: payload.mode,
            command: None,
            output: payload.output,
            error: payload.error,
            error_reason: payload.error_reason,
            exit_code: payload.exit_code,
            preview: None,
        }
    }

    /// Narrow projection for a live output preview. The typed Tools payload
    /// cannot carry command metadata, dynamic arguments, or unbounded stderr.
    pub(crate) fn from_output_payload(payload: ToolRunOutputPayload) -> Self {
        Self {
            id: payload.tool_run_id,
            kind: ToolRunKindDto::Background,
            status: Some(ToolRunStatus::Running),
            session_id: None,
            source_step_id: payload.source_step_id,
            started_at: None,
            finished_at: None,
            due_at: None,
            title: None,
            body: None,
            mode: None,
            command: None,
            output: Some(payload.output),
            error: None,
            error_reason: None,
            exit_code: None,
            preview: None,
        }
    }

    pub(crate) fn from_session_attached_payload(
        payload: haven_tools::ToolRunSessionAttachedPayload,
    ) -> Self {
        Self {
            id: payload.tool_run_id,
            kind: ToolRunKindDto::Background,
            status: None,
            session_id: Some(payload.session_id),
            source_step_id: payload.source_step_id,
            started_at: None,
            finished_at: None,
            due_at: None,
            title: None,
            body: None,
            mode: None,
            command: None,
            output: None,
            error: None,
            error_reason: None,
            exit_code: None,
            preview: None,
        }
    }
}

impl From<ToolRunView> for ToolRunEvent {
    fn from(view: ToolRunView) -> Self {
        Self {
            id: view.id,
            kind: match view.kind {
                haven_tools::ToolRunKind::Background => ToolRunKindDto::Background,
                haven_tools::ToolRunKind::Scheduled => ToolRunKindDto::Scheduled,
            },
            status: Some(view.status),
            session_id: view.session_id,
            source_step_id: view.source_step_id,
            started_at: view.started_at,
            finished_at: view.finished_at,
            due_at: view.due_at,
            title: view.title,
            body: view.body,
            mode: view.mode,
            command: view.command,
            output: view.output,
            error: view.error,
            error_reason: view.error_reason,
            exit_code: view.exit_code,
            preview: view.preview,
        }
    }
}

impl ToolRunKindDto {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Scheduled => "scheduled",
        }
    }
}

/// One typed wire contract for every session lifecycle transition.
///
/// A terminal variant carries its own reason/error so consumers handle status
/// and terminal cleanup from the same event. The update status deliberately
/// excludes completed/error; those states have dedicated variants.
#[derive(Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
pub(crate) enum SessionLifecycleEvent {
    Created {
        session_id: String,
        status: SessionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        waiting_reason: Option<SessionWaitingReason>,
        title: Option<String>,
    },
    Updated {
        session_id: String,
        status: SessionUpdateStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        waiting_reason: Option<SessionWaitingReason>,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Completed {
        session_id: String,
        title: String,
        reason: String,
    },
    Error {
        session_id: String,
        title: String,
        error: String,
    },
    TitleUpdated {
        session_id: String,
        title: String,
    },
    Deleted {
        /// `None` means every session was removed by `clear_history`.
        session_id: Option<String>,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionUpdateStatus {
    Pending,
    Running,
    Paused,
}

#[derive(Clone, Serialize)]
pub struct RecordingEvent {
    pub is_recording: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<haven_common::types::SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Serialize)]
pub struct VadStatusEvent {
    pub signal: String,
    pub state: String,
}

#[derive(Clone, Serialize)]
pub struct TranscriptionResultEvent {
    pub session_id: haven_common::types::SessionId,
    pub text: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Emitted right before STT runs, so the UI can show a "transcribing" hint
/// covering the gap between `recording:stopped` and `transcription:result`.
#[derive(Clone, Serialize)]
pub struct TranscriptionStartedEvent {
    pub session_id: haven_common::types::SessionId,
}

#[derive(Clone, Serialize)]
pub struct TranscriptionErrorEvent {
    pub session_id: haven_common::types::SessionId,
    pub error: String,
}

/// User-facing recording failure. This is deliberately separate from a
/// transcription failure because it can occur before capture begins.
#[derive(Clone, Serialize)]
pub struct RecordingErrorEvent {
    pub session_id: haven_common::types::SessionId,
    pub error: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AppBootstrapEvent {
    pub status: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrayStatusChangedEvent {
    pub status: String,
    pub tooltip: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct MuteChangedEvent {
    pub muted: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct McpStatusChangedEvent {
    pub name: String,
    pub status: haven_tools::McpClientStatus,
}

#[derive(Clone, Serialize)]
pub(crate) struct SkillsStatusChangedEvent {
    pub op: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct HotkeyConflictEvent {
    pub binding: String,
    pub error: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct HotkeyRebindEvent {
    pub old_binding: String,
    pub new_binding: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentThoughtEvent {
    pub session_id: String,
    pub thought: String,
    pub step_number: u32,
    pub run_id: u64,
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentToolCallEvent {
    pub session_id: String,
    pub tool_name: String,
    pub input: Value,
    pub step_number: u32,
    pub run_id: u64,
    pub tool_call_id: Option<String>,
    pub tool_index: u32,
    pub step_id: String,
    pub suppress_streamed_thought: bool,
    pub silent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentObservationEvent {
    pub session_id: String,
    pub observation: String,
    pub tool_name: String,
    pub step_number: u32,
    pub run_id: u64,
    pub silent: bool,
    pub tool_call_id: Option<String>,
    pub tool_index: u32,
    pub ask_options: Vec<String>,
    pub step_id: String,
    pub outcome: String,
    pub idempotency: String,
    pub operation_scope: String,
    pub renderer: String,
    pub result: haven_tools::ToolResultEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

/// Renderer-safe projection of every human decision request. Internal
/// `InteractionRequest` values may carry raw tool input and receipts; this
/// DTO deliberately contains only the information needed to render a card
/// and resolve it by id.
#[derive(Clone, Serialize)]
pub(crate) struct InteractionRequestedEvent {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub owner: haven_agent::InteractionOwner,
    pub kind: haven_agent::InteractionKind,
    pub status: haven_agent::InteractionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<haven_common::types::RiskLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentThoughtChunkEvent {
    pub session_id: String,
    pub delta: String,
    pub step_number: u32,
    pub run_id: u64,
    pub message_id: String,
    pub seq: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentReasoningChunkEvent {
    pub session_id: String,
    pub delta: String,
    pub step_number: u32,
    pub run_id: u64,
    pub message_id: String,
    pub seq: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentStreamResetEvent {
    pub session_id: String,
    pub step_number: u32,
    pub run_id: u64,
    pub thought_message_id: String,
    pub reasoning_message_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentWebSearchEvent {
    pub session_id: String,
    pub phase: String,
    pub step_number: u32,
    pub run_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentStreamStalledEvent {
    pub session_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentMediaPlanEvent {
    pub session_id: String,
    pub step_number: u32,
    pub run_id: u64,
    /// Request kind serialized under the established `role` wire field.
    pub role: haven_common::config::RequestKind,
    pub strategy: haven_common::media::MediaInputStrategy,
    pub projections: Vec<haven_common::media::MediaProjection>,
    pub notices: Vec<haven_common::media::MediaPlanNotice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentSupplementEvent {
    pub session_id: String,
    pub additional_context: String,
    pub step_number: u32,
    pub run_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub supplement_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_source: Option<haven_common::types::InjectSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentCompactionEvent {
    pub session_id: String,
    pub summary: String,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentNotificationEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_kind: Option<AgentNotificationKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_run_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_run_status: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentNotificationKind {
    ToolRunCompletion,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentUsageEvent {
    pub session_id: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_miss_tokens: u32,
    pub context_tokens: u32,
    pub cache_exclusive: bool,
    pub cache_accounting: String,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub cumulative_prompt_tokens: u32,
    pub cumulative_completion_tokens: u32,
    pub cumulative_total_tokens: u32,
    pub cumulative_cached_tokens: u32,
    pub cumulative_cache_creation_tokens: u32,
    pub cumulative_cache_miss_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_diagnostics: Option<haven_llm::CacheDiagnostics>,
    pub cumulative_cost_usd: Option<f64>,
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Request kind serialized under the established `role` wire field.
    pub role: Option<haven_common::config::RequestKind>,
    pub call_kind: String,
    pub has_cost: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct AgentToolOutputEvent {
    pub session_id: String,
    pub step_id: String,
    pub output: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_event_serde() {
        let ev = RecordingEvent {
            is_recording: true,
            session_id: Some("s1".into()),
            reason: Some("manual".into()),
            duration_ms: Some(1000),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"is_recording\":true"));
        assert!(json.contains("\"session_id\":\"s1\""));
    }

    #[test]
    fn session_lifecycle_event_uses_a_discriminated_union() {
        let event = SessionLifecycleEvent::Updated {
            session_id: "ses-1".into(),
            status: SessionUpdateStatus::Paused,
            waiting_reason: Some(SessionWaitingReason::UserInterrupt),
            title: "Plan migration".into(),
            reason: Some("用户主动打断输出".into()),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "type": "updated",
                "session_id": "ses-1",
                "status": "paused",
                "title": "Plan migration",
                "waiting_reason": "user_interrupt",
                "reason": "用户主动打断输出",
            })
        );

        let error = SessionLifecycleEvent::Error {
            session_id: "ses-2".into(),
            title: "Build".into(),
            error: "provider unavailable".into(),
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({
                "type": "error",
                "session_id": "ses-2",
                "title": "Build",
                "error": "provider unavailable",
            })
        );
    }

    #[test]
    fn session_deleted_event_can_signal_global_clear() {
        let event = SessionLifecycleEvent::Deleted { session_id: None };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({ "type": "deleted", "session_id": null })
        );
    }

    #[test]
    fn tool_run_event_projects_background_status_to_the_stable_wire_shape() {
        let mut payload = ToolRunLifecyclePayload::new(
            haven_tools::ToolRunKind::Background,
            "toolrun-1",
            haven_tools::ToolRunLifecycleState::Completed {
                started_at: "2026-09-23T10:00:00Z".into(),
                finished_at: "2026-09-23T10:00:01Z".into(),
            },
        );
        payload.session_id = Some("ses-1".into());
        payload.source_step_id = Some("step-1".into());
        payload.output = Some("done".into());
        payload.exit_code = Some(0);
        let event = ToolRunEvent::from_lifecycle_payload(payload);

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "id": "toolrun-1",
                "kind": "background",
                "status": "completed",
                "started_at": "2026-09-23T10:00:00Z",
                "finished_at": "2026-09-23T10:00:01Z",
                "session_id": "ses-1",
                "source_step_id": "step-1",
                "output": "done",
                "exit_code": 0,
            })
        );
    }

    #[test]
    fn tool_run_event_hides_scheduled_execution_details() {
        let mut payload = ToolRunLifecyclePayload::new(
            haven_tools::ToolRunKind::Scheduled,
            "toolrun-2",
            haven_tools::ToolRunLifecycleState::Waiting,
        );
        payload.title = Some("Reminder".into());
        payload.body = Some("Take a break".into());
        payload.mode = Some("tool".into());
        let event = ToolRunEvent::from_lifecycle_payload(payload);
        let wire = serde_json::to_value(event).unwrap();

        assert_eq!(wire["id"], "toolrun-2");
        assert_eq!(wire["kind"], "scheduled");
        assert_eq!(wire["status"], "waiting");
        assert!(wire.get("tool_name").is_none());
        assert!(wire.get("tool_args").is_none());
        assert!(wire.get("prompt").is_none());
    }

    #[test]
    fn background_tool_run_view_preserves_the_tool_run_event_wire_contract() {
        let event = ToolRunEvent::from(ToolRunView {
            id: "toolrun-board-background".into(),
            kind: haven_tools::ToolRunKind::Background,
            status: ToolRunStatus::Completed,
            session_id: Some("ses-1".into()),
            source_step_id: Some("step-source".into()),
            started_at: Some("2026-09-23T10:00:00Z".into()),
            finished_at: Some("2026-09-23T10:00:01Z".into()),
            due_at: None,
            title: None,
            body: None,
            mode: None,
            command: None,
            output: Some("done".into()),
            error: None,
            error_reason: None,
            exit_code: Some(0),
            preview: Some("done".into()),
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "id": "toolrun-board-background",
                "kind": "background",
                "status": "completed",
                "session_id": "ses-1",
                "source_step_id": "step-source",
                "started_at": "2026-09-23T10:00:00Z",
                "finished_at": "2026-09-23T10:00:01Z",
                "output": "done",
                "exit_code": 0,
                "preview": "done",
            })
        );
    }

    #[test]
    fn scheduled_tool_run_view_preserves_wire_contract_without_internal_fields() {
        let event = ToolRunEvent::from(ToolRunView {
            id: "toolrun-board-scheduled".into(),
            kind: haven_tools::ToolRunKind::Scheduled,
            status: ToolRunStatus::Waiting,
            session_id: Some("ses-2".into()),
            source_step_id: None,
            started_at: None,
            finished_at: None,
            due_at: Some("2026-09-24T10:00:00Z".into()),
            title: Some("Take a break".into()),
            body: Some("Stand up".into()),
            mode: Some("continue".into()),
            command: None,
            output: None,
            error: None,
            error_reason: None,
            exit_code: None,
            preview: None,
        });
        let wire = serde_json::to_value(event).unwrap();

        assert_eq!(
            wire,
            serde_json::json!({
                "id": "toolrun-board-scheduled",
                "kind": "scheduled",
                "status": "waiting",
                "session_id": "ses-2",
                "due_at": "2026-09-24T10:00:00Z",
                "title": "Take a break",
                "body": "Stand up",
                "mode": "continue",
            })
        );
        for internal_field in [
            "tool_args",
            "prompt",
            "tool_name",
            "watch_tool_run_id",
            "log_path",
        ] {
            assert!(
                wire.get(internal_field).is_none(),
                "leaked {internal_field}"
            );
        }
    }

    #[test]
    fn test_recording_event_skips_optional_none() {
        let ev = RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: None,
            duration_ms: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("session_id"));
        assert!(!json.contains("reason"));
    }

    #[test]
    fn test_vad_status_event_serde() {
        let ev = VadStatusEvent {
            signal: "speech".into(),
            state: "speaking".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"signal\":\"speech\""));
    }

    #[test]
    fn test_transcription_result_event_serde() {
        let ev = TranscriptionResultEvent {
            session_id: "s1".into(),
            text: "hello".into(),
            duration_ms: 500,
            confidence: Some(0.95),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"session_id\":\"s1\""));
        assert!(json.contains("\"confidence\":0.95"));
    }

    #[test]
    fn test_transcription_started_event_serde() {
        let ev = TranscriptionStartedEvent {
            session_id: "rec-abc".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"session_id\":\"rec-abc\""));
    }

    #[test]
    fn test_transcription_error_event_serde() {
        let ev = TranscriptionErrorEvent {
            session_id: "s1".into(),
            error: "timeout".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"error\":\"timeout\""));
    }

    #[test]
    fn recording_error_event_has_the_stable_wire_shape() {
        let event = RecordingErrorEvent {
            session_id: "rec-1".into(),
            error: "microphone unavailable".into(),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({ "session_id": "rec-1", "error": "microphone unavailable" })
        );
    }

    #[test]
    fn test_recording_event_all_fields_serialized() {
        let ev = RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: None,
            duration_ms: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        // Should only contain is_recording
        assert_eq!(json, r#"{"is_recording":false}"#);
    }

    #[test]
    fn notification_omits_missing_session_association_and_keeps_real_one() {
        let app_notification = AgentNotificationEvent {
            session_id: None,
            title: "操作已完成".into(),
            body: "结果".into(),
            notification_kind: None,
            tool_run_kind: None,
            tool_run_id: None,
            tool_run_status: None,
        };
        assert_eq!(
            serde_json::to_value(app_notification).unwrap(),
            serde_json::json!({ "title": "操作已完成", "body": "结果" })
        );

        let session_notification = AgentNotificationEvent {
            session_id: Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            title: "会话通知".into(),
            body: "内容".into(),
            notification_kind: None,
            tool_run_kind: None,
            tool_run_id: None,
            tool_run_status: None,
        };
        assert_eq!(
            serde_json::to_value(session_notification).unwrap(),
            serde_json::json!({
                "session_id": "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "title": "会话通知",
                "body": "内容"
            })
        );
    }
}
