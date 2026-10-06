use std::collections::HashSet;
use std::sync::Arc;

mod agent_tool_ports;
mod canonical;
mod compactor;
mod event;
mod fact_extraction;
mod fact_inference;
mod ingress;
pub mod interaction;
mod lifecycle;
mod memory_index;
mod memory_inference;
mod memory_runtime;
mod memory_service;
mod memory_trigger;
mod memory_worker;
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
mod storage_error;
mod title;
mod types;

pub(crate) use canonical::{is_dangling_boundary, sanitize_canonical};

pub use agent_tool_ports::AgentToolPorts;
pub use compactor::ContextCompactor;
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
    MemoryEventProcessOutcome, MemoryLiveTask, MemoryReady, MemoryStartup, PreparedMemoryRuntime,
};
pub use memory_service::{MemoryService, MemoryServiceStores};
pub use memory_worker::MemoryWorker;
pub use prompt::SystemPromptBuilder;
pub use prompt_context::{
    PromptCatalogContent, PromptCatalogVersions, PromptContextProvider, PromptRuntimeContext,
    PromptToolPort,
};
pub use prompt_renderer::{MemorySections, PromptRenderer};
pub use react::{
    LoopExit, MetricsSnapshot, PauseReason, ReActEngine, ToolCatalogPort, UiMetricsSnapshot,
};
pub use session::{
    ConfirmResolution, ManagedAssetLeasePort, RunEngine, RunHandler, SessionInfo, SessionStatus,
    SessionSupervisor, SessionSupervisorEvent, SessionToolOverlayPort, SessionToolPorts,
    SessionWaitingReason, StepInfo, ToolAuthorizationPort, ToolExecution, ToolExecutionContext,
    ToolExecutionPort, ToolObservationPort,
};
pub use storage_error::sqlite_storage_failure_message;

pub use types::{
    BranchPoint, ProcessResult, ReActRound, RunBudget, ToolCall, ToolRecord, TranscriptRecord,
    project_transcript, project_transcript_with_strategy, seed_events_from_canonical,
};
// The store owns SQLite ordering/persistence in `haven-memory`; these
// re-exports keep the Agent's durable session boundary discoverable to callers.
pub use haven_memory::{SessionEvent, SessionEventInput, SessionEventSubscription, SessionStore};

use haven_common::config::ContextLimitsConfig;
use haven_common::types::MessageAttachment;
use haven_llm::LlmRouter;
#[cfg(test)]
use haven_memory::Database;
use haven_tools::ScheduleMode;
use tokio::sync::Mutex;

use crate::title::TitleGenerator;

/// Persist an accepted input routed into an existing session. The durable
/// pending marker is inserted atomically with the user message and is cleared
/// only by the matching committed `UserInject` event.
pub(crate) async fn persist_pending_user_input(
    executor: &crate::session::SessionSupervisor,
    session_id: &str,
    content: &str,
    message_type: Option<&str>,
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
