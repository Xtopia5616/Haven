//! Session start / resume drivers for [`AgentLayer`]: fresh ReAct runs,
//! snapshot restore and fresh-session startup.
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
//! - **Event stream present with a legacy/corrupt JSON cache** → ignore the
//!   cache and continue from durable events.
//!
//! ## Queue durability (Phase 7 / D2)
//!
//! RAM follow-up / steering queues are a same-process cache. Durability is
//! DB messages + checkpoint ingress cursor + undelivered scan. Resume
//! re-queues by `message_id` and is idempotent (duplicate id is skipped); the
//! durable event sequence, rather than snapshot contents, decides transcript
//! recovery.

use crate::AgentLayer;
use crate::react::DurableEventState;
use crate::react::{RunInput, RunReplay};
use crate::resume_support::{
    builtin_selection, infer_resume_step, load_mcp_tool_names, load_skill_names,
    merge_recovery_candidates,
};

use crate::session::SessionStatus;
use crate::types::{
    ReActRound, TranscriptRecord, project_transcript_with_strategy, seed_events_from_canonical,
};
use haven_common::media::MediaInput;
use haven_common::types::{CanonicalMessage, ContentPart};

/// A recent conversation message (role, content) used by the fresh-session
/// system-prompt path. **S1 authority:** canonical is the LLM truth; this
/// window may feed Additional context only for turns not already represented
/// as the first canonical user message. Resume does not use this type: the
/// durable event stream is the authority and post-checkpoint inputs are
/// recovered by ingress sequence, not by content comparison.
#[derive(Debug, Clone)]
pub(crate) struct ConversationMessage {
    role: String,
    content: String,
}

pub(crate) struct InitialUserInput<'a> {
    attachments: &'a [haven_common::types::MessageAttachment],
    media_inputs: &'a [MediaInput],
    message_id: Option<&'a str>,
}

