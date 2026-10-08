//! Session start / resume drivers for [`AgentLayer`]: fresh ReAct runs,
//! durable event replay and fresh-session startup.
//!
//! Split out of `layer.rs` so the facade stays focused on wiring; these
//! methods operate on the same private fields via `impl AgentLayer` blocks.
//!
//! ## Resume authority
//!
//! - **Session event stream present** → single authority. [`run_session_resumed`]
//!   replays it (canonical + rounds are projected); checkpoint metadata and RAM
//!   queues are caches only.
//! - **Event stream missing** → fresh-session startup for a session that has
//!   not entered the ReAct loop yet.
//!
//! ## Queue durability (Phase 7 / D2)
//!
//! RAM follow-up / steering queues are a same-process cache. Durability is
//! DB messages plus explicit pending-input markers, acknowledged atomically
//! with `UserInject`. Resume re-queues by `message_id`; the durable event
//! sequence decides transcript recovery.

use crate::AgentLayer;
use crate::event::{AgentEvent, AgentEventEmitter};
use crate::react::DurableEventState;
use crate::react::{ReActRunInput, ReActRunReplay};
use crate::resume_support::{
    decode_builtin_tool_selection, infer_resume_step, load_mcp_tool_names, load_skill_names,
};

use crate::session::{DirectSessionRunLease, SessionStatus};
use crate::types::{
    ReActRound, TranscriptRecord, project_transcript_with_strategy, seed_events_from_canonical,
};
use haven_common::media::MediaInput;
use haven_common::types::{CanonicalMessage, ContentPart};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum TerminalErrorEventOwner {
    AgentEventBus,
    SessionSupervisor,
}

struct DirectSessionRunGuard {
    lease: Option<DirectSessionRunLease>,
}

impl DirectSessionRunGuard {
    async fn finish(&mut self) {
        if let Some(lease) = self.lease.as_mut() {
            lease.finish().await;
        }
        self.lease.take();
    }
}

struct SuppressSessionErrorEmitter(Arc<dyn AgentEventEmitter>);

#[async_trait::async_trait]
impl AgentEventEmitter for SuppressSessionErrorEmitter {
    async fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::SessionError { .. } => {}
            event => self.0.emit(event).await,
        }
    }
}

struct SuppressLifecycleCancelledSessionErrorEmitter {
    inner: Arc<dyn AgentEventEmitter>,
    executor: Arc<crate::session::SessionSupervisor>,
}

#[async_trait::async_trait]
impl AgentEventEmitter for SuppressLifecycleCancelledSessionErrorEmitter {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::SessionError { session_id, error } = &event {
            let _lifecycle = self.executor.lifecycle_guard().await;
            if self.executor.ensure_lifecycle_open().is_err()
                || self.executor.is_session_closing(session_id)
            {
                tracing::debug!(
                    session_id,
                    "suppressing a direct-run error after lifecycle closing began"
                );
                return;
            }
            match self.executor.mark_run_failed_if_active(session_id).await {
                Ok(true) => {
                    if let Err(persist_error) = self
                        .executor
                        .persist_session_run_end_reason(session_id, error)
                        .await
                    {
                        tracing::error!(
                            session_id,
                            error = %persist_error,
                            "failed to persist session run-end reason"
                        );
                    }
                }
                Ok(false) => {
                    tracing::debug!(
                        session_id,
                        "suppressing a direct-run error after lifecycle state changed"
                    );
                    return;
                }
                Err(error) => {
                    tracing::error!(
                        session_id,
                        error = %error,
                        "failed to commit direct-run error status; suppressing stale event"
                    );
                    return;
                }
            }
        }
        self.inner.emit(event).await;
    }
}

/// A recent session prompt message (id, role, content) used by the fresh-session
/// system-prompt path. **S1 authority:** canonical is the LLM truth; this
/// window may feed Additional context only for turns not already represented
/// as the first canonical user message. Resume does not use this type: the
/// durable event stream is the authority and pending inputs are recovered by
/// durable message identity, not by content comparison.
#[derive(Debug, Clone)]
pub(crate) struct SessionPromptMessage {
    id: String,
    role: haven_common::types::CanonicalRole,
    content: String,
}

