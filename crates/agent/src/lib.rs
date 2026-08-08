use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod compactor;
mod event;
mod inference;
mod prompt;
mod react;
mod title;
mod types;

pub use compactor::ContextCompactor;
pub use event::{AgentEvent, AgentEventEmitter, EventBus, EventDispatcher};
pub use haven_task::{RunHandler, TaskExecutor, TaskInfo, TaskStatus};
pub use inference::InferenceEngine;
pub use prompt::SystemPromptBuilder;
pub use react::ReActEngine;
pub use types::{Action, BranchPoint, ProcessResult, ReActSnapshot, ReActStep};

use haven_common::config::ContextLimitsConfig;
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::LlmRouter;
use haven_memory::Database;
use haven_memory::repositories::messages::{Message, MessageAttachment};
use haven_tools::ReminderMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::title::TitleGenerator;

/// The single persistence entry point for chat messages: insert a message
/// into a task's message stream, dropping any checkpointed partial stream
/// text first (a real message supersedes it). Both user turns (AgentLayer)
/// and assistant turns (ReActEngine) go through this one implementation so
/// the two paths cannot drift apart.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_task_message(
    db: &Arc<Database>,
    task_id: &str,
    role: &str,
    content: &str,
    message_type: Option<&str>,
    attachments: &[MessageAttachment],
    voice: bool,
) -> anyhow::Result<Message> {
    let task_id = task_id.to_string();
    let role = role.to_string();
    let content = content.to_string();
    let message_type = message_type.map(String::from);
    let attachments = attachments.to_vec();
    let task_dup = task_id.clone();
    let _ = db
        .run_blocking(move |db| db.delete_partial_message(&task_dup))
        .await;
    db.run_blocking(move |db| {
        db.add_message_full(
            &task_id,
            &role,
            &content,
            message_type.as_deref(),
            None,
            &attachments,
            voice,
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

/// Repair a canonical message array so it is acceptable to tool-calling LLM
/// APIs: every `tool` message must be the response to a preceding assistant
/// message that declared `tool_calls`, and a trailing assistant message that
/// declares `tool_calls` must be followed by its results. Both violations are
/// rejected with a 400 by providers.
///
/// True when a single `CanonicalMessage` is part of a dangling boundary
/// that must not start a suffix ??either a `Tool` result or an `Assistant`
/// message that declared `tool_calls`. Providers reject the former when its
/// declaration is missing above it and the latter when its results are
/// missing below it, so both forms need to slide past (in
/// `ContextCompactor::safe_end_idx`) or get dropped (in
/// `sanitize_canonical`).
pub(crate) fn is_dangling_boundary(msg: &CanonicalMessage) -> bool {
    msg.role == CanonicalRole::Tool
        || (msg.role == CanonicalRole::Assistant && msg.tool_calls.is_some())
}

/// The ReAct loop only ever builds valid arrays, but snapshots/compaction
/// output can be corrupted by an interruption: a compaction split between an
/// assistant tool_call message and its tool results (the assistant is
/// summarized away while the results survive), or an app exit right after the
/// assistant message was appended. This drops the orphaned `tool` messages
/// and trims the dangling trailing assistant message so the loop re-requests
/// the tool call cleanly.
pub(crate) fn sanitize_canonical(canonical: &mut Vec<CanonicalMessage>) {
    let mut preceded_by_calls = false;
    canonical.retain(|m| match m.role {
        CanonicalRole::Tool => {
            let keep = preceded_by_calls;
            if !keep {
                tracing::warn!(
                    "dropping orphaned tool message (tool_call_id={:?}) with no preceding assistant tool_calls",
                    m.tool_call_id
                );
            }
            keep
        }
        CanonicalRole::Assistant => {
            preceded_by_calls = m.tool_calls.is_some();
            true
        }
        _ => {
            preceded_by_calls = false;
            true
        }
    });
    if canonical.last().is_some_and(is_dangling_boundary)
        && canonical
            .last()
            .is_some_and(|m| m.role == CanonicalRole::Assistant)
    {
        canonical.pop();
    }
}

pub struct AgentLayer {
    db: Arc<Database>,
    executor: Arc<TaskExecutor>,
    conversation_window_size: usize,
    context_limits: ContextLimitsConfig,
    events: Arc<EventDispatcher>,
    prompt_builder: Arc<SystemPromptBuilder>,
    react_engine: Arc<ReActEngine>,
    inference: Arc<InferenceEngine>,
    title: Option<TitleGenerator>,
}

/// A recent conversation message used to re-seed context when resuming a
/// task. Kept as (role, content) pairs so the resume path can deduplicate
/// against the restored canonical instead of blindly duplicating every turn.
#[derive(Debug, Clone)]
struct ConversationMessage {
    role: String,
    content: String,
}

impl AgentLayer {
    pub fn new(
        db: Arc<Database>,
        executor: Arc<TaskExecutor>,
        router: Arc<LlmRouter>,
        max_steps: u32,
        conversation_window_size: usize,
        context_limits: ContextLimitsConfig,
    ) -> Self {
        let events = Arc::new(EventDispatcher::new());
        let prompt_builder = Arc::new(SystemPromptBuilder::new(executor.get_tools(), db.clone()));
        let react_engine = Arc::new(ReActEngine::new(
            router.clone(),
            executor.clone(),
            db.clone(),
            max_steps,
            context_limits.clone(),
        ));
        let inference = Arc::new(InferenceEngine::new(
            db.clone(),
            router.clone(),
            context_limits.max_transcript_chars,
            context_limits.embedding_chunk_size,
            context_limits.max_known_facts,
            context_limits.sanitize_field_max_chars,
        ));
        let _ = db.set_preference("name", "Xtopia");
        // Idempotent: repeated startup seeding must not pile up duplicates
        // (historically one `name=Xtopia` fact was inserted per launch).
        let _ = db.ensure_fact("user", "name", "Xtopia", "user", 1.0, &["identity"]);
        // Title generator is always available: it routes through the shared
        // LlmRouter, which uses EndpointRole::SmallModel. If the small_model
        // endpoint isn't configured the router will simply surface the error
        // and `generate` returns None.
        let title = Some(TitleGenerator::new(router));

        Self {
            db,
            executor,
            conversation_window_size,
            context_limits,
            events,
            prompt_builder,
            react_engine,
            inference,
            title,
        }
    }

    /// Persist a message into the task's message stream (conversation history).
    /// Returns the persisted message so callers can roll it back precisely
    /// (e.g. when the task turns out to be terminal right after).
    async fn persist_message_parts(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        attachments: &[haven_memory::repositories::messages::MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<haven_memory::repositories::messages::Message> {
        persist_task_message(
            &self.db,
            task_id,
            role,
            content,
            message_type,
            attachments,
            voice,
        )
        .await
    }

    /// Update a task's status in the executor and notify the frontend.
    /// The status string always comes from `TaskStatus::as_str()` so the
    /// persisted value and the emitted event cannot drift.
    async fn set_task_status(&self, task_id: &str, status: TaskStatus) -> anyhow::Result<()> {
        let status_str = status.as_str().to_string();
        self.executor.update_task_status(task_id, status).await?;
        self.events.emit_task_updated(task_id, &status_str).await;
        Ok(())
    }

    /// Reopen a terminal task to Paused state.
    /// Used by the history review flow ??shows the task as active on the chat
    /// page.  The dispatcher won't pick it up until the user sends a
    /// follow-up message (which calls supplement_task ??Paused鈫扨ending).
    pub async fn reopen_task(&self, task_id: &str) -> anyhow::Result<()> {
        // Terminal tasks (Error/Completed) are removed from the in-memory
        // list by unmark_running, so ensure_task_loaded is needed to bring
        // them back before we can update their status.
        self.executor.ensure_task_loaded(task_id).await?;
        let state = self.executor.get_task_state(task_id).await;
        if state == TaskStatus::Completed || state == TaskStatus::Error {
            self.set_task_status(task_id, TaskStatus::Paused).await?;
        }
        Ok(())
    }

    pub fn set_emitter(&self, emitter: Arc<dyn AgentEventEmitter>) {
        self.events.set_emitter(emitter);
    }

    /// Install an `EventBus` as the active emitter and return it so callers
    /// can register multiple subscribers via `subscribe`.
    pub fn install_event_bus(&self) -> Arc<EventBus> {
        self.events.install_bus()
    }

    pub fn replace_router(&self, new_router: Arc<LlmRouter>) {
        // Pre-warm the new router's HTTP connection pool so the next request
        // doesn't pay TCP+TLS handshake latency after a provider switch.
        let warm = new_router.clone();
        tokio::spawn(async move {
            match warm
                .health_check(haven_llm::EndpointRole::DefaultModel)
                .await
            {
                Ok(()) => tracing::info!("LLM connection pre-warmed after router swap"),
                Err(e) => tracing::warn!("LLM pre-warm after swap failed: {}", e),
            }
        });
        self.react_engine.replace_router(new_router);
    }

    /// Run the full memory maintenance pass (fact dedup, sensitive purge,
    /// low-confidence flush, embedding pruning). Exposed for the app-level
    /// scheduler so decay/cleanup runs on a timer, not only post-inference.
    pub async fn run_memory_maintenance(&self) {
        self.inference.run_memory_maintenance().await;
    }

    /// Retrieve memory items (facts or episodes) most relevant to `query`.
    pub async fn recall_memory(
        &self,
        query: &str,
        kind: &str,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        self.inference.recall_memory(query, kind, limit).await
    }

    pub fn set_max_steps(&self, max_steps: u32) {
        self.react_engine.set_max_steps(max_steps);
    }

    /// Live connectivity probe to the default-model endpoint (GET /models).
    /// Used by the top-right status indicator to show Ready/Disconnected.
    pub async fn check_llm_connection(&self) -> bool {
        self.react_engine.check_connection().await
    }

    /// Spawn the TaskExecutor dispatcher with a runner wired to this
    /// AgentLayer. Must be called exactly once after construction.
    pub fn start(self: Arc<Self>) {
        let agent = self.clone();
        let executor = self.executor.clone();
        let handler: RunHandler = Arc::new(move |task_id: String| {
            let agent = agent.clone();
            Box::pin(async move { agent.run_task_from_id(&task_id).await.map(|_| ()) })
        });
        executor.start_dispatcher(handler);

        // Spawn a consumer for background-job completions. When a job
        // finishes, inject the result into the owning task's context at the
        // next ReAct step (via the job-completions buffer) and, if the task was
        // Paused for scheduling reasons, wake it to Pending so the dispatcher
        // resumes and the model processes the result no manual `status`
        // polling required.
        //
        // A task Paused because the `ask` tool is awaiting a human reply is
        // NOT woken: resuming it would let the agent continue (and run tools)
        // based on subprocess output before the user has answered. The result
        // is still buffered and delivered as context once the user resumes.
        let agent = self.clone();
        let tools = self.executor.get_tools();
        if let Some(mut rx) = tools.background_jobs.take_completion_receiver() {
            tokio::spawn(async move {
                while let Some(comp) = rx.recv().await {
                    // Skip cancellations: a cancelled job was killed
                    // intentionally (end_task/rollback), so notifying would
                    // risk resurrecting an ended task.
                    if comp.status == "cancelled" {
                        continue;
                    }
                    let Some(tid) = comp.task_id else {
                        continue;
                    };
                    // Only completed/failed carry a useful payload.
                    let payload = match comp.status_json.get("output").and_then(|v| v.as_str()) {
                        Some(o) => o.to_string(),
                        None => comp
                            .status_json
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    };
                    let msg = format!(
                        "Background job {} {}.\nOutput:\n{}",
                        comp.job_id, comp.status, payload
                    );
                    agent.executor.add_job_completion(&tid, &msg).await;
                    let state = agent.executor.get_task_state(&tid).await;
                    let awaiting = agent.executor.is_awaiting_answer(&tid).await;
                    if state == TaskStatus::Paused && !awaiting {
                        if let Err(e) = agent
                            .executor
                            .update_task_status(&tid, TaskStatus::Pending)
                            .await
                        {
                            tracing::warn!("job-completion wake task {} failed: {}", tid, e);
                            continue;
                        }
                        agent.events.emit_task_updated(&tid, "pending").await;
                    }
                }
            });
        }
        // Spawn a consumer for fired reminders: the fire behavior is chosen
        // by the reminder's mode.
        // - `notify`: surface it as a Notification event (in-app toast +
        //   Windows notification), exactly like the `notify` tool's signal.
        // - `tool`: execute the scheduled tool with its stored arguments
        //   (no LLM round-trip), then notify the user of the outcome.
        // - `continue`: resume the task that scheduled the reminder ??the
        //   reminder text is injected into that task's conversation and the
        //   task is woken, so a scheduled "keep going at 3pm" continues the
        //   same ReAct loop without anyone speaking. Legacy rows without a
        //   task id fall back to running the text as a brand-new task.
        let agent = self.clone();
        let tools = self.executor.get_tools();
        if let Some(mut rx) = tools.reminders.take_fired_receiver() {
            tokio::spawn(async move {
                while let Some(fired) = rx.recv().await {
                    match fired.mode {
                        ReminderMode::Tool => {
                            let Some(tool_name) = fired.tool_name else {
                                agent
                                    .events
                                    .emit_notification(&fired.title, &fired.body)
                                    .await;
                                continue;
                            };
                            let args = fired.tool_args.unwrap_or(Value::Null);
                            let exec_tools = agent.executor.get_tools();
                            match exec_tools
                                .execute_tool(
                                    fired.task_id.as_deref(),
                                    &tool_name,
                                    args,
                                    CancellationToken::new(),
                                )
                                .await
                            {
                                Ok(result) => {
                                    let summary = truncate_notification(
                                        &result.summary_text(),
                                        agent.context_limits.notification_summary_chars,
                                    );
                                    agent
                                        .events
                                        .emit_notification(
                                            &fired.title,
                                            &format!("reminder tool '{tool_name}':\n{summary}"),
                                        )
                                        .await;
                                }
                                Err(e) => {
                                    agent
                                        .events
                                        .emit_notification(
                                            &fired.title,
                                            &format!("reminder tool '{tool_name}' failed: {e}"),
                                        )
                                        .await;
                                }
                            }
                        }
                        ReminderMode::Continue => {
                            let message = fired
                                .prompt
                                .clone()
                                .or_else(|| Some(fired.body.clone()))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| "Reminder fired: continue the task.".into());
                            match agent
                                .process_input_with_attachments(
                                    &message,
                                    fired.task_id.clone(),
                                    &[],
                                    false,
                                )
                                .await
                            {
                                Ok(result) => tracing::info!(
                                    "reminder {} resumed task: {:?}",
                                    fired.reminder_id,
                                    result
                                ),
                                Err(e) => tracing::warn!(
                                    "reminder {} failed to resume task: {}",
                                    fired.reminder_id,
                                    e
                                ),
                            }
                            // Also surface the notification so the user sees
                            // the reminder while the task continues.
                            agent
                                .events
                                .emit_notification(&fired.title, &fired.body)
                                .await;
                        }
                    }
                }
            });
        }
        // Re-arm reminders persisted by a previous run: overdue ones (the app
        // was closed when they expired) fire immediately, future ones resume
        // their countdown. Runs in the background; the notification consumer
        // spawned above delivers the overdue fires.
        let restore_tools = self.executor.get_tools();
        tokio::spawn(async move {
            let overdue = restore_tools.reminders.restore_pending().await;
            if overdue > 0 {
                tracing::info!("restored {} overdue reminder(s) from previous run", overdue);
            }
        });
    }

    /// Load the most recent conversation messages for a task as (role,
    /// content) pairs. The resume path deduplicates them against the
    /// restored canonical before injecting, so the fresh-run system-prompt
    /// path (`prompt_builder.build`) and the resume path share one source.
    fn load_conversation_history(&self, task_id: &str) -> Vec<ConversationMessage> {
        self.db
            .get_task_messages_limit(task_id, self.conversation_window_size)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|m| ConversationMessage {
                role: m.role,
                content: m.content,
            })
            .collect()
    }

    /// Rebuild per-task tool registrations from saved step history.
    ///
    /// Per-task registrations (loaded via `load_skill`/`load_mcp`) live in
    /// memory and are lost on app restart or rollback. This method clears any
    /// existing registrations for the task, then scans the history for
    /// `load_skill`/`load_mcp` actions and re-registers the corresponding
    /// adapters. Only steps present in the (possibly truncated) history are
    /// replayed, so rolling back to step N correctly drops tools loaded after
    /// step N.
    async fn restore_per_task_tools(&self, task_id: &str, history: &[ReActStep]) {
        let tools = self.executor.get_tools();
        // Clear stale registrations first (e.g. tools loaded after a rollback
        // point, or leftover from a previous run before restart).
        tools.unregister_task(task_id).await;

        for step in history {
            let Some(ref action) = step.action else {
                continue;
            };
            match action.tool_name.as_str() {
                "load_skill" => {
                    if let Some(name) = action.tool_input["skill_name"].as_str() {
                        tools.register_skill_for_task(task_id, name).await;
                    }
                }
                "load_mcp" => {
                    if let Some(name) = action.tool_input["server_name"].as_str() {
                        tools.register_mcp_for_task(task_id, name).await;
                    }
                }
                _ => {}
            }
        }
    }

    /// Roll back a task to a specific branch point. The task state is
    /// replaced with the branch point snapshot, session messages persisted
    /// after that point are deleted, branch points created after the target
    /// step are pruned. When `pause` is false the task is set to Pending for
    /// immediate re-execution; when true it is set to Paused so the user can
    /// edit and re-send the message before the dispatcher picks it up.
    ///
    /// `target_message_id` identifies the exact message being rolled back
    /// (the frontend knows which bubble the user right-clicked). It is used
    /// to detect an orphan rollback: a user message persisted after the
    /// newest branch point was never processed into the ReAct canonical, so
    /// rolling it back must discard ONLY that message.
    pub async fn rollback_task(
        &self,
        task_id: &str,
        target_step: u32,
        pause: bool,
        target_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // If the task is currently Running, cancel it first so the ReAct loop
        // exits cleanly. Otherwise the loop's in-memory canonical/history would
        // diverge from the restored snapshot and overwrite it on the next save.
        // The loop observes the token at every wait point (step top, LLM call,
        // tool batch drain) and exits without touching status, so no Error
        // marking is needed ??setting Error here would only emit a spurious
        // "task interrupted" error event and trigger terminal cleanup.
        let state = self.executor.get_task_state(task_id).await;
        if state == TaskStatus::Running {
            let cancel = self.executor.cancellation_token(task_id).await;
            cancel.cancel();
            // Wait until the loop handler releases the running slot.
            let mut waited = false;
            for _ in 0..50 {
                if !self
                    .executor
                    .running_tasks_list()
                    .await
                    .contains(&task_id.to_string())
                {
                    break;
                }
                waited = true;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if waited
                && self
                    .executor
                    .running_tasks_list()
                    .await
                    .contains(&task_id.to_string())
            {
                tracing::warn!(
                    "rollback_task {}: handler did not exit within 5s; proceeding with restore (late step writes are guarded by execute_step)",
                    task_id
                );
            }
        }

        // Background jobs spawned before the rollback are stale relative to
        // the restored snapshot: kill them so their children cannot leak.
        self.executor.cancel_task_jobs(task_id).await;

        let state_json = match self.db.get_react_state(task_id)? {
            Some(s) => s,
            None => {
                // No saved state at all ??this happens when a task errored
                // before any snapshot was saved (e.g. first LLM call failed
                // in an older version without Fix 1). We can't restore
                // canonical, but we can still truncate session messages so
                // the user can edit and re-send their input.
                tracing::warn!(
                    "rollback_task {}: no react_state ??falling back to message-only truncation",
                    task_id
                );
                let cutoff = self.db.last_user_message_ts(task_id);
                if let Some(ref ts) = cutoff {
                    if pause {
                        let _ = self.db.delete_messages_from(task_id, ts);
                    } else {
                        let _ = self.db.delete_messages_after(task_id, ts);
                    }
                }
                // Reload into memory and set status.
                self.executor.ensure_task_loaded(task_id).await?;
                self.set_task_status(
                    task_id,
                    if pause {
                        TaskStatus::Paused
                    } else {
                        TaskStatus::Pending
                    },
                )
                .await?;
                return Ok(());
            }
        };
        let mut snapshot: ReActSnapshot = serde_json::from_str(&state_json)?;

        // If no branch_point exists at the target step, the step likely
        // failed before save_branch_point was called (e.g. LLM error
        // mid-stream). In that case the snapshot's current canonical/history
        // IS the pre-step state ??use it directly.
        let bp = if let Some(bp) = snapshot.branch_points.get(&target_step).cloned() {
            bp
        } else {
            tracing::warn!(
                "rollback_task {}: no branch_point at step {}, using snapshot state (step_number={})",
                task_id,
                target_step,
                snapshot.step_number
            );
            // Determine the cutoff timestamp from session messages: the last
            // user message for user-rollback (pause=true), or the last user
            // message for agent-rollback too (delete the partial output after
            // it).
            let cutoff_ts = self.db.last_user_message_ts(task_id);
            BranchPoint {
                canonical: snapshot.canonical.clone(),
                history: snapshot.history.clone(),
                step_number: target_step,
                last_msg_at: cutoff_ts,
            }
        };

        // Restore the canonical/history/step from the branch point.
        snapshot.canonical = bp.canonical;
        snapshot.history = bp.history;
        snapshot.step_number = bp.step_number;

        // If the branch point was saved right after an assistant tool_call
        // message but before the tool results were appended, the canonical
        // array ends with an assistant message carrying `tool_calls` but no
        // matching tool-result messages. Sending this to the LLM triggers a
        // 400 error (dangling tool_call). Trim such a trailing assistant
        // message so the loop re-requests the tool call cleanly.
        Self::trim_dangling_tool_call(&mut snapshot.canonical, &mut snapshot.history);

        // Newest branch-point cutoff (computed BEFORE pruning): used below to
        // detect a user message persisted after every branch point.
        let max_bp_ts = snapshot
            .branch_points
            .values()
            .filter_map(|b| b.last_msg_at.clone())
            .max();

        // Prune branch points that were created after the target step so the
        // session tree does not accumulate stale entries from the discarded
        // timeline.
        snapshot.branch_points.retain(|&k, _| k <= target_step);

        // Truncate task messages persisted after the branch point so the
        // conversation context matches the restored snapshot.
        //
        // A user message may be persisted AFTER the newest branch point:
        // an interjection sent while the task was erroring (its supplement
        // was dropped as the task was terminal) or before the app closed
        // mid-generation (the steering queue is in-memory only and is lost).
        // Such a message was never added to the ReAct canonical, so rolling
        // back to it must discard ONLY that message ??deleting from the
        // branch point's cutoff would wipe valid earlier history.
        let task_msgs = self.db.get_task_messages(task_id).unwrap_or_default();
        let target_msg = target_message_id.and_then(|id| task_msgs.iter().find(|m| m.id == id));
        let is_orphan_rollback = target_msg.is_some_and(|m| {
            m.role == "user"
                && max_bp_ts
                    .as_deref()
                    .is_some_and(|max| m.created_at.as_str() > max)
        });

        if let Some(ref ts) = bp.last_msg_at {
            if pause {
                // User-message rollback: delete the user message itself too
                // (inclusive), so the context is clean when the user re-sends
                // an edited version.
                //
                // `ts` (bp.last_msg_at) is the timestamp of the last message
                // persisted BEFORE the target step ran ??usually the step's
                // thought, which is AFTER the user message. Deleting from
                // `ts` alone would leave the rolled-back user input in the
                // task, so it would reappear on the next review rebuild.
                // Delete from the user message's own timestamp instead.
                let user_ts = if is_orphan_rollback {
                    target_msg.map(|m| m.created_at.clone())
                } else {
                    task_msgs
                        .iter()
                        .filter(|m| m.role == "user" && m.created_at.as_str() <= ts.as_str())
                        .map(|m| m.created_at.clone())
                        .max()
                };
                match user_ts {
                    Some(u_ts) => {
                        self.db.delete_messages_from(task_id, &u_ts)?;
                        // Rollback overwrites: drop step rows recorded after
                        // the user message too (they belong to the discarded
                        // timeline).
                        self.db.delete_task_steps_after(task_id, &u_ts)?;
                    }
                    None => {
                        self.db.delete_messages_from(task_id, ts)?;
                        self.db.delete_task_steps_after(task_id, ts)?;
                    }
                }
            } else {
                // Strict `>` for both: the branch-point cutoff is the last
                // message BEFORE the discarded step, so we keep the cutoff
                // itself intact (truncate_task_after is non-inclusive).
                self.db.truncate_task_after(task_id, ts, false)?;
            }
        }

        // Drop any checkpointed partial stream text: the restored timeline
        // must not inherit a stale partial from the discarded run.
        let db = self.db.clone();
        let tid = task_id.to_string();
        let _ = db
            .run_blocking(move |db| db.delete_partial_message(&tid))
            .await;

        // For user-message rollback, also remove the user message from the
        // restored canonical so the LLM doesn't see it when the task resumes.
        // The user message is the last CanonicalRole::User entry; everything
        // after the preceding message should be trimmed. Skipped for orphan
        // rollback: the orphaned message was never in the canonical, so the
        // last User entry there is a legitimately processed message that must
        // stay in the restored context.
        if pause
            && !is_orphan_rollback
            && let Some(pos) = snapshot
                .canonical
                .iter()
                .rposition(|m| m.role == CanonicalRole::User)
        {
            // Keep everything before the last user message. Drop the user
            // message and any assistant messages that followed it.
            snapshot.canonical.truncate(pos);
        }

        let json = serde_json::to_string(&snapshot)?;
        self.db.save_react_state(task_id, &json)?;

        // Rebuild per-task tool registrations from the restored history so
        // that tools loaded after the rollback point are dropped, and tools
        // loaded before it remain available.
        self.restore_per_task_tools(task_id, &snapshot.history)
            .await;

        // Reload the task into executor memory (it may have been removed if we
        // marked a Running task as Error above, or was never loaded after restart).
        self.executor.ensure_task_loaded(task_id).await?;

        self.set_task_status(
            task_id,
            if pause {
                TaskStatus::Paused
            } else {
                TaskStatus::Pending
            },
        )
        .await?;
        if pause {
            tracing::info!(
                "rollback_task {} to step {}: task set to Paused (user-edit mode)",
                task_id,
                target_step
            );
        } else {
            tracing::info!(
                "rollback_task {} to step {}: task set to Pending",
                task_id,
                target_step
            );
        }
        Ok(())
    }

    /// Resume a task that errored mid-step. Removes any partial assistant
    /// output that was persisted on error (so the retry produces a clean
    /// message), then sets the task to Pending so the dispatcher picks it up
    /// and `run_task_from_id` restores from the saved snapshot.
    pub async fn continue_task(&self, task_id: &str) -> anyhow::Result<()> {
        // Ensure the task is loaded in executor memory.
        self.executor.ensure_task_loaded(task_id).await?;

        let state = self.executor.get_task_state(task_id).await;
        // Accept both Error (directly after interruption) and Paused (after
        // reopen_task set it to Paused during review). Both states indicate
        // the task can be retried from its saved snapshot.
        if state != TaskStatus::Error && state != TaskStatus::Paused {
            return Err(anyhow::anyhow!(
                "task is not in a retryable state (current: {:?})",
                state
            ));
        }

        // Load the snapshot saved on error to find the branch point's
        // last_msg_at ??the timestamp of the last message BEFORE the partial
        // output. We delete everything after it so the retry starts clean.
        if let Ok(Some(state_json)) = self.db.get_react_state(task_id)
            && let Ok(snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            // The snapshot's step_number is the step that failed. Try to
            // find a branch_point at that step; if none (the error
            // happened before save_branch_point was called), fall back to
            // the last user message's timestamp.
            let cutoff = snapshot
                .branch_points
                .get(&snapshot.step_number)
                .and_then(|bp| bp.last_msg_at.clone())
                .or_else(|| self.db.last_user_message_ts(task_id));
            if let Some(ts) = cutoff {
                // Retry OVERWRITES the previous attempt: drop both messages
                // and step rows (tool badges, thought entries) after the
                // branch point so the review history stays linear. Only
                // branching creates separate timelines.
                self.db.truncate_task_after(task_id, &ts, false)?;
            }
        }

        // Drop any checkpointed partial stream text: the retry re-streams
        // from scratch, so a crash during the retry must not promote the
        // pre-retry partial.
        let db = self.db.clone();
        let tid = task_id.to_string();
        let _ = db
            .run_blocking(move |db| db.delete_partial_message(&tid))
            .await;

        // Set to Pending for the dispatcher to pick up.
        self.set_task_status(task_id, TaskStatus::Pending).await?;

        tracing::info!("continue_task: task {} set to Pending for retry", task_id);
        Ok(())
    }

    /// If the canonical array ends with an assistant message carrying
    /// `tool_calls` but no matching tool-result messages, sending it to the
    /// LLM triggers a 400 error ("assistant message with tool calls must be
    /// followed by tool messages responding to each tool call"). This happens
    /// when a snapshot/branch point was saved right after the assistant
    /// message but before the tool results were appended (save_branch_point
    /// runs before tool execution; the app may die or be cancelled mid-batch).
    /// Trim such a trailing assistant message and the matching half-built
    /// history step so the loop re-requests the tool call cleanly.
    fn trim_dangling_tool_call(
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
    ) {
        sanitize_canonical(canonical);
        // A snapshot can also end with a half-built step: the assistant
        // message declared tool calls but the app died before the results
        // were appended (sanitize_canonical popped that message). Drop its
        // history step too (thought set, action=None) so the loop re-requests
        // the tool call cleanly.
        if history.last().is_some_and(|s| s.action.is_none()) {
            history.pop();
        }
    }

    /// Dispatcher entrypoint. Looks up the task by id, fills in the
    /// description and original transcript (context),
    /// loads conversation history, then runs the ReAct loop.
    pub async fn run_task_from_id(&self, task_id: &str) -> anyhow::Result<Vec<ReActStep>> {
        tracing::debug!("run_task_from_id: task_id={}", task_id);
        let task = self
            .executor
            .get_task(task_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found by dispatcher", task_id))?;

        let run_id = self.react_engine.next_run_id();

        let description = if task.summary.is_empty() {
            task.input.clone()
        } else {
            task.summary.clone()
        };
        let context = task.input.clone();

        // Conversation history and message persistence are keyed by the task
        // itself ??there is no separate session indirection anymore.
        let conv_history = self.load_conversation_history(task_id);

        // Multimodal: carry the first user message's image attachments into
        // the initial canonical user message so the model sees them from the
        // first turn (they were persisted by process_input_with_attachments).
        // The FIRST user message is always the task's own input; later image
        // follow-ups are supplements (injected by the ReAct loop at step
        // start) and must NOT be attached to the initial turn or they would
        // be duplicated.
        let initial_attachments = self
            .db
            .get_task_messages(task_id)
            .ok()
            .and_then(|msgs| {
                msgs.into_iter()
                    .find(|m| m.role == "user")
                    .filter(|m| !m.attachments.is_empty())
                    .map(|m| m.attachments)
            })
            .unwrap_or_default();

        let result = if let Ok(Some(state_json)) = self.db.get_react_state(task_id)
            && let Ok(mut snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            tracing::info!(
                "restoring ReAct state for task {} ({} steps)",
                task_id,
                snapshot.history.len()
            );
            // The snapshot may end with a dangling assistant tool_call
            // message (saved before tool results were appended). Sending it
            // to the LLM on resume triggers a 400 error, so trim it first.
            Self::trim_dangling_tool_call(&mut snapshot.canonical, &mut snapshot.history);
            // Re-register per-task tools (skills/MCP) from saved history,
            // since in-memory registrations are lost on app restart.
            self.restore_per_task_tools(task_id, &snapshot.history)
                .await;
            self.run_task_resumed(task_id, snapshot, &conv_history, run_id)
                .await
        } else {
            self.run_task(
                &task.id,
                &description,
                &context,
                &conv_history,
                &initial_attachments,
            )
            .await
        };

        // Generate title after first ReAct loop if not already set
        if task.title.is_none() {
            let db = self.db.clone();
            let executor = self.executor.clone();
            let title = self.title.clone();
            let events = self.events.clone();
            let tid = task_id.to_string();
            tokio::spawn(async move {
                Self::try_generate_title(db, executor, title, events, tid).await;
            });
        }

        result
    }

    pub async fn emit_task_completed(&self, task_id: &str, title: &str) {
        self.events.emit_task_completed(task_id, title).await;
        // Drop cumulative token counters for the finished task.
        self.react_engine.reset_cumulative_usage(task_id);
    }

    /// Generate a short title using small_model after the first ReAct loop
    /// completes. Spawned as a background task so it does not block the
    /// dispatcher. Only runs once per task (when title is None).
    async fn try_generate_title(
        db: Arc<Database>,
        executor: Arc<TaskExecutor>,
        title: Option<TitleGenerator>,
        events: Arc<EventDispatcher>,
        task_id: String,
    ) {
        let Some(generator) = title else { return };
        // Check if the task already has a title in the DB
        if let Ok(Some(task)) = db.get_task(&task_id)
            && task.title.is_some()
        {
            return;
        }
        // Build conversation context from user messages only. The agent's
        // replies (assistant/tool) are excluded to keep the prompt small ??        // a title only needs to reflect what the user asked for.
        let messages = db.get_task_messages_limit(&task_id, 10).unwrap_or_default();
        let user_lines: Vec<String> = messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.clone())
            .collect();
        if user_lines.is_empty() {
            return;
        }
        let title = match generator.generate(&user_lines).await {
            Some(t) => t,
            None => return,
        };
        // Save to DB
        if let Err(e) = db.update_task_title(&task_id, &title) {
            tracing::warn!("failed to save generated title: {}", e);
            return;
        }
        // Update in-memory TaskInfo in executor
        executor.update_task_title(&task_id, &title).await;
        // Notify frontend
        events.emit_title_updated(&task_id, &title).await;
        tracing::info!("generated title for task {}: {}", task_id, title);
    }

    async fn run_task_resumed(
        &self,
        task_id: &str,
        snapshot: ReActSnapshot,
        conversation_history: &[ConversationMessage],
        run_id: u64,
    ) -> anyhow::Result<Vec<ReActStep>> {
        let mut history = snapshot.history;
        let mut canonical = snapshot.canonical;
        let start_step = snapshot.step_number;
        let mut branch_points = snapshot.branch_points;

        if !conversation_history.is_empty() {
            // The restored canonical already contains the full transcript in
            // the common (non-compacted) case, and may itself carry
            // `[conversation]` lines left by a previous resume ??blindly
            // re-inserting the recent window would duplicate every turn and
            // make the model treat stale questions as newly pending (it then
            // answers questions from long ago instead of the current one).
            // Strip stale `[conversation]` lines, then re-seed only messages
            // whose content is not already present in the canonical.
            canonical.retain(|m| {
                !(m.role == CanonicalRole::User
                    && m.content.iter().any(
                        |p| matches!(p, ContentPart::Text(t) if t.starts_with("[conversation] ")),
                    ))
            });
            let mut present: HashSet<String> = canonical
                .iter()
                .flat_map(|m| m.content.iter())
                .filter_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            let sys_end = canonical
                .iter()
                .position(|m| m.role != CanonicalRole::System)
                .unwrap_or(canonical.len());
            let mut inserted = 0usize;
            for msg in conversation_history {
                if present.contains(&msg.content) {
                    continue;
                }
                canonical.insert(
                    sys_end + inserted,
                    CanonicalMessage::user_text(format!(
                        "[conversation] [{}] {}",
                        msg.role, msg.content
                    )),
                );
                inserted += 1;
                present.insert(msg.content.clone());
            }
            if inserted > 0 {
                tracing::debug!(
                    "run_task_resumed: seeded {} conversation message(s) missing from canonical",
                    inserted
                );
            }
        }

        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let inference = self.inference.clone();
        let tid = task_id.to_string();
        let infer = move || {
            let inference = inference.clone();
            let tid = tid.clone();
            tokio::spawn(async move {
                inference.infer_all(&tid).await;
            });
        };
        self.react_engine
            .run_react_loop(
                task_id,
                &mut canonical,
                &mut history,
                start_step,
                &mut branch_points,
                emitter_arc,
                &infer,
                run_id,
            )
            .await?;
        Ok(history)
    }

    pub(crate) async fn run_task(
        &self,
        task_id: &str,
        description: &str,
        context: &str,
        conversation_history: &[ConversationMessage],
        initial_attachments: &[haven_memory::repositories::messages::MessageAttachment],
    ) -> anyhow::Result<Vec<ReActStep>> {
        tracing::debug!(
            "run_task start: task_id={:?} context={:?} attachments={}",
            task_id,
            context,
            initial_attachments.len()
        );
        let mut history: Vec<ReActStep> = Vec::new();
        let history_lines: Vec<String> = conversation_history
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect();
        let system_prompt = self
            .prompt_builder
            .build(description, &[], &history_lines)
            .await;
        tracing::debug!("run_task: system_prompt {} chars", system_prompt.len());

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

        let mut branch_points: HashMap<u32, BranchPoint> = HashMap::new();
        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let inference = self.inference.clone();
        let tid = task_id.to_string();
        let infer = move || {
            let inference = inference.clone();
            let tid = tid.clone();
            tokio::spawn(async move {
                inference.infer_all(&tid).await;
            });
        };
        let run_id = self.react_engine.next_run_id();
        self.react_engine
            .run_react_loop(
                task_id,
                &mut canonical,
                &mut history,
                1,
                &mut branch_points,
                emitter_arc,
                &infer,
                run_id,
            )
            .await?;
        Ok(history)
    }

    pub async fn process_input(
        &self,
        transcript: &str,
        active_task_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        self.process_input_with_attachments(transcript, active_task_id, &[], false)
            .await
    }

    /// Like `process_input`, but attaches binary payloads (images and
    /// user-uploaded files) to the user message. Attachments are persisted
    /// with the message; images are injected into the ReAct context as image
    /// parts, while file attachments (which carry a `path`) surface as a text
    /// reference the agent resolves with the file tool. `voice` marks messages
    /// transcribed from audio so the UI can keep the mic style across reloads.
    pub async fn process_input_with_attachments(
        &self,
        transcript: &str,
        active_task_id: Option<String>,
        attachments: &[haven_memory::repositories::messages::MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<ProcessResult> {
        tracing::debug!(
            "process_input: text={:?} active_task_id={:?} attachments={} voice={}",
            transcript,
            active_task_id,
            attachments.len(),
            voice
        );

        // The message is persisted BEFORE the state check on purpose: the
        // steering/supplement fallback paths below rely on it being on disk.
        // If the task turns out to be terminal, the persisted row is removed
        // again below so history never shows a ghost user message.
        let mut persisted_msg = None;
        if let Some(task_id) = active_task_id {
            match self
                .persist_message_parts(
                    &task_id,
                    "user",
                    transcript,
                    Some("text"),
                    attachments,
                    voice,
                )
                .await
            {
                Ok(msg) => persisted_msg = Some(msg),
                Err(e) => {
                    tracing::warn!(
                        "process_input: failed to persist user message for task {}: {}",
                        task_id,
                        e
                    );
                }
            }

            let state = self.executor.get_task_state(&task_id).await;

            // Running tasks take the message as a steering interjection,
            // injected into the ReAct loop in the gap between tool calls and
            // the final content. If the steering queue is unavailable (task
            // vanished from memory between the state read and the enqueue),
            // fall through to the supplement path instead of failing: the
            // user message is already persisted, and the supplement path
            // reloads the task / handles the terminal guard / wakes it.
            let steering_delivered = state == TaskStatus::Running
                && match self
                    .executor
                    .add_steering_with_attachments(&task_id, transcript, attachments)
                    .await
                {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(
                            "process_input: steering failed for task {} ({}); falling back to supplement path",
                            task_id,
                            e
                        );
                        false
                    }
                };

            if !steering_delivered {
                // A Paused task that is awaiting an `ask` answer: this
                // message IS the reply to the pending question. Queue it as
                // an answer so the loop injects a paired "Answer to your
                // previous question" instead of generic context ??otherwise
                // the model sees the old question as still open and answers
                // questions from long ago. The flag must be read BEFORE
                // set_task_status(Pending) below, which clears it.
                let is_answer =
                    state == TaskStatus::Paused && self.executor.is_awaiting_answer(&task_id).await;
                let was_in_memory = if is_answer {
                    self.executor
                        .add_answer_with_attachments(&task_id, transcript, attachments)
                        .await
                        .is_ok()
                } else {
                    self.executor
                        .add_supplement_with_attachments(&task_id, transcript, attachments)
                        .await
                        .is_ok()
                };
                if !was_in_memory {
                    // Task may be stale/deleted ??fall back to creating a new task
                    if self.executor.ensure_task_loaded(&task_id).await.is_err() {
                        let task = self
                            .create_task_with_first_message(transcript, attachments, voice)
                            .await?;
                        self.events.emit_task_created(&task).await;
                        return Ok(ProcessResult::TaskCreated(task.id));
                    }
                    // Re-read state after ensure_task_loaded may have reloaded
                    // the task from DB (M3/H10 TOCTOU: end_task may have ended
                    // it between the get_task_state read above and the failed
                    // add_supplement). Only non-terminal tasks may be
                    // reactivated by a follow-up message; Completed/Error tasks
                    // were ended on purpose and must be reopened explicitly via
                    // the review flow ??auto-converting them would resurrect a
                    // ghost task.
                    let fresh_state = self.executor.get_task_state(&task_id).await;
                    if fresh_state == TaskStatus::Completed || fresh_state == TaskStatus::Error {
                        tracing::warn!(
                            "process_input: task {} is terminal ({:?}) despite active_task_id; dropping supplement to avoid resurrection",
                            task_id,
                            fresh_state
                        );
                        // Remove the just-persisted user message so history
                        // does not show an unanswered ghost bubble (the
                        // frontend is told to drop its copy below).
                        if let Some(msg) = persisted_msg.take() {
                            let db = self.db.clone();
                            let tid = task_id.clone();
                            let msg_id = msg.id.clone();
                            let tid_c = tid.clone();
                            let msg_id_c = msg_id.clone();
                            if let Err(e) = db
                                .run_blocking(move |db| db.delete_message_by_id(&tid_c, &msg_id_c))
                                .await
                            {
                                tracing::warn!(
                                    "process_input: failed to remove ghost user message {} for task {}: {}",
                                    msg_id,
                                    tid,
                                    e
                                );
                            }
                        }
                        // Notify the frontend so it can drop the stale
                        // activeTaskId and reset the model indicator instead of
                        // showing an orphaned bubble with no response.
                        self.events
                            .emit_task_updated(&task_id, fresh_state.as_str())
                            .await;
                        // Do not keep the reloaded terminal task in the working
                        // set ??it was ended and should not be dispatchable.
                        self.executor.remove_task(&task_id).await;
                    } else {
                        if is_answer {
                            self.executor
                                .add_answer_with_attachments(&task_id, transcript, attachments)
                                .await?;
                        } else {
                            self.executor
                                .add_supplement_with_attachments(&task_id, transcript, attachments)
                                .await?;
                        }
                        if fresh_state == TaskStatus::Paused {
                            self.set_task_status(&task_id, TaskStatus::Pending).await?;
                        }
                    }
                    return Ok(ProcessResult::Supplemented);
                }
                if state == TaskStatus::Paused {
                    self.set_task_status(&task_id, TaskStatus::Pending).await?;
                }
            }
            Ok(ProcessResult::Supplemented)
        } else {
            let task = self
                .create_task_with_first_message(transcript, attachments, voice)
                .await?;
            tracing::info!("process_input created task: id={:?}", task.id);
            self.events.emit_task_created(&task).await;
            Ok(ProcessResult::TaskCreated(task.id))
        }
    }

    /// Create a new task and persist the triggering user message into it,
    /// in that order ??the message (and its attachments) must be on disk
    /// BEFORE the task is registered with the executor, otherwise the
    /// dispatcher could start the ReAct loop and miss the first user turn.
    async fn create_task_with_first_message(
        &self,
        input: &str,
        attachments: &[haven_memory::repositories::messages::MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<haven_task::TaskInfo> {
        let record = self.db.create_task(input, input)?;
        // The first user turn (and its attachments) must be on disk BEFORE
        // the dispatcher can pick the task up; if persisting fails, remove
        // the task row again so no input-less task ever gets dispatched.
        if let Err(e) = self
            .persist_message_parts(&record.id, "user", input, Some("text"), attachments, voice)
            .await
        {
            let _ = self.db.delete_task(&record.id);
            return Err(e);
        }
        self.executor.ensure_task_loaded(&record.id).await?;
        // Wake the dispatcher now that the message is persisted.
        self.executor
            .update_task_status(&record.id, TaskStatus::Pending)
            .await?;
        let task = self
            .executor
            .get_task(&record.id)
            .await
            .ok_or_else(|| anyhow::anyhow!("task '{}' not registered", record.id))?;
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::stream;
    use haven_common::types::{CanonicalToolCall, RiskLevel};
    use haven_llm::{
        FinishReason, LlmClient, LlmError, LlmMessage, LlmResponse, LlmRole, StreamChunk, ToolCall,
        ToolDefinition, Usage,
    };
    use haven_tools::{Tool, ToolBox, ToolResult, ToolsManager};
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;

    fn temp_db() -> Arc<Database> {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        Arc::new(Database::open(&p).unwrap())
    }

    /// Mock LlmClient whose `chat_stream_with_tools` returns a single chunk
    /// containing the `final_answer` tool call so the ReAct loop terminates
    /// in one step.
    struct FinalAnswerMock;

    #[async_trait]
    impl LlmClient for FinalAnswerMock {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: chat not implemented".into()))
        }
        async fn chat_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown(
                "mock: chat_with_tools not implemented".into(),
            ))
        }
        async fn chat_stream(
            &self,
            _: Vec<LlmMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "mock: chat_stream not implemented".into(),
            ))
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let chunk = StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            };
            Ok(Box::pin(stream::iter(vec![Ok(chunk)])))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    struct RecordingEmitter {
        thoughts: std::sync::Mutex<Vec<String>>,
        supplements: std::sync::Mutex<Vec<String>>,
        notifications: std::sync::Mutex<Vec<(String, String)>>,
        completed: std::sync::Mutex<bool>,
    }

    #[async_trait]
    impl AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: AgentEvent) {
            match event {
                AgentEvent::Thought { thought, .. } => {
                    self.thoughts.lock().unwrap().push(thought);
                }
                AgentEvent::TaskCompleted { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::TaskUpdated { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::Supplement {
                    additional_context, ..
                } => {
                    self.supplements.lock().unwrap().push(additional_context);
                }
                AgentEvent::Notification { title, body, .. } => {
                    self.notifications.lock().unwrap().push((title, body));
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn run_task_emits_supplement_when_additional_context_queued() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));

        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let task = executor
            .create_task_with_summary("do stuff", "do stuff summary")
            .await
            .unwrap();
        executor
            .add_supplement(&task.id, "extra: remember path X")
            .await
            .unwrap();

        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());

        let sups = recorder.supplements.lock().unwrap().clone();
        assert_eq!(sups.len(), 1, "exactly one supplement event expected");
        assert_eq!(sups[0], "extra: remember path X");
        // With supplements, task pauses instead of completing (conversation mode)
        let state = executor.get_task_state(&task.id).await;
        assert_eq!(
            state,
            TaskStatus::Paused,
            "task should be paused (not completed) when supplements were processed"
        );
    }

    // 鈹€鈹€鈹€ Pure-logic and data-layer tests (no LLM required) 鈹€鈹€鈹€

    fn make_test_agent() -> (Arc<AgentLayer>, Arc<TaskExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));
        (agent, executor)
    }

    #[test]
    fn agent_new_constructor_works() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_new_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = AgentLayer::new(db, executor, router, 10, 20, ContextLimitsConfig::default());
        // Verify construction succeeded; no per-session indirection remains.
        let task = agent.db.create_task("input", "transcript").unwrap();
        assert!(!task.id.is_empty());
    }

    #[test]
    fn agent_constructs_without_session_machinery() {
        let (agent, _) = make_test_agent();
        let task = agent.db.create_task("input", "").unwrap();
        assert!(!task.id.is_empty());
        // Two tasks never share message keys ??each owns its own stream.
        let other = agent.db.create_task("input2", "").unwrap();
        assert_ne!(task.id, other.id);
    }

    #[test]
    fn set_emitter_stores_reference() {
        let (agent, _) = make_test_agent();
        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());
        // Verify emitter is stored without panic (set_emitter succeeds)
    }

    #[tokio::test]
    async fn replace_router_and_router_work() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_router_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client_a = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_a = Arc::new(LlmRouter::new_with_clients(
            client_a.clone(),
            client_a.clone(),
            client_a.clone(),
            client_a.clone(),
            client_a,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor,
            router_a,
            10,
            20,
            ContextLimitsConfig::default(),
        ));
        // Create a new router via the same mock client factory
        let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_b = Arc::new(LlmRouter::new_with_clients(
            client_b.clone(),
            client_b.clone(),
            client_b.clone(),
            client_b.clone(),
            client_b,
        ));
        agent.replace_router(router_b);
        // No panic == success
    }

    #[tokio::test]
    async fn set_max_steps_updates_field() {
        let (agent, executor) = make_test_agent();
        agent.set_max_steps(5);

        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let task = executor.create_task("test").await.unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
    }

    #[tokio::test]
    async fn build_system_prompt_succeeds() {
        let (agent, _) = make_test_agent();
        let prompt = agent.prompt_builder.build("test task", &[], &[]).await;
        assert!(prompt.contains("Available builtin tools"));
    }

    #[tokio::test]
    async fn build_system_prompt_excludes_sensitive_and_duplicate_facts() {
        let dir =
            std::env::temp_dir().join(format!("haven_prompt_facts_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&dir).unwrap());
        // Duplicate triple (same tags, same everything).
        db.insert_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        // A legitimate preference.
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        // Secrets that must never reach the prompt.
        db.insert_fact(
            "user",
            "tavily_api_key",
            "tvly-dev-secret",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();
        db.insert_fact(
            "user",
            "secret_token",
            "ghp_abc",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();

        let tools = Arc::new(ToolsManager::new());
        let builder = SystemPromptBuilder::new(tools, db);
        let prompt = builder.build("test task", &[], &[]).await;

        assert!(prompt.contains("name=Xtopia"));
        assert!(prompt.contains("likes=Rust"));
        assert!(!prompt.contains("tavily_api_key"));
        assert!(!prompt.contains("tvly-dev-secret"));
        assert!(!prompt.contains("secret_token"));
        assert!(!prompt.contains("ghp_abc"));
        // Duplicates are collapsed: the name fact is rendered exactly once.
        assert_eq!(prompt.matches("name=Xtopia").count(), 1);
    }

    #[tokio::test]
    async fn restore_per_task_tools_rebuilds_from_history() {
        // Create a skill on disk so SkillsEngine can discover it.
        let dir = std::env::temp_dir().join(format!("haven_restore_test_{}", uuid::Uuid::new_v4()));
        let skill_dir = dir.join("echo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
        )
        .unwrap();

        let db = Arc::new(
            Database::open(
                &std::env::temp_dir().join(format!("haven_restore_db_{}.db", uuid::Uuid::new_v4())),
            )
            .unwrap(),
        );
        let tools = Arc::new(ToolsManager::new());
        tools
            .skills_engine
            .set_config(Some(dir.clone()), None)
            .await
            .unwrap();
        tools.rebuild_catalog().await;
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools.clone(), 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor,
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));

        // Simulate a history where load_skill was called.
        let history = vec![ReActStep {
            step_number: 1,
            thought: Some("I need the echo skill".into()),
            action: Some(Action {
                tool_name: "load_skill".into(),
                tool_input: serde_json::json!({"skill_name": "echo"}),
                is_final: false,
                tool_call_id: Some("tc1".into()),
            }),
            observation: Some(r#"{"skill":{"name":"skill__echo"}}"#.into()),
        }];

        // Before restore, no per-task tools.
        let before = tools.list_schemas_for_task("task-x").await;
        assert!(!before.iter().any(|s| s["name"] == "skill__echo"));

        agent.restore_per_task_tools("task-x", &history).await;

        // After restore, the skill tool should be visible per-task.
        let after = tools.list_schemas_for_task("task-x").await;
        assert!(
            after.iter().any(|s| s["name"] == "skill__echo"),
            "restored skill should appear in per-task schemas"
        );

        // Other tasks should NOT see it.
        let other = tools.list_schemas_for_task("task-y").await;
        assert!(!other.iter().any(|s| s["name"] == "skill__echo"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persist_message_adds_to_db() {
        let (agent, _) = make_test_agent();
        let task = agent.db.create_task("input", "").unwrap();
        agent
            .persist_message_parts(&task.id, "user", "test message", Some("text"), &[], false)
            .await
            .unwrap();
        // Read back via db
        let agent_ref = agent.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_task_messages_limit(&task.id, 50).unwrap();
        // Messages may or may not be immediately flushed depending on cache
        // ??verify at minimum the message is retrievable
        let found = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "test message");
        assert!(found.is_some(), "persisted user message not found in db");
    }

    #[tokio::test]
    async fn persist_message_with_attachments_roundtrips() {
        let (agent, _) = make_test_agent();
        let task = agent.db.create_task("input", "").unwrap();
        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .persist_message_parts(
                &task.id,
                "user",
                "鐪嬬湅",
                Some("text"),
                std::slice::from_ref(&att),
                false,
            )
            .await
            .unwrap();
        let agent_ref = agent.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_task_messages_limit(&task.id, 50).unwrap();
        let found = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "鐪嬬湅");
        assert!(found.is_some(), "persisted message not found in db");
        let msg = found.unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].media_type, "image/png");
        assert_eq!(msg.attachments[0].data, "aGVsbG8=");
    }

    #[test]
    fn parse_default_model_response_final_answer_from_text() {
        let resp = LlmResponse {
            text: "Task done.".into(),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                model_name: None,
                cost: None,
            },
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };
        let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 1);
        assert_eq!(thought, Some("Task done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
        assert_eq!(actions[0].tool_name, "final_answer");
    }

    #[test]
    fn parse_default_model_response_with_tool_calls() {
        let resp = LlmResponse {
            text: "Opening file.".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "open_file".into(),
                arguments: r#"{"path":"/tmp/test"}"#.into(),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: haven_llm::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                model_name: None,
                cost: None,
            },
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };
        let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 2);
        assert_eq!(thought, Some("Opening file.".into()));
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
        assert_eq!(actions[0].tool_name, "open_file");
        assert_eq!(
            actions[0].tool_input,
            serde_json::json!({"path": "/tmp/test"})
        );
    }

    #[test]
    fn parse_default_model_response_final_answer_tool_call() {
        let resp = LlmResponse {
            text: "All done.".into(),
            tool_calls: vec![ToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: "{}".into(),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                model_name: None,
                cost: None,
            },
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };
        let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 1);
        assert_eq!(thought, Some("All done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
    }

    /// M3/H10: a follow-up message must NOT resurrect a task that was ended.
    /// Terminal tasks are only reactivated explicitly via `reopen_task`
    /// (Completed/Error ??Paused) in the review flow.
    #[tokio::test]
    async fn process_input_does_not_resurrect_ended_task() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("original").await.unwrap();
        executor.end_task(&task.id).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Error);

        let result = agent
            .process_input("more context", Some(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        // Task is not reloaded into the working set and never becomes Pending.
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Error);
        assert!(executor.get_supplements(&task.id).await.is_empty());
    }

    #[tokio::test]
    async fn process_input_reactivates_paused_task() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("original").await.unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();

        let result = agent
            .process_input("more context", Some(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        let supps: Vec<String> = executor
            .get_supplements(&task.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(supps, vec!["more context"]);
    }

    #[tokio::test]
    async fn process_input_marks_reply_as_answer_when_awaiting() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("original").await.unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();
        executor.set_awaiting_answer(&task.id, true).await;

        let result = agent
            .process_input("the answer", Some(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        let supps = executor.get_supplements(&task.id).await;
        assert_eq!(supps.len(), 1);
        assert!(
            supps[0].is_answer,
            "reply to an ask must be marked as answer"
        );
        assert_eq!(supps[0].text, "the answer");
        assert!(
            !executor.is_awaiting_answer(&task.id).await,
            "reactivation must clear the awaiting-answer gate"
        );
    }

    #[tokio::test]
    async fn process_input_paused_without_ask_is_plain_supplement() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("original").await.unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();

        let result = agent
            .process_input("follow up", Some(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        let supps = executor.get_supplements(&task.id).await;
        assert_eq!(supps.len(), 1);
        assert!(
            !supps[0].is_answer,
            "a follow-up to a normal pause is not an ask reply"
        );
    }

    #[tokio::test]
    async fn resume_dedups_conversation_prefix_against_canonical() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let task = executor.create_task("hello").await.unwrap();
        agent
            .persist_message_parts(&task.id, "user", "hello", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(&task.id, "assistant", "hi there", Some("text"), &[], false)
            .await
            .unwrap();
        // Snapshot whose canonical already carries the full transcript PLUS
        // a stale `[conversation]` prefix left by a previous resume ??the
        // exact duplication that made the model re-answer old questions.
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("hello"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("hi there")],
                None,
                None,
                Vec::new(),
            ),
            CanonicalMessage::user_text("[conversation] [user] hello"),
            CanonicalMessage::user_text("[conversation] [assistant] hi there"),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_task_from_id(&task.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&task.id).unwrap().unwrap()).unwrap();
        let user_texts: Vec<String> = saved
            .canonical
            .iter()
            .filter(|m| m.role == CanonicalRole::User)
            .filter_map(|m| {
                m.content.iter().find_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert!(
            user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
            "stale [conversation] lines must be stripped: {:?}",
            user_texts
        );
        assert_eq!(
            user_texts.iter().filter(|t| t.as_str() == "hello").count(),
            1,
            "already-present messages must not be duplicated"
        );
    }

    #[tokio::test]
    async fn loop_pauses_on_pending_ask_instead_of_heuristic_final() {
        // The model responds with text + Stop and no tool calls while an
        // unanswered `ask` is pending: the turn must not end on the
        // synthesized heuristic final ??the loop must pause and wait for
        // the user's answer instead.
        let client = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("I'll stop here.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
        agent.set_emitter(make_recording_emitter());
        let task = executor.create_task("task").await.unwrap();
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("help me"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("let me ask")],
                Some(vec![CanonicalToolCall {
                    id: "call_ask".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "which file?"}),
                }]),
                None,
                Vec::new(),
            ),
            CanonicalMessage::tool(
                vec![ContentPart::text(
                    r#"{"ask":true,"question":"which file?","awaiting_answer":true,"options":[]}"#,
                )],
                Some("call_ask".into()),
            ),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_task_from_id(&task.id).await.unwrap();

        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "task must pause for the pending question instead of completing"
        );
        assert!(
            executor.is_awaiting_answer(&task.id).await,
            "pause must be flagged as awaiting the user's answer"
        );
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert_eq!(
            msgs.last().unwrap().content,
            "which file?",
            "the pending question must be surfaced as the pause message"
        );
    }

    #[tokio::test]
    async fn budget_exhaustion_pauses_with_notification_and_no_chat_message() {
        // The scripted LLM always returns a non-final tool call, so the run
        // consumes its 1-step budget without ever producing a final answer.
        let client = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("keep working".into()),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"x"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
        agent.set_max_steps(1);
        let recorder = make_recording_emitter();
        agent.set_emitter(recorder.clone());
        let task = executor.create_task("task").await.unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();

        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "budget exhaustion must pause the task as a checkpoint"
        );
        // The notice must NOT be persisted as an assistant chat message.
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert!(
            msgs.iter()
                .all(|m| !m.content.contains("任务步骤上限已用尽")),
            "budget notice must not appear as a chat message: {:?}",
            msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>()
        );
        // It must be surfaced as a Notification event instead.
        let notifications = recorder.notifications.lock().unwrap().clone();
        assert!(
            notifications
                .iter()
                .any(|(title, _)| title == "任务步骤上限已用尽"),
            "budget notice must be emitted as a notification: {:?}",
            notifications
        );
    }

    #[tokio::test]
    async fn truncated_text_only_response_retried_before_final() {
        // First response: text with a Length finish (generation cut off) ??        // must NOT end the turn as if it were the final answer. Second
        // response: a complete Stop answer, which ends the turn.
        let client = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Here is the partial answer".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Length),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Here is the complete answer.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
        agent.set_emitter(make_recording_emitter());
        let task = executor.create_task("task").await.unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();

        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "turn must end paused after the retried final"
        );
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert_eq!(
            msgs.last().unwrap().content,
            "Here is the complete answer.",
            "the retried (complete) response must be the final message, not the truncated one"
        );
    }

    fn make_recording_emitter() -> Arc<RecordingEmitter> {
        Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        })
    }

    #[tokio::test]
    async fn run_task_from_id_attaches_first_user_message_images() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let task = executor
            .create_task_with_summary("鐪嬬湅", "鐪嬬湅")
            .await
            .unwrap();
        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .persist_message_parts(&task.id, "user", "鐪嬬湅", Some("text"), &[att], false)
            .await
            .unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();
        let snapshot: crate::types::ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&task.id).unwrap().unwrap()).unwrap();
        let user_msg = snapshot
            .canonical
            .iter()
            .find(|m| m.role == CanonicalRole::User)
            .expect("initial user message exists");
        assert!(
            user_msg
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. })),
            "initial user message should carry the image part"
        );
    }

    #[tokio::test]
    async fn run_task_from_id_ignores_later_image_supplement() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let task = executor
            .create_task_with_summary("plain task", "plain task")
            .await
            .unwrap();
        agent
            .persist_message_parts(&task.id, "user", "plain task", Some("text"), &[], false)
            .await
            .unwrap();
        // Image arrives AFTER the task input (a supplement) ??it must not be
        // attached to the initial user turn.
        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .process_input_with_attachments("琛ュ厖鐪嬪浘", Some(task.id.clone()), &[att], false)
            .await
            .unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();
        let snapshot: crate::types::ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&task.id).unwrap().unwrap()).unwrap();
        let first_user = snapshot
            .canonical
            .iter()
            .find(|m| m.role == CanonicalRole::User)
            .expect("initial user message exists");
        assert!(
            !first_user
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. })),
            "image supplement must not be attached to the initial user turn"
        );
        // The supplement itself is still injected (with its image) later.
        assert!(
            snapshot.canonical.iter().any(|m| m
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }))),
            "supplement image should be injected into the conversation"
        );
    }

    #[tokio::test]
    async fn run_task_from_id_trims_dangling_tool_call_before_resume() {
        // Simulate a snapshot saved by save_branch_point right after the
        // assistant tool_call message but before tool results were appended
        // (e.g. the app was closed mid-tool-execution). Resuming must trim
        // the dangling assistant message instead of sending it to the LLM,
        // which would reject it with a 400 error.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("resume me").await.unwrap();

        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("system prompt")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("resume me")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("calling echo")],
                tool_calls: Some(vec![CanonicalToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "hi"}),
                }]),
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let history = vec![ReActStep {
            step_number: 1,
            thought: Some("calling echo".into()),
            action: None,
            observation: None,
        }];
        let snapshot = ReActSnapshot {
            canonical,
            history,
            step_number: 2,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        let result = agent.run_task_from_id(&task.id).await.unwrap();

        // No batch sent to the LLM may end with a dangling assistant tool_call.
        {
            let seen = mock.seen.lock().unwrap();
            assert!(!seen.is_empty(), "LLM should have been called after resume");
            for batch in seen.iter() {
                let last = batch.last().expect("batch has messages");
                assert!(
                    !(matches!(last.role, LlmRole::Assistant) && last.tool_calls.is_some()),
                    "batch must not end with a dangling assistant tool_call: {:?}",
                    batch
                );
            }
        }
        assert!(!result.is_empty(), "resumed loop should produce history");
        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "final_answer should complete the resumed task"
        );
    }

    fn make_canonical(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text.to_string())],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    fn make_assistant_with_calls(ids: &[&str]) -> CanonicalMessage {
        let mut m = make_canonical(CanonicalRole::Assistant, "");
        m.tool_calls = Some(
            ids.iter()
                .map(|id| CanonicalToolCall {
                    id: id.to_string(),
                    name: "tool".into(),
                    arguments: serde_json::Value::Null,
                })
                .collect(),
        );
        m
    }

    fn make_tool_result(call_id: &str, text: &str) -> CanonicalMessage {
        let mut m = make_canonical(CanonicalRole::Tool, text);
        m.tool_call_id = Some(call_id.to_string());
        m
    }

    #[test]
    fn sanitize_canonical_drops_orphaned_tool_messages_and_dangling_calls() {
        // Mirrors the corruption found in a real interrupted task: a
        // compaction split the assistant(tool_calls)/tool-results pair, so
        // the summary assistant (no tool_calls) is followed by orphaned tool
        // messages. A valid pair and a dangling trailing assistant follow.
        let mut canonical = vec![
            make_canonical(CanonicalRole::System, "sys"),
            make_canonical(CanonicalRole::User, "hello"),
            make_canonical(CanonicalRole::Assistant, "[Compacted summary]"),
            make_tool_result("call_00_a", "result a"),
            make_tool_result("call_01_b", "result b"),
            make_assistant_with_calls(&["call_00_c", "call_01_d"]),
            make_tool_result("call_00_c", "result c"),
            make_tool_result("call_01_d", "result d"),
            make_assistant_with_calls(&["call_00_e"]),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::System,
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::Tool,
            ],
            "orphaned tools and the dangling trailing assistant must be removed"
        );
        // The surviving pair's tool results are intact.
        assert_eq!(canonical[4].tool_call_id.as_deref(), Some("call_00_c"));
        assert_eq!(canonical[5].tool_call_id.as_deref(), Some("call_01_d"));
    }

    #[test]
    fn sanitize_canonical_keeps_user_reset_and_trailing_tool() {
        // A tool message following a user message is orphaned; a trailing
        // tool message after its assistant-with-calls is valid.
        let mut canonical = vec![
            make_canonical(CanonicalRole::User, "a"),
            make_assistant_with_calls(&["call_1"]),
            make_tool_result("call_1", "r"),
            make_canonical(CanonicalRole::User, "b"),
            make_tool_result("call_1", "orphan after user"),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::User,
            ],
            "only the orphaned trailing tool must be removed"
        );
    }

    #[tokio::test]
    async fn process_input_with_attachments_queues_and_persists_attachments() {
        let (agent, executor) = make_test_agent();
        let task = executor
            .create_task_with_summary("original", "original")
            .await
            .unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();

        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        let result = agent
            .process_input_with_attachments(
                "鐪嬬湅",
                Some(task.id.clone()),
                std::slice::from_ref(&att),
                false,
            )
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        let supps = executor.get_supplements(&task.id).await;
        assert_eq!(supps.len(), 1);
        assert_eq!(supps[0].text, "鐪嬬湅");
        assert_eq!(supps[0].attachments, vec![att]);

        // Persisted with attachments in the task's message stream.
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let user_msg = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "鐪嬬湅")
            .expect("user message persisted");
        assert_eq!(user_msg.attachments.len(), 1);
        assert_eq!(user_msg.attachments[0].media_type, "image/png");
    }

    #[tokio::test]
    async fn process_input_creates_new_task() {
        let (agent, executor) = make_test_agent();
        let result = agent.process_input("open notepad", None).await.unwrap();
        match result {
            ProcessResult::TaskCreated(task_id) => {
                assert!(!task_id.is_empty());
                let state = executor.get_task_state(&task_id).await;
                assert_eq!(state, TaskStatus::Pending);
            }
            ProcessResult::Supplemented => panic!("expected TaskCreated"),
        }
    }

    #[tokio::test]
    async fn run_fact_inference_does_not_panic() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("test").await.unwrap();
        executor.end_task(&task.id).await.unwrap();
        agent.inference.infer_facts(&task.id).await;
        agent.inference.infer_preferences(&task.id).await;
    }

    // 鈹€鈹€鈹€ Integration tests for the ReAct core loop (refine ??1) 鈹€鈹€鈹€

    fn make_test_agent_with(
        client: Arc<dyn LlmClient>,
        tools: Arc<ToolsManager>,
    ) -> (Arc<AgentLayer>, Arc<TaskExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));
        (agent, executor)
    }

    /// Scripted LlmClient that returns a pre-programmed sequence of responses
    /// from `chat_stream_with_tools`, enabling full ReAct-loop integration
    /// tests without a live LLM. Mirrors Pi's `MockLlmClient` pattern.
    struct ScriptedMock {
        stream_responses: std::sync::Mutex<VecDeque<ScriptedResponse>>,
        chat_text: std::sync::Mutex<String>,
        /// Every message batch sent to `chat_stream_with_tools`, for
        /// assertions (e.g. that no dangling tool_call is sent).
        seen: std::sync::Mutex<Vec<Vec<LlmMessage>>>,
    }

    enum ScriptedResponse {
        Err(LlmError),
        Chunk(StreamChunk),
        ChunkThenErr(StreamChunk, LlmError),
        /// Yield the chunk only after `delay_ms`, so a test can deliver
        /// steering/supplements while the LLM call is in flight.
        ChunkDelayed(StreamChunk, u64),
    }

    impl ScriptedMock {
        fn new(responses: Vec<ScriptedResponse>) -> Self {
            Self {
                stream_responses: std::sync::Mutex::new(VecDeque::from(responses)),
                chat_text: std::sync::Mutex::new("Compacted summary.".into()),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedMock {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            let text = self.chat_text.lock().unwrap().clone();
            Ok(LlmResponse {
                text,
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    model_name: None,
                    cost: None,
                },
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            })
        }
        async fn chat_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
        }
        async fn chat_stream(
            &self,
            _: Vec<LlmMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
        }
        async fn chat_stream_with_tools(
            &self,
            messages: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.seen.lock().unwrap().push(messages);
            let resp =
                self.stream_responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(ScriptedResponse::Err(LlmError::Unknown(
                        "scripted responses exhausted".into(),
                    )));
            match resp {
                ScriptedResponse::Err(e) => Err(e),
                ScriptedResponse::Chunk(chunk) => Ok(Box::pin(stream::iter(vec![Ok(chunk)]))),
                ScriptedResponse::ChunkThenErr(chunk, e) => {
                    Ok(Box::pin(stream::iter(vec![Ok(chunk), Err(e)])))
                }
                ScriptedResponse::ChunkDelayed(chunk, delay_ms) => {
                    Ok(Box::pin(stream::once(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        Ok(chunk)
                    })))
                }
            }
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    struct EventCollector {
        events: std::sync::Mutex<Vec<AgentEvent>>,
    }
    impl EventCollector {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn has_action(&self, tool_name: &str) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::Action { tool_name: tn, .. } if tn == tool_name))
        }
        fn has_observation(&self, tool_name: &str) -> bool {
            self.events.lock().unwrap().iter().any(
                |e| matches!(e, AgentEvent::Observation { tool_name: tn, .. } if tn == tool_name),
            )
        }
        fn has_compaction(&self) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::Compaction { .. }))
        }
        fn has_notification(&self) -> Option<(String, String)> {
            self.events.lock().unwrap().iter().find_map(|e| {
                if let AgentEvent::Notification { title, body, .. } = e {
                    Some((title.clone(), body.clone()))
                } else {
                    None
                }
            })
        }
    }
    #[async_trait]
    impl AgentEventEmitter for EventCollector {
        async fn emit(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> String {
            "echo".into()
        }
        fn description(&self) -> String {
            "Echo back the input text".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(
                serde_json::json!({"echoed": input["text"].as_str().unwrap_or("")}),
            ))
        }
    }

    struct TimingState {
        intervals: std::sync::Mutex<Vec<(Instant, Instant)>>,
    }
    impl TimingState {
        fn new() -> Self {
            Self {
                intervals: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    struct TimingTool {
        tool_name: String,
        state: Arc<TimingState>,
    }
    impl TimingTool {
        fn new(name: &str, state: Arc<TimingState>) -> Self {
            Self {
                tool_name: name.into(),
                state,
            }
        }
    }
    #[async_trait]
    impl Tool for TimingTool {
        fn name(&self) -> String {
            self.tool_name.clone()
        }
        fn description(&self) -> String {
            "Delayed tool for parallel testing".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            let start = Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            self.state
                .intervals
                .lock()
                .unwrap()
                .push((start, Instant::now()));
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn run_task_executes_tool_then_final_answer() {
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"text":"hello"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("echo hello").await.unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");
        assert!(collector.has_action("echo"));
        assert!(collector.has_observation("echo"));
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_injects_mid_turn_steering_before_final_content() {
        // A user message sent while the agent is generating its final answer
        // must be injected before the turn ends (between the tool calls and
        // the final content) instead of being deferred until after
        // completion. The final LLM response is delayed so the steering
        // arrives while that call is still in flight; the agent must then
        // re-run with the message in context.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"text":"hello"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            // Delayed final answer: the steering is added while this call is
            // in flight, so the final-content branch must pick it up.
            ScriptedResponse::ChunkDelayed(
                StreamChunk {
                    text: Some("Done.".into()),
                    tool_calls: vec![ToolCall {
                        id: "final".into(),
                        name: "final_answer".into(),
                        arguments: "{}".into(),
                    }],
                    finish_reason: Some(FinishReason::Stop),
                    usage: None,
                    model: None,
                    reasoning: None,
                    web_search: None,
                    web_search_calls: Vec::new(),
                },
                300,
            ),
            // The re-run after the steering was injected also answers finally.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Understood, continuing in French.".into()),
                tool_calls: vec![ToolCall {
                    id: "final2".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("echo hello").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let task_id = task.id.clone();
            async move { agent.run_task_from_id(&task_id).await }
        });
        // Wait until the echo step completed and the delayed final LLM call
        // is in flight, then deliver the user's mid-turn message.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        executor
            .add_steering(&task.id, "stop and use French")
            .await
            .unwrap();
        let history = run.await.unwrap().unwrap();

        {
            let seen = mock.seen.lock().unwrap();
            assert_eq!(seen.len(), 3, "agent must re-run after mid-turn steering");
            let last_call = seen.last().unwrap();
            assert!(
                last_call.iter().any(|m| matches!(m.role, LlmRole::User)
                    && m.content.iter().any(|c| matches!(
                        c,
                        ContentPart::Text(t) if t.contains("Steering: stop and use French")
                    ))),
                "steering must be injected into the re-run LLM call"
            );
        }
        assert!(
            history.len() >= 3,
            "should have re-run after steering injection"
        );
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_injects_steering_between_tool_calls() {
        // A user message sent while the agent is executing tools is drained
        // at the next step boundary ??between tool calls ??so the final
        // answer is generated with the new context.
        let tools = Arc::new(ToolsManager::new());
        let timing = Arc::new(TimingState::new());
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Running the tool.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "delay_a".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("run tool").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let task_id = task.id.clone();
            async move { agent.run_task_from_id(&task_id).await }
        });
        // Deliver the message while delay_a (200ms) is still executing.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        executor
            .add_steering(&task.id, "add more detail")
            .await
            .unwrap();
        let _ = run.await.unwrap().unwrap();

        {
            let seen = mock.seen.lock().unwrap();
            assert_eq!(
                seen.len(),
                2,
                "no re-run needed: steering is drained at step boundary"
            );
            assert!(
                seen[1].iter().any(|m| matches!(m.role, LlmRole::User)
                    && m.content.iter().any(|c| matches!(
                        c,
                        ContentPart::Text(t) if t.contains("Steering: add more detail")
                    ))),
                "steering must be injected into the next LLM call after the tool step"
            );
        }
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_ask_tool_pauses_and_surfaces_question() {
        // The `ask` tool signals the ReAct loop to pause and wait for the
        // user's reply (delivered as a supplement on resume). Verify the task
        // ends Paused and the question is persisted as an assistant message.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I need to clarify before proceeding.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: r#"{"question":"Which path should I take: A or B?"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            // The loop pauses after `ask`, so a second response is never
            // consumed; include a final_answer anyway to catch regressions
            // where the loop incorrectly continues.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("decide a path").await.unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();

        // Task must be Paused, awaiting the user's answer.
        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "ask should pause the task"
        );
        assert!(collector.has_action("ask"));
        assert!(collector.has_observation("ask"));

        // The question must be persisted so the user can see and answer it.
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let found = msgs
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Which path should I take"));
        assert!(found, "question should be persisted as assistant message");
    }

    #[tokio::test]
    async fn run_task_ask_resumes_after_user_answer() {
        // After `ask` pauses the task, the user's reply arrives as a
        // supplement; the loop resumes and should reach final_answer.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            // Step 1: agent asks.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Clarifying.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: r#"{"question":"A or B?"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            // Step 2 (after resume): final answer.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Going with A.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("pick a path").await.unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();
        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "ask should pause"
        );

        // User answers; the supplement flips the task back to Pending.
        executor
            .add_supplement(&task.id, "Use option A.")
            .await
            .unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();
        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "task should pause again after final answer"
        );
        // The final answer text should be persisted, proving the loop resumed
        // past the `ask` step and reached final_answer.
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let answered = msgs
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Going with A"));
        assert!(answered, "final answer should be persisted after resume");
    }

    #[tokio::test]
    async fn retry_after_ask_answer_error_keeps_single_history() {
        // Reproduce the reported issue: the agent asks a question, the user
        // answers, the resumed step fails, and the user retries. Every retry
        // must OVERWRITE the previous attempt's persisted output ??the review
        // history should show exactly one question, one answer, one response.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            // Step 1: ask the question.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Asking.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: r#"{"question":"Proceed?"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            // Step 2 (after the answer): streams a partial thought, then fails.
            ScriptedResponse::ChunkThenErr(
                StreamChunk {
                    text: Some("Let me think...".into()),
                    tool_calls: vec![],
                    finish_reason: None,
                    usage: None,
                    model: None,
                    reasoning: None,
                    web_search: None,
                    web_search_calls: Vec::new(),
                },
                LlmError::Unknown("mock mid-stream failure".into()),
            ),
            // Step 2 retry: final answer.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Answer accepted.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        agent.set_emitter(Arc::new(EventCollector::new()));
        let task = executor.create_task("ask retry").await.unwrap();

        // Turn 1: the ask pauses the task.
        agent.run_task_from_id(&task.id).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);

        // Turn 2: the user answers; the resumed step fails mid-stream.
        executor.add_supplement(&task.id, "Yes").await.unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        let _ = agent.run_task_from_id(&task.id).await;
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Error);

        // Turn 3: retry via continue_task ??Pending ??re-run.
        agent.continue_task(&task.id).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        agent.run_task_from_id(&task.id).await.unwrap();

        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let steps = agent.db.get_task_steps(&task.id).unwrap();

        // The failed attempt's partial text must be gone (overwritten).
        let partials: Vec<&str> = msgs
            .iter()
            .filter(|m| m.role == "assistant" && m.content.contains("Let me think"))
            .map(|m| m.content.as_str())
            .collect();
        assert!(
            partials.is_empty(),
            "partial output from the failed attempt should be deleted, got {:?}",
            partials
        );
        // Exactly one question and one final answer.
        let questions = msgs
            .iter()
            .filter(|m| m.role == "assistant" && m.content.contains("Proceed?"))
            .count();
        let finals = msgs
            .iter()
            .filter(|m| m.role == "assistant" && m.content.contains("Answer accepted."))
            .count();
        assert_eq!(questions, 1, "ask question must appear exactly once");
        assert_eq!(finals, 1, "final answer must appear exactly once");

        // Step rows from the failed attempt must be overwritten too ??the
        // review history stays linear (only branching splits timelines).
        let stale_steps = steps
            .iter()
            .filter(|s| {
                s.thought
                    .as_deref()
                    .is_some_and(|t| t.contains("Let me think"))
            })
            .count();
        assert_eq!(
            stale_steps, 0,
            "step rows from the failed attempt should be deleted, got {:?}",
            steps
        );
        let final_steps = steps
            .iter()
            .filter(|s| {
                s.thought
                    .as_deref()
                    .is_some_and(|t| t.contains("Answer accepted."))
            })
            .count();
        assert_eq!(final_steps, 1, "retried step must appear exactly once");
    }

    #[tokio::test]
    async fn run_task_notify_tool_emits_notification_without_pausing() {
        // The `notify` tool signals the ReAct loop to emit a Notification
        // event (in-app toast + Windows). Unlike `ask`, it must NOT pause the
        // task: the loop continues to the final answer.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::notify::NotifyTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Notifying the user.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "notify".into(),
                    arguments: r#"{"title":"Build","body":"Compilation finished"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("build and notify").await.unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");

        // The Notification event must carry the tool's title/body.
        let (title, body) = collector
            .has_notification()
            .expect("notify should emit a Notification event");
        assert_eq!(title, "Build");
        assert_eq!(body, "Compilation finished");

        // The chat/review observation must be readable, not raw JSON.
        assert!(collector.has_observation("notify"));

        // Unlike `ask`, notify must not pause the task mid-loop: the loop
        // continued past the notify step (history has 2 steps) and reached the
        // normal end state (Paused = conversation mode, waiting for follow-up).
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_multiple_asks_surface_all_questions() {
        // Two `ask` calls in one batch must both be surfaced (joined into one
        // assistant message), not just the first.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("Two questions.".into()),
                tool_calls: vec![
                    ToolCall {
                        id: "tc1".into(),
                        name: "ask".into(),
                        arguments: r#"{"question":"First?"}"#.into(),
                    },
                    ToolCall {
                        id: "tc2".into(),
                        name: "ask".into(),
                        arguments: r#"{"question":"Second?"}"#.into(),
                    },
                ],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("two questions").await.unwrap();
        agent.run_task_from_id(&task.id).await.unwrap();
        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Paused,
            "ask should pause"
        );
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let persisted: String = msgs
            .iter()
            .filter(|m| m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            persisted.contains("First?"),
            "first question missing: {}",
            persisted
        );
        assert!(
            persisted.contains("Second?"),
            "second question missing: {}",
            persisted
        );
    }

    #[tokio::test]
    async fn run_task_parallel_tool_execution() {
        let tools = Arc::new(ToolsManager::new());
        let timing = Arc::new(TimingState::new());
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
            .await;
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Running both in parallel.".into()),
                tool_calls: vec![
                    ToolCall {
                        id: "tc1".into(),
                        name: "delay_a".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "tc2".into(),
                        name: "delay_b".into(),
                        arguments: "{}".into(),
                    },
                ],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("run parallel").await.unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(collector.has_action("delay_a"));
        assert!(collector.has_action("delay_b"));
        let mut intervals = timing.intervals.lock().unwrap().clone();
        assert_eq!(intervals.len(), 2, "both tools should have executed");
        intervals.sort_by_key(|(start, _)| *start);
        let (_, a_end) = intervals[0];
        let (b_start, _) = intervals[1];
        assert!(
            b_start < a_end,
            "tools should execute in parallel (overlap)"
        );
    }

    #[tokio::test]
    async fn run_task_compaction_retry_on_context_exceeded() {
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Calling echo.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"text":"data"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Err(LlmError::ContextLengthExceeded),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done after compaction.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("test compaction").await.unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(
            collector.has_compaction(),
            "Compaction event should be emitted"
        );
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_context_exceeded_compaction_fails() {
        let tools = Arc::new(ToolsManager::new());
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Err(
            LlmError::ContextLengthExceeded,
        )]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor.create_task("compaction fail").await.unwrap();
        let result = agent.run_task_from_id(&task.id).await;
        assert!(result.is_err(), "should error when compaction fails");
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Error);
    }

    #[tokio::test]
    async fn continue_task_resumes_errored_task() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("test continue").await.unwrap();
        // Simulate an errored task with a saved snapshot.
        agent.db.update_task_status(&task.id, "error").unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Error)
            .await
            .unwrap();
        let snapshot = ReActSnapshot {
            canonical: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            }],
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        // Add a partial assistant message that should be cleaned up.
        agent
            .db
            .add_message(&task.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "assistant", "partial output", Some("text"), None)
            .unwrap();

        agent.continue_task(&task.id).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        // The partial output should have been deleted (only the user message remains).
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[tokio::test]
    async fn continue_task_non_error_fails() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("not error").await.unwrap();
        // Task is Pending, not Error ??should refuse.
        let result = agent.continue_task(&task.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rollback_without_react_state_truncates_messages() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("no state").await.unwrap();
        // No react_state saved ??simulate an old task that errored before
        // snapshots were persisted.
        agent.db.update_task_status(&task.id, "error").unwrap();
        executor
            .update_task_status(&task.id, TaskStatus::Error)
            .await
            .unwrap();
        agent
            .db
            .add_message(&task.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "assistant", "partial", Some("text"), None)
            .unwrap();

        // User-message rollback (pause=true) should truncate from the user msg.
        agent.rollback_task(&task.id, 1, true, None).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert!(msgs.is_empty(), "messages should be empty after rollback");
    }

    #[tokio::test]
    async fn rollback_with_snapshot_no_branch_point_uses_snapshot() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("no bp").await.unwrap();
        // Save a snapshot with no branch_points at the target step.
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let snapshot = ReActSnapshot {
            canonical: canonical.clone(),
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        agent
            .db
            .add_message(&task.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "assistant", "partial", Some("text"), None)
            .unwrap();

        // Rollback to step 1 with pause=false (agent rollback).
        agent.rollback_task(&task.id, 1, false, None).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        // The partial assistant message should be deleted, user message kept.
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[tokio::test]
    async fn rollback_pause_true_removes_user_message_from_task() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("user rollback").await.unwrap();
        agent
            .db
            .add_message(&task.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        // Branch point at step 1: canonical ends at the user message, but
        // last_msg_at points at the thought that was persisted AFTER it (the
        // realistic shape saved by save_branch_point).
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let thought_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(thought_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points,
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // User-message rollback: the user message itself must be removed from
        // the task (its text returns to the composer for editing) ??not
        // left behind to reappear on the next review rebuild.
        agent.rollback_task(&task.id, 1, true, None).await.unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert!(
            msgs.is_empty(),
            "user message should be deleted from the task, got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn rollback_orphan_user_message_deletes_only_that_message() {
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("orphan rollback").await.unwrap();
        agent
            .db
            .add_message(&task.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        // The interrupted message: persisted AFTER the branch point because
        // the task errored before it was ever drained into the ReAct context.
        agent
            .db
            .add_message(&task.id, "user", "interrupt", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let thinking_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(thinking_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points,
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // Roll back the interrupted message: only it must be discarded; the
        // earlier exchange ("hello" / "thinking") survives.
        let interrupt_id = msgs
            .iter()
            .find(|m| m.content == "interrupt")
            .unwrap()
            .id
            .clone();
        agent
            .rollback_task(&task.id, 1, true, Some(&interrupt_id))
            .await
            .unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["hello", "thinking"],
            "orphan rollback must not wipe earlier history, got {:?}",
            contents
        );
        // The canonical must NOT be truncated: "hello" is a legitimately
        // processed message and stays in the restored context.
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&task.id).unwrap().unwrap()).unwrap();
        assert!(
            restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "orphan rollback must not truncate the processed user message from canonical"
        );
    }

    #[tokio::test]
    async fn rollback_processed_user_message_with_later_orphan_wipes_target_timeline() {
        // Same layout as the orphan test, but the user rolls back the
        // PROCESSED message ("hello") rather than the orphan. The orphan's
        // existence must not hijack the rollback: deleting from the target's
        // own timestamp also discards the later orphan (it belongs to the
        // discarded timeline), and the canonical IS truncated.
        let (agent, executor) = make_test_agent();
        let task = executor.create_task("processed rollback").await.unwrap();
        agent
            .db
            .add_message(&task.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&task.id, "user", "interrupt", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        let thinking_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(thinking_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points,
        };
        agent
            .db
            .save_react_state(&task.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
        agent
            .rollback_task(&task.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
        let msgs = agent.db.get_task_messages(&task.id).unwrap();
        assert!(
            msgs.is_empty(),
            "rollback of the processed message must wipe the orphan too, got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&task.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "rollback of a processed message must truncate the canonical"
        );
    }
}
