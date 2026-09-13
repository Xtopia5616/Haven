//! Session start / resume drivers for [`AgentLayer`]: fresh ReAct runs,
//! snapshot restore and fresh-session startup.
//!
//! Split out of `layer.rs` so the facade stays focused on wiring; these
//! methods operate on the same private fields via `impl AgentLayer` blocks.
//!
//! ## Resume authority (Phase 7 / B4 + D2; Phase 8 / B1)
//!
//! - **Snapshot present and valid** → single authority. [`run_session_resumed`]
//!   restores `events` (canonical + rounds are projected); RAM queues are a
//!   cache only.
//! - **Snapshot missing (`react_state` row absent)** → fresh-session startup.
//! - **Snapshot corrupt / unparsable** → **hard-fail** with a user-visible
//!   error. A different transcript reconstruction path is never attempted.
//!
//! ## Queue durability (Phase 7 / D2)
//!
//! RAM follow-up / steering queues are a same-process cache. Durability is
//! DB messages + snapshot ingress cursor + undelivered scan. Resume re-queues by
//! `message_id` and is idempotent (duplicate id is skipped).

use crate::AgentLayer;
use crate::react::{ReActState, RunInput};
use crate::resume_support::{
    load_mcp_tool_names, merge_recovery_candidates, reconcile_dangling_tool_call,
};
use crate::rollback_support::trim_dangling_tool_call;

use crate::session::SessionStatus;
use crate::types::{
    BranchPoint, ReActRound, ReActSnapshot, TranscriptRecord, project_transcript_with_strategy,
    seed_events_from_canonical,
};
use haven_common::media::MediaInput;
use haven_common::types::{CanonicalMessage, ContentPart};
use std::collections::HashMap;