impl AgentLayer {
    /// Load the most recent conversation messages for a session as (role,
    /// content) pairs, for the FRESH-run system-prompt path
    /// (`prompt_builder.build`). Resume does not consume this: the restored
    /// event stream is the authority, and post-checkpoint inputs are recovered
    /// by ingress sequence in `run_session_resumed`.
    async fn load_conversation_history(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<ConversationMessage>> {
        let db = self.db.clone();
        let sid = session_id.to_string();
        let limit = self.conversation_window_size;
        db.run_blocking(move |db| {
            Ok(db
                .get_session_messages_limit(&sid, limit)?
                .into_iter()
                .map(|m| ConversationMessage {
                    role: m.role,
                    content: m.content,
                })
                .collect())
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to load conversation history: {error}"))
    }

    /// Dispatcher entrypoint. Looks up the session by id, fills in the
    /// description and original transcript (context),
    /// loads conversation history, then runs the ReAct loop.
    pub async fn run_session_from_id(&self, session_id: &str) -> anyhow::Result<Vec<ReActRound>> {
        tracing::debug!("run_session_from_id: session_id={}", session_id);
        let session =
            self.executor.get_session(session_id).await.ok_or_else(|| {
                anyhow::anyhow!("session '{}' not found by dispatcher", session_id)
            })?;

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

        // R6: direct callers (tests / continue without claim) register the same
        // run slot the dispatcher would, so rollback can cancel+join before
        // restore. When the dispatcher already claimed, this is a no-op and
        // `unmark_running` owns the slot. Released via `DirectRunGuard` on every
        // exit path (including `?` / early return).
        let direct_lease = self.executor.begin_direct_run(session_id).await;
        struct DirectRunGuard {
            executor: std::sync::Arc<crate::session::SessionSupervisor>,
            lease: Option<crate::session::DirectRunLease>,
            session_id: String,
        }
        impl Drop for DirectRunGuard {
            fn drop(&mut self) {
                let Some(lease) = self.lease.take() else {
                    return;
                };
                lease.actor.release_run_now();
                let exec = self.executor.clone();
                let sid = self.session_id.clone();
                tokio::spawn(async move {
                    exec.end_direct_run(&sid).await;
                });
            }
        }
        let _direct_guard = DirectRunGuard {
            executor: self.executor.clone(),
            lease: direct_lease,
            session_id: session_id.to_string(),
        };

        let run_id = self.react_engine.next_run_id();

        let description = if session.summary.is_empty() {
            session.input.clone()
        } else {
            session.summary.clone()
        };
        let context = session.input.clone();

        // Conversation history and message persistence are keyed by the session
        // itself — there is no separate session indirection anymore.
        let conv_history = self.load_conversation_history(session_id).await?;

        // Multimodal: carry the first user message's image attachments into
        // the initial canonical user message so the model sees them from the
        // first turn (they were persisted by process_input_with_attachments).
        // The FIRST user message is always the session's own input; later image
        // follow-ups are supplements (injected by the ReAct loop at step
        // start) and must NOT be attached to the initial turn or they would
        // be duplicated.
        let db = self.db.clone();
        let sid = session_id.to_string();
        let (initial_message_id, initial_attachments, initial_media_inputs, all_attachments) = db
            .run_blocking(move |db| {
                let messages = db.get_session_messages(&sid)?;
                let all_attachments = messages
                    .iter()
                    .flat_map(|message| message.attachments.iter().cloned())
                    .collect::<Vec<_>>();
                let initial_message = messages.iter().find(|m| m.role == "user").cloned();
                let initial_message_id = initial_message.as_ref().map(|message| message.id.clone());
                let initial_attachments = initial_message
                    .as_ref()
                    .filter(|m| !m.attachments.is_empty())
                    .map(|m| m.attachments.clone())
                    .unwrap_or_default();
                let initial_media_inputs = initial_message
                    .filter(|m| !m.media_inputs.is_empty())
                    .map(|m| m.media_inputs)
                    .unwrap_or_default();
                Ok((
                    initial_message_id,
                    initial_attachments,
                    initial_media_inputs,
                    all_attachments,
                ))
            })
            .await
            .map_err(|error| anyhow::anyhow!("failed to load session resume data: {error}"))?;

        // Snapshot events carry metadata-only media inputs. Re-register the
        // host-owned files from the materialized media projection before a
        // resumed request can ask the `files` tool to resolve them.
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
                let (_, rounds) = project_transcript_with_strategy(
                    &replay.events,
                    self.react_engine.media_strategy(),
                );
                self.restore_per_session_tools(session_id, &rounds).await;
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
                self.run_session_resumed(session_id, replay, run_id, &description)
                    .await
            }
            None => {
                self.run_session(
                    &session.id,
                    &description,
                    &context,
                    &conv_history,
                    InitialUserInput {
                        attachments: &initial_attachments,
                        media_inputs: &initial_media_inputs,
                        message_id: initial_message_id.as_deref(),
                    },
                )
                .await
            }
        }
    }

    /// Reopen a terminal session for history viewing without dispatching it.
    ///
    /// Reopening is a resume concern, but it is deliberately not a run: the
    /// session remains `Paused` until a real follow-up or Continue request.
    /// Recent user rows without a session-step anchor are re-queued by id so a
    /// restart cannot strand an input that never reached the event log.
    pub async fn reopen_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.executor.ensure_session_loaded(session_id).await?;
        let state = self.executor.get_session_status(session_id).await;
        let mut answer_pending = self
            .executor
            .has_pending_interaction(session_id, crate::interaction::InteractionKind::Ask)
            .await;
        if state == Some(SessionStatus::Completed) || state == Some(SessionStatus::Error) {
            // History viewing must not persist a terminal session as active;
            // the memory-only transition only enables a later user action in
            // this process.
            self.executor
                .update_session_status_memory_only(session_id, SessionStatus::Paused)
                .await?;
        }
        // A live queue is authoritative for the current process. Scanning the
        // DB while it still owns inputs would enqueue a second copy.
        if self.executor.has_pending_context(session_id).await {
            return Ok(());
        }
        let undelivered = self
            .react_engine
            .event_store
            .recent_unanchored_user_messages(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("failed to scan pending inputs: {e}"))?;
        if undelivered.is_empty() {
            return Ok(());
        }
        tracing::info!(
            "reopen_session: re-queueing {} recent undelivered user input(s) for session {} (staying Paused until Continue)",
            undelivered.len(),
            session_id
        );
        for message in undelivered {
            let result = if answer_pending {
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
            if let Err(error) = result {
                tracing::warn!(
                    "reopen_session: failed to re-queue input {} for session {}: {}",
                    message.id,
                    session_id,
                    error
                );
            } else if answer_pending {
                // Only the first recovered message answers the outstanding
                // question; later messages are ordinary follow-ups.
                answer_pending = false;
            }
        }
        Ok(())
    }

    async fn run_session_resumed(
        &self,
        session_id: &str,
        replay: DurableEventState,
        run_id: u64,
        description: &str,
    ) -> anyhow::Result<Vec<ReActRound>> {
        let events = replay.events;
        let (mut canonical, _) =
            project_transcript_with_strategy(&events, self.react_engine.media_strategy());
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

        // Phase 7 / D2 — post-checkpoint recovery (durability ≠ RAM queues):
        //
        // RAM follow-up / steering queues are a same-process cache only.
        // Durability = DB user messages + the SessionStore ingress cursor + undelivered
        // (anchor-less) scan. Replay is idempotent by `message_id`
        // (`push_follow_up` / steering skip duplicates).
        //
        // By ingress sequence instead of timestamps or content matching: any
        // message persisted after the ingress cursor cannot be in the
        // restored events, even when the wall clock moves backwards or two
        // writes share a millisecond. The durable event stream is the single
        // authority for the transcript; the checkpoint only supplies the
        // ingress cursor for messages that have not been applied yet.
        //
        // This alone misses inputs that PREDATE the checkpoint yet were never
        // injected: a steering/supplement queued after the loop's last
        // per-step drain is not in the events. Those rows carry no step
        // anchor (see `push_user_context`), so they are recovered by the
        // undelivered scan below.
        //
        // When the in-memory queues still hold the inputs (pause → answer in
        // the same process), the ReAct loop injects them and the DB copy
        // must NOT be re-queued — that would double-inject.
        if !self.executor.has_pending_context(session_id).await {
            let ingress_cursor = replay.cursor.message_ingress_seq;
            let store = &self.react_engine.event_store;
            let pending = store
                .messages_after_ingress_cursor(session_id, ingress_cursor)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "failed to recover post-checkpoint inputs for session {session_id}: {error}"
                    )
                })?;
            let undelivered = store
                .recent_unanchored_user_messages(session_id)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "failed to recover post-checkpoint inputs for session {session_id}: {error}"
                    )
                })?;
            let mut restored = 0usize;
            let mut answer_pending = self.executor.is_ask_gated(session_id).await;
            for msg in merge_recovery_candidates(pending, undelivered) {
                self.executor
                    .register_managed_assets_for_session(session_id, &msg.attachments);
                let is_answer = answer_pending;
                let queued = if is_answer {
                    self.executor
                        .add_answer_with_attachments(
                            session_id,
                            &msg.content,
                            &msg.attachments,
                            Some(msg.id.clone()),
                        )
                        .await
                } else {
                    self.executor
                        .add_follow_up_with_attachments(
                            session_id,
                            &msg.content,
                            &msg.attachments,
                            Some(msg.id.clone()),
                        )
                        .await
                };
                if queued.is_ok() {
                    restored += 1;
                    if is_answer {
                        answer_pending = false;
                    }
                }
            }
            if restored > 0 {
                tracing::info!(
                    "run_session_resumed: recovered {} post-checkpoint input(s) for session {} (ingress_seq {})",
                    restored,
                    session_id,
                    ingress_cursor
                );
            }
        }

        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => {
                return Ok(project_transcript_with_strategy(
                    &events,
                    self.react_engine.media_strategy(),
                )
                .1);
            }
        };
        let actor = self.executor.actor_for(session_id).await.ok_or_else(|| {
            anyhow::anyhow!("session actor '{session_id}' disappeared before run")
        })?;
        let result = actor
            .run_react_loop(
                self.react_engine.clone(),
                RunReplay {
                    events,
                    canonical,
                    branch_points,
                },
                RunInput {
                    session_id: session_id.to_string(),
                    start_step,
                    emitter: emitter_arc,
                    run_id,
                },
            )
            .await?;
        // C2: soft LoopExit::Error must hit the same host failure path as
        // hard Err so dispatcher cleanup (cancel actions / fail steps /
        // on_session_error) still runs.
        match result.exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => Ok(project_transcript_with_strategy(
                &result.events,
                self.react_engine.media_strategy(),
            )
            .1),
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
                if tool.action.tool_name.as_str() == "load_mcp"
                    && let Some(name) = tool.action.tool_input["server_name"].as_str()
                {
                    let tool_names = load_mcp_tool_names(&tool.action.tool_input);
                    let _ = self
                        .executor
                        .register_mcp_tool_overlay(session_id, name, tool_names.as_deref())
                        .await;
                } else if tool.action.tool_name.as_str() == "tool_catalog"
                    && tool.action.tool_input["action"].as_str() == Some("load")
                {
                    let (operations, roots) = builtin_selection(&tool.action.tool_input);
                    if operations.as_ref().is_some_and(|values| !values.is_empty())
                        || roots.as_ref().is_some_and(|values| !values.is_empty())
                    {
                        let _ = self
                            .executor
                            .load_builtin_operations_tool_overlay(session_id, operations, roots)
                            .await;
                    }
                } else if tool.action.tool_name.as_str() == "load_skill" {
                    let names = load_skill_names(&tool.action.tool_input);
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

    pub(crate) async fn run_session(
        &self,
        session_id: &str,
        description: &str,
        context: &str,
        conversation_history: &[ConversationMessage],
        initial: InitialUserInput<'_>,
    ) -> anyhow::Result<Vec<ReActRound>> {
        self.executor
            .register_managed_assets_for_session(session_id, initial.attachments);
        tracing::debug!(
            "run_session start: session_id={:?} context={:?} attachments={}",
            session_id,
            context,
            initial.attachments.len()
        );
        // S1: do not restate the *first* user turn (already canonical[1])
        // inside system Additional context. Later turns that happen to equal
        // `context` (user repeating the same text) must stay in the fresh-run
        // context.
        let mut skipped_first_user = false;
        let history_lines: Vec<String> = conversation_history
            .iter()
            .filter(|m| {
                if !skipped_first_user && m.role == "user" && m.content == context {
                    skipped_first_user = true;
                    return false;
                }
                true
            })
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect();
        // Semantic memory recall (including embedding) is a best-effort
        // background prefetch. The first provider request must not wait for a
        // remote embedding endpoint; a later before-step MEMORY patch consumes
        // the bounded cached result when it is ready.
        self.memory_worker
            .prefetch_prompt_memory(session_id, description);
        // S2: exclude this session from Past conversation excerpts. The first
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
        // call, so a crash before the first snapshot is still resumable.
        let events: Vec<TranscriptRecord> = seed_events_from_canonical(canonical.clone());
        self.react_engine
            .seed_transcript_events(session_id, &events, 0)
            .await?;
        let branch_points = std::collections::HashMap::new();
        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(project_transcript_with_strategy(&events, media_strategy).1),
        };
        let run_id = self.react_engine.next_run_id();
        let actor = self.executor.actor_for(session_id).await.ok_or_else(|| {
            anyhow::anyhow!("session actor '{session_id}' disappeared before run")
        })?;
        let result = actor
            .run_react_loop(
                self.react_engine.clone(),
                RunReplay {
                    events,
                    canonical,
                    branch_points,
                },
                RunInput {
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
                Ok(project_transcript_with_strategy(&result.events, media_strategy).1)
            }
        }
    }
}
