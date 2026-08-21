//! Pending-context injection: steering / follow_up / answer / action_results
//! and cross-session inbox polling.
//!
//! Split from `react.rs` (Phase 1 mechanical extract). Phase 6 / B3: inject
//! origin is structured [`InjectSource`]; prefixes render via
//! `InjectSource::render_prefix`. Phase 6 / H1: user injects go through
//! [`ReActEngine::apply_transcript`].

use super::*;
use haven_common::types::InjectSource;
use haven_tools::inbox::{InboxBus, MessageType};
use tokio::sync::watch;

/// State for the automatic cross-session inbox check, one per engine (shared
/// across sessions — each session's mailbox is keyed by its own id).
pub(super) struct MessagingState {
    /// Shared file bus (default root, process-wide notifier).
    bus: InboxBus,
    /// Delivery notifications: `changed()` fires when any mailbox got a
    /// message, so sessions react immediately instead of only polling.
    rx: watch::Receiver<u64>,
    /// Steps since the last actual inbox drain (fallback cadence for
    /// missed notifications, e.g. a different process wrote the mailbox).
    steps_since_poll: u32,
    /// Session title cache for the registry heartbeat (read once from the
    /// DB; titles change rarely).
    title_cache: HashMap<String, Option<String>>,
}

impl MessagingState {
    pub(super) fn new() -> Self {
        let bus = InboxBus::default_root();
        let rx = bus.subscribe();
        Self {
            bus,
            rx,
            steps_since_poll: 0,
            title_cache: HashMap::new(),
        }
    }
}

/// Fallback interval (in ReAct steps) for the automatic cross-session inbox
/// check. Delivery notifications drive the check in-process (immediate), and
/// this cadence only catches missed notifications (e.g. another process
/// wrote to the mailbox).
const MESSAGING_POLL_EVERY_STEPS: u32 = 3;

/// Per-message text cap when injecting cross-session messages into the
/// model context (defensive: a full message is at most 16 KiB, but a burst
/// must not flood the observation budget).
const MESSAGING_INJECT_CHARS: usize = 400;

impl ReActEngine {
    /// Drain user-facing context into the canonical message list: follow-ups
    /// (paused-session replies / ask answers), steering (mid-run user
    /// interjections) and completed background-action results (system inject).
    /// Each becomes a `User` message so the agent sees it on the next LLM call.
    ///
    /// Returns `true` when at least one message was injected. Called at the
    /// top of every step, and again right before a step completes with final
    /// content —a message that arrived while the LLM call was in flight is
    /// delivered there instead of being deferred until the turn ends.
    pub(super) async fn inject_pending_context(
        &self,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
    ) -> bool {
        let mut injected = false;
        let mut cleared_ask = false;

        // One combined drain pass instead of three separate queue reads:
        // the ses-map lock is taken once per step instead of three times.
        let (follow_ups, steering, action_results) =
            self.executor.drain_pending_context(&ctx.session_id).await;
        for follow_up in &follow_ups {
            // A reply to a pending `ask` is injected as a paired answer so
            // the model sees the old question as resolved instead of treating
            // it as a second open question to answer again.
            let source = if follow_up.is_answer {
                cleared_ask = true;
                InjectSource::Answer
            } else {
                InjectSource::FollowUp
            };
            self.push_user_context(
                ctx,
                canonical,
                source,
                &follow_up.text,
                &follow_up.attachments,
                follow_up.message_id.as_deref(),
            )
            .await;
            injected = true;
        }

        for s in &steering {
            // Mid-run steering marked as answer at the ask-pause boundary
            // (C3) uses the Answer prefix too — no queue transfer required.
            let source = if s.is_answer {
                cleared_ask = true;
                InjectSource::Answer
            } else {
                InjectSource::Steering
            };
            self.push_user_context(
                ctx,
                canonical,
                source,
                &s.text,
                &s.attachments,
                s.message_id.as_deref(),
            )
            .await;
            injected = true;
        }

        if cleared_ask {
            self.executor
                .clear_awaiting_answer_persisted(&ctx.session_id)
                .await;
        }

        // Deliver completed background-action results as context. Kept
        // separate from user queues so action output is never mistaken for a
        // user reply. Payload is self-labelled (`[Background action result]…`);
        // InjectSource is set for structured origin without Supplement/DB
        // side effects (see UserInject ActionResult arm).
        for s in &action_results {
            self.push_user_context(ctx, canonical, InjectSource::ActionResult, s, &[], None)
                .await;
            injected = true;
        }

        injected
    }