pub(crate) struct InitialUserInput<'a> {
    attachments: &'a [haven_common::types::MessageAttachment],
    media_inputs: &'a [MediaInput],
    message_id: Option<&'a str>,
}

impl AgentLayer {
    /// Re-queue inputs that were durably accepted but have not yet committed
    /// their `UserInject` event. Their disposition is frozen in the pending
    /// marker, which is acknowledged in the same transaction as that event.
    async fn restore_pending_user_inputs(&self, session_id: &str) -> anyhow::Result<usize> {
        // A live executor queue is authoritative within this process. Avoid
        // re-enqueuing it from the durable copy.
        if self.executor.has_pending_context(session_id).await {
            return Ok(0);
        }

        let pending_inputs = self
            .react_engine
            .event_store
            .pending_session_inputs(session_id)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to recover pending inputs for session {session_id}: {error}"
                )
            })?;
        let mut restored = 0usize;
        for pending_input in pending_inputs {
            let message = pending_input.message;
            self.executor
                .register_managed_assets_for_session(session_id, &message.attachments);
            let is_answer =
                pending_input.disposition == haven_memory::PendingInputDisposition::Answer;
            let queued = if is_answer {
                self.executor
                    .add_answer_with_attachments(
                        session_id,
                        &message.content,
                        &message.attachments,
                        Some(message.id.clone()),
                    )
                    .await
            } else {
                self.executor
                    .add_follow_up_with_attachments(
                        session_id,
                        &message.content,
                        &message.attachments,
                        Some(message.id.clone()),
                    )
                    .await
            };
            match queued {
                Ok(()) => {
                    restored += 1;
                }
                Err(error) => tracing::warn!(
                    "failed to re-queue pending input {} for session {}: {}",
                    message.id,
                    session_id,
                    error
                ),
            }
        }
        Ok(restored)
    }

    /// Load the most recent session messages for the FRESH-run system-prompt path
    /// (`prompt_builder.build`). Resume does not consume this: the restored
    /// event stream is the authority, and explicitly pending inputs are
    /// recovered by message identity in `run_session_resumed`.
    async fn load_session_prompt_history(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionPromptMessage>> {
        self.executor
            .session_store()
            .list_session_prompt_messages(session_id, self.session_prompt_history_limit)
            .await
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|message| SessionPromptMessage {
                        id: message.id,
                        role: message.role,
                        content: message.content,
                    })
                    .collect()
            })
            .map_err(|error| anyhow::anyhow!("failed to load session prompt history: {error}"))
    }

    /// Run a session directly by id. ReAct terminal errors are published on
    /// the Agent event bus; dispatcher callers use
    /// [`Self::run_session_from_dispatcher`] so the supervisor owns that event.
    pub async fn run_session_from_id(&self, session_id: &str) -> anyhow::Result<Vec<ReActRound>> {
        self.run_session_from_id_with_error_owner(
            session_id,
            TerminalErrorEventOwner::AgentEventBus,
        )
        .await
    }

    /// Dispatcher-only entrypoint. The supervisor reports a failed run on
    /// its typed lifecycle stream, so suppress the matching ReAct error event
    /// while preserving all other Agent events and the returned error.
    pub(crate) async fn run_session_from_dispatcher(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<ReActRound>> {
        self.run_session_from_id_with_error_owner(
            session_id,
            TerminalErrorEventOwner::SessionSupervisor,
        )
        .await
    }

    async fn run_session_from_id_with_error_owner(
        &self,
        session_id: &str,
        terminal_error_owner: TerminalErrorEventOwner,
    ) -> anyhow::Result<Vec<ReActRound>> {
        tracing::debug!("run_session_from_id: session_id={}", session_id);
        let session =
            self.executor.get_session(session_id).await.ok_or_else(|| {
                anyhow::anyhow!("session '{}' not found by dispatcher", session_id)
            })?;

        // Dispatcher calls already own the run slot. A public direct call must
        // acquire its own lease before changing status or doing any work; a
        // rejected lease means the session is closing or another run owns it.
        let direct_lease = match terminal_error_owner {
            TerminalErrorEventOwner::AgentEventBus => Some(
                self.executor
                    .begin_direct_session_run(session_id)
                    .await
                    .ok_or_else(|| {
                        anyhow::anyhow!("session '{}' is closing or already running", session_id)
                    })?,
            ),
            TerminalErrorEventOwner::SessionSupervisor => None,
        };
        let _direct_guard = DirectSessionRunGuard {
            lease: direct_lease,
        };

        let result = async {
        // Claim flips Pending→Running in memory/DB without emitting. Direct
        // callers (tests / continue) may still be Pending — promote, then always
        // emit `running` so the UI busy chip tracks a real transition instead of
        // treating Pending as a stand-in for Running.
        if self.executor.get_active_session_status(session_id).await == Some(SessionStatus::Pending)
            && let Err(error) = self
                .executor
                .update_session_status(session_id, SessionStatus::Running)
                .await
        {
            tracing::warn!(
                session_id,
                error = %error,
                "failed to persist session running status before resume"
            );
        }
        if self.executor.get_active_session_status(session_id).await == Some(SessionStatus::Running)
        {
            self.events
                .emit_session_updated(session_id, SessionStatus::Running)
                .await;
        }

        let run_id = self.react_engine.next_run_id();

        let description = if session.summary.is_empty() {
            session.input.clone()
        } else {
            session.summary.clone()
        };
        let context = session.input.clone();

        // Prompt history and message persistence are keyed by session_id;
        // there is no separate session indirection anymore.
        let session_prompt_history = self.load_session_prompt_history(session_id).await?;

        // Multimodal: carry the first user message's image attachments into
        // the initial canonical user message so the model sees them from the
        // first turn (they were persisted by process_input_with_attachments).
        // The FIRST user message is always the session's own input; later image
        // follow-ups are supplements (injected by the ReAct loop at step
        // start) and must NOT be attached to the initial turn or they would
        // be duplicated.
        let resume_media = self
            .executor
            .session_store()
            .session_resume_media(session_id)
            .await
            .map_err(|error| anyhow::anyhow!("failed to load session resume data: {error}"))?;
        let haven_memory::SessionResumeMedia {
            initial_message_id,
            initial_attachments,
            initial_media_inputs,
            all_attachments,
        } = resume_media;

        // Durable transcript events carry metadata-only media inputs.
        // Re-register the host-owned files from the materialized media
        // projection before a resumed request can ask the `files` tool to
        // resolve them.
        self.executor
            .register_managed_assets_for_session(session_id, &all_attachments);

        // The event stream is authoritative. A checkpoint supplies no runtime
        // state; sessions without a durable event stream take the fresh-run
        // path below.
        let durable_state = self
            .react_engine
            .load_durable_event_state(session_id)
            .await?;
        match durable_state {
            Some(replay) => {
                tracing::info!(
                    "restoring ReAct state for session {} ({} events)",
                    session_id,
                    replay.events.len()
                );
                // Re-register per-session tools (skills/MCP) from projected
                // rounds, since in-memory registrations are lost on restart.
                let projection = project_transcript_with_strategy(
                    &replay.events,
                    self.react_engine.media_strategy(),
                );
                self.restore_per_session_tools(session_id, &projection.react_rounds)
                    .await;
                let interactions = self.executor.interaction_requests(session_id).await;
                let has_pending_ask = interactions.iter().any(|request| {
                    request.kind == crate::interaction::InteractionKind::Ask
                        && request.status == crate::interaction::InteractionStatus::Pending
                });
                let confirm_requests = interactions
                    .iter()
                    .filter(|request| request.kind == crate::interaction::InteractionKind::Confirm)
                    .collect::<Vec<_>>();
                // A pending interaction is the pause reason; session
                // status stays the single generic `Paused` state.
                let _ = has_pending_ask;
                if !confirm_requests.is_empty() {
                    // The dispatcher has already claimed this session as
                    // Running before entering the resume handler. Do not
                    // wake it back to Pending here: that would enqueue a
                    // second run and violate the ReAct run-entry
                    // invariant. Direct callers can still arrive from
                    // Paused, so promote that state directly to Running.
                    if confirm_requests
                        .iter()
                        .all(|request| request.decision().is_some())
                        && let Err(e) = self
                            .set_session_status_if(
                                session_id,
                                SessionStatus::Paused,
                                SessionStatus::Running,
                            )
                            .await
                    {
                        tracing::warn!(
                            "failed to restore running session {} after all-decided confirm on resume: {}",
                            session_id,
                            e
                        );
                    }
                }
                self.run_session_resumed(
                    session_id,
                    replay,
                    run_id,
                    &description,
                    terminal_error_owner,
                )
                .await
            }
            None => {
                let restored = self.restore_pending_user_inputs(session_id).await?;
                if restored > 0 {
                    tracing::info!(
                        "run_session_from_id: recovered {} pending input(s) for fresh session {}",
                        restored,
                        session_id
                    );
                }
                self.run_session(
                    &session.id,
                    &description,
                    &context,
                    &session_prompt_history,
                    InitialUserInput {
                        attachments: &initial_attachments,
                        media_inputs: &initial_media_inputs,
                        message_id: initial_message_id.as_deref(),
                    },
                    terminal_error_owner,
                )
                .await
            }
        }
        }
        .await;
        let mut direct_guard = _direct_guard;
        direct_guard.finish().await;
        result
    }

    /// Reopen a terminal session for history viewing without dispatching it.
    ///
    /// Reopening is a resume concern, but it is deliberately not a run: the
    /// session remains `Paused` until a real follow-up or Continue request.
    /// Persistently pending user inputs are re-queued by id so a restart
    /// cannot strand an input that never reached the event log, regardless of
    /// how long the process was down.
    pub async fn reopen_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.executor.ensure_session_loaded(session_id).await?;
        let state = self.executor.get_session_status(session_id).await;
        if state == Some(SessionStatus::Completed) || state == Some(SessionStatus::Error) {
            // History viewing must not persist a terminal session as active;
            // the memory-only transition only enables a later user action in
            // this process.
            self.executor
                .update_session_status_memory_only(session_id, SessionStatus::Paused)
                .await?;
        }
        let restored = self.restore_pending_user_inputs(session_id).await?;
        if restored > 0 {
            tracing::info!(
                "reopen_session: re-queued {} pending user input(s) for session {} (staying Paused until Continue)",
                restored,
                session_id
            );
        }
        Ok(())
    }

    async fn run_session_resumed(
        &self,
        session_id: &str,
        replay: DurableEventState,
        run_id: u64,
        description: &str,
        terminal_error_owner: TerminalErrorEventOwner,
    ) -> anyhow::Result<Vec<ReActRound>> {
        let events = replay.events;
        let mut canonical =
            project_transcript_with_strategy(&events, self.react_engine.media_strategy())
                .canonical_messages;
        let start_step = infer_resume_step(&events);
        let branch_points = replay.branch_points;

        // X2: rebuild the tool/runtime shell immediately on resume. Semantic
        // memory is prefetched in the background so a slow embedding provider
        // cannot delay the first resumed model request.
        self.memory_worker
            .prefetch_prompt_memory(session_id, description);
        self.prompt_builder
            .rebuild_canonical_system_without_memory(description, &mut canonical)
            .await;

        // Phase 7 / D2 — pending-input recovery (durability ≠ RAM queues):
        //
        // RAM follow-up / steering queues are a same-process cache only.
        // Durability = DB user messages + explicit pending-input state.
        // The marker also preserves whether each message was accepted as an
        // Ask answer or ordinary follow-up; later gate changes cannot
        // reinterpret it during recovery.
        //
        // The durable event stream is the single transcript authority.
        // Pending markers cover accepted ingress that has not committed its
        // UserInject event yet, independent of downtime and timestamp.
        //
        // When the in-memory queues still hold the inputs (pause → answer in
        // the same process), the ReAct loop injects them and the DB copy
        // must NOT be re-queued — that would double-inject.
        let restored = self.restore_pending_user_inputs(session_id).await?;
        if restored > 0 {
            tracing::info!(
                "run_session_resumed: recovered {} pending input(s) for session {}",
                restored,
                session_id
            );
        }

        let emitter_arc = match self.run_emitter(terminal_error_owner) {
            Some(e) => e,
            None => {
                return Ok(project_transcript_with_strategy(
                    &events,
                    self.react_engine.media_strategy(),
                )
                .react_rounds);
            }
        };
        let actor = self.executor.actor_for(session_id).await.ok_or_else(|| {
            anyhow::anyhow!("session actor '{session_id}' disappeared before run")
        })?;
        let result = actor
            .run_react_loop(
                self.react_engine.clone(),
                ReActRunReplay {
                    events,
                    canonical,
                    branch_points,
                },
                ReActRunInput {
                    session_id: session_id.to_string(),
                    start_step,
                    emitter: emitter_arc,
                    run_id,
                },
            )
            .await?;
        // C2: soft LoopExit::Error must hit the same host failure path as
        // hard Err so dispatcher cleanup (cancel tool_runs / fail steps /
        // on_session_error) still runs.
        match result.exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => Ok(project_transcript_with_strategy(
                &result.events,
                self.react_engine.media_strategy(),
            )
            .react_rounds),
        }
    }

    /// Rebuild per-session skill/MCP registrations from the restored rounds.
    ///
    /// Registrations are runtime state, not transcript state. Resume and
    /// rollback both call this after projecting the authoritative event log so
    /// tools loaded after a branch point cannot leak into the restored run.
    pub(crate) async fn restore_per_session_tools(&self, session_id: &str, rounds: &[ReActRound]) {
        self.executor
            .unregister_session_tool_overlay(session_id)
            .await;
        for round in rounds {
            for tool in &round.tools {
                if tool.tool_call.tool_name.as_str() == "load_mcp"
                    && let Some(name) = tool.tool_call.tool_input["server_name"].as_str()
                {
                    let tool_names = load_mcp_tool_names(&tool.tool_call.tool_input);
                    let _ = self
                        .executor
                        .register_mcp_tool_overlay(session_id, name, tool_names.as_deref())
                        .await;
                } else if tool.tool_call.tool_name.as_str() == "tool_catalog"
                    && tool.tool_call.tool_input["action"].as_str() == Some("load")
                {
                    let selection = decode_builtin_tool_selection(&tool.tool_call.tool_input);
                    if selection
                        .operations
                        .as_ref()
                        .is_some_and(|values| !values.is_empty())
                        || selection
                            .roots
                            .as_ref()
                            .is_some_and(|values| !values.is_empty())
                    {
                        let _ = self
                            .executor
                            .load_builtin_operations_tool_overlay(
                                session_id,
                                selection.operations,
                                selection.roots,
                            )
                            .await;
                    }
                } else if tool.tool_call.tool_name.as_str() == "load_skill" {
                    let names = load_skill_names(&tool.tool_call.tool_input);
                    if !names.is_empty() {
                        let _ = self
                            .executor
                            .load_skill_tool_overlay(session_id, names)
                            .await;
                    }
                }
            }
        }
    }

    async fn run_session(
        &self,
        session_id: &str,
        description: &str,
        context: &str,
        session_prompt_history: &[SessionPromptMessage],
        initial: InitialUserInput<'_>,
        terminal_error_owner: TerminalErrorEventOwner,
    ) -> anyhow::Result<Vec<ReActRound>> {
        self.executor
            .register_managed_assets_for_session(session_id, initial.attachments);
        tracing::debug!(
            "run_session start: session_id={:?} context={:?} attachments={}",
            session_id,
            context,
            initial.attachments.len()
        );
        // S1: do not restate the initial user turn inside system Additional
        // context. Use its durable identity; if it is unavailable, keep the
        // history row rather than guessing from equal text.
        let history_lines: Vec<String> = session_prompt_history
            .iter()
            .filter(|m| {
                !(m.role == haven_common::types::CanonicalRole::User
                    && initial
                        .message_id
                        .is_some_and(|message_id| m.id.as_str() == message_id))
            })
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect();
        // Semantic memory recall (including embedding) is a best-effort
        // background prefetch. The first provider request must not wait for a
        // remote embedding endpoint; a later before-step MEMORY patch consumes
        // the bounded cached result when it is ready.
        self.memory_worker
            .prefetch_prompt_memory(session_id, description);
        // S2: exclude this session from past conversation excerpts. The first
        // prompt deliberately carries an empty MEMORY fence and is patched in
        // place once the prefetch completes.
        let system_prompt = self
            .prompt_builder
            .build_for_session_without_memory(description, &history_lines)
            .await;
        tracing::debug!("run_session: system_prompt {} chars", system_prompt.len());

        let mut initial_content = vec![ContentPart::text(context.to_string())];
        let media_strategy = self.react_engine.media_strategy();
        // A host-owned path is rehydrated into the attachment preview
        // during the same process, which lets the request keep the raw image
        // or audio bytes. If only the durable media projection is available
        // (for example when a row has no readable host file), use
        // its metadata-only fallback instead of reviving an inline payload.
        if initial
            .attachments
            .iter()
            .any(|attachment| !attachment.data.is_empty())
        {
            initial_content.extend(initial.attachments.iter().map(|attachment| {
                crate::react::attachment_to_content_part_with_strategy(attachment, media_strategy)
            }));
        } else {
            initial_content.extend(initial.media_inputs.iter().map(|input| {
                crate::react::media_input_to_content_part_with_strategy(input, media_strategy)
            }));
        }

        let mut initial_user = CanonicalMessage::user(initial_content);
        initial_user.id = initial.message_id.map(str::to_owned);
        let canonical: Vec<CanonicalMessage> = vec![
            CanonicalMessage::system(vec![ContentPart::text(system_prompt)]),
            initial_user,
        ];

        // Seed events so pause/resume has a durable system and initial
        // user request as a CompactSummary; later applies append. The same
        // seed is written to the durable event stream before the first model
        // call, so a crash before the first model response is still resumable.
        let events: Vec<TranscriptRecord> = seed_events_from_canonical(canonical.clone());
        self.react_engine
            .seed_transcript_events(session_id, &events, 0)
            .await?;
        let branch_points = std::collections::HashMap::new();
        let emitter_arc = match self.run_emitter(terminal_error_owner) {
            Some(e) => e,
            None => {
                return Ok(project_transcript_with_strategy(&events, media_strategy).react_rounds);
            }
        };
        let run_id = self.react_engine.next_run_id();
        let actor = self.executor.actor_for(session_id).await.ok_or_else(|| {
            anyhow::anyhow!("session actor '{session_id}' disappeared before run")
        })?;
        let result = actor
            .run_react_loop(
                self.react_engine.clone(),
                ReActRunReplay {
                    events,
                    canonical,
                    branch_points,
                },
                ReActRunInput {
                    session_id: session_id.to_string(),
                    start_step: 1,
                    emitter: emitter_arc,
                    run_id,
                },
            )
            .await?;
        match result.exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => {
                Ok(project_transcript_with_strategy(&result.events, media_strategy).react_rounds)
            }
        }
    }

    fn run_emitter(
        &self,
        terminal_error_owner: TerminalErrorEventOwner,
    ) -> Option<Arc<dyn AgentEventEmitter>> {
        let emitter = self.events.emitter_arc()?;
        match terminal_error_owner {
            TerminalErrorEventOwner::AgentEventBus => {
                Some(Arc::new(SuppressLifecycleCancelledSessionErrorEmitter {
                    inner: emitter,
                    executor: self.executor.clone(),
                }))
            }
            TerminalErrorEventOwner::SessionSupervisor => {
                Some(Arc::new(SuppressSessionErrorEmitter(emitter)))
            }
        }
    }
}

