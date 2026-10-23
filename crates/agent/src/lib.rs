use std::sync::Arc;

mod agent_tool_ports;
mod canonical;
mod compactor;
mod event;
mod fact_extraction;
mod fact_inference;
mod fork;
mod ingress;
mod interaction;
mod lifecycle;
mod memory_index;
mod memory_inference;
mod memory_runtime;
mod memory_service;
mod memory_trigger;
mod memory_worker;
mod model_runtime;
mod partial;
mod prompt;
mod prompt_context;
mod prompt_renderer;
mod react;
mod resume;
mod resume_support;
mod rollback;
mod rollback_support;
mod session;
mod session_execution_workspace;
mod storage_error;
#[cfg(test)]
mod test_support;
mod title;
mod token_budget;
mod types;
mod working_document;

pub(crate) use canonical::{is_dangling_boundary, sanitize_canonical};

pub use agent_tool_ports::{AgentToolPorts, LifecycleHooksPort};
pub use event::{
    AgentEvent, AgentEventEmitter, BufferedEmitter, EventBus, EventDispatcher,
    ToolRunCompletionStatus, ToolRunNotificationSource,
};
pub use interaction::{
    InteractionDetails, InteractionEnvelope, InteractionKind, InteractionOwner, InteractionRequest,
    InteractionStatus, replay_session_interactions,
};
pub use layer::{AgentStartup, PendingSessionRecovery};
pub use memory_runtime::{
    MemoryLiveConsumerHandoff, MemoryReady, MemoryStartup, PreparedMemoryRuntime,
};
pub use memory_service::MemoryService;
pub(crate) use memory_worker::MemoryWorker;
pub(crate) use prompt::SystemPromptBuilder;
pub use prompt_context::{
    PromptCatalogContent, PromptCatalogVersions, PromptRuntimeContext, PromptToolPort,
};
pub(crate) use react::ReActEngine;
pub use react::{METRICS_PHASE_COUNT, MetricsSnapshot, ToolCatalogPort, UiMetricsSnapshot};
pub use session::{
    AgentSchedulePort, AgentToolRunPort, ConfirmResolution, ManagedAssetLeasePort,
    RecordedConfirmationDecision, ScheduleFiredReceiverPort, SessionInfo, SessionSupervisor,
    SessionSupervisorEvent, SessionToolOverlayPort, SessionToolPorts, StepInfo,
    ToolAuthorizationPort, ToolExecution, ToolExecutionContext, ToolExecutionPort,
    ToolObservationPort, ToolRunCompletionReceiverPort,
};
pub(crate) use session_execution_workspace::session_execution_workspace_from_events;
pub use storage_error::sqlite_storage_failure_message;
pub use working_document::{
    MAX_SESSION_WORKING_DOCUMENT_BYTES, SESSION_WORKING_DOCUMENT_EVENT_TYPE,
    SessionWorkingDocumentEvent, session_working_document_from_events,
    validate_session_working_document,
};

use haven_common::config::ContextLimitsConfig;
use haven_common::schedule::ScheduleMode;
use haven_common::types::MessageAttachment;
use haven_llm::LlmRouter;
#[cfg(test)]
use haven_memory::Database;
#[cfg(test)]
pub(crate) use react::ParsedAgentResponse;
pub use types::TranscriptSubmissionResult;
#[cfg(test)]
pub(crate) use types::{
    AgentToolCall, BranchPoint, TranscriptRecord, project_transcript, seed_events_from_canonical,
};

use crate::title::TitleGenerator;

/// Persist an accepted input routed into an existing session. The durable
/// pending marker is inserted atomically with the user message and is cleared
/// only by the matching committed `UserInject` event.
pub(crate) async fn persist_pending_user_input(
    executor: &crate::session::SessionSupervisor,
    session_id: &str,
    content: &str,
    message_type: Option<haven_common::types::TranscriptMessageKind>,
    attachments: &[MessageAttachment],
    voice: bool,
    disposition: haven_memory::PendingInputDisposition,
) -> anyhow::Result<haven_memory::PendingSessionInput> {
    executor.partials.discard(session_id).await;
    executor
        .session_store()
        .persist_pending_user_input(
            session_id,
            content,
            message_type,
            attachments,
            voice,
            None,
            disposition,
            None,
        )
        .await
}

/// Checkpoint throttle for streamed partial text lives in
/// `context_limits.partial_checkpoint_interval_secs` /
/// `partial_checkpoint_min_chars` (see `ReActEngine::stream_llm_response`).
/// Trim a long tool result to fit a notification body.
fn truncate_notification(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cutoff = text.floor_char_boundary(max_chars);
    format!(
        "{}[... {} chars omitted]",
        &text[..cutoff],
        text.chars().count() - cutoff
    )
}

mod layer;
pub use layer::AgentLayer;

#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;