    /// Cross-session messaging integration, run at the top of every ReAct
    /// step (after `inject_pending_context`, before the LLM call):
    ///
    /// 1. **Heartbeat** — re-register this session (`last_seen = now`) with
    ///    its DB title, every step, so long-thinking sessions stay `online`
    ///    and `agents_list`/the UI can show what a session is about.
    /// 2. **Automatic inbox check** — drain the mailbox when an in-process
    ///    delivery notification arrived (push, immediate) or every
    ///    [`MESSAGING_POLL_EVERY_STEPS`] steps (fallback for cross-process
    ///    writers). Each message is injected as low-trust user context for
    ///    the next LLM call — no reliance on the agent remembering to poll.
    /// 3. **Receipts** — freshly read messages are auto-acked so senders
    ///    learn their message was consumed.
    pub(super) async fn maybe_poll_inbox(
        &self,
        session_id: &str,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
    ) {
        // Session title for the registry (read once from the DB, then cached
        // per session; never hold the engine mutex across an await).
        let cached_title = {
            let st = self.messaging.lock().unwrap();
            st.title_cache.get(session_id).cloned()
        };
        let title = match cached_title {
            Some(t) => t,
            None => {
                let t = self
                    .db
                    .run_blocking({
                        let sid = session_id.to_string();
                        move |db| {
                            let title = db.get_session(&sid).ok().flatten().and_then(|s| s.title);
                            Ok::<Option<String>, anyhow::Error>(title)
                        }
                    })
                    .await
                    .unwrap_or(None);
                self.messaging
                    .lock()
                    .unwrap()
                    .title_cache
                    .insert(session_id.to_string(), t.clone());
                t
            }
        };

        let (bus, due) = {
            let mut st = self.messaging.lock().unwrap();
            let bus = st.bus.clone();
            st.steps_since_poll += 1;
            let notified = st.rx.has_changed().unwrap_or(false);
            if notified {
                let _ = st.rx.borrow_and_update();
            }
            let due = notified || st.steps_since_poll >= MESSAGING_POLL_EVERY_STEPS;
            if due {
                st.steps_since_poll = 0;
            }
            (bus, due)
        };

        // Heartbeat on the blocking pool, every step regardless of polling.
        let sid = session_id.to_string();
        let hb_sid = sid.clone();
        let hb_title = title.clone();
        let hb_bus = bus.clone();
        tokio::task::spawn_blocking(move || {
            let _ = hb_bus.register_with_title(&hb_sid, &[], hb_title.as_deref());
        })
        .await
        .ok();

        if !due {
            return;
        }

        let poll_sid = sid.clone();
        let messages = match tokio::task::spawn_blocking(move || {
            let read = bus.read_and_archive(&poll_sid)?;
            let _receipts = bus.send_receipts(&poll_sid, &read);
            Ok::<_, anyhow::Error>(read)
        })
        .await
        {
            Ok(Ok(msgs)) => msgs,
            Ok(Err(e)) => {
                tracing::debug!("messaging inbox poll failed for {session_id}: {e}");
                return;
            }
            Err(e) => {
                tracing::debug!("messaging inbox poll join failed: {e}");
                return;
            }
        };
        if messages.is_empty() {
            return;
        }

        let mut text = String::new();
        for env in &messages {
            let body: String = env.text.chars().take(MESSAGING_INJECT_CHARS).collect();
            match env.r#type {
                MessageType::Receipt => {
                    let of = env.in_reply_to.as_deref().unwrap_or("<unknown>");
                    text.push_str(&format!(
                        "[Read receipt] {} read your message {of}\n",
                        env.from
                    ));
                }
                _ => {
                    text.push_str(&format!(
                        "[Cross-session message from {} ({})]: {body}\n",
                        env.from, env.r#type
                    ));
                }
            }
        }
        self.push_user_context(
            ctx,
            canonical,
            InjectSource::CrossSession,
            text.trim_end(),
            &[],
            None,
        )
        .await;
    }

    /// Emit + persist + project a user inject via [`TranscriptEvent::UserInject`]
    /// (Phase 6 / H1). Shared by follow-up / steering / cross-session so the
    /// paths cannot drift. No content-based dedup — see AGENTS.md resume rules.
    pub(super) async fn push_user_context(
        &self,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        source: InjectSource,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<&str>,
    ) {
        // history is unused for UserInject; pass a scratch vec.
        let mut history = Vec::new();
        self.apply_transcript(
            ctx,
            TranscriptEvent::UserInject {
                source,
                text: text.to_string(),
                attachments: attachments.to_vec(),
                message_id: message_id.map(str::to_string),
            },
            canonical,
            &mut history,
        )
        .await;
    }

    /// Shared tail of the two "final answer" branches when a user message or
    /// background-action result arrived while the LLM was generating: persist
    /// the finished answer, insert it BEFORE the injected messages (so the
    /// re-run's LLM call sees the completed answer followed by the
    /// interjection, instead of answering blind and duplicating the bubble),
    /// and keep a rollback target for the interrupted final step.
    #[allow(clippy::too_many_arguments)] // consolidates two near-identical final branches
    pub(super) async fn deliver_final_with_pending_context(
        &self,
        ctx: &StepCtx,
        final_text: &str,
        reasoning: Option<String>,
        canonical: &mut Vec<CanonicalMessage>,
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        before_inject_len: usize,
        already_pushed: bool,
    ) {
        let message_id = self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        self.persist_session_message(
            &ctx.session_id,
            "assistant",
            final_text,
            Some("text"),
            None,
            Some(&message_id),
        )
        .await;
        if !already_pushed {
            canonical.insert(
                before_inject_len,
                CanonicalMessage::assistant(
                    vec![ContentPart::text(final_text.to_string())],
                    None,
                    reasoning,
                    Vec::new(),
                    Vec::new(),
                ),
            );
        }
        self.save_branch_point(
            &ctx.session_id,
            canonical,
            history,
            ctx.step_num,
            branch_points,
            false,
        )
        .await;
    }

    /// Phase 7 / C6: shared turn-end for empty-actions and explicit
    /// `final_answer`. Both paths enter the same inject / canonical-push /
    /// `pause_turn` / `PauseReason::TurnEnd` implementation.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_turn_end(
        &self,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        final_text: &str,
        reasoning: Option<String>,
        thinking_blocks: Vec<serde_json::Value>,
        already_pushed: bool,
    ) -> anyhow::Result<TurnEndOutcome> {
        let before_inject_len = canonical.len();
        if self.inject_pending_context(ctx, canonical).await {
            self.deliver_final_with_pending_context(
                ctx,
                final_text,
                reasoning,
                canonical,
                history,
                branch_points,
                before_inject_len,
                already_pushed,
            )
            .await;
            return Ok(TurnEndOutcome::Continue);
        }
        // Mirror the finished answer into the canonical before the pause so
        // the snapshot carries the complete conversation in order.
        if !already_pushed {
            canonical.push(CanonicalMessage::assistant(
                vec![ContentPart::text(final_text.to_string())],
                None,
                if thinking_blocks.is_empty() {
                    reasoning
                } else {
                    None
                },
                Vec::new(),
                thinking_blocks,
            ));
        }
        let persist_message_id =
            self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        self.pause_turn(
            &ctx.session_id,
            canonical,
            history,
            ctx.step_num + 1,
            branch_points,
            &ctx.emitter,
            SessionStatus::Paused,
            final_text,
            Some(ctx.step_num),
            Some(&persist_message_id),
            false,
        )
        .await?;
        Ok(TurnEndOutcome::Done(LoopExit::Paused {
            reason: PauseReason::TurnEnd,
        }))
    }
}

/// Phase 7 / C6: outcome of the shared turn-end helper.
#[derive(Debug)]
pub(crate) enum TurnEndOutcome {
    /// Pending context injected mid-final; loop should continue.
    Continue,
    /// Turn paused (`PauseReason::TurnEnd`).
    Done(LoopExit),
}