#[cfg(test)]
mod direct_session_run_guard_tests {
    use super::*;
    use crate::session::SessionSupervisor;
    use haven_common::config::ContextLimitsConfig;
    use haven_common::types::CanonicalMessage;
    use haven_llm::{LlmClient, LlmError, LlmResponse, LlmToolDefinition, StreamChunk};
    use haven_memory::Database;
    use haven_tools::ToolsFacade;
    use std::pin::Pin;
    use std::time::Duration;

    struct BlockingStreamClient {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl LlmClient for BlockingStreamClient {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown(
                "blocking test client: unexpected chat".into(),
            ))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "blocking test client: unexpected non-tool stream".into(),
            ))
        }

        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _messages: Arc<[CanonicalMessage]>,
            _tools: Arc<[LlmToolDefinition]>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.started.notify_one();
            Ok(Box::pin(futures_util::stream::pending()))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    struct NoopEmitter;

    #[async_trait::async_trait]
    impl AgentEventEmitter for NoopEmitter {
        async fn emit(&self, _event: AgentEvent) {}
    }

    #[tokio::test]
    async fn cancelled_direct_session_run_finish_retries_terminal_cleanup() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let executor = Arc::new(SessionSupervisor::new_for_test(
            db,
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = executor
            .create_session("cancelled direct run finish")
            .await
            .unwrap();
        let waiting_session = executor.create_session("waiting direct run").await.unwrap();
        let lease = executor
            .begin_direct_session_run(&session.id)
            .await
            .expect("direct run should acquire a lease");
        executor
            .update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        let actor = lease.actor.clone();
        let mut guard = DirectSessionRunGuard { lease: Some(lease) };
        let lifecycle = executor.lifecycle_guard().await;
        let mut finish = tokio::spawn(async move { guard.finish().await });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut finish)
                .await
                .is_err(),
            "direct-run finish should wait for the held lifecycle gate"
        );
        assert!(actor.is_running().await);

        finish.abort();
        assert!(finish.await.unwrap_err().is_cancelled());

        let waiting_executor = executor.clone();
        let waiting_id = waiting_session.id.clone();
        let mut waiting_admission =
            tokio::spawn(
                async move { waiting_executor.begin_direct_session_run(&waiting_id).await },
            );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut waiting_admission)
                .await
                .is_err(),
            "Drop must retain the direct-run permit through reconciliation"
        );
        assert!(actor.is_running().await);
        drop(lifecycle);

        let waiting_lease = tokio::time::timeout(Duration::from_secs(1), waiting_admission)
            .await
            .expect("the waiting session should be admitted after cleanup")
            .unwrap()
            .expect("the waiting session should acquire the released permit");
        tokio::time::timeout(Duration::from_secs(1), async {
            while executor.actor_for(&session.id).await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Drop fallback must retry cleanup after finish is cancelled");

        executor
            .update_session_status(&waiting_session.id, SessionStatus::Completed)
            .await
            .unwrap();
        let mut waiting_lease = waiting_lease;
        waiting_lease.finish().await;
    }

    #[tokio::test]
    async fn cancelled_direct_session_run_reconciliation_reserves_same_session_until_cleanup_finishes()
     {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let executor = Arc::new(SessionSupervisor::new_for_test(
            db,
            Arc::new(ToolsFacade::new()),
            2,
        ));
        let session = executor
            .create_session("direct run reconciliation reservation")
            .await
            .unwrap();
        let mut lease = executor
            .begin_direct_session_run(&session.id)
            .await
            .expect("direct run should acquire a lease");
        let actor = lease.actor.clone();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        // Queue a second lifecycle waiter behind lease.finish's initial gate.
        // Once it owns the gate, finish has cleared the Actor run bit but is
        // still inside exit reconciliation.
        let initial_gate = executor.lifecycle_guard().await;
        let (finish_started_tx, finish_started_rx) = tokio::sync::oneshot::channel();
        let finishing = tokio::spawn(async move {
            let _ = finish_started_tx.send(());
            lease.finish().await;
        });
        finish_started_rx.await.expect("finish task should start");

        let (observer_started_tx, observer_started_rx) = tokio::sync::oneshot::channel();
        let observer_executor = executor.clone();
        let observer = tokio::spawn(async move {
            let _ = observer_started_tx.send(());
            observer_executor.lifecycle_guard().await
        });
        observer_started_rx
            .await
            .expect("observer should queue behind finish");
        drop(initial_gate);
        let observer_gate = observer.await.expect("observer should acquire the gate");
        assert!(!actor.is_running().await);

        let waiting_executor = executor.clone();
        let waiting_id = session.id.clone();
        let mut waiting_admission =
            tokio::spawn(
                async move { waiting_executor.begin_direct_session_run(&waiting_id).await },
            );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut waiting_admission)
                .await
                .is_err(),
            "same-session admission should wait for the held reconciliation gate"
        );

        finishing.abort();
        assert!(finishing.await.unwrap_err().is_cancelled());
        drop(observer_gate);

        let attempted_admission = tokio::time::timeout(Duration::from_secs(1), waiting_admission)
            .await
            .expect("same-session admission should leave the lifecycle gate")
            .expect("admission task should complete");
        let admission_rejected = match attempted_admission {
            None => true,
            Some(mut unexpected_lease) => {
                unexpected_lease.finish().await;
                false
            }
        };
        assert!(
            admission_rejected,
            "a new run must not enter while the cancelled old lease reconciles"
        );

        let released_lease = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(lease) = executor.begin_direct_session_run(&session.id).await {
                    break lease;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fallback cleanup should release same-session admission");
        let mut released_lease = released_lease;
        released_lease.finish().await;
    }

    #[tokio::test]
    async fn dropping_direct_session_run_guard_cancels_actor_owned_react_loop_before_releasing_permit()
     {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let executor = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = executor
            .create_session("cancel actor-owned ReAct loop")
            .await
            .unwrap();
        let waiting_session = executor
            .create_session("wait for cancelled ReAct loop")
            .await
            .unwrap();
        let lease = executor
            .begin_direct_session_run(&session.id)
            .await
            .expect("direct run should acquire a lease");
        let actor = lease.actor.clone();
        let loop_actor = actor.clone();

        let started = Arc::new(tokio::sync::Notify::new());
        let client = Arc::new(BlockingStreamClient {
            started: started.clone(),
        });
        let router = Arc::new(haven_llm::LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let engine = Arc::new(crate::react::ReActEngine::new(
            router,
            crate::react::test_tool_catalog_port(&executor),
            executor.clone(),
            haven_memory::MemoryStore::new(db),
            4,
            ContextLimitsConfig::default(),
        ));

        let session_id = session.id.clone();
        let react_task = tokio::spawn(async move {
            let _guard = DirectSessionRunGuard { lease: Some(lease) };
            loop_actor
                .run_react_loop(
                    engine,
                    ReActRunReplay {
                        events: Vec::new(),
                        canonical: vec![CanonicalMessage::user_text("keep the model call open")],
                        branch_points: Default::default(),
                    },
                    ReActRunInput {
                        session_id,
                        start_step: 1,
                        emitter: Arc::new(NoopEmitter),
                        run_id: 1,
                    },
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .expect("actor-owned loop should start the blocking provider stream");
        let waiting_executor = executor.clone();
        let waiting_id = waiting_session.id.clone();
        let mut waiting_admission =
            tokio::spawn(
                async move { waiting_executor.begin_direct_session_run(&waiting_id).await },
            );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut waiting_admission)
                .await
                .is_err(),
            "the active direct run must retain its permit while the actor polls ReAct"
        );

        react_task.abort();
        let error = match react_task.await {
            Ok(_) => panic!("cancelled ReAct caller unexpectedly completed"),
            Err(error) => error,
        };
        assert!(error.is_cancelled());

        let waiting_lease = tokio::time::timeout(Duration::from_secs(2), waiting_admission)
            .await
            .expect("cancelling the caller should stop the actor loop and release admission")
            .unwrap()
            .expect("the waiting direct run should acquire the released permit");
        assert!(
            !actor.is_running().await,
            "cancelled actor loop cleanup must clear the actor run bit"
        );
        assert_eq!(
            executor.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused),
            "cancelled direct runs should return to Paused"
        );

        executor
            .update_session_status(&waiting_session.id, SessionStatus::Completed)
            .await
            .unwrap();
        let mut waiting_lease = waiting_lease;
        waiting_lease.finish().await;
    }
}
