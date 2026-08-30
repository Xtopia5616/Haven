//! Session start / resume drivers for [`AgentLayer`]: fresh ReAct runs,
//! snapshot restore, and the snapshot-less tool-chain projector fallback.
//!
//! Split out of `layer.rs` so the facade stays focused on wiring; these
//! methods operate on the same private fields via `impl AgentLayer` blocks.
//!
//! ## Resume authority (Phase 7 / B4 + D2; Phase 8 / B1)
//!
//! - **Snapshot present and valid** → single authority. [`run_session_resumed`]
//!   restores `events` (canonical + rounds are projected); RAM queues are a
//!   cache only.
//! - **Snapshot missing (`react_state` row absent)** → best-effort fresh run
//!   that projects tool-call/result pairs via
//!   [`project_tool_chain_from_steps`] (same projector shape as would appear
//!   in a snapshot). Call ids prefer a real `messages.tool_call_id` when one
//!   matches the observation; otherwise `resumed_{step_id}`.
//! - **Snapshot corrupt / unparsable** → **hard-fail** with a user-visible
//!   error. Never silently fall through to the projector (that would fork
//!   semantics: synthetic ids, no awaiting_answer/confirm, different
//!   canonical shape).
//!
//! ## Queue durability (Phase 7 / D2)
//!
//! RAM follow-up / steering queues are a same-process cache. Durability is
//! DB messages + snapshot `saved_at` + undelivered scan. Resume re-queues by
//! `message_id` and is idempotent (duplicate id is skipped).

use crate::AgentLayer;
use crate::resume_support::{
    load_mcp_tool_names, merge_recovery_candidates, project_tool_chain_from_steps,
};
use crate::rollback_support::trim_dangling_tool_call;

use crate::session::SessionStatus;
use crate::types::{
    BranchPoint, ReActRound, ReActSnapshot, TranscriptRecord, project_transcript,
    seed_events_from_canonical,
};
use haven_common::types::{CanonicalMessage, ContentPart};
use std::collections::HashMap;

/// A recent conversation message (role, content) used by the FRESH-run /
/// snapshot-less path. **S1 authority:** canonical is the LLM truth; this
/// window may feed Additional context only for turns not already represented
/// as the first canonical user message. Resume does not use this type: the
/// snapshot is the single authority and post-snapshot inputs are recovered by
/// timestamp, not by content comparison.
#[derive(Debug, Clone)]
pub(crate) struct ConversationMessage {
    role: String,
    content: String,
}