/// A recent conversation message (role, content) used by the fresh-session
/// system-prompt path. **S1 authority:** canonical is the LLM truth; this
/// window may feed Additional context only for turns not already represented
/// as the first canonical user message. Resume does not use this type: the
/// snapshot is the single authority and post-snapshot inputs are recovered by
/// ingress sequence, not by content comparison.
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
    /// events snapshot is the single authority, and post-snapshot inputs
    /// are recovered by timestamp in `run_session_resumed`.
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
        if self.executor.get_session_state(session_id).await == Some(SessionStatus::Pending)
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
        if self.executor.get_session_state(session_id).await == Some(SessionStatus::Running) {
            self.events
                .emit_session_updated(session_id, "running")
                .await;
        }

        // R6: direct callers (tests / continue without claim) register the same
        // run slot the dispatcher would, so rollback can cancel+join before
        // restore. When the dispatcher already claimed, this is a no-op and
        // `unmark_running` owns the slot. Released via `DirectRunGuard` on every
        // exit path (including `?` / early return).
        let owns_direct_slot = self.executor.begin_direct_run(session_id).await;
        struct DirectRunGuard {
            executor: std::sync::Arc<crate::session::SessionExecutor>,
            session_id: String,
            owns: bool,
        }
        impl Drop for DirectRunGuard {
            fn drop(&mut self) {
                if !self.owns {
                    return;
                }
                let exec = self.executor.clone();
                let sid = self.session_id.clone();
                tokio::spawn(async move {
                    exec.end_direct_run(&sid).await;
                });
            }
        }
        let _direct_guard = DirectRunGuard {
            executor: self.executor.clone(),
            session_id: session_id.to_string(),
            owns: owns_direct_slot,
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
        let (
            initial_message_id,
            initial_attachments,
            initial_media_inputs,
            all_attachments,
            react_state,
        ) = db
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
                let react_state = db.get_react_state(&sid)?;
                Ok((
                    initial_message_id,
                    initial_attachments,
                    initial_media_inputs,
                    all_attachments,
                    react_state,
                ))
            })
            .await
            .map_err(|error| anyhow::anyhow!("failed to load session resume data: {error}"))?;

        // Snapshot events carry metadata-only media inputs. Re-register the
        // host-owned files from the materialized media projection before a
        // resumed request can ask the `files` tool to resolve them.
        self.executor
            .get_tools()
            .register_managed_assets_for_session(session_id, &all_attachments);

        match react_state {
            Some(state_json) => match ReActSnapshot::from_json(&state_json) {
                Ok(mut snapshot) => {
                    tracing::info!(
                        "restoring ReAct state for session {} ({} events)",
                        session_id,
                        snapshot.events.len()
                    );
                    // The snapshot and materialized projections are written at
                    // different boundaries. If the process died after an
                    // action step was persisted but before the next snapshot,
                    // the restored event log can end at ToolCall while the DB
                    // already knows the result. Reconcile that edge before
                    // any dangling-call trim; replaying the call would mint a
                    // new step id and could repeat an external side effect.
                    let db = self.db.clone();
                    let sid = session_id.to_string();
                    let (durable_steps, checkpoint, projection_cursor) = db
                        .run_blocking(move |db| {
                            Ok((
                                db.get_session_steps(&sid)?,
                                db.get_react_checkpoint(&sid)?,
                                db.get_react_projection_cursor(&sid)?,
                            ))
                        })
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!(
                                "failed to load durable action steps for session {session_id}: {error}"
                            )
                        })?;
                    if let Some(checkpoint) = checkpoint {
                        if checkpoint.event_cursor != snapshot.events.len() as i64 {
                            tracing::warn!(
                                session_id,
                                snapshot_events = snapshot.events.len(),
                                checkpoint_events = checkpoint.event_cursor,
                                revision = checkpoint.revision,
                                "snapshot event cursor differs from its durable checkpoint"
                            );
                        }
                        let projection_behind = projection_cursor.0
                            < checkpoint.message_ingress_seq
                            || projection_cursor.1 < checkpoint.step_seq;
                        if projection_behind {
                            return Err(anyhow::anyhow!(
                                "session '{}' materialized projection is behind snapshot revision {}; refusing resume",
                                session_id,
                                checkpoint.revision
                            ));
                        }
                        if projection_cursor.0 > checkpoint.message_ingress_seq
                            || projection_cursor.1 > checkpoint.step_seq
                        {
                            tracing::warn!(
                                session_id,
                                revision = checkpoint.revision,
                                saved_message_ingress_seq = checkpoint.message_ingress_seq,
                                current_message_ingress_seq = projection_cursor.0,
                                saved_step_seq = checkpoint.step_seq,
                                current_step_seq = projection_cursor.1,
                                "materialized projection is ahead of snapshot; attempting reconciliation"
                            );
                        }
                    }
                    if reconcile_dangling_tool_call(&mut snapshot.events, &durable_steps) {
                        for branch in snapshot.branch_points.values_mut() {
                            branch.event_cursor = branch.event_cursor.min(snapshot.events.len());
                        }
                        let repaired_json = serde_json::to_string(&snapshot)?;
                        let db = self.db.clone();
                        let sid = session_id.to_string();
                        db.run_blocking(move |db| db.save_react_state(&sid, &repaired_json))
                            .await
                            .map_err(|error| {
                                anyhow::anyhow!(
                                    "failed to persist reconciled snapshot for session {session_id}: {error}"
                                )
                            })?;
                        tracing::warn!(
                            "reconciled a dangling tool call from durable action steps before resuming session {}",
                            session_id
                        );
                    }
                    // Re-register per-session tools (skills/MCP) from projected
                    // rounds, since in-memory registrations are lost on restart.
                    let (_, rounds) = snapshot.project();
                    self.restore_per_session_tools(session_id, &rounds).await;
                    // Phase 4 / C5+F2: restore the explicit ask gate from the
                    // snapshot. Upgrade a plain paused status BEFORE publishing
                    // the flag so auto-wake cannot race on plain Paused.
                    if let Some(pending) = snapshot.awaiting_answer.clone() {
                        if matches!(
                            self.executor.get_session_state(session_id).await,
                            Some(SessionStatus::Paused)
                        ) && let Err(e) = self
                            .executor
                            .update_session_status(session_id, SessionStatus::PausedAwaitingAnswer)
                            .await
                        {
                            tracing::warn!(
                                "failed to restore session {} as paused_awaiting_answer on resume: {}",
                                session_id,
                                e
                            );
                        }
                        self.executor
                            .set_awaiting_answer(session_id, Some(pending))
                            .await;
                    }
                    // Phase 5 / E3: restore confirm gate. Prefer in-memory
                    // (same-process decisions already recorded) over the
                    // snapshot so resolve_confirmation is not wiped.
                    if self
                        .executor
                        .get_awaiting_confirm(session_id)
                        .await
                        .is_none()
                        && let Some(pending) = snapshot.awaiting_confirm.clone()
                    {
                        // Decisions already recorded but wake to Pending
                        // never landed (crash between persist and status):
                        // finish the confirm gate instead of restoring a
                        // permanently stuck PausedAwaitingConfirm.
                        if pending.all_decided() {
                            self.executor
                                .set_awaiting_confirm(session_id, Some(pending))
                                .await;
                            if let Err(e) = self
                                .set_session_status(session_id, SessionStatus::Pending)
                                .await
                            {
                                tracing::warn!(
                                    "failed to wake session {} after all-decided confirm on resume: {}",
                                    session_id,
                                    e
                                );
                            }
                        } else {
                            if matches!(
                                self.executor.get_session_state(session_id).await,
                                Some(SessionStatus::Paused)
                            ) && let Err(e) = self
                                .executor
                                .update_session_status(
                                    session_id,
                                    SessionStatus::PausedAwaitingConfirm,
                                )
                                .await
                            {
                                tracing::warn!(
                                    "failed to restore session {} as paused_awaiting_confirm on resume: {}",
                                    session_id,
                                    e
                                );
                            }
                            self.executor
                                .set_awaiting_confirm(session_id, Some(pending))
                                .await;
                        }
                    }
                    // Skip trim when a confirm batch still needs results —
                    // sanitize would mark gated tools Interrupted before
                    // finish_confirm_batch can append real observations.
                    let has_confirm = self
                        .executor
                        .get_awaiting_confirm(session_id)
                        .await
                        .is_some();
                    if !has_confirm {
                        trim_dangling_tool_call(&mut snapshot.events);
                        let event_len = snapshot.events.len();
                        for bp in snapshot.branch_points.values_mut() {
                            if bp.event_cursor > event_len {
                                bp.event_cursor = event_len;
                            }
                        }
                    }
                    self.run_session_resumed(session_id, snapshot, run_id, &description)
                        .await
                }
                Err(e) => {
                    // Phase 7 / B4: corrupt or schema-drifted react_state must
                    // hard-fail so the user sees the loss instead of a forked
                    // resume.
                    tracing::error!(
                        "react_state for session {} failed to parse ({}); refusing resume",
                        session_id,
                        e
                    );
                    Err(anyhow::anyhow!(
                        "session '{}' snapshot is corrupt or incompatible ({}); cannot resume — start a new session or clear react_state",
                        session_id,
                        e
                    ))
                }
            },
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
        let state = self.executor.get_session_state(session_id).await;
        let mut answer_pending = state == Some(SessionStatus::PausedAwaitingAnswer);
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
        let db = self.db.clone();
        let sid = session_id.to_string();
        let since = haven_memory::repositories::messages::undelivered_recovery_since();
        let undelivered = db
            .run_blocking(move |db| db.get_undelivered_user_messages_since(&sid, since.as_str()))
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

    /// X2 / G7 (freeze-per-run): fully rebuild `canonical[0]` on resume
    /// (tools/skills/MCP short index + MEMORY + session). Mid-run memory
    /// refresh stays fence-only via hooks / M2.
    async fn rebuild_canonical_system(
        &self,
        session_id: &str,
        description: &str,
        canonical: &mut [CanonicalMessage],
    ) {
        self.prompt_builder
            .rebuild_canonical_system(session_id, description, canonical)
            .await;
    }

    async fn run_session_resumed(
        &self,
        session_id: &str,
        snapshot: ReActSnapshot,
        run_id: u64,
        description: &str,
    ) -> anyhow::Result<Vec<ReActRound>> {
        let events = snapshot.events;
        let (mut canonical, _) =
            project_transcript_with_strategy(&events, self.react_engine.media_strategy());
        let start_step = snapshot.step_number;
        let branch_points = snapshot.branch_points;

        // X2: full system rebuild on resume (tool index + MEMORY + session).
        // Pause-path infer writes the DB; this rebuild makes facts and any
        // newly discovered skills/MCP visible on the next run.
        self.rebuild_canonical_system(session_id, description, &mut canonical)
            .await;

        // Phase 7 / D2 — post-snapshot recovery (durability ≠ RAM queues):
        //
        // RAM follow-up / steering queues are a same-process cache only.
        // Durability = DB user messages + snapshot ingress cursor + undelivered
        // (anchor-less) scan. Replay is idempotent by `message_id`
        // (`push_follow_up` / steering skip duplicates).
        //
        // By ingress sequence instead of timestamps or content matching: any
        // message persisted after the snapshot cursor cannot be in the
        // restored events, even when the wall clock moves backwards or two
        // writes share a millisecond. The events snapshot is the single
        // authority for everything older.
        //
        // This alone misses inputs that PREDATE the snapshot yet were never
        // injected: a steering/supplement queued after the loop's last
        // per-step drain is not in the events. Those rows carry no step
        // anchor (see `push_user_context`), so they are recovered by the
        // undelivered scan below.
        //
        // When the in-memory queues still hold the inputs (pause → answer in
        // the same process), the ReAct loop injects them and the DB copy
        // must NOT be re-queued — that would double-inject.
        if !self.executor.has_pending_context(session_id).await {
            let ingress_cursor = snapshot.last_ingress_seq;
            let since = haven_memory::repositories::messages::undelivered_recovery_since();
            let db = self.db.clone();
            let sid = session_id.to_string();
            let (pending, undelivered) = db
                .run_blocking(move |db| {
                    let pending =
                        db.get_session_messages_since_ingress_seq(&sid, ingress_cursor)?;
                    let undelivered =
                        db.get_undelivered_user_messages_since(&sid, since.as_str())?;
                    Ok((pending, undelivered))
                })
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "failed to recover post-snapshot inputs for session {session_id}: {error}"
                    )
                })?;
            let mut restored = 0usize;
            let mut answer_pending = self.executor.is_ask_gated(session_id).await;
            for msg in merge_recovery_candidates(pending, undelivered) {
                self.executor
                    .get_tools()
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
                    "run_session_resumed: recovered {} post-snapshot input(s) for session {} (ingress_seq {})",
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
        let mut state = ReActState::new(events, canonical, branch_points);
        let exit = self
            .react_engine
            .run_react_loop(RunInput {
                session_id,
                state: &mut state,
                start_step,
                emitter: emitter_arc,
                run_id,
            })
            .await?;
        // C2: soft LoopExit::Error must hit the same host failure path as
        // hard Err so dispatcher cleanup (cancel actions / fail steps /
        // on_session_error) still runs.
        match exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => Ok(project_transcript_with_strategy(
                &state.events,
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
        let tools = self.executor.get_tools();
        tools.unregister_session(session_id).await;
        for round in rounds {
            for tool in &round.tools {
                if tool.action.tool_name.as_str() == "load_mcp"
                    && let Some(name) = tool.action.tool_input["server_name"].as_str()
                {
                    let tool_names = load_mcp_tool_names(&tool.action.tool_input);
                    tools
                        .register_mcp_for_session(session_id, name, tool_names.as_deref())
                        .await;
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
            .get_tools()
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
        // S2: exclude this session from Past conversation excerpts.
        let system_prompt = self
            .prompt_builder
            .build_for_session(description, &history_lines, Some(session_id))
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

        // Seed events so pause/resume snapshots carry the system and initial
        // user request as a CompactSummary; later applies append.
        let events: Vec<TranscriptRecord> = seed_events_from_canonical(canonical.clone());
        let branch_points: HashMap<u32, BranchPoint> = HashMap::new();
        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(project_transcript_with_strategy(&events, media_strategy).1),
        };
        let mut state = ReActState::new(events, canonical, branch_points);
        let run_id = self.react_engine.next_run_id();
        let exit = self
            .react_engine
            .run_react_loop(RunInput {
                session_id,
                state: &mut state,
                start_step: 1,
                emitter: emitter_arc,
                run_id,
            })
            .await?;
        match exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => {
                Ok(project_transcript_with_strategy(&state.events, media_strategy).1)
            }
        }
    }
}
