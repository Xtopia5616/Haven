//! Pending-context injection: steering / follow_up / answer / action_results
//! and cross-session inbox polling.
//!
//! Split from `react.rs` (Phase 1 mechanical extract; behavior unchanged).

use super::*;
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
            let prefix = if follow_up.is_answer {
                cleared_ask = true;
                "Answer to your previous question"
            } else {
                "Additional context from user"
            };
            self.push_user_context(
                ctx,
                canonical,
                prefix,
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
            let prefix = if s.is_answer {
                cleared_ask = true;
                "Answer to your previous question"
            } else {
                "Steering"
            };
            self.push_user_context(
                ctx,
                canonical,
                prefix,
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

        // Deliver completed background-action results as context. These are
        // kept separate from the user queues so action output is never
        // mistaken for a user reply. The payload text is self-labelling
        // (`[Background action result] ... action_id ...`) and is pushed as a
        // User-role message because a mid-conversation System message is
        // rejected by some providers and a Tool message would need a
        // preceding assistant tool_call (see `is_dangling_boundary`).
        for s in &action_results {
            canonical.push(CanonicalMessage::user_text(s));
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
            "Cross-session message",
            text.trim_end(),
            &[],
            None,
        )
        .await;
    }

    /// Emit a Supplement event, persist a matching thought-step row and push
    /// a user message into the canonical array. Shared by the supplement and
    /// steering queues (identical mechanics, different text prefixes) so the
    /// two paths cannot drift. The thought-step row anchors the user message
    /// to a step after a reload: the row is created under the message's own
    /// id (`message_id`, persisted at submit time) so review/rollback can
    /// resolve the step by id; without it an interrupted input would have no
    /// determinable step. The step row stores no text — the user message row
    /// is the single content authority.
    pub(super) async fn push_user_context(
        &self,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        prefix: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<&str>,
    ) {
        ctx.emitter
            .emit(crate::event::AgentEvent::Supplement {
                session_id: ctx.session_id.clone(),
                additional_context: text.to_string(),
                step_number: ctx.step_num,
                run_id: ctx.run_id,
            })
            .await;
        let step_id = message_id
            .map(String::from)
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        let _ = self
            .db
            .run_blocking({
                let session_id = ctx.session_id.clone();
                let step_id = step_id.clone();
                let step_num = ctx.step_num;
                move |db| {
                    if let Err(e) = db.create_thought_step(&session_id, step_num as i32, &step_id) {
                        tracing::warn!(
                            "create_thought_step failed (session={} step={}): {}",
                            session_id,
                            step_num,
                            e
                        );
                    }
                    Ok::<(), anyhow::Error>(())
                }
            })
            .await;
        let mut content = vec![ContentPart::text(format!("{prefix}: {text}"))];
        content.extend(attachments.iter().map(attachment_to_content_part));
        // No content-based dedup here: duplicate submissions are prevented at
        // the UI layer (the submit path is in-flight locked), and the DB rows
        // carry unique ids that anchor each input to its own step row. The
        // canonical is an append-only transcript of what the user actually
        // sent — collapsing identical inputs would silently drop legitimate
        // repeated turns (e.g. the user saying "继续" twice on purpose).
        canonical.push(CanonicalMessage::user(content));
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

}