impl AgentLayer {
    /// Load the most recent conversation messages for a session as (role,
    /// content) pairs, for the FRESH-run system-prompt path
    /// (`prompt_builder.build`). Resume does not consume this: the restored
    /// events snapshot is the single authority, and post-snapshot inputs
    /// are recovered by timestamp in `run_session_resumed`.
    fn load_conversation_history(&self, session_id: &str) -> Vec<ConversationMessage> {
        self.db
            .get_session_messages_limit(session_id, self.conversation_window_size)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|m| ConversationMessage {
                role: m.role,
                content: m.content,
            })
            .collect()
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
        if self.executor.get_session_state(session_id).await == Some(SessionStatus::Pending) {
            let _ = self
                .executor
                .update_session_status(session_id, SessionStatus::Running)
                .await;
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
        let conv_history = self.load_conversation_history(session_id);

        // Multimodal: carry the first user message's image attachments into
        // the initial canonical user message so the model sees them from the
        // first turn (they were persisted by process_input_with_attachments).
        // The FIRST user message is always the session's own input; later image
        // follow-ups are supplements (injected by the ReAct loop at step
        // start) and must NOT be attached to the initial turn or they would
        // be duplicated.
        let initial_attachments = match self.db.get_session_messages(session_id) {
            Ok(msgs) => msgs
                .into_iter()
                .find(|m| m.role == "user")
                .filter(|m| !m.attachments.is_empty())
                .map(|m| m.attachments)
                .unwrap_or_default(),
            Err(e) => {
                tracing::warn!(
                    "continue_session {}: get_session_messages failed, attachments not restored: {}",
                    session_id,
                    e
                );
                Vec::new()
            }
        };

        let result = match self.db.get_react_state(session_id) {
            Ok(Some(state_json)) => match ReActSnapshot::from_json(&state_json) {
                Ok(mut snapshot) => {
                    tracing::info!(
                        "restoring ReAct state for session {} ({} events)",
                        session_id,
                        snapshot.events.len()
                    );
                    // Re-register per-session tools (skills/MCP) from projected
                    // rounds, since in-memory registrations are lost on restart.
                    let (_, rounds) = snapshot.project();
                    self.restore_per_session_tools(session_id, &rounds).await;
                    // Phase 4 / C5+F2: restore the explicit ask gate from the
                    // snapshot. Upgrade legacy "paused" status BEFORE publishing
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
                                "failed to upgrade session {} to paused_awaiting_answer on resume: {}",
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
                                    "failed to upgrade session {} to paused_awaiting_confirm on resume: {}",
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
                    // NOT silently fall through to the snapshot-less projector
                    // (synthetic call ids, no gate restore). Hard-fail so the
                    // user sees the loss instead of a forked resume.
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
            Ok(None) => {
                // No snapshot row: best-effort fresh run +
                // `project_tool_chain_from_steps` (documented Phase 7 / B4).
                self.run_session(
                    &session.id,
                    &description,
                    &context,
                    &conv_history,
                    &initial_attachments,
                )
                .await
            }
            Err(e) => {
                // Phase 7 / B4 review: IO failure must not soft-fall through to
                // the snapshot-less projector (forked synthetic ids / lost
                // ask-confirm). Align with corrupt-body hard-fail; missing row
                // (`Ok(None)`) remains the only best-effort projector path.
                tracing::error!(
                    "failed to read react_state for session {} ({}); refusing resume",
                    session_id,
                    e
                );
                Err(anyhow::anyhow!(
                    "session '{}' snapshot is unreadable ({}); cannot resume — retry or start a new session",
                    session_id,
                    e
                ))
            }
        };

        // Generate title after the ReAct loop if not already set. Only
        // spawned when the run itself succeeded: a failed run (e.g. all LLM
        // endpoints down) would burn the full title retry budget on the same
        // dead endpoint and duplicate the conversation's own retry latency.
        // A resumed session whose title was never generated gets its title
        // attempt on the next successful run instead.
        if session.title.is_none() && result.is_ok() {
            let db = self.db.clone();
            let executor = self.executor.clone();
            let title = self.title.clone();
            let events = self.events.clone();
            let in_flight = self.title_in_flight.clone();
            let tid = session_id.to_string();
            tokio::spawn(async move {
                Self::try_generate_title(db, executor, title, events, in_flight, tid).await;
            });
        }

        result
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
            .run_blocking(move |db| {
                db.get_undelivered_user_messages_since(&sid, Some(since.as_str()))
            })
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
            if let Err(error) = self
                .executor
                .add_supplement_with_attachments(
                    session_id,
                    &message.content,
                    &message.attachments,
                    Some(message.id.clone()),
                )
                .await
            {
                tracing::warn!(
                    "reopen_session: failed to re-queue input {} for session {}: {}",
                    message.id,
                    session_id,
                    error
                );
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
        let mut events = snapshot.events;
        let (mut canonical, _) = project_transcript(&events);
        let start_step = snapshot.step_number;
        let mut branch_points = snapshot.branch_points;

        // X2 / G7: full system rebuild on resume (short index + MEMORY +
        // session). Pause-path infer writes the DB; this rebuild makes facts
        // and any newly installed skills/MCP visible on the next run. Mid-run
        // load_skill still only updates API tools[] (freeze-per-run).
        self.rebuild_canonical_system(session_id, description, &mut canonical)
            .await;

        // Phase 7 / D2 — post-snapshot recovery (durability ≠ RAM queues):
        //
        // RAM follow-up / steering queues are a same-process cache only.
        // Durability = DB user messages + snapshot `saved_at` + undelivered
        // (anchor-less) scan. Replay is idempotent by `message_id`
        // (`push_follow_up` / steering skip duplicates).
        //
        // By TIMESTAMP instead of content matching: any message persisted
        // after `saved_at` cannot be in the restored events, so it is
        // unambiguously new — supplements, steering and `ask` answers that
        // arrived while paused, or were persisted before a crash and lost
        // from the in-memory queues. The events snapshot is the single
        // authority for everything older.
        //
        // This alone misses inputs that PREDATE the snapshot yet were never
        // injected: a steering/supplement queued after the loop's last
        // per-step drain is not in the events, but the error/exit
        // snapshot written afterwards carries a `saved_at` NEWER than the
        // input's persisted row. Those rows carry no step anchor (see
        // `push_user_context`), so they are recovered by the undelivered
        // scan below regardless of timestamp.
        //
        // When the in-memory queues still hold the inputs (pause → answer in
        // the same process), the ReAct loop injects them and the DB copy
        // must NOT be re-queued — that would double-inject.
        if let Some(saved_at) = snapshot.saved_at.as_deref()
            && !self.executor.has_pending_context(session_id).await
        {
            let pending = self
                .db
                .get_session_messages_since(session_id, saved_at)
                .unwrap_or_default();
            // Bound the anchor-less scan to the recovery window so ancient
            // false positives (legacy missing anchors) are never re-injected
            // on first post-upgrade resume. Rows newer than `saved_at` are
            // already covered by `pending` above.
            let since = haven_memory::repositories::messages::undelivered_recovery_since();
            let undelivered = self
                .db
                .get_undelivered_user_messages_since(session_id, Some(since.as_str()))
                .unwrap_or_default();
            let mut restored = 0usize;
            for msg in merge_recovery_candidates(pending, undelivered) {
                let is_answer = self.executor.is_ask_gated(session_id).await;
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
                }
            }
            if restored > 0 {
                tracing::info!(
                    "run_session_resumed: recovered {} post-snapshot input(s) for session {} (saved_at {})",
                    restored,
                    session_id,
                    saved_at
                );
            }
        }

        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(project_transcript(&events).1),
        };
        let exit = self
            .react_engine
            .run_react_loop(
                session_id,
                &mut canonical,
                &mut events,
                start_step,
                &mut branch_points,
                emitter_arc,
                run_id,
            )
            .await?;
        // C2: soft LoopExit::Error must hit the same host failure path as
        // hard Err so dispatcher cleanup (cancel actions / fail steps /
        // on_session_error) still runs.
        match exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => Ok(project_transcript(&events).1),
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
                match tool.action.tool_name.as_str() {
                    "load_skill" => {
                        if let Some(name) = tool.action.tool_input["skill_name"].as_str() {
                            tools.register_skill_for_session(session_id, name).await;
                        }
                    }
                    "load_mcp" => {
                        if let Some(name) = tool.action.tool_input["server_name"].as_str() {
                            let tool_names = load_mcp_tool_names(&tool.action.tool_input);
                            tools
                                .register_mcp_for_session(session_id, name, tool_names.as_deref())
                                .await;
                        }
                    }
                    _ => {}
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
        initial_attachments: &[haven_common::types::MessageAttachment],
    ) -> anyhow::Result<Vec<ReActRound>> {
        tracing::debug!(
            "run_session start: session_id={:?} context={:?} attachments={}",
            session_id,
            context,
            initial_attachments.len()
        );
        // S1: do not restate the *first* user turn (already canonical[1])
        // inside system Additional context. Later turns that happen to equal
        // `context` (user repeating the same text) must stay — snapshot-less
        // resume has no other channel for them.
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
        initial_content.extend(
            initial_attachments
                .iter()
                .map(crate::react::attachment_to_content_part),
        );

        let mut canonical: Vec<CanonicalMessage> = vec![
            CanonicalMessage::system(vec![ContentPart::text(system_prompt)]),
            CanonicalMessage::user(initial_content),
        ];

        // Snapshot-less path only (react_state missing): project tool-call /
        // result pairs from session_steps via the shared B4 projector.
        // Corrupt snapshots hard-fail in `run_session_from_id` and never
        // reach here.
        project_tool_chain_from_steps(self.db.as_ref(), session_id, &mut canonical);

        // Seed events so pause/resume snapshots carry system+user (+ any
        // projected tool chain) as a CompactSummary; later applies append.
        let mut events: Vec<TranscriptRecord> = seed_events_from_canonical(canonical.clone());
        let mut branch_points: HashMap<u32, BranchPoint> = HashMap::new();
        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(project_transcript(&events).1),
        };
        let run_id = self.react_engine.next_run_id();
        let exit = self
            .react_engine
            .run_react_loop(
                session_id,
                &mut canonical,
                &mut events,
                1,
                &mut branch_points,
                emitter_arc,
                run_id,
            )
            .await?;
        match exit {
            crate::react::LoopExit::Error(msg) => Err(anyhow::anyhow!(msg)),
            crate::react::LoopExit::Paused { .. }
            | crate::react::LoopExit::Cancelled
            | crate::react::LoopExit::Completed => Ok(project_transcript(&events).1),
        }
    }
}
