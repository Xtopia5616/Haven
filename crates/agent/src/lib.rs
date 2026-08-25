use std::collections::HashSet;
use std::sync::Arc;

mod canonical;
mod compactor;
mod event;
mod inference;
mod ingress;
mod lifecycle;
mod partial;
mod prompt;
mod react;
mod resume;
mod rollback;
mod session;
mod title;
mod types;

pub(crate) use canonical::{interrupted_result_text, is_dangling_boundary, sanitize_canonical};

pub use compactor::ContextCompactor;
pub use event::{AgentEvent, AgentEventEmitter, BufferedEmitter, EventBus, EventDispatcher};
pub use inference::InferenceEngine;
pub use prompt::{MemorySections, SystemPromptBuilder};
pub use react::{LoopExit, PauseReason, ReActEngine};
pub use session::{
    ConfirmResolution, RunHandler, SessionExecutor, SessionInfo, SessionStatus, StepInfo,
    ToolExecution,
};
pub use types::{
    Action, BranchPoint, ProcessResult, ReActRound, ReActSnapshot, RunBudget, ToolRecord,
    TranscriptRecord, project_transcript, seed_events_from_canonical,
};

use haven_common::config::ContextLimitsConfig;
use haven_common::types::MessageAttachment;
use haven_llm::LlmRouter;
use haven_memory::Database;
use haven_memory::repositories::messages::Message;
use haven_tools::ScheduleMode;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::title::TitleGenerator;

/// Low-level `messages` insert (partial discard + `add_message_full`).
///
/// X12: ReAct-loop assistant/thought/ask/reasoning rows must go through
/// `ReActEngine::apply_transcript` → `project_chat_message`. Direct callers
/// are limited to ingress user seeds, terminal action-result history, and
/// recovery partials. Do not reintroduce parallel assistant writers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_session_message(
    executor: &crate::session::SessionExecutor,
    session_id: &str,
    role: &str,
    content: &str,
    message_type: Option<&str>,
    attachments: &[MessageAttachment],
    voice: bool,
    // When `Some`, insert the row under this pre-minted id instead of
    // minting a fresh one. Streaming message ids are minted when the
    // thought/reasoning block starts so the live bubble and the DB row
    // share one identity.
    message_id: Option<&str>,
    // Optional `tool_call_id` for the row; `None` for ordinary messages.
    tool_call_id: Option<&str>,
) -> anyhow::Result<Message> {
    executor.partials.discard(session_id).await;
    let db = executor.db().clone();
    let session_id = session_id.to_string();
    let role = role.to_string();
    let content = content.to_string();
    let message_type = message_type.map(String::from);
    let attachments = attachments.to_vec();
    let message_id = message_id.map(String::from);
    let tool_call_id = tool_call_id.map(String::from);
    db.run_blocking(move |db| {
        db.add_message_full(
            &session_id,
            &role,
            &content,
            message_type.as_deref(),
            tool_call_id.as_deref(),
            &attachments,
            voice,
            message_id.as_deref(),
        )
    })
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
