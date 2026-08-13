use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod compactor;
mod event;
mod inference;
mod prompt;
mod react;
mod rollback;
mod title;
mod types;

pub use compactor::ContextCompactor;
pub use event::{AgentEvent, AgentEventEmitter, BufferedEmitter, EventBus, EventDispatcher};
pub use haven_session::{RunHandler, SessionExecutor, SessionInfo, SessionStatus};
pub use inference::InferenceEngine;
pub use prompt::SystemPromptBuilder;
pub use react::ReActEngine;
pub use types::{Action, BranchPoint, ProcessResult, ReActSnapshot, ReActStep};

use haven_common::config::ContextLimitsConfig;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::LlmRouter;
use haven_memory::Database;
use haven_memory::repositories::messages::{Message, MessageAttachment};
use haven_tools::ScheduleMode;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::title::TitleGenerator;

/// The single persistence entry point for chat messages: insert a message
/// into a session's message stream, dropping any checkpointed partial stream
/// text first (a real message supersedes it). Both user turns (AgentLayer)
/// and assistant turns (ReActEngine) go through this one implementation so
/// the two paths cannot drift apart. The partial discard goes through the
/// executor's `PartialStore` so an in-flight stream checkpoint can never
/// re-create the row after the real message landed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_session_message(
    executor: &haven_session::SessionExecutor,
    session_id: &str,
    role: &str,
    content: &str,
    message_type: Option<&str>,
    attachments: &[MessageAttachment],
    voice: bool,
) -> anyhow::Result<Message> {
    executor.partials.discard(session_id).await;
    let db = executor.db().clone();
    let session_id = session_id.to_string();
    let role = role.to_string();
    let content = content.to_string();
    let message_type = message_type.map(String::from);
    let attachments = attachments.to_vec();
    db.run_blocking(move |db| {
        db.add_message_full(
            &session_id,
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
/// summarized away while the results survive), an app exit right after the
/// assistant message was appended, or a tool batch cancelled mid-flight with
/// only some of its results appended. This drops orphaned `tool` messages
/// (no preceding assistant tool_calls) and, for every tool_call an assistant
/// declared without a matching result, inserts a synthetic `Tool` result
/// marked "Interrupted". Inserting an interrupted result (instead of trimming
/// the dangling assistant) keeps the array valid for providers that reject a
/// tool_call with no following result as a 400 — including a partial batch
/// where one of two declared calls never returned — and lets the loop see that
/// the tool was cut off and retry it if needed.
///
/// Text used for the synthetic interrupted result.
const INTERRUPTED_RESULT: &str =
    "Interrupted: the tool call was cut off before it returned a result.";

/// Enrich the interrupted-result text with the tool name and the arguments
/// that were attempted, so the model can see exactly which call was cut off
/// and retry it with the same input instead of guessing. Used by both the
/// live cancel path and the snapshot sanitize/repair path.
pub(crate) fn interrupted_result_text(tool_name: &str, arguments: &Value) -> String {
    if tool_name.is_empty() {
        INTERRUPTED_RESULT.to_string()
    } else {
        format!(
            "{} (tool: {}, arguments: {})",
            INTERRUPTED_RESULT, tool_name, arguments
        )
    }
}

pub(crate) fn sanitize_canonical(canonical: &mut Vec<CanonicalMessage>) {
    let mut out: Vec<CanonicalMessage> = Vec::with_capacity(canonical.len());
    // Tool_calls declared by the most recent assistant that have not yet been
    // answered by a tool result. Orphaned tool messages (this is empty) are
    // dropped; every call left pending when a non-tool message (or the array
    // end) arrives is repaired with an "Interrupted" result carrying the call's
    // own fields (id, name, arguments).
    let mut pending_calls: Vec<CanonicalToolCall> = Vec::new();
    for m in canonical.drain(..) {
        match m.role {
            CanonicalRole::Tool => {
                if pending_calls.is_empty() {
                    tracing::warn!(
                        "dropping orphaned tool message (tool_call_id={:?}) with no preceding assistant tool_calls",
                        m.tool_call_id
                    );
                    continue;
                }
                if let Some(cid) = &m.tool_call_id {
                    if let Some(pos) = pending_calls.iter().position(|c| &c.id == cid) {
                        pending_calls.remove(pos);
                    } else {
                        // The id doesn't match any outstanding call (some
                        // providers/agents don't echo it): consume the next
                        // pending call in order to keep the pairing aligned.
                        pending_calls.pop();
                    }
                } else {
                    pending_calls.pop();
                }
                out.push(m);
            }
            CanonicalRole::Assistant => {
                // A new assistant supersedes the previous assistant's
                // tool_calls: any still-unanswered ones were interrupted.
                repair_interrupted_tool_calls(&mut out, &mut pending_calls);
                pending_calls = m.tool_calls.clone().unwrap_or_default();
                out.push(m);
            }
            _ => {
                // A user/system/other message breaks the tool-call chain.
                repair_interrupted_tool_calls(&mut out, &mut pending_calls);
                out.push(m);
            }
        }
    }
    repair_interrupted_tool_calls(&mut out, &mut pending_calls);
    *canonical = out;
}

/// Append a synthetic `Tool` result marked "Interrupted" for every tool_call
/// still pending (declared by an assistant but never answered). This keeps the
/// canonical array valid — providers reject an assistant tool_call with no
/// following result as a 400 — while preserving the fact that the tool was
/// attempted, so the model can retry it. The result text carries the call's
/// own name and arguments so the model sees exactly what was attempted.
fn repair_interrupted_tool_calls(
    out: &mut Vec<CanonicalMessage>,
    pending_calls: &mut Vec<CanonicalToolCall>,
) {
    while let Some(call) = pending_calls.pop() {
        tracing::info!(
            "repairing interrupted tool_call {} with an Interrupted result",
            call.id
        );
        let text = interrupted_result_text(&call.name, &call.arguments);
        out.push(CanonicalMessage::tool(
            vec![ContentPart::text(text)],
            Some(call.id),
        ));
    }
}

pub struct AgentLayer {
    db: Arc<Database>,
    executor: Arc<SessionExecutor>,
    conversation_window_size: usize,
    context_limits: ContextLimitsConfig,
    events: Arc<EventDispatcher>,
    prompt_builder: Arc<SystemPromptBuilder>,
    react_engine: Arc<ReActEngine>,
    inference: Arc<InferenceEngine>,
    title: Option<TitleGenerator>,
    title_in_flight: Arc<Mutex<HashSet<String>>>,
}

/// A recent conversation message used to re-seed context when resuming a
/// session. Kept as (role, content) pairs so the resume path can deduplicate
/// against the restored canonical instead of blindly duplicating every turn.
#[derive(Debug, Clone)]
struct ConversationMessage {
    role: String,
    content: String,
}

impl AgentLayer {
    pub fn new(
        db: Arc<Database>,
        executor: Arc<SessionExecutor>,
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
            title_in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Persist a message into the session's message stream (conversation history).
    /// Returns the persisted message so callers can roll it back precisely
    /// (e.g. when the session turns out to be terminal right after).
    async fn persist_message_parts(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        attachments: &[haven_memory::repositories::messages::MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<haven_memory::repositories::messages::Message> {
        persist_session_message(
            &self.executor,
            session_id,
            role,
            content,
            message_type,
            attachments,
            voice,
        )
        .await
    }

    /// Update a session's status in the executor and notify the frontend.
    /// The status string always comes from `SessionStatus::as_str()` so the
    /// persisted value and the emitted event cannot drift. Shared
    /// implementation with the ReAct loop (`set_status_and_emit`); without a
    /// wired emitter (tests, degraded startup) only the executor is updated,
    /// mirroring the old `emit_session_updated` no-op behavior.
    async fn set_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        if let Some(emitter) = self.events.emitter_arc() {
            crate::react::set_status_and_emit(&self.executor, &emitter, session_id, status).await
        } else {
            self.executor
                .update_session_status(session_id, status)
                .await?;
            Ok(())
        }
    }

    /// Build the `infer` callback handed to the ReAct loop: spawns a
    /// background inference pass over the session's transcript. Shared by the
    /// fresh-start and resume paths.
    fn spawn_infer(&self, session_id: &str) -> impl Fn() + Send + Sync + '_ {
        let inference = self.inference.clone();
        let tid = session_id.to_string();
        move || {
            let inference = inference.clone();
            let tid = tid.clone();
            tokio::spawn(async move {
                inference.infer_all(&tid).await;
            });
        }
    }

    /// Reopen a terminal session to Paused state.
    /// Used by the history review flow ??shows the session as active on the chat
    /// page.  The dispatcher won't pick it up until the user sends a
    /// follow-up message (which calls supplement_session ??Paused→Pending).
    pub async fn reopen_session(&self, session_id: &str) -> anyhow::Result<()> {
        // Terminal sessions (Error/Completed) are removed from the in-memory
        // list by unmark_running, so ensure_session_loaded is needed to bring
        // them back before we can update their status.
        self.executor.ensure_session_loaded(session_id).await?;
        let state = self.executor.get_session_state(session_id).await;
        if state == Some(SessionStatus::Completed) || state == Some(SessionStatus::Error) {
            self.set_session_status(session_id, SessionStatus::Paused)
                .await?;
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

    /// Spawn the SessionExecutor dispatcher with a runner wired to this
    /// AgentLayer. Must be called exactly once after construction.
    pub fn start(self: Arc<Self>) {
        let agent = self.clone();
        let executor = self.executor.clone();
        let handler: RunHandler = Arc::new(move |session_id: String| {
            let agent = agent.clone();
            Box::pin(async move { agent.run_session_from_id(&session_id).await.map(|_| ()) })
        });
        executor.start_dispatcher(handler);

        // Spawn a consumer for background-task completions. When a task
        // finishes, inject the result into the owning session's context at the
        // next ReAct step (via the task-completions buffer) and, if the session was
        // Paused for scheduling reasons, wake it to Pending so the dispatcher
        // resumes and the model processes the result no manual `status`
        // polling required.
        //
        // A session Paused because the `ask` tool is awaiting a human reply is
        // NOT woken: resuming it would let the agent continue (and run tools)
        // based on subprocess output before the user has answered. The result
        // is still buffered and delivered as context once the user resumes.
        let agent = self.clone();
        let tools = self.executor.get_tools();
        if let Some(mut rx) = tools.background_tasks.take_completion_receiver() {
            tokio::spawn(async move {
                while let Some(comp) = rx.recv().await {
                    // Skip cancellations: a cancelled task was killed
                    // intentionally (end_session/rollback), so notifying would
                    // risk resurrecting an ended session.
                    if comp.status == "cancelled" {
                        continue;
                    }
                    let Some(tid) = comp.session_id else {
                        continue;
                    };
                    // Per-completion span so every log line in the consumer
                    // (wake, injection, notification) carries both the task and
                    // the owning session — parallel tasks stay distinguishable.
                    let comp_span = tracing::info_span!("task_completion", task_id = %comp.task_id, session_id = %tid);
                    let _comp_guard = comp_span.enter();
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
                    // Failed tasks carry a pre-condensed reason (progress bars
                    // stripped, tail kept) so the model and the notification
                    // see the real error, not a multi-KB progress dump. The
                    // injected context is capped either way: the model needs
                    // the reason, not the full transcript.
                    let reason = if comp.status == "failed" {
                        comp.status_json
                            .get("error_reason")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or(&payload)
                            .to_string()
                    } else {
                        payload
                    };
                    let mut msg = format!(
                        "[Background task result]\ntask_id: {}\nstatus: {}\n\n{}",
                        comp.task_id,
                        comp.status,
                        truncate_notification(
                            &reason,
                            agent.context_limits.task_result_context_chars
                        )
                    );
                    // Failed tasks write the full output to a log file; point
                    // the model at it so a condensed reason never hides the
                    // root cause.
                    if let Some(log_path) = comp
                        .status_json
                        .get("log_path")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        msg.push_str(&format!("\nFull log: {log_path}"));
                    }
                    agent.executor.add_task_completion(&tid, &msg).await;
                    let state = agent.executor.get_session_state(&tid).await;
                    // Awaiting-answer pauses must not be auto-woken by
                    // background-task completions (the model is blocked on the
                    // user, not on task results).
                    let awaiting = matches!(&state, Some(s) if s.is_awaiting_answer());
                    if state == Some(SessionStatus::Paused) && !awaiting {
                        if let Err(e) = agent
                            .executor
                            .update_session_status(&tid, SessionStatus::Pending)
                            .await
                        {
                            tracing::warn!("task-completion wake session {} failed: {}", tid, e);
                            continue;
                        }
                        agent.events.emit_session_updated(&tid, "pending").await;
                    }
                    // A session that is no longer alive (completed, errored, or
                    // removed) has no ReAct loop left to inject the result
                    // into, so the buffered context above would be dropped.
                    // Persist the result as a message in the session's history
                    // instead — reopening the session shows what the background
                    // task produced. (Live/paused sessions get the result via the
                    // next ReAct step; awaiting-answer sessions keep it buffered
                    // until the user replies.)
                    if (matches!(&state, Some(s) if s.is_terminal()) || state.is_none())
                        && let Err(e) = crate::persist_session_message(
                            &agent.executor,
                            &tid,
                            "user",
                            &msg,
                            Some("text"),
                            &[],
                            false,
                        )
                        .await
                    {
                        tracing::warn!(
                            "task-completion persist for ended session {} failed: {}",
                            tid,
                            e
                        );
                    }
                    // Active push so the user never has to poll for status:
                    // a toast (in-app + Windows) announces the transition.
                    let (title, status_label) = if comp.status == "completed" {
                        ("后台任务已完成".to_string(), "已完成".to_string())
                    } else {
                        ("后台任务失败".to_string(), "失败".to_string())
                    };
                    let summary = truncate_notification(
                        &reason,
                        agent.context_limits.notification_summary_chars,
                    );
                    let body = if summary.trim().is_empty() {
                        format!("{} {}", comp.task_id, status_label)
                    } else {
                        format!("{} {}\n{}", comp.task_id, status_label, summary)
                    };
                    agent.events.emit_notification(&title, &body).await;
                }
            });
        }
        // Spawn a consumer for fired scheduled_tasks: the fire behavior is chosen
        // by the scheduled task's mode.
        // - `notify`: surface it as a Notification event (in-app toast +
        //   Windows notification), exactly like the `notify` tool's signal.
        // - `tool`: execute the scheduled tool with its stored arguments
        //   (no LLM round-trip), then notify the user of the outcome.
        // - `continue`: resume the session that scheduled the task ??the
        //   scheduled task text is injected into that session's conversation and the
        //   session is woken, so a scheduled "keep going at 3pm" continues the
        //   same ReAct loop without anyone speaking. A continue-mode task
        //   without a session id is an error (no fallback).
        let agent = self.clone();
        let tools = self.executor.get_tools();
        if let Some(mut rx) = tools.scheduled_tasks.take_fired_receiver() {
            tokio::spawn(async move {
                while let Some(fired) = rx.recv().await {
                    // Per-scheduled-task span so fire logs carry the scheduled task and
                    // its owning session; parallel scheduled-task fires stay distinct.
                    let fire_span = tracing::info_span!(
                        "reminder_fired",
                        reminder_id = %fired.task_id,
                        session_id = %fired.session_id.as_deref().unwrap_or("-")
                    );
                    let _fire_guard = fire_span.enter();
                    match fired.mode {
                        ScheduleMode::Tool => {
                            let Some(tool_name) = fired.tool_name else {
                                agent
                                    .events
                                    .emit_notification(&fired.title, &fired.body)
                                    .await;
                                continue;
                            };
                            let args = fired.tool_args.unwrap_or(Value::Null);
                            // Run through the safety gateway like any other
                            // tool call: a scheduled operation at/above the
                            // risk threshold still requires the user's
                            // confirmation before it executes.
                            let outcome = agent
                                .executor
                                .execute_gated(
                                    fired.session_id.as_deref(),
                                    &tool_name,
                                    args,
                                    CancellationToken::new(),
                                )
                                .await;
                            match outcome {
                                // The scheduled call needed confirmation and
                                // was declined or timed out (nobody was around
                                // when it fired). Surface a distinct message:
                                // the call was deliberately skipped, not broken.
                                Ok(g) if g.confirmed == Some(false) => {
                                    agent
                                        .events
                                        .emit_notification(
                                            &fired.title,
                                            &format!(
                                                "Scheduled tool '{tool_name}' was NOT executed: \
                                                 confirmation was declined or timed out."
                                            ),
                                        )
                                        .await;
                                }
                                Ok(g) => {
                                    let summary = truncate_notification(
                                        &g.result.summary_text(),
                                        agent.context_limits.notification_summary_chars,
                                    );
                                    agent
                                        .events
                                        .emit_notification(
                                            &fired.title,
                                            &format!("schedule tool '{tool_name}':\n{summary}"),
                                        )
                                        .await;
                                }
                                Err(e) => {
                                    agent
                                        .events
                                        .emit_notification(
                                            &fired.title,
                                            &format!("schedule tool '{tool_name}' failed: {e}"),
                                        )
                                        .await;
                                }
                            }
                        }
                        ScheduleMode::Continue => {
                            let message = fired
                                .prompt
                                .clone()
                                .or_else(|| Some(fired.body.clone()))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| {
                                    "ScheduledTask fired: continue the session.".into()
                                });
                            // A continue-mode task requires the session it
                            // continues; without one it cannot run (there is
                            // no fallback to a brand-new session).
                            let Some(session_id) = fired.session_id.clone() else {
                                agent
                                    .events
                                    .emit_notification(
                                        &fired.title,
                                        &format!(
                                            "定时任务 '{title}' 无法继续：未关联会话。",
                                            title = fired.title
                                        ),
                                    )
                                    .await;
                                continue;
                            };
                            match agent
                                .process_input_with_attachments(
                                    &message,
                                    Some(session_id),
                                    &[],
                                    false,
                                )
                                .await
                            {
                                Ok(result) => tracing::info!(
                                    "scheduled task {} resumed session: {:?}",
                                    fired.task_id,
                                    result
                                ),
                                Err(e) => tracing::warn!(
                                    "scheduled task {} failed to resume session: {}",
                                    fired.task_id,
                                    e
                                ),
                            }
                            // Also surface the notification so the user sees
                            // the scheduled task while the session continues.
                            agent
                                .events
                                .emit_notification(&fired.title, &fired.body)
                                .await;
                        }
                    }
                }
            });
        }
        // Re-arm scheduled_tasks persisted by a previous run: overdue ones (the app
        // was closed when they expired) fire immediately, future ones resume
        // their countdown. Runs in the background; the notification consumer
        // spawned above delivers the overdue fires. Also clean up task rows a
        // previous run left `running` (their child processes died with the
        // app), so persisted task history never shows stale live work.
        let restore_tools = self.executor.get_tools();
        tokio::spawn(async move {
            let overdue = restore_tools.scheduled_tasks.restore_pending().await;
            if overdue > 0 {
                tracing::info!(
                    "restored {} overdue scheduled task(s) from previous run",
                    overdue
                );
            }
            let interrupted = restore_tools.background_tasks.restore_after_restart().await;
            if interrupted > 0 {
                tracing::info!(
                    "marked {} interrupted background task(s) as failed",
                    interrupted
                );
            }
        });
    }

    /// Load the most recent conversation messages for a session as (role,
    /// content) pairs. The resume path deduplicates them against the
    /// restored canonical before injecting, so the fresh-run system-prompt
    /// path (`prompt_builder.build`) and the resume path share one source.
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

    /// Rebuild per-session tool registrations from saved step history.
    ///
    /// Per-session registrations (loaded via `load_skill`/`load_mcp`) live in
    /// memory and are lost on app restart or rollback. This method clears any
    /// existing registrations for the session, then scans the history for
    /// `load_skill`/`load_mcp` actions and re-registers the corresponding
    /// adapters. Only steps present in the (possibly truncated) history are
    /// replayed, so rolling back to step N correctly drops tools loaded after
    /// step N.
    async fn restore_per_session_tools(&self, session_id: &str, history: &[ReActStep]) {
        let tools = self.executor.get_tools();
        // Clear stale registrations first (e.g. tools loaded after a rollback
        // point, or leftover from a previous run before restart).
        tools.unregister_session(session_id).await;

        for step in history {
            let Some(ref action) = step.action else {
                continue;
            };
            match action.tool_name.as_str() {
                "load_skill" => {
                    if let Some(name) = action.tool_input["skill_name"].as_str() {
                        tools.register_skill_for_session(session_id, name).await;
                    }
                }
                "load_mcp" => {
                    if let Some(name) = action.tool_input["server_name"].as_str() {
                        tools.register_mcp_for_session(session_id, name).await;
                    }
                }
                _ => {}
            }
        }
    }

    /// Dispatcher entrypoint. Looks up the session by id, fills in the
    /// description and original transcript (context),
    /// loads conversation history, then runs the ReAct loop.
    pub async fn run_session_from_id(&self, session_id: &str) -> anyhow::Result<Vec<ReActStep>> {
        tracing::debug!("run_session_from_id: session_id={}", session_id);
        let session =
            self.executor.get_session(session_id).await.ok_or_else(|| {
                anyhow::anyhow!("session '{}' not found by dispatcher", session_id)
            })?;

        let run_id = self.react_engine.next_run_id();

        let description = if session.summary.is_empty() {
            session.input.clone()
        } else {
            session.summary.clone()
        };
        let context = session.input.clone();

        // Conversation history and message persistence are keyed by the session
        // itself ??there is no separate session indirection anymore.
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
            Ok(Some(state_json)) => match serde_json::from_str::<ReActSnapshot>(&state_json) {
                Ok(mut snapshot) => {
                    tracing::info!(
                        "restoring ReAct state for session {} ({} steps)",
                        session_id,
                        snapshot.history.len()
                    );
                    // The snapshot may end with a dangling assistant tool_call
                    // message (saved before tool results were appended). Sending it
                    // to the LLM on resume triggers a 400 error, so trim it first.
                    Self::trim_dangling_tool_call(&mut snapshot.canonical, &mut snapshot.history);
                    // Re-register per-session tools (skills/MCP) from saved history,
                    // since in-memory registrations are lost on app restart.
                    self.restore_per_session_tools(session_id, &snapshot.history)
                        .await;
                    self.run_session_resumed(session_id, snapshot, &conv_history, run_id)
                        .await
                }
                Err(e) => {
                    // A corrupt or schema-drifted react_state silently
                    // degrades to a fresh run below, losing all tool
                    // execution context. Surface it so the loss is visible
                    // (and so a snapshot-format bug can't hide).
                    tracing::warn!(
                        "react_state for session {} failed to parse ({}); falling back to fresh run",
                        session_id,
                        e
                    );
                    self.run_session(
                        &session.id,
                        &description,
                        &context,
                        &conv_history,
                        &initial_attachments,
                    )
                    .await
                }
            },
            Ok(None) => {
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
                tracing::warn!(
                    "failed to read react_state for session {} ({}); falling back to fresh run",
                    session_id,
                    e
                );
                self.run_session(
                    &session.id,
                    &description,
                    &context,
                    &conv_history,
                    &initial_attachments,
                )
                .await
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

    pub async fn emit_session_completed(&self, session_id: &str, title: &str) {
        self.events.emit_session_completed(session_id, title).await;
        // Drop cumulative token counters for the finished session.
        self.react_engine.reset_cumulative_usage(session_id);
    }

    /// Generate a short title using small_model after a successful ReAct
    /// loop. Spawned as a background session so it does not block the
    /// dispatcher. Only runs once per session (when title is None), and only
    /// once at a time: overlapping dispatches of the same session (auto-reload
    /// on app start plus a manual continue) must not fire concurrent title
    /// calls.
    async fn try_generate_title(
        db: Arc<Database>,
        executor: Arc<SessionExecutor>,
        title: Option<TitleGenerator>,
        events: Arc<EventDispatcher>,
        in_flight: Arc<Mutex<HashSet<String>>>,
        session_id: String,
    ) {
        let Some(generator) = title else { return };
        // Claim the in-flight slot before the DB check so two concurrent
        // spawns both pass the title check only once. Released after the
        // generation attempt ends (success or failure).
        {
            let mut set = in_flight.lock().await;
            if !set.insert(session_id.clone()) {
                return;
            }
        }
        Self::generate_title(db, executor, generator, events, session_id.clone()).await;
        in_flight.lock().await.remove(&session_id);
    }

    async fn generate_title(
        db: Arc<Database>,
        executor: Arc<SessionExecutor>,
        generator: TitleGenerator,
        events: Arc<EventDispatcher>,
        session_id: String,
    ) {
        // Check if the session already has a title in the DB
        if let Ok(Some(session)) = db.get_session(&session_id)
            && session.title.is_some()
        {
            return;
        }
        // Build conversation context from user messages only. The agent's
        // replies (assistant/tool) are excluded to keep the prompt small ??        // a title only needs to reflect what the user asked for.
        let messages = match db.get_session_messages_limit(&session_id, 10) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "title generation: get_session_messages_limit failed (session={}): {}",
                    session_id,
                    e
                );
                Vec::new()
            }
        };
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
        if let Err(e) = db.update_session_title(&session_id, &title) {
            tracing::warn!("failed to save generated title: {}", e);
            return;
        }
        // Update in-memory SessionInfo in executor
        executor.update_session_title(&session_id, &title).await;
        // Notify frontend
        events.emit_title_updated(&session_id, &title).await;
        tracing::info!("generated title for session {}: {}", session_id, title);
    }

    async fn run_session_resumed(
        &self,
        session_id: &str,
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
            // Compaction replaced the old turns with a summary inside the
            // canonical, but the DB message stream was left untouched. If we
            // re-seed the recent window here, every summarized-away turn is
            // resurrected as a fresh User message, undoing the compaction and
            // making the model re-answer stale questions (the summary would
            // also be duplicated alongside the restored originals). Skip the
            // re-seed entirely when the canonical already carries a
            // compaction summary.
            let compacted = canonical.iter().any(|m| {
                m.content.iter().any(|p| {
                    matches!(
                        p,
                        ContentPart::Text(t)
                            if t.starts_with(haven_common::prompts::COMPACTED_SUMMARY_PREFIX)
                    )
                })
            });
            if compacted {
                tracing::debug!(
                    "run_session_resumed: canonical is compacted; skipping conversation re-seed"
                );
            } else {
                // Count occurrences instead of a plain membership set: two
                // distinct turns with identical text (e.g. the user said
                // "好的" twice) are both legitimate history, and a set-based
                // dedup would silently drop the second one. Each DB message
                // consumes one occurrence from the canonical tally; only
                // messages that exhaust the tally are re-seeded.
                let mut present: HashMap<String, usize> = HashMap::new();
                for t in canonical
                    .iter()
                    .flat_map(|m| m.content.iter())
                    .filter_map(|p| match p {
                        ContentPart::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                {
                    *present.entry(t.clone()).or_default() += 1;
                    // History is carried in two wrapped forms besides plain
                    // text: `[conversation] [role] content` (a previous
                    // resume's re-seed, stripped above for User but possibly
                    // still present for Assistant) and `  [role] content`
                    // (history embedded in the system prompt by `run_session`,
                    // indented under "Additional context:"). Both represent
                    // the same raw content, which must count as "already
                    // present" or every resume would duplicate the whole
                    // recent window on top of the system prompt.
                    let rest = t.strip_prefix("[conversation] ").unwrap_or(&t);
                    let rest = rest.trim_start();
                    if let Some((head, tail)) = rest.split_once(']')
                        && head.starts_with('[')
                        && matches!(&head[1..], "user" | "assistant" | "system" | "tool")
                    {
                        *present.entry(tail.trim_start().to_string()).or_default() += 1;
                    }
                }
                // Supplement/steering inputs are pushed into the canonical
                // with a text prefix ("Additional context from user: —,
                // "Answer to your previous question: —, "Steering: —) while
                // the DB stores the raw text only. Exact-match dedup would
                // re-inject those inputs on every resume, so register the
                // un-prefixed variant as present too.
                let prefixes = [
                    "Additional context from user: ",
                    "Answer to your previous question: ",
                    "Steering: ",
                ];
                let prefixed: Vec<(String, usize)> = present
                    .iter()
                    .filter_map(|(t, n)| {
                        prefixes
                            .iter()
                            .find_map(|p| t.strip_prefix(p))
                            .map(|rest| (rest.to_string(), *n))
                    })
                    .collect();
                for (t, n) in prefixed {
                    *present.entry(t).or_default() += n;
                }
                let sys_end = canonical
                    .iter()
                    .position(|m| m.role != CanonicalRole::System)
                    .unwrap_or(canonical.len());
                let mut inserted = 0usize;
                for msg in conversation_history {
                    let remaining = present.entry(msg.content.clone()).or_insert(0);
                    if *remaining > 0 {
                        *remaining -= 1;
                        continue;
                    }
                    // Re-seed with the message's original role: DB-stored
                    // assistant turns (e.g. a paused `ask` question) flattened
                    // to User would otherwise make the model treat the old
                    // question as a new open prompt and answer it again.
                    let content = format!("[conversation] [{}] {}", msg.role, msg.content);
                    let cm = if msg.role == "assistant" {
                        CanonicalMessage::assistant(
                            vec![ContentPart::text(content)],
                            None,
                            None,
                            Vec::new(),
                        )
                    } else {
                        CanonicalMessage::user_text(content)
                    };
                    canonical.insert(sys_end + inserted, cm);
                    inserted += 1;
                }
                if inserted > 0 {
                    tracing::debug!(
                        "run_session_resumed: seeded {} conversation message(s) missing from canonical",
                        inserted
                    );
                }
            }
        }

        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let infer = self.spawn_infer(session_id);
        self.react_engine
            .run_react_loop(
                session_id,
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

    /// Rebuild canonical tool-call/result pairs from the persisted
    /// `session_steps` rows. Used only on the snapshot-less resume fallback
    /// (react_state missing or corrupt): without it the model forgets every
    /// tool it already ran and re-executes them.
    ///
    /// The DB message stream deliberately stores only text (user/assistant
    /// content) —tool calls and results live exclusively in session_steps
    /// (and the snapshot). `session_steps` is a plain sequence of rows with
    /// `action_tool`/`action_input`/`observation`; interleaving them back
    /// into the canonical yields:
    ///
    /// ```text
    /// assistant { tool_calls: [echo(...)] }   →from action_tool/action_input
    /// tool { "result" }                       →from observation
    /// ```
    ///
    /// Thought-only rows are skipped: their text is already present in the
    /// DB message stream re-seeded by the caller. Rows without an
    /// observation (an interrupted in-flight tool) are skipped too —the
    /// dangling assistant tool_call would be dropped by sanitize anyway.
    fn rebuild_tool_chain_from_steps(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
    ) {
        let Ok(steps) = self.db.get_session_steps(session_id) else {
            return;
        };
        let mut rebuilt = 0usize;
        for step in steps {
            let Some(tool) = step.action_tool else {
                continue;
            };
            let Some(obs) = step.observation else {
                continue;
            };
            // The persisted observation may be the raw tool JSON (ask etc.)
            // or a readable result; either way it is the canonical Tool text.
            let args: serde_json::Value = step
                .action_input
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Null);
            let call_id = format!("resumed_{}", step.id);
            canonical.push(CanonicalMessage::assistant(
                vec![],
                Some(vec![CanonicalToolCall {
                    id: call_id.clone(),
                    name: tool,
                    arguments: args,
                }]),
                None,
                Vec::new(),
            ));
            canonical.push(CanonicalMessage::tool(
                vec![ContentPart::text(obs)],
                Some(call_id),
            ));
            rebuilt += 1;
        }
        if rebuilt > 0 {
            tracing::info!(
                "run_session: rebuilt {} tool step(s) from session_steps for session {}",
                rebuilt,
                session_id
            );
        }
    }

    pub(crate) async fn run_session(
        &self,
        session_id: &str,
        description: &str,
        context: &str,
        conversation_history: &[ConversationMessage],
        initial_attachments: &[haven_memory::repositories::messages::MessageAttachment],
    ) -> anyhow::Result<Vec<ReActStep>> {
        tracing::debug!(
            "run_session start: session_id={:?} context={:?} attachments={}",
            session_id,
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

        // Snapshot-less resume fallback (react_state missing or unparsable):
        // the DB message stream holds only text turns, so the model would
        // lose every previously executed tool call and re-run them. The
        // session_steps table retains the full action chain (tool name, input,
        // observation); rebuild the canonical tool-call/result pairs from it
        // so the resumed context is as complete as the snapshot's would be.
        self.rebuild_tool_chain_from_steps(session_id, &mut canonical);

        let mut branch_points: HashMap<u32, BranchPoint> = HashMap::new();
        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let infer = self.spawn_infer(session_id);
        let run_id = self.react_engine.next_run_id();
        self.react_engine
            .run_react_loop(
                session_id,
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
        active_session_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        self.process_input_with_attachments(transcript, active_session_id, &[], false)
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
        active_session_id: Option<String>,
        attachments: &[haven_memory::repositories::messages::MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<ProcessResult> {
        tracing::debug!(
            "process_input: text={:?} active_session_id={:?} attachments={} voice={}",
            transcript,
            active_session_id,
            attachments.len(),
            voice
        );

        // The message is persisted BEFORE the state check on purpose: the
        // steering/supplement fallback paths below rely on it being on disk.
        // If the session turns out to be terminal, the persisted row is removed
        // again below so history never shows a ghost user message.
        let mut persisted_msg = None;
        if let Some(session_id) = active_session_id {
            match self
                .persist_message_parts(
                    &session_id,
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
                        "process_input: failed to persist user message for session {}: {}",
                        session_id,
                        e
                    );
                }
            }

            let state = self.executor.get_session_state(&session_id).await;

            // Running sessions take the message as a steering interjection,
            // injected into the ReAct loop in the gap between tool calls and
            // the final content. If the steering queue is unavailable (session
            // vanished from memory between the state read and the enqueue),
            // fall through to the supplement path instead of failing: the
            // user message is already persisted, and the supplement path
            // reloads the session / handles the terminal guard / wakes it.
            let steering_delivered = state == Some(SessionStatus::Running)
                && match self
                    .executor
                    .add_steering_with_attachments(&session_id, transcript, attachments)
                    .await
                {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(
                            "process_input: steering failed for session {} ({}); falling back to supplement path",
                            session_id,
                            e
                        );
                        false
                    }
                };

            if !steering_delivered {
                // A Paused session that is awaiting an `ask` answer: this
                // message IS the reply to the pending question. Queue it as
                // an answer so the loop injects a paired "Answer to your
                // previous question" instead of generic context —otherwise
                // the model sees the old question as still open and answers
                // questions from long ago. The awaiting-answer flavor is
                // carried by the status itself (`PausedAwaitingAnswer`), so
                // it is read BEFORE set_session_status(Pending) below clears it.
                let is_answer = matches!(
                    &state,
                    Some(s) if s.is_awaiting_answer()
                );
                let was_in_memory = if is_answer {
                    self.executor
                        .add_answer_with_attachments(&session_id, transcript, attachments)
                        .await
                        .is_ok()
                } else {
                    self.executor
                        .add_supplement_with_attachments(&session_id, transcript, attachments)
                        .await
                        .is_ok()
                };
                if !was_in_memory {
                    // Session may be stale/deleted ??fall back to creating a new session
                    if self
                        .executor
                        .ensure_session_loaded(&session_id)
                        .await
                        .is_err()
                    {
                        let session = self
                            .create_session_with_first_message(transcript, attachments, voice)
                            .await?;
                        self.events.emit_session_created(&session).await;
                        return Ok(ProcessResult::SessionCreated(session.id));
                    }
                    // Re-read state after ensure_session_loaded may have reloaded
                    // the session from DB (M3/H10 TOCTOU: end_session may have ended
                    // it between the get_session_state read above and the failed
                    // add_supplement). Only non-terminal sessions may be
                    // reactivated by a follow-up message; Completed/Error sessions
                    // were ended on purpose and must be reopened explicitly via
                    // the review flow ??auto-converting them would resurrect a
                    // ghost session.
                    let fresh_state = self.executor.get_session_state(&session_id).await;
                    if fresh_state == Some(SessionStatus::Completed)
                        || fresh_state == Some(SessionStatus::Error)
                    {
                        tracing::warn!(
                            "process_input: session {} is terminal ({:?}) despite active_session_id; dropping supplement to avoid resurrection",
                            session_id,
                            fresh_state
                        );
                        // Remove the just-persisted user message so history
                        // does not show an unanswered ghost bubble (the
                        // frontend is told to drop its copy below).
                        if let Some(msg) = persisted_msg.take() {
                            let db = self.db.clone();
                            let tid = session_id.clone();
                            let msg_id = msg.id.clone();
                            let tid_c = tid.clone();
                            let msg_id_c = msg_id.clone();
                            if let Err(e) = db
                                .run_blocking(move |db| db.delete_message_by_id(&tid_c, &msg_id_c))
                                .await
                            {
                                tracing::warn!(
                                    "process_input: failed to remove ghost user message {} for session {}: {}",
                                    msg_id,
                                    tid,
                                    e
                                );
                            }
                        }
                        // Notify the frontend so it can drop the stale
                        // activeTaskId and reset the model indicator instead of
                        // showing an orphaned bubble with no response.
                        let fresh_status =
                            fresh_state.as_ref().map(|s| s.as_str()).unwrap_or("error");
                        self.events
                            .emit_session_updated(&session_id, fresh_status)
                            .await;
                        // Do not keep the reloaded terminal session in the working
                        // set ??it was ended and should not be dispatchable.
                        self.executor.remove_session(&session_id).await;
                    } else {
                        if is_answer {
                            self.executor
                                .add_answer_with_attachments(&session_id, transcript, attachments)
                                .await?;
                        } else {
                            self.executor
                                .add_supplement_with_attachments(
                                    &session_id,
                                    transcript,
                                    attachments,
                                )
                                .await?;
                        }
                        if matches!(fresh_state, Some(s) if s.is_paused()) {
                            self.set_session_status(&session_id, SessionStatus::Pending)
                                .await?;
                        }
                    }
                    return Ok(ProcessResult::Supplemented);
                }
                if matches!(state.as_ref(), Some(s) if s.is_paused()) {
                    self.set_session_status(&session_id, SessionStatus::Pending)
                        .await?;
                }
            }
            Ok(ProcessResult::Supplemented)
        } else {
            let session = self
                .create_session_with_first_message(transcript, attachments, voice)
                .await?;
            tracing::info!("process_input created session: id={:?}", session.id);
            self.events.emit_session_created(&session).await;
            Ok(ProcessResult::SessionCreated(session.id))
        }
    }

    /// Create a new session and persist the triggering user message into it,
    /// in that order ??the message (and its attachments) must be on disk
    /// BEFORE the session is registered with the executor, otherwise the
    /// dispatcher could start the ReAct loop and miss the first user turn.
    async fn create_session_with_first_message(
        &self,
        input: &str,
        attachments: &[haven_memory::repositories::messages::MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<haven_session::SessionInfo> {
        let record = self.db.create_session(input, input)?;
        // The first user turn (and its attachments) must be on disk BEFORE
        // the dispatcher can pick the session up; if persisting fails, remove
        // the session row again so no input-less session ever gets dispatched.
        if let Err(e) = self
            .persist_message_parts(&record.id, "user", input, Some("text"), attachments, voice)
            .await
        {
            let _ = self.db.delete_session(&record.id);
            return Err(e);
        }
        self.executor.ensure_session_loaded(&record.id).await?;
        // Wake the dispatcher now that the message is persisted.
        self.executor
            .update_session_status(&record.id, SessionStatus::Pending)
            .await?;
        let session = self
            .executor
            .get_session(&record.id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not registered", record.id))?;
        Ok(session)
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
                AgentEvent::SessionCompleted { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::SessionUpdated { .. } => {
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
    async fn run_session_emits_supplement_when_additional_context_queued() {
        let tools = Arc::new(ToolsManager::new());
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);

        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let session = executor
            .create_session_with_summary("do stuff", "do stuff summary")
            .await
            .unwrap();
        executor
            .add_supplement(&session.id, "extra: remember path X")
            .await
            .unwrap();

        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());

        let sups = recorder.supplements.lock().unwrap().clone();
        assert_eq!(sups.len(), 1, "exactly one supplement event expected");
        assert_eq!(sups[0], "extra: remember path X");
        // With supplements, session pauses instead of completing (conversation mode)
        let state = executor.get_session_state(&session.id).await;
        assert_eq!(
            state,
            Some(SessionStatus::Paused),
            "session should be paused (not completed) when supplements were processed"
        );
    }

    // 鈹€鈹€鈹€ Pure-logic and data-layer tests (no LLM required) 鈹€鈹€鈹€

    fn make_test_agent() -> (Arc<AgentLayer>, Arc<SessionExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
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
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
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
        let session = agent.db.create_session("input", "transcript").unwrap();
        assert!(!session.id.is_empty());
    }

    #[test]
    fn agent_constructs_without_session_machinery() {
        let (agent, _) = make_test_agent();
        let session = agent.db.create_session("input", "").unwrap();
        assert!(!session.id.is_empty());
        // Two sessions never share message keys ??each owns its own stream.
        let other = agent.db.create_session("input2", "").unwrap();
        assert_ne!(session.id, other.id);
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
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
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

        let session = executor.create_session("test").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());
    }

    #[tokio::test]
    async fn build_system_prompt_succeeds() {
        let (agent, _) = make_test_agent();
        let prompt = agent.prompt_builder.build("test session", &[], &[]).await;
        assert!(prompt.contains("You have access to the following built-in tools"));
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
        let prompt = builder.build("test session", &[], &[]).await;

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
    async fn restore_per_session_tools_rebuilds_from_history() {
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
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools.clone(), 1));
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

        // Before restore, no per-session tools.
        let before = tools.list_schemas_for_session("ses-x").await;
        assert!(!before.iter().any(|s| s["name"] == "skill__echo"));

        agent.restore_per_session_tools("ses-x", &history).await;

        // After restore, the skill tool should be visible per-session.
        let after = tools.list_schemas_for_session("ses-x").await;
        assert!(
            after.iter().any(|s| s["name"] == "skill__echo"),
            "restored skill should appear in per-session schemas"
        );

        // Other sessions should NOT see it.
        let other = tools.list_schemas_for_session("ses-y").await;
        assert!(!other.iter().any(|s| s["name"] == "skill__echo"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persist_message_adds_to_db() {
        let (agent, _) = make_test_agent();
        let session = agent.db.create_session("input", "").unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "test message",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Read back via db
        let agent_ref = agent.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_session_messages_limit(&session.id, 50).unwrap();
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
        let session = agent.db.create_session("input", "").unwrap();
        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "看图",
                Some("text"),
                std::slice::from_ref(&att),
                false,
            )
            .await
            .unwrap();
        let agent_ref = agent.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_session_messages_limit(&session.id, 50).unwrap();
        let found = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "看图");
        assert!(found.is_some(), "persisted message not found in db");
        let msg = found.unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].media_type, "image/png");
        assert_eq!(msg.attachments[0].data, "aGVsbG8=");
    }

    #[test]
    fn parse_default_model_response_final_answer_from_text() {
        let resp = LlmResponse {
            text: "Session done.".into(),
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
        assert_eq!(thought, Some("Session done.".into()));
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

    /// M3/H10: a follow-up message must NOT resurrect a session that was ended.
    /// Terminal sessions are only reactivated explicitly via `reopen_session`
    /// (Completed/Error ??Paused) in the review flow.
    #[tokio::test]
    async fn process_input_does_not_resurrect_ended_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor.end_session(&session.id).await.unwrap();
        // end_session removes the session from the working set entirely.
        assert_eq!(executor.get_session_state(&session.id).await, None);

        let result = agent
            .process_input("more context", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        // Session is not reloaded into the working set and never becomes Pending.
        assert_eq!(executor.get_session_state(&session.id).await, None);
        assert!(executor.get_supplements(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn process_input_reactivates_paused_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let result = agent
            .process_input("more context", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let supps: Vec<String> = executor
            .get_supplements(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(supps, vec!["more context"]);
    }

    #[tokio::test]
    async fn process_input_marks_reply_as_answer_when_awaiting() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::PausedAwaitingAnswer)
            .await
            .unwrap();

        let result = agent
            .process_input("the answer", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let supps = executor.get_supplements(&session.id).await;
        assert_eq!(supps.len(), 1);
        assert!(
            supps[0].is_answer,
            "reply to an ask must be marked as answer"
        );
        assert_eq!(supps[0].text, "the answer");
        assert!(
            !executor
                .get_session_state(&session.id)
                .await
                .is_some_and(|s| s.is_awaiting_answer()),
            "reactivation must clear the awaiting-answer gate"
        );
    }

    #[tokio::test]
    async fn process_input_paused_without_ask_is_plain_supplement() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let result = agent
            .process_input("follow up", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        let supps = executor.get_supplements(&session.id).await;
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
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "assistant",
                "hi there",
                Some("text"),
                &[],
                false,
            )
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
    async fn resume_dedups_supplement_inputs_against_prefixed_canonical() {
        // Supplement/steering inputs are pushed into the canonical with a
        // text prefix ("Additional context from user: —, "Steering: —)
        // while the DB stores the raw text. A resume that only matched raw
        // text would re-inject the input as a fresh User message, making the
        // model answer it again as a new question.
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        // DB stores the RAW user text (this is what process_input persists).
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "please be brief",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Canonical carries the prefixed form (as push_user_context emits it).
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("hello"),
            CanonicalMessage::user_text("Additional context from user: please be brief"),
            CanonicalMessage::user_text("Steering: please be brief"),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
        assert_eq!(
            user_texts
                .iter()
                .filter(|t| t.as_str() == "[conversation] [user] please be brief")
                .count(),
            0,
            "supplement text already present (prefixed) must not be re-seeded: {:?}",
            user_texts
        );
    }

    #[tokio::test]
    async fn resume_keeps_repeated_same_text_turns() {
        // Two distinct turns with identical text (user said "好的" twice) are
        // both legitimate history. Count-based dedup consumes one canonical
        // occurrence per DB message; the second identical message must still
        // be re-seeded instead of being silently dropped.
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "好的", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(&session.id, "assistant", "好的", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(&session.id, "user", "好的", Some("text"), &[], false)
            .await
            .unwrap();
        // Canonical holds only the first "好的" turn pair (the second user
        // "好的" is the one missing from the snapshot).
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("好的"),
            CanonicalMessage::assistant(vec![ContentPart::text("好的")], None, None, Vec::new()),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
        assert_eq!(
            user_texts
                .iter()
                .filter(|t| t.as_str() == "[conversation] [user] 好的")
                .count(),
            1,
            "the second identical user turn must be re-seeded (count-based dedup): {:?}",
            user_texts
        );
        assert_eq!(
            user_texts.iter().filter(|t| t.as_str() == "好的").count(),
            1,
            "the first user turn must not be duplicated: {:?}",
            user_texts
        );
    }

    #[tokio::test]
    async fn resume_skips_conversation_reseed_when_canonical_is_compacted() {
        // Compaction replaces the old turns with a summary inside the
        // canonical but leaves the DB message stream untouched. Re-seeding
        // the window would resurrect every summarized-away turn and undo the
        // compaction, so a compacted canonical must skip the re-seed.
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "assistant",
                "long ago answer",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::assistant(
                vec![ContentPart::text(
                    "[Compacted summary of previous messages]: hello / long ago answer",
                )],
                None,
                None,
                Vec::new(),
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
            "compacted canonical must not be re-seeded from the DB window: {:?}",
            user_texts
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
        let session = executor.create_session("session").await.unwrap();
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "session must pause for the pending question instead of completing"
        );
        assert!(
            executor
                .get_session_state(&session.id)
                .await
                .is_some_and(|s| s.is_awaiting_answer()),
            "pause must be flagged as awaiting the user's answer"
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
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
        let session = executor.create_session("session").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "budget exhaustion must pause the session as a checkpoint"
        );
        // The notice must NOT be persisted as an assistant chat message.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
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
        let session = executor.create_session("session").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "turn must end paused after the retried final"
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
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
    async fn run_session_from_id_attaches_first_user_message_images() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor
            .create_session_with_summary("看图", "看图")
            .await
            .unwrap();
        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .persist_message_parts(&session.id, "user", "看图", Some("text"), &[att], false)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        let snapshot: crate::types::ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
    async fn run_session_from_id_ignores_later_image_supplement() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor
            .create_session_with_summary("plain session", "plain session")
            .await
            .unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "plain session",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Image arrives AFTER the session input (a supplement) ??it must not be
        // attached to the initial user turn.
        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .process_input_with_attachments("补充看图", Some(session.id.clone()), &[att], false)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        let snapshot: crate::types::ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
    async fn run_session_from_id_trims_dangling_tool_call_before_resume() {
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
        let session = executor.create_session("resume me").await.unwrap();

        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("system prompt")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("resume me")],
                tool_calls: None,
                tool_call_id: None,
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        let result = agent.run_session_from_id(&session.id).await.unwrap();

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
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "final_answer should complete the resumed session"
        );
    }

    #[tokio::test]
    async fn run_session_rebuilds_tool_chain_from_steps_without_snapshot() {
        // When react_state is missing (corrupt or schema-drifted), resume
        // falls back to a fresh run. The DB message stream holds only text,
        // so the rebuilt canonical must recover the tool-call/result pairs
        // from session_steps —otherwise the model forgets every tool it ran
        // and re-executes them.
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
        let session = executor.create_session("resume me").await.unwrap();
        // Persisted text turns (what the DB message stream holds)—
        agent
            .persist_message_parts(&session.id, "user", "resume me", Some("text"), &[], false)
            .await
            .unwrap();
        // …plus the action chain in session_steps (what a snapshot-less resume
        // must reconstruct). Use raw repo calls to avoid going through the
        // ReAct loop.
        agent
            .db
            .run_blocking({
                let session_id = session.id.clone();
                move |db| {
                    db.create_thought_step(&session_id, 1, "let me echo first")?;
                    let step = db.create_action_step(
                        &session_id,
                        2,
                        "echo",
                        r#"{"text":"hi"}"#,
                        false,
                        false,
                        None,
                    )?;
                    db.complete_action_step(&step.id, "hi", true)?;
                    Ok::<(), anyhow::Error>(())
                }
            })
            .await
            .unwrap();
        // NO react_state row: fallback path.
        assert!(agent.db.get_react_state(&session.id).unwrap().is_none());

        agent.run_session_from_id(&session.id).await.unwrap();

        {
            let seen = mock.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "fresh run after snapshot-less resume");
            let first = &seen[0];
            let roles: Vec<String> = first.iter().map(|m| m.role.to_string()).collect();
            // The rebuilt chain must appear: assistant with tool_calls,
            // followed by its tool result (sanitize may keep them intact).
            assert!(
                roles.iter().any(|r| r == "assistant"),
                "expected an assistant tool-call message: {:?}",
                roles
            );
            let rebuilt_tool = first.iter().any(|m| {
                matches!(m.role, LlmRole::Assistant)
                    && m.tool_calls.as_ref().is_some_and(|c| {
                        c.iter()
                            .any(|tc| tc.name == "echo" && tc.id.starts_with("resumed_"))
                    })
            });
            assert!(
                rebuilt_tool,
                "snapshot-less resume must rebuild the echo call from session_steps"
            );
            let rebuilt_result = first.iter().any(|m| {
                matches!(m.role, LlmRole::Tool)
                    && m.tool_call_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with("resumed_"))
            });
            assert!(
                rebuilt_result,
                "snapshot-less resume must rebuild the echo result from session_steps"
            );
        }
    }

    fn make_canonical(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text.to_string())],
            tool_calls: None,
            tool_call_id: None,
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
        // Mirrors the corruption found in a real interrupted session: a
        // compaction split the assistant(tool_calls)/tool-results pair, so
        // the summary assistant (no tool_calls) is followed by orphaned tool
        // messages. A valid pair and a dangling trailing assistant follow —
        // the dangling call is repaired with an Interrupted result instead of
        // being dropped.
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
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
            ],
            "orphaned tools must be dropped and the dangling tool_call repaired with an Interrupted result"
        );
        // The surviving pair's tool results are intact.
        assert_eq!(canonical[4].tool_call_id.as_deref(), Some("call_00_c"));
        assert_eq!(canonical[5].tool_call_id.as_deref(), Some("call_01_d"));
        // The dangling trailing call was answered with an Interrupted result.
        assert_eq!(canonical[7].tool_call_id.as_deref(), Some("call_00_e"));
        assert!(canonical[7].content.iter().any(|p| matches!(
            p,
            ContentPart::Text(t) if t.contains("Interrupted")
        )));
    }

    #[test]
    fn sanitize_canonical_repairs_partial_tool_batch() {
        // The real interrupted-batch failure: an assistant declared TWO tool
        // calls but only one result came back (the other tool was cut off
        // mid-execution). Providers reject an incomplete batch with a 400, so
        // the missing result must be repaired with an Interrupted one.
        let mut canonical = vec![
            make_canonical(CanonicalRole::User, "hi"),
            make_assistant_with_calls(&["call_a", "call_b"]),
            make_tool_result("call_a", "result a"),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::Tool,
            ],
            "the missing tool_call result must be repaired with an Interrupted result"
        );
        assert_eq!(canonical[2].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(canonical[3].tool_call_id.as_deref(), Some("call_b"));
        assert!(canonical[3].content.iter().any(|p| matches!(
            p,
            ContentPart::Text(t)
                if t.contains("Interrupted") && t.contains("tool:") && t.contains("arguments")
        )));
    }

    #[test]
    fn sanitize_canonical_repairs_interrupted_call_before_user_message() {
        // A dangling tool_call followed by a new user message: the interrupted
        // call must get an Interrupted result inserted before the user message
        // (it can no longer be trimmed as trailing), keeping the array valid.
        let mut canonical = vec![
            make_canonical(CanonicalRole::User, "a"),
            make_assistant_with_calls(&["call_1"]),
            make_canonical(CanonicalRole::User, "next"),
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
            "an Interrupted result must be inserted between the dangling call and the next user message"
        );
        assert_eq!(canonical[2].tool_call_id.as_deref(), Some("call_1"));
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
        let session = executor
            .create_session_with_summary("original", "original")
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let att =
            haven_memory::repositories::messages::MessageAttachment::new("image/png", "aGVsbG8=");
        let result = agent
            .process_input_with_attachments(
                "看图",
                Some(session.id.clone()),
                std::slice::from_ref(&att),
                false,
            )
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let supps = executor.get_supplements(&session.id).await;
        assert_eq!(supps.len(), 1);
        assert_eq!(supps[0].text, "看图");
        assert_eq!(supps[0].attachments, vec![att]);

        // Persisted with attachments in the session's message stream.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let user_msg = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "看图")
            .expect("user message persisted");
        assert_eq!(user_msg.attachments.len(), 1);
        assert_eq!(user_msg.attachments[0].media_type, "image/png");
    }

    #[tokio::test]
    async fn process_input_creates_new_session() {
        let (agent, executor) = make_test_agent();
        let result = agent.process_input("open notepad", None).await.unwrap();
        match result {
            ProcessResult::SessionCreated(session_id) => {
                assert!(!session_id.is_empty());
                let state = executor.get_session_state(&session_id).await;
                assert_eq!(state, Some(SessionStatus::Pending));
            }
            ProcessResult::Supplemented => panic!("expected SessionCreated"),
        }
    }

    #[tokio::test]
    async fn run_fact_inference_does_not_panic() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("test").await.unwrap();
        executor.end_session(&session.id).await.unwrap();
        agent.inference.infer_facts(&session.id).await;
    }

    // 鈹€鈹€鈹€ Integration tests for the ReAct core loop (refine ??1) 鈹€鈹€鈹€

    fn make_test_agent_with(
        client: Arc<dyn LlmClient>,
        tools: Arc<ToolsManager>,
    ) -> (Arc<AgentLayer>, Arc<SessionExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
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

    /// A mock tool whose schema requires an `action` field, mirroring the
    /// production failure where a call's arguments are missing a required
    /// discriminator field and the provider rejects the request body.
    struct ActionRequiredTool;
    #[async_trait]
    impl Tool for ActionRequiredTool {
        fn name(&self) -> String {
            "action_required".into()
        }
        fn description(&self) -> String {
            "requires an action".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["go", "stop"],
                        "default": "go"
                    },
                    "query": { "type": "string" }
                },
                "required": ["action", "query"]
            })
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_fills_schema_defaults() {
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        // A call missing both required fields (`action` and `query`).
        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        let input = &actions[0].tool_input;
        // `action` has a schema default ("go"); `query` has none, so it gets
        // a type-appropriate placeholder (empty string).
        assert_eq!(input["action"], "go");
        assert_eq!(input["query"], "");
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_skips_fully_populated_and_final() {
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        // Complete call: nothing to supplement.
        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": "stop", "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 0);

        // Final actions are never repaired.
        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({}),
            is_final: true,
            tool_call_id: None,
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 0);
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_repairs_null_input() {
        // Interrupted/truncated generation yields unparseable arguments,
        // which parse_default_model_response converts to Null. The repair
        // must still fill the required fields instead of shipping the bare
        // Null to the tool (which fails validate_input for every field).
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::Value::Null,
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert!(actions[0].tool_input.is_object());
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "");
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_fills_null_valued_fields() {
        // A required field explicitly set to null is as unusable as a
        // missing one: the validator rejects null for typed fields.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": null, "query": null}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "");
    }
    /// A mock tool whose required field is enum-constrained with NO schema
    /// default, mirroring the `input` tool's `operation` discriminator.
    /// The type placeholder (`""`) would violate the enum, so the repair
    /// must fall back to the first declared enum value.
    struct EnumRequiredTool;
    #[async_trait]
    impl Tool for EnumRequiredTool {
        fn name(&self) -> String {
            "enum_required".into()
        }
        fn description(&self) -> String {
            "requires an enum value".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["type", "key", "click"]
                    }
                },
                "required": ["operation"]
            })
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    /// A mock tool with an optional enum-constrained field: the value is
    /// validated when present, but the field itself is not required.
    struct EnumWithOptionalTool;
    #[async_trait]
    impl Tool for EnumWithOptionalTool {
        fn name(&self) -> String {
            "enum_with_optional".into()
        }
        fn description(&self) -> String {
            "optional enum field".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["type", "key", "click"]
                    },
                    "optional": {
                        "type": "string",
                        "enum": ["a", "b", "c"]
                    }
                },
                "required": ["operation"]
            })
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_enum_field_gets_first_value() {
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(EnumRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "enum_required".into(),
            tool_input: serde_json::json!({}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["operation"], "type");
    }

    #[tokio::test]
    async fn supplement_repairs_present_value_not_in_enum() {
        // The `action` field is PRESENT but its value is not in the schema
        // enum. Strict providers validate tool_use input against the declared
        // schema and reject the request with a 400 ("Failed to deserialize
        // the JSON body into the target type: input.action: ...") — the value
        // must be replaced with the schema default before it reaches the
        // provider, not just when the field is missing.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": "bogus", "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        // `action` falls back to the schema default "go"; the valid `query`
        // is left untouched.
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "hi");
    }

    #[tokio::test]
    async fn supplement_repairs_present_value_of_wrong_type() {
        // Same provider 400 when a field's value type contradicts the schema
        // (e.g. a number where the schema declares a string enum).
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": 42, "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "hi");
    }

    #[tokio::test]
    async fn supplement_keeps_valid_enum_values_untouched() {
        // A value that conforms to the schema (in the enum, correct type)
        // must NOT be repaired.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": "stop", "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 0);
        assert_eq!(actions[0].tool_input["action"], "stop");
    }

    #[tokio::test]
    async fn supplement_repairs_invalid_optional_field() {
        // Even a non-required property with an invalid value can trip the
        // provider's deserialization (the input object is validated as a
        // whole), so it is repaired too.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(EnumWithOptionalTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "enum_with_optional".into(),
            tool_input: serde_json::json!({"operation": "type", "optional": "nope"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["operation"], "type");
        // Optional invalid enum field falls back to the first enum value.
        assert_eq!(actions[0].tool_input["optional"], "a");
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
        fn interrupted_observations(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| {
                    if let AgentEvent::Observation { observation, .. } = e {
                        if observation.contains("Interrupted") {
                            Some(observation.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect()
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
    async fn run_session_executes_tool_then_final_answer() {
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
        let session = executor.create_session("echo hello").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");
        assert!(collector.has_action("echo"));
        assert!(collector.has_observation("echo"));
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_empty_tool_call_id_stays_consistent_in_canonical() {
        // Some providers return an empty tool_call_id. The Action side
        // synthesizes a UUID; the canonical assistant declaration must echo
        // the SAME id (not the raw empty string), otherwise the tool result
        // references an id the assistant never declared and the next request
        // is rejected with a 400.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![ToolCall {
                    id: String::new(), // provider sends empty id
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
        let session = executor.create_session("echo hello").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        // Inspect the saved snapshot's canonical: the assistant declaration
        // and the tool result must share the same (non-empty) id.
        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let mut declared: Option<String> = None;
        for m in &saved.canonical {
            if let Some(calls) = &m.tool_calls {
                for tc in calls {
                    assert!(!tc.id.is_empty(), "declared id must not be empty");
                    declared = Some(tc.id.clone());
                }
            }
            if let Some(tid) = &m.tool_call_id {
                assert_eq!(
                    Some(tid),
                    declared.as_ref(),
                    "tool result id must match the assistant's declared call id"
                );
            }
        }
        assert!(
            declared.is_some(),
            "an echo tool call must have been declared"
        );
    }

    #[tokio::test]
    async fn run_session_injects_mid_turn_steering_before_final_content() {
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
        let session = executor.create_session("echo hello").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let session_id = session.id.clone();
            async move { agent.run_session_from_id(&session_id).await }
        });
        // Wait until the echo step completed and the delayed final LLM call
        // is in flight, then deliver the user's mid-turn message.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        executor
            .add_steering(&session.id, "stop and use French")
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
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_injects_steering_between_tool_calls() {
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
        let session = executor.create_session("run tool").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let session_id = session.id.clone();
            async move { agent.run_session_from_id(&session_id).await }
        });
        // Deliver the message while delay_a (200ms) is still executing.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        executor
            .add_steering(&session.id, "add more detail")
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
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_ask_tool_pauses_and_surfaces_question() {
        // The `ask` tool signals the ReAct loop to pause and wait for the
        // user's reply (delivered as a supplement on resume). Verify the session
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
        let session = executor.create_session("decide a path").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        // Session must be paused, awaiting the user's answer.
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "ask should pause the session"
        );
        assert!(collector.has_action("ask"));
        assert!(collector.has_observation("ask"));

        // The question must be persisted so the user can see and answer it.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let found = msgs
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Which path should I take"));
        assert!(found, "question should be persisted as assistant message");
    }

    #[tokio::test]
    async fn run_session_ask_resumes_after_user_answer() {
        // After `ask` pauses the session, the user's reply arrives as a
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
        let session = executor.create_session("pick a path").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "ask should pause"
        );

        // User answers; the supplement flips the session back to Pending.
        executor
            .add_supplement(&session.id, "Use option A.")
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "session should pause again after final answer"
        );
        // The final answer text should be persisted, proving the loop resumed
        // past the `ask` step and reached final_answer.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
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
        let session = executor.create_session("ask retry").await.unwrap();

        // Turn 1: the ask pauses the session.
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer)
        );

        // Turn 2: the user answers; the resumed step fails mid-stream.
        executor.add_supplement(&session.id, "Yes").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        let _ = agent.run_session_from_id(&session.id).await;
        // The failed run ended in Error; terminal cleanup removed the session
        // from the working set.
        assert_eq!(executor.get_session_state(&session.id).await, None);

        // Turn 3: retry via continue_session ??Pending ??re-run.
        agent.continue_session(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        agent.run_session_from_id(&session.id).await.unwrap();

        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let steps = agent.db.get_session_steps(&session.id).unwrap();

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
    async fn run_session_notify_tool_emits_notification_without_pausing() {
        // The `notify` tool signals the ReAct loop to emit a Notification
        // event (in-app toast + Windows). Unlike `ask`, it must NOT pause the
        // session: the loop continues to the final answer.
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
        let session = executor.create_session("build and notify").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");

        // The Notification event must carry the tool's title/body.
        let (title, body) = collector
            .has_notification()
            .expect("notify should emit a Notification event");
        assert_eq!(title, "Build");
        assert_eq!(body, "Compilation finished");

        // The chat/review observation must be readable, not raw JSON.
        assert!(collector.has_observation("notify"));

        // Unlike `ask`, notify must not pause the session mid-loop: the loop
        // continued past the notify step (history has 2 steps) and reached the
        // normal end state (Paused = conversation mode, waiting for follow-up).
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_multiple_asks_surface_all_questions() {
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
        let session = executor.create_session("two questions").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "ask should pause"
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
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
    async fn run_session_parallel_tool_execution() {
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
        let session = executor.create_session("run parallel").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(collector.has_action("delay_a"));
        assert!(collector.has_action("delay_b"));
        let step1_tool_entries = history
            .iter()
            .filter(|s| s.step_number == 1 && s.action.is_some())
            .count();
        assert_eq!(
            step1_tool_entries, 2,
            "each parallel tool must have its own history entry (the old code kept only the last one)"
        );
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
    async fn run_session_cancelled_mid_batch_surfaces_interrupted_tools() {
        // A tool batch cancelled mid-flight must NOT silently drop the
        // in-flight calls: each one is repaired with an "Interrupted"
        // observation (so the UI shows it and the model can retry) and the
        // snapshot canonical stays a valid assistant/tool chain.
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
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
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
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("run parallel").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let session_id = session.id.clone();
            async move { agent.run_session_from_id(&session_id).await }
        });
        // Wait until both action events were emitted (the assistant message
        // with tool_calls is in canonical and the drain loop is running), then
        // cancel while both tools (200ms sleeps) are still in flight.
        for _ in 0..50 {
            if collector.has_action("delay_a") && collector.has_action("delay_b") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            collector.has_action("delay_a") && collector.has_action("delay_b"),
            "batch must have started before the cancel"
        );
        // end_session registers a real cancellation token (entry().or_insert) and
        // cancels it — the same path the frontend's "end session" button uses —
        // so the in-flight tool batch observes the cancellation mid-drain.
        executor.end_session(&session.id).await.unwrap();
        let history = run.await.unwrap().unwrap();

        // Every in-flight tool got an "Interrupted" observation emitted to
        // the UI — the cancelled tools are not silently skipped.
        let interrupted = collector.interrupted_observations();
        assert_eq!(
            interrupted.len(),
            2,
            "both in-flight tools must emit an Interrupted observation"
        );
        // The observation must carry the tool name and the attempted arguments
        // (field supplementation), not a bare "Interrupted" marker, so the UI
        // and the model can see exactly which call was cut off.
        let all_text = interrupted.join("\n");
        for tool in ["delay_a", "delay_b"] {
            assert!(
                all_text.contains(tool),
                "interrupted observation must name tool '{}' (got: {})",
                tool,
                all_text
            );
        }
        assert!(
            all_text.contains("arguments"),
            "interrupted observation must carry the attempted arguments (got: {})",
            all_text
        );
        // The history recorded the interrupted steps so a resume keeps them.
        let interrupted_steps = history
            .iter()
            .filter(|s| {
                s.observation
                    .as_deref()
                    .is_some_and(|o| o.contains("Interrupted"))
            })
            .count();
        assert_eq!(
            interrupted_steps, 2,
            "interrupted tool calls must be recorded in history"
        );
        // The history entries must also carry the enriched fields.
        assert!(
            history
                .iter()
                .all(|s| s.observation.as_deref().is_none_or(|o| {
                    !o.contains("Interrupted") || (o.contains("tool:") && o.contains("arguments"))
                })),
            "interrupted history observations must carry tool name and arguments"
        );
        // The saved snapshot canonical stays a valid assistant/tool chain: no
        // dangling assistant tool_calls without a following result (which
        // providers would reject as a 400 on resume).
        let state_json = agent
            .db
            .get_react_state(&session.id)
            .unwrap()
            .expect("exit snapshot must be saved after mid-batch cancel");
        let snapshot: ReActSnapshot = serde_json::from_str(&state_json).unwrap();
        let mut pending: Vec<String> = Vec::new();
        let mut interrupted_with_fields = 0;
        for m in &snapshot.canonical {
            match m.role {
                CanonicalRole::Tool => {
                    if let Some(cid) = &m.tool_call_id {
                        if let Some(pos) = pending.iter().position(|p| p == cid) {
                            pending.remove(pos);
                        }
                    } else if let Some(cid) = pending.pop() {
                        let _ = cid;
                    }
                    // The repaired Interrupted tool results must carry the
                    // tool name and arguments so a resume sees what happened.
                    if m.content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Text(t) if t.contains("Interrupted")))
                    {
                        interrupted_with_fields += 1;
                        assert!(
                            m.content.iter().any(|p| matches!(
                                p,
                                ContentPart::Text(t) if t.contains("tool:") && t.contains("arguments")
                            )),
                            "repaired Interrupted result must include tool name and arguments: {:?}",
                            m.content
                        );
                    }
                }
                CanonicalRole::Assistant => {
                    pending = m
                        .tool_calls
                        .as_ref()
                        .map(|tc| tc.iter().map(|t| t.id.clone()).collect())
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
        assert_eq!(
            interrupted_with_fields, 2,
            "both interrupted tool results in the snapshot must carry fields"
        );
        assert!(
            pending.is_empty(),
            "snapshot canonical must not end with unanswered tool_calls (got {:?})",
            pending
        );
    }

    #[tokio::test]
    async fn pause_snapshot_and_resume_keep_own_final_answer_in_canonical() {
        // The pause snapshot must end with the agent's own final answer (not
        // right after the tool results), so a resume sees the completed
        // answer BEFORE the follow-up instead of having the re-seed re-insert
        // it at the transcript head, out of order.
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("First answer.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Second answer.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
            }),
        ]));
        let client: Arc<dyn LlmClient> = mock.clone();
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db.clone(),
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("question one").await.unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );

        let state_json = db
            .get_react_state(&session.id)
            .unwrap()
            .expect("snapshot must exist after the pause");
        let snapshot: ReActSnapshot = serde_json::from_str(&state_json).unwrap();
        let last = snapshot.canonical.last().expect("canonical not empty");
        assert_eq!(
            last.role,
            CanonicalRole::Assistant,
            "pause snapshot canonical must end with the agent's own answer"
        );
        let snapshot_text: String = last
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            snapshot_text.contains("First answer."),
            "snapshot canonical must carry the final answer, got: {snapshot_text:?}"
        );

        // Resume with a follow-up: the next LLM request must show the agent's
        // own completed answer BEFORE the injected follow-up, so the model
        // answers with knowledge of what it already said.
        executor
            .add_supplement(&session.id, "next question")
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        let seen = mock.seen.lock().unwrap();
        assert!(
            seen.len() >= 2,
            "expected a resumed request, got {:?}",
            seen.len()
        );
        let last_req = seen.last().unwrap();
        let idx_answer = last_req.iter().position(|m| {
            matches!(m.role, LlmRole::Assistant)
                && m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("First answer.")))
        });
        let idx_followup = last_req.iter().position(|m| {
            matches!(m.role, LlmRole::User)
                && m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("next question")))
        });
        let roles: Vec<String> = last_req.iter().map(|m| m.role.to_string()).collect();
        assert!(
            idx_answer.is_some(),
            "resumed request must contain the agent's own answer, roles: {roles:?}"
        );
        assert!(
            idx_followup.is_some(),
            "resumed request must contain the follow-up, roles: {roles:?}"
        );
        assert!(
            idx_answer.unwrap() < idx_followup.unwrap(),
            "the agent's own answer must precede the follow-up message"
        );
    }

    #[tokio::test]
    async fn run_session_compaction_retry_on_context_exceeded() {
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
        let session = executor.create_session("test compaction").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(
            collector.has_compaction(),
            "Compaction event should be emitted"
        );
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_context_exceeded_compaction_fails() {
        let tools = Arc::new(ToolsManager::new());
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Err(
            LlmError::ContextLengthExceeded,
        )]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("compaction fail").await.unwrap();
        let result = agent.run_session_from_id(&session.id).await;
        assert!(result.is_err(), "should error when compaction fails");
        // Terminal cleanup removed the session from the working set.
        assert_eq!(executor.get_session_state(&session.id).await, None);
    }

    #[tokio::test]
    async fn continue_session_resumes_errored_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("test continue").await.unwrap();
        // Simulate an errored session with a saved snapshot.
        agent
            .db
            .update_session_status(&session.id, "error")
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();
        let snapshot = ReActSnapshot {
            canonical: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            }],
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        // Add a partial assistant message that should be cleaned up.
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(
                &session.id,
                "assistant",
                "partial output",
                Some("text"),
                None,
            )
            .unwrap();

        agent.continue_session(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        // The partial output should have been deleted (only the user message remains).
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[tokio::test]
    async fn continue_session_non_error_fails() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("not error").await.unwrap();
        // Session is Pending, not Error ??should refuse.
        let result = agent.continue_session(&session.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rollback_without_react_state_truncates_messages() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("no state").await.unwrap();
        // No react_state saved ??simulate an old session that errored before
        // snapshots were persisted.
        agent
            .db
            .update_session_status(&session.id, "error")
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "partial", Some("text"), None)
            .unwrap();
        let hello_id = agent
            .db
            .get_session_messages(&session.id)
            .unwrap()
            .into_iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id;

        // User-message rollback (pause=true) should truncate from the user msg.
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(msgs.is_empty(), "messages should be empty after rollback");
    }

    #[tokio::test]
    async fn rollback_with_snapshot_no_branch_point_uses_snapshot() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("no bp").await.unwrap();
        // Save a snapshot with no branch_points at the target step.
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "partial", Some("text"), None)
            .unwrap();

        // Rollback to step 1 with pause=false (agent rollback).
        agent
            .rollback_session(&session.id, 1, false, None)
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        // The partial assistant message should be deleted, user message kept.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[tokio::test]
    async fn rollback_pause_true_removes_user_message_from_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("user rollback").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        // Branch point at step 1: canonical ends at the user message, but
        // last_msg_at points at the thought that was persisted AFTER it (the
        // realistic shape saved by save_branch_point).
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
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
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // User-message rollback: the user message itself must be removed from
        // the session (its text returns to the composer for editing) ??not
        // left behind to reappear on the next review rebuild.
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.is_empty(),
            "user message should be deleted from the session, got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn rollback_fallback_no_branch_point_pause_true_deletes_from_last_user_message() {
        // Regression: rollback to a step that has NO branch point (e.g. the
        // step failed before save_branch_point ran) falls back to a cutoff
        // derived from session messages. With pause=true the user message
        // itself must be removed too — and because the clicked message's
        // live-view id never matches a DB id, the backend can only guess the
        // target from the newest user message at/before the cutoff.
        let (agent, executor) = make_test_agent();
        let session = executor
            .create_session("fallback user rollback")
            .await
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "first", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply1", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "second", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply2", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let reply1_ts = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.content == "reply1")
            .unwrap()
            .created_at
            .clone();
        let second_id = msgs
            .iter()
            .find(|m| m.content == "second")
            .unwrap()
            .id
            .clone();
        // Snapshot with a branch point ONLY at step 1; the target step 2 has
        // no branch point (realistic: step 2's save_branch_point never ran).
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(reply1_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 2,
            branch_points,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // The user clicked "second" (the newest user message, whose id
        // resolves to a persisted row).
        agent
            .rollback_session(&session.id, 2, true, Some(&second_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["first", "reply1"],
            "rollback must delete the clicked user message and everything after it: {:?}",
            contents
        );
    }

    #[tokio::test]
    async fn rollback_errors_when_target_message_id_does_not_match() {
        // Regression: user-message rollback used to fall back to matching by
        // content when the clicked message's id missed, and to guessing the
        // newest user message when even that failed. Both guesses could
        // delete the wrong message; an unresolvable id is now a direct error
        // and the session is left untouched.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("strict rollback").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "first question", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply A", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "second question", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply B", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let reply_a_ts = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.content == "reply A")
            .unwrap()
            .created_at
            .clone();
        // Branch point at step 1 only; target step 2 has none (fallback).
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(reply_a_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 2,
            branch_points,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // The live-view id never matches a DB id and no content fallback
        // exists anymore: rollback must error and delete nothing.
        let err = agent
            .rollback_session(&session.id, 1, true, Some("live-view-local-id"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not found in session messages"),
            "unexpected error: {}",
            err
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(msgs.len(), 4, "no message may be deleted on error");
    }

    /// Seed the common rollback-test fixture: messages `hello` / `thinking` /
    /// `interrupt` plus a saved ReAct snapshot at step 1 (canonical =
    /// [System "sys", User "hello"], branch point after the thinking turn).
    /// Returns the persisted messages so tests can resolve specific ids.
    fn seed_hello_snapshot(
        agent: &AgentLayer,
        session_id: &str,
    ) -> Vec<haven_memory::repositories::messages::Message> {
        agent
            .db
            .add_message(session_id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(session_id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(session_id, "user", "interrupt", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(session_id).unwrap();
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
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
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
            .save_react_state(session_id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        msgs
    }

    #[tokio::test]
    async fn rollback_orphan_after_processed_turn_preserves_earlier_history() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("orphan rollback").await.unwrap();
        let msgs = seed_hello_snapshot(&agent, &session.id);

        // Roll back the interrupted message: only it must be discarded; the
        // earlier exchange ("hello" / "thinking") survives.
        let interrupt_id = msgs
            .iter()
            .find(|m| m.content == "interrupt")
            .unwrap()
            .id
            .clone();
        agent
            .rollback_session(&session.id, 1, true, Some(&interrupt_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
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
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
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
        let session = executor.create_session("processed rollback").await.unwrap();
        let msgs = seed_hello_snapshot(&agent, &session.id);

        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.is_empty(),
            "rollback of the processed message must wipe the orphan too, got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "rollback of a processed message must truncate the canonical"
        );
    }

    #[tokio::test]
    async fn rollback_pause_uses_target_message_ts_not_latest_user() {
        // A steering interjection persisted between the rolled-back user
        // message and the branch point must NOT hijack the delete range: the
        // target message's own timestamp wins, so the target is removed too.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("target ts").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        // A steering interjection persisted after "hello" but BEFORE the
        // branch-point thought timestamp (the user typed while the agent was
        // working on the first step).
        agent
            .db
            .add_message(
                &session.id,
                "user",
                "also check the time",
                Some("text"),
                None,
            )
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
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
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
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
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // Roll back "hello" specifically —the steering interjection must
        // NOT keep "hello" alive.
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.is_empty(),
            "rolling back 'hello' must delete it (and the interjection), got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "canonical must not keep the rolled-back user message"
        );
    }

    #[tokio::test]
    async fn rollback_pause_matches_prefixed_supplement_in_canonical() {
        // The canonical stores supplement/steering inputs with a prefix
        // ("Steering: —, "Additional context from user: —) while the DB
        // stores the raw text. Rolling back such a message must find the
        // prefixed canonical entry (not merely the last User), so the
        // message is removed from the restored context.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("prefixed rollback").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "do it", Some("text"), None)
            .unwrap();
        // The steering is injected BEFORE the step's LLM call, so it is
        // persisted before the branch-point thought timestamp.
        agent
            .db
            .add_message(&session.id, "user", "use French", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let steering_id = msgs
            .iter()
            .find(|m| m.content == "use French")
            .unwrap()
            .id
            .clone();
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
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("do it")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            // The steering was pushed into the canonical with its prefix.
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("Steering: use French")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            2,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 2,
                last_msg_at: Some(thinking_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 2,
            branch_points,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent
            .rollback_session(&session.id, 2, true, Some(&steering_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "the steering message itself must be deleted, 'do it' stays: {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        assert_eq!(msgs[0].content, "do it");
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User
                    && m.content.iter().any(|p| matches!(
                        p,
                        ContentPart::Text(t) if t.contains("use French")
                    ))),
            "the prefixed steering entry must be trimmed from the canonical"
        );
        assert!(
            restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User
                    && m.content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Text(t) if t == "do it"))),
            "'do it' must stay in the canonical"
        );
    }
}
