use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::types::{LlmMessage, LlmRole};
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition, ToolFunction};
use haven_memory::Database;
use haven_memory::repositories::messages::MessageAttachment;
use haven_task::{TaskExecutor, TaskStatus};
use haven_tools::is_silent_action;

use crate::compactor::ContextCompactor;
use crate::event::{AgentEventEmitter, EventDispatcher, UsagePayload};
use crate::types::{Action, BranchPoint, ReActStep};

/// Convert a stored message attachment into a content part for the LLM.
/// Images become vision content parts (base64 payload); non-image file
/// attachments (persisted on disk with a `path`) become a short text
/// reference so the agent knows the file exists and where to read it with
/// the file tool — the raw bytes are never shipped to the model.
pub(crate) fn attachment_to_content_part(att: &MessageAttachment) -> ContentPart {
    if att.is_image() {
        ContentPart::Image {
            content_type: "image_url".into(),
            media_type: att.media_type.clone(),
            data: att.data.clone(),
        }
    } else {
        let name = att.filename.as_deref().unwrap_or("attachment");
        match &att.path {
            Some(path) => ContentPart::text(format!("[附件: {name}，路径: {path}]")),
            None => ContentPart::text(format!("[附件: {name}]")),
        }
    }
}

/// Pick the endpoint role for an agent step. Conversations that carry image
/// content parts route through the router's vision role 鈥?the dedicated
/// `image_model` (vision-capable) endpoint when configured, otherwise the
/// default model. Everything else uses the default model.
async fn choose_agent_role(router: &LlmRouter, messages: &[LlmMessage]) -> EndpointRole {
    let has_image = messages.iter().any(|m| {
        m.content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. }))
    });
    if has_image {
        router.vision_role().await
    } else {
        EndpointRole::DefaultModel
    }
}

/// Upper bound for the pause-wait before re-checking task state. The status
/// notifier is edge-triggered, so a transition that fires between the state
/// check and the wait registration would otherwise be lost and hang the
/// handler forever. Bounded polling converts that into an extra latency.
const PAUSE_POLL_MS: u64 = 500;

/// Interval (in ReAct steps) at which long-running tasks re-run fact and
/// preference inference mid-task, so memory is refreshed before the task
/// ever pauses or completes.
const FACT_INFER_INTERVAL_STEPS: u32 = 25;

/// Message persisted when a run exhausts its step budget (`max_steps`). The
/// task is intentionally paused as a checkpoint — the task is NOT finished,
/// and the next user message resumes it with a fresh budget. System notices
/// like this must NOT land in the chat as an assistant bubble; they are
/// surfaced as a notification (in-app toast + Windows) instead.
const BUDGET_EXHAUSTED_TITLE: &str = "任务步骤上限已用尽";
const BUDGET_EXHAUSTED_BODY: &str = "本轮运行的步骤上限已用完，任务已暂停。发一条消息即可继续。";

/// Nudge appended to the retry call when a text-only response looks cut off
/// (truncated generation or text ending mid-sentence). The retry is private
/// to the loop — the nudge is never persisted into the canonical, so the
/// conversation stream stays clean if the retry succeeds or falls back.
const CUT_OFF_RETRY_NUDGE: &str =
    "Your previous response was cut off before you finished. Please continue and complete it.";

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<TaskExecutor>,
    db: Arc<Database>,
    max_steps: Mutex<u32>,
    max_observation_chars: usize,
    message_window_size: usize,
    balanced_model_notified: Mutex<HashSet<String>>,
    run_counter: AtomicU64,
    current_run_id: AtomicU64,
    /// Per-task cumulative token usage. Keyed by `task_id` so multiple
    /// parallel tasks each track their own counters. Reset on task
    /// completion to avoid leaking finished-task entries.
    cumulative_usage: Mutex<HashMap<String, CumulativeUsage>>,
    /// Reusable serialization buffer for ReAct snapshots (see
    /// `save_snapshot_with_branches`): avoids a fresh allocation for every
    /// per-step snapshot write.
    snapshot_buf: Mutex<Vec<u8>>,
}

/// Borrowed serialization view of a `ReActSnapshot`. Serializing this instead
/// of building an owned `ReActSnapshot` skips the per-step deep copies of
/// canonical/history/branch_points (which accumulate to O(n²) over a long
/// task). Field names/shape match `ReActSnapshot` exactly so the persisted
/// JSON stays wire-compatible.
#[derive(serde::Serialize)]
struct SnapshotView<'a> {
    canonical: &'a [CanonicalMessage],
    history: &'a [ReActStep],
    step_number: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    branch_points: &'a HashMap<u32, BranchPoint>,
}

#[derive(Debug, Clone, Default)]
struct CumulativeUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cost_usd: f64,
    has_cost: bool,
}

impl From<haven_memory::repositories::usage::TaskUsage> for CumulativeUsage {
    fn from(u: haven_memory::repositories::usage::TaskUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cost_usd: u.cost_usd,
            has_cost: u.has_cost,
        }
    }
}

impl ReActEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<LlmRouter>,
        executor: Arc<TaskExecutor>,
        db: Arc<Database>,
        max_steps: u32,
        max_observation_chars: usize,
        message_window_size: usize,
    ) -> Self {
        Self {
            router: Arc::new(RwLock::new(router)),
            executor,
            db,
            max_steps: Mutex::new(max_steps),
            max_observation_chars,
            message_window_size,
            balanced_model_notified: Mutex::new(HashSet::new()),
            run_counter: AtomicU64::new(0),
            current_run_id: AtomicU64::new(0),
            cumulative_usage: Mutex::new(HashMap::new()),
            snapshot_buf: Mutex::new(Vec::new()),
        }
    }

    pub fn replace_router(&self, new_router: Arc<LlmRouter>) {
        *self.router.write().unwrap() = new_router;
    }

    pub fn set_max_steps(&self, max_steps: u32) {
        *self.max_steps.lock().unwrap() = max_steps;
    }

    pub fn next_run_id(&self) -> u64 {
        self.run_counter.fetch_add(1, Ordering::SeqCst)
    }

    pub fn current_run_id(&self) -> u64 {
        self.current_run_id.load(Ordering::SeqCst)
    }

    pub fn set_current_run_id(&self, id: u64) {
        self.current_run_id.store(id, Ordering::SeqCst);
    }

    /// Live connectivity probe to the default-model endpoint (GET /models).
    /// Used by the top-right status indicator to show Ready/Disconnected.
    pub async fn check_connection(&self) -> bool {
        let router = self.router();
        router
            .health_check(haven_llm::EndpointRole::DefaultModel)
            .await
            .is_ok()
    }

    fn router(&self) -> Arc<LlmRouter> {
        self.router.read().unwrap().clone()
    }

    /// Build the full tool-definition list for a task: global registry tools
    /// plus per-task skill/MCP adapters registered via `load_skill`/`load_mcp`.
    /// Called each step so freshly loaded tools are immediately visible.
    async fn build_tool_definitions_for_task(&self, task_id: &str) -> Vec<ToolDefinition> {
        let schemas = self
            .executor
            .get_tools()
            .list_schemas_for_task(task_id)
            .await;
        schemas
            .iter()
            .map(|s| {
                let name = s["name"].as_str().unwrap_or("");
                let desc = s["description"].as_str().unwrap_or("");
                let params = s["input_schema"].clone();
                ToolDefinition {
                    tool_type: "function".into(),
                    function: ToolFunction {
                        name: name.into(),
                        description: desc.into(),
                        parameters: params,
                    },
                }
            })
            .collect()
    }

    /// Drain user-facing context into the canonical message list: supplements
    /// (paused-task replies), steering (mid-run user interjections) and
    /// completed background-job results. Each becomes a `User` message so the
    /// agent sees it on the next LLM call.
    ///
    /// Returns `true` when at least one message was injected. Called at the
    /// top of every step, and again right before a step completes with final
    /// content — a message that arrived while the LLM call was in flight is
    /// delivered there instead of being deferred until the turn ends.
    async fn inject_pending_context(
        &self,
        task_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        step_num: u32,
        run_id: u64,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) -> bool {
        let mut injected = false;

        let supplements = self.executor.get_supplements(task_id).await;
        for supplement in &supplements {
            // A reply to a pending `ask` is injected as a paired answer so
            // the model sees the old question as resolved instead of treating
            // it as a second open question to answer again.
            let prefix = if supplement.is_answer {
                "Answer to your previous question"
            } else {
                "Additional context from user"
            };
            self.push_user_context(
                task_id,
                step_num,
                run_id,
                emitter,
                canonical,
                prefix,
                &supplement.text,
                &supplement.attachments,
            )
            .await;
            injected = true;
        }

        let steering = self.executor.get_steering(task_id).await;
        for s in &steering {
            self.push_user_context(
                task_id,
                step_num,
                run_id,
                emitter,
                canonical,
                "Steering",
                &s.text,
                &s.attachments,
            )
            .await;
            injected = true;
        }

        // Deliver completed background-job results as context. These are
        // kept separate from the steering queue so job output is never
        // mistaken for a user reply (which would let the `ask` pause path
        // resume the task without the user's answer).
        let job_results = self.executor.drain_job_completions(task_id).await;
        for s in &job_results {
            canonical.push(CanonicalMessage::user_text(format!("Steering: {s}")));
            injected = true;
        }

        injected
    }

    /// Emit a Supplement event, persist a matching thought-step row and push
    /// a user message into the canonical array. Shared by the supplement and
    /// steering queues (identical mechanics, different text prefixes) so the
    /// two paths cannot drift. The thought-step row lets the session message
    /// be matched back to a step after a reload; without it an interrupted
    /// input would have no determinable step for review/rollback.
    #[allow(clippy::too_many_arguments)]
    async fn push_user_context(
        &self,
        task_id: &str,
        step_num: u32,
        run_id: u64,
        emitter: &Arc<dyn AgentEventEmitter>,
        canonical: &mut Vec<CanonicalMessage>,
        prefix: &str,
        text: &str,
        attachments: &[MessageAttachment],
    ) {
        emitter
            .emit(crate::event::AgentEvent::Supplement {
                task_id: task_id.into(),
                additional_context: text.to_string(),
                step_number: step_num,
                run_id,
            })
            .await;
        let _ = self
            .db
            .run_blocking({
                let task_id = task_id.to_string();
                let text = text.to_string();
                move |db| db.create_thought_step(&task_id, step_num as i32, &text)
            })
            .await;
        let mut content = vec![ContentPart::text(format!("{prefix}: {text}"))];
        content.extend(attachments.iter().map(attachment_to_content_part));
        canonical.push(CanonicalMessage::user(content));
    }

    /// Shared ReAct loop body. Runs from `start_step` through `max_steps`.
    /// Called by both `run_task` (fresh) and `run_task_resumed` (resumed from
    /// snapshot).
    ///
    /// Tool definitions are rebuilt at the top of each step so that tools
    /// loaded via `load_skill` / `load_mcp` (registered per-task) become
    /// visible to the LLM on the very next step.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_react_loop(
        &self,
        task_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
        start_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn() + Send + Sync),
        run_id: u64,
    ) -> anyhow::Result<()> {
        let max_steps = *self.max_steps.lock().unwrap();
        // When resuming past the configured cap (e.g. a task that used all
        // `max_steps` then paused for the user's next turn), give the loop
        // another full budget so the resume doesn't degenerate into an
        // immediate budget-exhaustion pause below. This intentionally
        // re-budgets on every resume — a task can run `max_steps` per run,
        // not once per task lifetime (documented in refactor-dedup.md A9).
        let effective_max = max_steps.max(start_step.saturating_sub(1).saturating_add(max_steps));
        let mut last_step = start_step.saturating_sub(1);
        // Guard so an empty or cut-off model response is retried at most once
        // per run. Initialized outside the step loop so the assignment below
        // is read by later iterations (keeps the lint clean).
        let mut retried_empty = false;

        for step_num in start_step..=effective_max {
            last_step = step_num;
            loop {
                // Check cancellation first: end_task / rollback cancel the
                // token, so the loop must exit silently without touching
                // status or emitting events. The state check below would
                // otherwise observe the Error sentinel of a task that
                // end_task already removed from memory and announce a
                // spurious "task interrupted" error.
                let cancel = self.executor.cancellation_token(task_id).await;
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    TaskStatus::Error | TaskStatus::Completed => {
                        if state != TaskStatus::Completed
                            && self
                                .executor
                                .list_tasks()
                                .await
                                .iter()
                                .any(|t| t.id == task_id)
                        {
                            self.emit_error(&emitter, task_id, "task interrupted").await;
                        }
                        return Ok(());
                    }
                    TaskStatus::Paused => {
                        self.save_snapshot_with_branches(
                            task_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                    }
                    _ => break,
                }
                if self.executor.get_task_state(task_id).await == TaskStatus::Paused {
                    // notify_waiters only wakes waiters registered at the
                    // moment of the notify: a transition between the state
                    // check above and the wait below would be lost and block
                    // forever. Bound the wait and re-evaluate state on timeout.
                    let notifier = self.executor.status_notifier(task_id).await;
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(PAUSE_POLL_MS),
                        notifier.notified(),
                    )
                    .await;
                }
            }

            // Deliver user interjections (supplements, steering) and
            // background-job results as context at the top of each step so
            // they land in the gap between tool calls and the next LLM call.
            self.inject_pending_context(task_id, canonical, step_num, run_id, &emitter)
                .await;

            self.maybe_compact(task_id, canonical, &emitter).await;

            // Incremental fact/preference inference on long-running tasks:
            // turns that never pause would otherwise only trigger extraction
            // at the very end. Every FACT_INFER_INTERVAL_STEPS steps we
            // re-run inference; the upsert/known-facts machinery makes this
            // idempotent (re-confirmed facts are reinforced, not duplicated).
            if step_num % FACT_INFER_INTERVAL_STEPS == 0 {
                infer();
            }

            // No canonical may be sent to the LLM containing a tool message
            // without a preceding assistant tool_calls (providers reject it
            // with a 400). Sanitize as a final gate so compaction or a
            // mid-batch interruption can never poison a request.
            crate::sanitize_canonical(canonical);

            // Rebuild tool definitions each step so that per-task tools
            // registered by `load_skill` / `load_mcp` are visible to the LLM.
            let tools: Vec<ToolDefinition> = self.build_tool_definitions_for_task(task_id).await;

            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
            let role = choose_agent_role(&router, &llm_messages).await;
            // Accumulate streamed text locally so that if the LLM call fails
            // mid-stream, we can persist whatever was already received instead
            // of losing it entirely.
            let partial_thought: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(String::new()));
            let partial_reasoning: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(String::new()));
            tracing::debug!(
                "ReAct step {} calling LLM, {} messages, {} tools",
                step_num,
                llm_messages.len(),
                tools.len()
            );
            tracing::trace!(
                "ReAct step {} canonical messages: {:?}",
                step_num,
                llm_messages
                    .iter()
                    .map(|m| (m.role.clone(), m.content.len()))
                    .collect::<Vec<_>>()
            );
            let response = match self
                .stream_llm_step(
                    router.clone(),
                    role,
                    llm_messages,
                    tools.to_vec(),
                    cancel_res.clone(),
                    &emitter,
                    task_id,
                    step_num,
                    run_id,
                    &partial_thought,
                    &partial_reasoning,
                )
                .await
            {
                Ok(resp) => {
                    if router.balanced_model_active() {
                        self.emit_balanced_model(&emitter, task_id, "switching to balanced model")
                            .await;
                    }
                    self.record_usage_and_emit(task_id, role, &resp, &emitter)
                        .await;
                    resp
                }
                Err(haven_llm::LlmError::ContextLengthExceeded) => {
                    tracing::warn!(
                        "context length exceeded for task {}, forcing compaction",
                        task_id
                    );
                    if let Some(result) = {
                        let compactor = self.context_compactor(role).await;
                        compactor.compact(canonical, &self.router()).await
                    } {
                        tracing::debug!(
                            "compacted {} 鈫?{} tokens",
                            result.tokens_before,
                            result.tokens_after
                        );
                        *canonical = result.compacted;
                        EventDispatcher::emit_compaction_from(
                            &emitter,
                            task_id,
                            &result.summary,
                            result.tokens_before,
                            result.tokens_after,
                        )
                        .await;
                        let llm_messages2 = haven_llm::types::convert_to_llm(canonical.clone());
                        // Reset the accumulators: the first attempt's partial
                        // text was based on pre-compaction context and should
                        // not be mixed with the retry's output.
                        partial_thought.lock().unwrap().clear();
                        partial_reasoning.lock().unwrap().clear();
                        match self
                            .stream_llm_step(
                                router.clone(),
                                role,
                                llm_messages2,
                                tools.to_vec(),
                                cancel_res.clone(),
                                &emitter,
                                task_id,
                                step_num,
                                run_id,
                                &partial_thought,
                                &partial_reasoning,
                            )
                            .await
                        {
                            Ok(retry_resp) => {
                                self.record_usage_and_emit(task_id, role, &retry_resp, &emitter)
                                    .await;
                                retry_resp
                            }
                            Err(haven_llm::LlmError::Cancelled) => {
                                return Ok(());
                            }
                            Err(e2) => {
                                let err_msg = format!("Compaction retry also failed: {}", e2);
                                self.persist_partial_on_error(
                                    task_id,
                                    step_num,
                                    run_id,
                                    &partial_thought,
                                    &partial_reasoning,
                                    canonical,
                                    history,
                                    branch_points,
                                    &emitter,
                                )
                                .await;
                                self.emit_error(&emitter, task_id, &err_msg).await;
                                self.executor
                                    .update_task_status(task_id, TaskStatus::Error)
                                    .await?;
                                return Err(anyhow::anyhow!("{}", err_msg));
                            }
                        }
                    } else {
                        let err_msg = "context length exceeded but compaction failed".to_string();
                        self.persist_partial_on_error(
                            task_id,
                            step_num,
                            run_id,
                            &partial_thought,
                            &partial_reasoning,
                            canonical,
                            history,
                            branch_points,
                            &emitter,
                        )
                        .await;
                        EventDispatcher::emit_task_error_from(&emitter, task_id, &err_msg).await;
                        self.executor
                            .update_task_status(task_id, TaskStatus::Error)
                            .await?;
                        return Err(anyhow::anyhow!("{}", err_msg));
                    }
                }
                Err(haven_llm::LlmError::Cancelled) => {
                    return Ok(());
                }
                Err(e) => {
                    let err_msg = format!("Both default model and balanced model failed: {}", e);
                    self.persist_partial_on_error(
                        task_id,
                        step_num,
                        run_id,
                        &partial_thought,
                        &partial_reasoning,
                        canonical,
                        history,
                        branch_points,
                        &emitter,
                    )
                    .await;
                    EventDispatcher::emit_task_error_from(&emitter, task_id, &err_msg).await;
                    self.executor
                        .update_task_status(task_id, TaskStatus::Error)
                        .await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
            };

            // L2/C2: a rollback or end_task may have cancelled the task while
            // the LLM call was in flight (the HTTP call itself may not observe
            // the token promptly and can return well after the 5s rollback
            // wait). Re-check before persisting anything so a stale response
            // cannot overwrite the restored snapshot or push ghost steps.
            if cancel_res.is_cancelled() {
                tracing::info!(
                    "ReAct step {} task {} cancelled during LLM call; discarding response",
                    step_num,
                    task_id
                );
                return Ok(());
            }

            tracing::debug!(
                "ReAct step {} LLM response: {} text chars, {} tool_calls, reasoning={}",
                step_num,
                response.text.len(),
                response.tool_calls.len(),
                response.reasoning.is_some()
            );

            if let Some(ref reasoning) = response.reasoning {
                self.persist_task_message(task_id, "assistant", reasoning, Some("reasoning"))
                    .await;
                // Reconcile the frontend's streamed reasoning with the
                // authoritative complete text. The frontend builds reasoning
                // only from batched deltas, so a dropped/delayed final chunk
                // would permanently lose trailing characters. Emitting the
                // complete reasoning as a final delta lets the frontend's
                // cumulative-detection (delta.startsWith(curr) 鈫?replace)
                // snap the content to the exact full text. This runs after the
                // chunk batcher has flushed, so it is guaranteed to be the
                // last reasoning event for this step.
                emitter
                    .emit(crate::event::AgentEvent::ReasoningChunk {
                        task_id: task_id.into(),
                        delta: reasoning.clone(),
                        step_number: step_num,
                        run_id,
                    })
                    .await;
            }

            let (mut thought, mut actions) =
                Self::parse_default_model_response(&response, step_num);

            // A completely empty model response (no text, no reasoning, no
            // tool calls) is almost always a transient upstream glitch. Retry
            // the same context once before concluding the model decided
            // nothing — otherwise the task would instantly "complete" with a
            // "No action decided." message and pause without answering.
            // A response carrying `web_search_call` items is NOT empty: it is
            // a server-side search round that must round-trip instead.
            if thought.is_none()
                && actions.is_empty()
                && response.web_search_calls.is_empty()
                && !retried_empty
            {
                retried_empty = true;
                tracing::warn!(
                    "ReAct step {} task {} model returned an empty response; retrying once",
                    step_num,
                    task_id
                );
                match router
                    .chat_stream_with_tools_aggregated(
                        role,
                        haven_llm::types::convert_to_llm(canonical.clone()),
                        tools.to_vec(),
                        |_| {},
                    )
                    .await
                {
                    Ok(retry_resp) => {
                        let (t2, a2) = Self::parse_default_model_response(&retry_resp, step_num);
                        if t2.is_some() || !a2.is_empty() {
                            thought = t2;
                            actions = a2;
                        } else {
                            tracing::warn!(
                                "ReAct step {} retry also returned an empty response",
                                step_num
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "ReAct step {} empty-response retry failed: {}",
                            step_num,
                            e
                        );
                    }
                }
            }

            // A text-only response that ends without an explicit tool call is
            // only trusted as a deliberate final answer when it looks
            // complete: the provider reported Stop AND the text does not end
            // mid-sentence. Anything else — a truncated generation (Length /
            // ContentFilter / unknown finish) or text cut off mid-thought —
            // must not end the turn presenting a partial answer as final.
            // Retry once with a continuation nudge (never persisted into the
            // canonical); if the retry is also unusable, fall back to the
            // original text below.
            let pending_ask = Self::canonical_has_pending_ask(canonical);
            // Responses that carried a web search call must never be re-asked:
            // the search round itself is a legitimate (non-cut-off) outcome,
            // and retrying would trigger a duplicate server-side search.
            if !retried_empty
                && !pending_ask
                && response.web_search_calls.is_empty()
                && Self::is_suspect_final(&thought, &actions, &response)
            {
                retried_empty = true;
                tracing::warn!(
                    "ReAct step {} task {} response looks cut off (finish={:?}); retrying once",
                    step_num,
                    task_id,
                    response.finish_reason
                );
                let mut retry_messages = haven_llm::types::convert_to_llm(canonical.clone());
                retry_messages.push(LlmMessage {
                    role: LlmRole::User,
                    content: vec![ContentPart::text(CUT_OFF_RETRY_NUDGE)],
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                    web_search_calls: Vec::new(),
                });
                match router
                    .chat_stream_with_tools_aggregated(role, retry_messages, tools.to_vec(), |_| {})
                    .await
                {
                    Ok(retry_resp) => {
                        let (t2, a2) = Self::parse_default_model_response(&retry_resp, step_num);
                        if t2.is_some() || !a2.is_empty() {
                            thought = t2;
                            actions = a2;
                        } else {
                            tracing::warn!(
                                "ReAct step {} cut-off retry also returned an empty response",
                                step_num
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("ReAct step {} cut-off retry failed: {}", step_num, e);
                    }
                }
            }

            // An unanswered `ask` still pending in the canonical (a reply was
            // lost to compaction/sanitization, or the model never resolved
            // the question): the text-only-Stop heuristic must not end the
            // turn. Drop the synthesized final so the empty-actions path
            // below re-surfaces the question and pauses for the user's
            // answer instead of "completing" with the question unanswered.
            // Applied after the retries so a retry that produced a
            // synthesized final is covered too; explicit final tool calls
            // (the model decided to answer despite the pending question) are
            // respected.
            if pending_ask
                && !actions.is_empty()
                && actions
                    .iter()
                    .all(|a| a.is_final && a.tool_call_id.is_none())
            {
                tracing::warn!(
                    "ReAct step {} task {} stopped while an ask is pending; keeping the turn open",
                    step_num,
                    task_id
                );
                actions.clear();
            }

            tracing::trace!(
                "ReAct step {} parsed: thought={}, actions={}",
                step_num,
                thought
                    .as_ref()
                    .map(|t| format!("{} chars", t.len()))
                    .unwrap_or_else(|| "none".into()),
                actions
                    .iter()
                    .map(|a| a.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            if let Some(ref t) = thought {
                EventDispatcher::emit_thought_from(
                    &emitter, task_id, t, step_num, run_id, &self.db,
                )
                .await;
                history.push(ReActStep {
                    step_number: step_num,
                    thought: Some(t.clone()),
                    action: None,
                    observation: None,
                });
            }

            // ── Web search round-trip ─────────────────────────────────────
            // `web_search_call` output items come from the provider's
            // server-side search tool (DeepSeek built-in). The search itself
            // runs on the provider; the items must be passed back verbatim in
            // the next request's input so the server restores the search
            // context. Push an assistant message carrying them into the
            // canonical so every subsequent path round-trips them.
            let has_web_search = !response.web_search_calls.is_empty();
            let synthesized_final = !actions.is_empty()
                && actions
                    .iter()
                    .all(|a| a.is_final && a.tool_call_id.is_none());
            if has_web_search && (actions.is_empty() || synthesized_final) {
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(response.text.clone())],
                    None,
                    response.reasoning.clone(),
                    response.web_search_calls.clone(),
                ));
                if actions.is_empty() {
                    // Search round: no answer yet, the follow-up request
                    // carries the search context and produces the answer.
                    // Keep the turn open and let the loop re-request.
                    if let Some(ref t) = thought {
                        self.persist_task_message(task_id, "assistant", t, Some("text"))
                            .await;
                    }
                    self.save_branch_point(task_id, canonical, history, step_num, branch_points)
                        .await;
                    tracing::debug!(
                        "ReAct step {} task {} server-side web search round ({} item(s)); continuing",
                        step_num,
                        task_id,
                        response.web_search_calls.len()
                    );
                    continue;
                }
                // synthesized_final: the answer arrived in the same response
                // as the search call — fall through to the final-answer path,
                // which ends the turn. The canonical push above keeps the
                // search context alive for follow-up turns.
            }

            if actions.is_empty() {
                // An `ask` is still pending unanswered (the model stopped
                // without resolving it, and no user reply arrived to pair
                // with it): the turn must not end on a heuristic final.
                // Re-surface the question and pause so the user's next
                // message is treated as the answer.
                if pending_ask {
                    let question = Self::extract_pending_ask_question(canonical);
                    self.pause_turn(
                        task_id,
                        canonical,
                        history,
                        step_num + 1,
                        branch_points,
                        &emitter,
                        TaskStatus::Paused,
                        &question,
                        None,
                        true,
                        infer,
                    )
                    .await?;
                    return Ok(());
                }
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                if let Some(last) = history.last_mut() {
                    last.action = Some(Action {
                        tool_name: "final_answer".into(),
                        tool_input: serde_json::Value::Null,
                        is_final: true,
                        tool_call_id: None,
                    });
                    if last.observation.is_none() {
                        last.observation = Some(msg.clone());
                    }
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: Some(msg.clone()),
                        action: Some(Action {
                            tool_name: "final_answer".into(),
                            tool_input: serde_json::Value::Null,
                            is_final: true,
                            tool_call_id: None,
                        }),
                        observation: Some(msg.clone()),
                    });
                }
                // A user message (or background-job result) arrived while the
                // model was generating this answer: deliver it in the gap
                // between the tool calls and the final content instead of
                // deferring it until after the turn completes. The finished
                // answer is persisted so the conversation stays consistent,
                // then the loop re-runs with the new context.
                if self
                    .inject_pending_context(task_id, canonical, step_num, run_id, &emitter)
                    .await
                {
                    self.persist_task_message(task_id, "assistant", &msg, Some("text"))
                        .await;
                    // Keep a rollback target for the interrupted final step:
                    // the normal pause_turn path saves one, so mirror it here
                    // or rollback to this step restores a stale snapshot.
                    self.save_branch_point(task_id, canonical, history, step_num, branch_points)
                        .await;
                    continue;
                }
                self.pause_turn(
                    task_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    TaskStatus::Paused,
                    &msg,
                    Some(step_num),
                    false,
                    infer,
                )
                .await?;
                return Ok(());
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Task completed.".into());
                if let Some(s) = history.last_mut() {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                // Same mid-turn delivery as the empty-actions branch: a
                // message that arrived during this final LLM call is injected
                // before the turn ends so it influences the answer.
                if self
                    .inject_pending_context(task_id, canonical, step_num, run_id, &emitter)
                    .await
                {
                    self.persist_task_message(task_id, "assistant", &final_text, Some("text"))
                        .await;
                    // Same branch-point guarantee as the empty-actions branch:
                    // the interrupted final step must retain a rollback target.
                    self.save_branch_point(task_id, canonical, history, step_num, branch_points)
                        .await;
                    continue;
                }
                self.pause_turn(
                    task_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    TaskStatus::Paused,
                    &final_text,
                    Some(step_num),
                    false,
                    infer,
                )
                .await?;
                return Ok(());
            }

            if let Some(ref t) = thought {
                let text = t.trim();
                if !text.is_empty() {
                    self.persist_task_message(task_id, "assistant", text, Some("text"))
                        .await;
                }
            }

            let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
            for action in &non_final {
                emitter
                    .emit(crate::event::AgentEvent::Action {
                        task_id: task_id.into(),
                        tool_name: action.tool_name.clone(),
                        input: action.tool_input.clone(),
                        step_number: step_num,
                        run_id,
                        tool_call_id: action.tool_call_id.clone(),
                    })
                    .await;
            }

            if !non_final.is_empty() {
                let tool_calls: Option<Vec<CanonicalToolCall>> = if response.tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        response
                            .tool_calls
                            .iter()
                            .map(|tc| CanonicalToolCall {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                arguments: serde_json::from_str(&tc.arguments)
                                    .unwrap_or(serde_json::Value::Null),
                            })
                            .collect(),
                    )
                };
                // A response mixing real tool calls with a web search round
                // carries both: the `web_search_call` items round-trip in the
                // same assistant message so the next request restores the
                // search context alongside the function tool results.
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(response.text.clone())],
                    tool_calls,
                    response.reasoning.clone(),
                    response.web_search_calls.clone(),
                ));
            }

            self.save_branch_point(task_id, canonical, history, step_num, branch_points)
                .await;

            use futures_util::StreamExt;

            let mut tool_futures = futures_util::stream::FuturesUnordered::new();
            for action in &non_final {
                let task_id = task_id.to_string();
                let tool_name = action.tool_name.clone();
                let tool_input = action.tool_input.clone();
                let action = (*action).clone();
                let max_obs = self.max_observation_chars;
                let db = self.db.clone();
                let executor = self.executor.clone();
                tool_futures.push(async move {
                    tracing::debug!(
                        "executing tool '{}' at step {} (input keys: {:?})",
                        tool_name,
                        step_num,
                        tool_input
                            .as_object()
                            .map(|o| o.keys().collect::<Vec<_>>())
                            .unwrap_or_default()
                    );
                    let result = executor
                        .execute_step(&task_id, &tool_name, tool_input.clone(), step_num)
                        .await;
                    let (text, is_error, ask_question, ask_options, notify_title, notify_body) =
                        match result {
                            Ok(r) => {
                                tracing::debug!(
                                    "tool '{}' at step {} completed: success={}, {} chars",
                                    tool_name,
                                    step_num,
                                    r.success,
                                    serde_json::to_string(&r.output)
                                        .map(|s| s.len())
                                        .unwrap_or(0)
                                );
                                let _ = db
                                    .run_blocking({
                                        let tool_name = tool_name.clone();
                                        let tool_input = tool_input.clone();
                                        move |db| {
                                            db.record_tool_usage(&tool_name, &tool_input, r.success)
                                        }
                                    })
                                    .await;
                                let text = r.summary_text();
                                let text = if text.len() > max_obs {
                                    let cutoff = text.floor_char_boundary(max_obs);
                                    format!(
                                        "{}[... truncated {} chars omitted]",
                                        &text[..cutoff],
                                        text.len() - cutoff
                                    )
                                } else {
                                    text
                                };
                                // The ask signal is read from the structured output
                                // BEFORE truncation: parsing the truncated text
                                // would yield invalid JSON when the output exceeds
                                // the observation budget, silently dropping the
                                // question and never pausing the task.
                                let (ask_question, ask_options) = if tool_name == "ask" && r.success
                                {
                                    haven_tools::extract_ask_signal(&r.output)
                                } else {
                                    (None, Vec::new())
                                };
                                // The notify signal (from the `notify` tool) is also
                                // read from the structured output BEFORE truncation,
                                // mirroring the ask handling above.
                                let (notify_title, notify_body) =
                                    if tool_name == "notify" && r.success {
                                        haven_tools::extract_notify_signal(&r.output)
                                    } else {
                                        (None, None)
                                    };
                                (
                                    text,
                                    !r.success,
                                    ask_question,
                                    ask_options,
                                    notify_title,
                                    notify_body,
                                )
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "tool '{}' at step {} failed: {}",
                                    tool_name,
                                    step_num,
                                    e
                                );
                                (e.to_string(), true, None, Vec::new(), None, None)
                            }
                        };
                    (
                        action,
                        tool_name,
                        text,
                        is_error,
                        ask_question,
                        ask_options,
                        notify_title,
                        notify_body,
                    )
                });
            }

            let mut any_tool_failure = false;
            // If the agent invoked the `ask` tool, the task must pause and
            // wait for the user's reply (delivered as a supplement). Collect
            // every question in the batch so all are surfaced.
            let mut asked_questions: Vec<String> = Vec::new();
            // Drain tool results while remaining responsive to cancellation.
            // Without select!, a cancel arriving mid-batch would only be
            // detected at the next step boundary 鈥?after all tools finish.
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_res.cancelled() => {
                        tracing::info!("ReAct loop cancelled during tool batch at step {}", step_num);
                        return Ok(());
                    }
                    item = tool_futures.next() => {
                        let Some((
                            action,
                            tool_name,
                            step_result,
                            is_error,
                            ask_question,
                            ask_options,
                            notify_title,
                            notify_body,
                        )) = item
                        else {
                            break;
                        };
                        if is_error {
                            any_tool_failure = true;
                        }
                        // The `notify` tool requests a user-facing notification:
                        // emit it (in-app toast + Windows) without pausing the
                        // ReAct loop.
                        if let (Some(title), Some(body)) = (&notify_title, &notify_body) {
                            emitter
                                .emit(crate::event::AgentEvent::Notification {
                                    task_id: task_id.into(),
                                    title: title.clone(),
                                    body: body.clone(),
                                })
                                .await;
                        }
                        // Surface an `ask` result as a readable question rather
                        // than raw JSON. The user's reply arrives via
                        // process_input 鈫?supplement 鈫?Paused鈫扨ending resume.
                        if let Some(q) = &ask_question {
                            asked_questions.push(q.clone());
                        }
                        // `ask` must never be silent: hiding the question
                        // while the task pauses for an answer would leave the
                        // user waiting on a question they can't see.
                        let silent = is_silent_action(&tool_name, &action.tool_input);
                        // For `ask`, the chat/review bubble shows the readable
                        // question text; the canonical (model) context keeps
                        // the raw JSON so the model can still parse the flag.
                        // Same for `notify`: show a readable confirmation
                        // instead of the raw signal JSON.
                        let display_observation = if let Some(q) = &ask_question {
                            q.clone()
                        } else if let Some(title) = &notify_title {
                            let body = notify_body.clone().unwrap_or_default();
                            if body.is_empty() {
                                step_result.clone()
                            } else {
                                format!("Notification sent: {title}: {body}")
                            }
                        } else {
                            step_result.clone()
                        };
                        emitter
                            .emit(crate::event::AgentEvent::Observation {
                                task_id: task_id.into(),
                                observation: display_observation.clone(),
                                tool_name: tool_name.clone(),
                                step_number: step_num,
                                run_id,
                                silent,
                                tool_call_id: action.tool_call_id.clone(),
                                ask_options: ask_options.clone(),
                            })
                            .await;

                        if let Some(last) = history.last_mut() {
                            last.action = Some(action.clone());
                            last.observation = Some(display_observation);
                        } else {
                            history.push(ReActStep {
                                step_number: step_num,
                                thought: None,
                                action: Some(action.clone()),
                                observation: Some(step_result.clone()),
                            });
                        }

                        canonical.push(CanonicalMessage::tool(
                            vec![ContentPart::text(step_result)],
                            action.tool_call_id.clone(),
                        ));
                    }
                }
            }

            // Skip the "try a different approach" nudge when the batch asked
            // the user: it would be baked into the paused snapshot ahead of the
            // user's real answer, contradicting the pending question.
            if any_tool_failure && asked_questions.is_empty() && step_num < max_steps - 1 {
                canonical.push(CanonicalMessage::user_text(
                    "The previous approach encountered errors. Please try a completely different approach this time.",
                ));
            }

            // The agent asked the human a question: pause so the user can
            // answer. Their reply arrives as a supplement and resumes the task
            // (Paused 鈫?Pending 鈫?dispatcher re-enters the loop, injecting the
            // answer as context at the top of the next step).
            if !asked_questions.is_empty() {
                let question = asked_questions.join("\n\n");
                // TOCTOU: a reply that arrived during the drain window went to
                // the steering queue (task was still Running). Convert it to a
                // supplement and, if present, resume immediately as Pending so
                // the answer isn't stranded while the task sits Paused. The
                // steering queue holds only user interjections now 鈥?background
                // job results are buffered separately 鈥?so `has_answer` truly
                // reflects a human reply.
                let steering = self.executor.get_steering(task_id).await;
                let has_answer = !steering.is_empty();
                for s in &steering {
                    // The interjection is the user's reply to the pending
                    // question: queue it as a paired answer so the model does
                    // not re-answer the old question on resume.
                    let _ = self
                        .executor
                        .add_answer_with_attachments(task_id, &s.text, &s.attachments)
                        .await;
                }
                let status = if has_answer {
                    TaskStatus::Pending
                } else {
                    TaskStatus::Paused
                };
                self.pause_turn(
                    task_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    status,
                    &question,
                    None,
                    true,
                    infer,
                )
                .await?;
                return Ok(());
            }

            let state = self.executor.get_task_state(task_id).await;
            if state == TaskStatus::Paused {
                self.save_snapshot_with_branches(
                    task_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                )
                .await;
                return Ok(());
            }
            if state == TaskStatus::Error || state == TaskStatus::Completed {
                return Ok(());
            }
        }

        self.pause_turn_budget(
            task_id,
            canonical,
            history,
            last_step + 1,
            branch_points,
            &emitter,
            infer,
        )
        .await?;
        Ok(())
    }

    /// Persist an assistant message into the task's message stream, applying
    /// the configured sliding-window trim. Delegates to the shared
    /// `crate::persist_task_message` so this path cannot drift from the
    /// user-turn persistence path (same trim, same error policy).
    async fn persist_task_message(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
    ) {
        let _ = crate::persist_task_message(
            &self.db,
            task_id,
            role,
            content,
            message_type,
            &[],
            self.message_window_size,
            false,
        )
        .await;
    }

    /// Finalize a turn: persist the assistant text, save the branch point
    /// (when requested), snapshot the ReAct state, then mark the task with
    /// the given status and notify the frontend + inference. Shared by all
    /// pause/complete paths so the persist → branch-point → snapshot →
    /// status → event ordering cannot drift between them. The snapshot is
    /// taken after the branch point so it includes the newly added entry.
    /// The step-budget checkpoint uses `pause_turn_budget` instead, which
    /// skips the assistant-message persist (the notice is a notification).
    #[allow(clippy::too_many_arguments)]
    async fn pause_turn(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        snapshot_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        status: TaskStatus,
        final_text: &str,
        branch_point_step: Option<u32>,
        awaiting_answer: bool,
        infer: &(dyn Fn() + Send + Sync),
    ) -> anyhow::Result<()> {
        self.persist_task_message(task_id, "assistant", final_text, Some("text"))
            .await;
        if let Some(step) = branch_point_step {
            self.save_branch_point(task_id, canonical, history, step, branch_points)
                .await;
        }
        self.save_snapshot_with_branches(task_id, canonical, history, snapshot_step, branch_points)
            .await;
        // Mark the task as awaiting a human answer BEFORE setting the status,
        // so a background-job completion landing concurrently cannot auto-wake
        // it (the consumer checks this gate). The flag is cleared centrally by
        // `update_task_status` on reactivation.
        if awaiting_answer {
            self.executor.set_awaiting_answer(task_id, true).await;
        }
        let status_str = status.as_str().to_string();
        self.executor.update_task_status(task_id, status).await?;
        emitter
            .emit(crate::event::AgentEvent::TaskUpdated {
                task_id: task_id.into(),
                status: status_str,
            })
            .await;
        infer();
        Ok(())
    }

    /// Pause the task because the run exhausted its step budget. Mirrors
    /// `pause_turn`'s checkpoint side effects (snapshot, Paused status,
    /// infer) but does NOT persist an assistant chat message: system notices
    /// of this kind must not pollute the conversation stream as fake agent
    /// replies — they are surfaced as a notification (in-app toast +
    /// Windows) instead, so the user sees them without the chat pretending
    /// the turn produced an answer.
    #[allow(clippy::too_many_arguments)]
    async fn pause_turn_budget(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        snapshot_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn() + Send + Sync),
    ) -> anyhow::Result<()> {
        self.save_snapshot_with_branches(task_id, canonical, history, snapshot_step, branch_points)
            .await;
        let status_str = TaskStatus::Paused.as_str().to_string();
        self.executor
            .update_task_status(task_id, TaskStatus::Paused)
            .await?;
        emitter
            .emit(crate::event::AgentEvent::TaskUpdated {
                task_id: task_id.into(),
                status: status_str,
            })
            .await;
        emitter
            .emit(crate::event::AgentEvent::Notification {
                task_id: task_id.into(),
                title: BUDGET_EXHAUSTED_TITLE.into(),
                body: BUDGET_EXHAUSTED_BODY.into(),
            })
            .await;
        infer();
        Ok(())
    }

    /// True when the canonical ends with an unanswered `ask`: an `ask` tool
    /// result is present and no user message follows it. The ask pause path
    /// normally prevents this state from reaching an LLM call, but a reply
    /// lost to compaction/sanitization or a dropped answer can leave the
    /// question dangling — and a model Stop response must then not be judged
    /// final (it would end the turn with the question still unanswered).
    fn canonical_has_pending_ask(canonical: &[CanonicalMessage]) -> bool {
        let mut last_ask = None;
        for (i, m) in canonical.iter().enumerate() {
            if m.role == CanonicalRole::Tool
                && m.content.iter().any(|p| match p {
                    ContentPart::Text(t) => {
                        t.contains("\"ask\":true") || t.contains("\"ask\": true")
                    }
                    _ => false,
                })
            {
                last_ask = Some(i);
            }
        }
        last_ask.is_some_and(|idx| {
            canonical[idx + 1..]
                .iter()
                .all(|m| m.role != CanonicalRole::User)
        })
    }

    /// Extract the question text of the last unanswered `ask` tool result in
    /// the canonical. Falls back to a generic prompt when the tool output is
    /// truncated or unparseable.
    fn extract_pending_ask_question(canonical: &[CanonicalMessage]) -> String {
        for m in canonical.iter().rev() {
            if m.role != CanonicalRole::Tool {
                continue;
            }
            for p in &m.content {
                let ContentPart::Text(t) = p else { continue };
                if !(t.contains("\"ask\":true") || t.contains("\"ask\": true")) {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t)
                    && let Some(q) = v.get("question").and_then(|q| q.as_str())
                {
                    return q.to_string();
                }
            }
        }
        "I have a pending question for you.".into()
    }

    /// True when a text-only response should not be trusted as a deliberate
    /// final answer: either the provider did not report Stop (truncated /
    /// filtered / unknown finish) or the text itself ends mid-sentence
    /// (trailing comma/connector/ellipsis — the generation was interrupted
    /// rather than concluded).
    fn looks_cut_off(text: &str) -> bool {
        text.ends_with("...")
            || text.ends_with("···")
            || matches!(
                text.chars().last(),
                Some('，' | '、' | '；' | '：' | ',' | ';' | ':' | '…')
            )
    }

    /// True when the parsed response is a text-only "final" that must be
    /// retried before ending the turn. Trusts explicit tool calls (final or
    /// not) and empty responses (handled by the empty-response retry); only
    /// a thought without actions is examined, and it must pass both the
    /// finish-reason and the mid-sentence checks.
    fn is_suspect_final(
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
    ) -> bool {
        if !actions.is_empty()
            && !actions
                .iter()
                .all(|a| a.is_final && a.tool_call_id.is_none())
        {
            return false;
        }
        match thought {
            Some(t) => response.finish_reason != Some(FinishReason::Stop) || Self::looks_cut_off(t),
            None => false,
        }
    }

    /// Parse LLM response into thought text and actions.
    pub fn parse_default_model_response(
        response: &LlmResponse,
        step_number: u32,
    ) -> (Option<String>, Vec<Action>) {
        let text = response.text.trim().to_string();

        let thought = if text.is_empty() {
            None
        } else {
            Some(text.clone())
        };

        let actions: Vec<Action> = if !response.tool_calls.is_empty() {
            response
                .tool_calls
                .iter()
                .map(|tc| {
                    let args: serde_json::Value =
                        serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);
                    let is_final =
                        tc.name == "final_answer" || tc.name == "answer" || tc.name == "done";
                    Action {
                        tool_name: tc.name.clone(),
                        tool_input: args,
                        is_final,
                        tool_call_id: Some(if tc.id.is_empty() {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            tc.id.clone()
                        }),
                    }
                })
                .collect()
        } else if !text.is_empty()
            && response.finish_reason == Some(FinishReason::Stop)
            && step_number > 0
        {
            vec![Action {
                tool_name: "final_answer".into(),
                tool_input: serde_json::Value::Null,
                is_final: true,
                tool_call_id: None,
            }]
        } else {
            Vec::new()
        };

        (thought, actions)
    }

    /// Save snapshot including branch points for tree-structured rollback (搂2).
    ///
    /// Serializes a borrowed view of the ReAct state (no per-step deep copies
    /// of canonical/history/branch_points — those clones were O(n²) over a
    /// long task) into a reusable buffer, then writes to SQLite on the
    /// blocking thread pool so the WAL fsync never stalls the async runtime.
    async fn save_snapshot_with_branches(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        let view = SnapshotView {
            canonical,
            history,
            step_number,
            branch_points,
        };
        // Serialize into the shared buffer inside a scoped block so the
        // mutex guard is dropped before the await below (the guard is not
        // Send, so it must not be live across the spawn_blocking boundary).
        let bytes = {
            let mut buf = self.snapshot_buf.lock().unwrap();
            buf.clear();
            if serde_json::to_writer(&mut *buf, &view).is_err() {
                return;
            }
            std::mem::take(&mut *buf)
        };
        let json = String::from_utf8(bytes).unwrap_or_default();
        let db = self.db.clone();
        let task_id = task_id.to_string();
        // Return ownership of the serialized bytes so the allocation is
        // handed back to the shared buffer for reuse on the next snapshot.
        let back: String = db
            .run_blocking(move |db| {
                let _ = db.save_react_state(&task_id, &json);
                Ok(json)
            })
            .await
            .unwrap_or_default();
        if let Ok(mut buf) = self.snapshot_buf.lock() {
            *buf = back.into_bytes();
        }
    }

    /// Persist partial thought/reasoning text when a step fails mid-stream,
    /// and save a snapshot so the task can be resumed via "continue" or
    /// rolled back. Without this, any text streamed before the error is lost
    /// on page refresh because it was only in the frontend's memory.
    #[allow(clippy::too_many_arguments)]
    async fn persist_partial_on_error(
        &self,
        task_id: &str,
        step_num: u32,
        run_id: u64,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) {
        // Save a branch point BEFORE persisting the partial output, so
        // last_msg_at captures the timestamp of the last message BEFORE the
        // partial. This lets continue_task / rollback_task precisely delete
        // only the partial output via delete_messages_after(last_msg_at).
        // The canonical/history here represent the state BEFORE the failed
        // LLM call (the response was never pushed to canonical), so resuming
        // will retry the step cleanly.
        self.save_branch_point(task_id, canonical, history, step_num, branch_points)
            .await;

        let thought_text = partial_thought.lock().unwrap().clone();
        let reasoning_text = partial_reasoning.lock().unwrap().clone();
        if !reasoning_text.trim().is_empty() {
            self.persist_task_message(
                task_id,
                "assistant",
                reasoning_text.trim(),
                Some("reasoning"),
            )
            .await;
        }
        if !thought_text.trim().is_empty() {
            let text = thought_text.trim();
            self.persist_task_message(task_id, "assistant", text, Some("text"))
                .await;
            EventDispatcher::emit_thought_from(emitter, task_id, text, step_num, run_id, &self.db)
                .await;
        }
    }

    /// Save a branch point at the current step before tool execution (搂2).
    async fn save_branch_point(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
    ) {
        // `get_last_message_created_at` is a blocking SQLite read; run it on
        // the blocking thread pool instead of the async runtime.
        let db = self.db.clone();
        let task_id_owned = task_id.to_string();
        let last_msg_at = db
            .run_blocking(move |db| Ok(db.get_last_message_created_at(&task_id_owned)))
            .await
            .ok()
            .flatten();
        branch_points.insert(
            step_number,
            BranchPoint {
                canonical: canonical.to_vec(),
                history: history.to_vec(),
                step_number,
                last_msg_at,
            },
        );
        self.save_snapshot_with_branches(task_id, canonical, history, step_number, branch_points)
            .await;
    }

    /// Run one streamed LLM call for an agent step: spawn the chunk consumer,
    /// forward text/reasoning chunks to the frontend while accumulating them
    /// into the partial buffers (persisted if the step fails mid-stream), then
    /// drain the consumer and return the aggregated response. Shared by the
    /// primary step call and the post-compaction retry so the two cannot
    /// drift. Error handling stays at the call site.
    #[allow(clippy::too_many_arguments)]
    async fn stream_llm_step(
        &self,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        llm_messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        cancel: tokio_util::sync::CancellationToken,
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        step_num: u32,
        run_id: u64,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<LlmResponse, haven_llm::LlmError> {
        let (chunk_tx, reasoning_tx, consumer_handle) =
            EventDispatcher::spawn_chunk_consumer_raw(emitter);
        let chunk_tx_c = chunk_tx.clone();
        let reasoning_tx_c = reasoning_tx.clone();
        let task_id_c = task_id.to_string();
        let pt = partial_thought.clone();
        let pr = partial_reasoning.clone();
        // Web search phases are emitted as discrete events (not deltas), so
        // they bypass the chunk batcher through their own channel: the
        // on_chunk callback is synchronous and cannot await the emitter.
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let ws_tx_c = ws_tx.clone();
        let em_ws = emitter.clone();
        let ws_task = tokio::spawn(async move {
            while let Some(event) = ws_rx.recv().await {
                em_ws.emit(event).await;
            }
        });
        let result = router
            .chat_stream_with_tools_aggregated_cancellable(
                role,
                llm_messages,
                tools,
                move |c: &haven_llm::StreamChunk| {
                    if let Some(t) = &c.text {
                        pt.lock().unwrap().push_str(t);
                        if let Err(e) =
                            chunk_tx_c.try_send((task_id_c.clone(), t.clone(), step_num, run_id))
                        {
                            tracing::warn!("thought chunk channel full, dropping: {}", e);
                        }
                    }
                    if let Some(r) = &c.reasoning {
                        pr.lock().unwrap().push_str(r);
                        if let Err(e) = reasoning_tx_c.try_send((
                            task_id_c.clone(),
                            r.clone(),
                            step_num,
                            run_id,
                        )) {
                            tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                        }
                    }
                    if let Some(phase) = c.web_search {
                        let _ = ws_tx_c.send(crate::event::AgentEvent::WebSearch {
                            task_id: task_id_c.clone(),
                            phase: phase.as_str().to_string(),
                            step_number: step_num,
                            run_id,
                        });
                    }
                },
                cancel,
            )
            .await;
        // Drop the senders then drain the consumer so all streamed chunks
        // reach the frontend before the caller continues (e.g. records usage).
        drop(chunk_tx);
        drop(reasoning_tx);
        drop(ws_tx);
        if let Some(handle) = consumer_handle {
            let _ = handle.await;
        }
        let _ = ws_task.await;
        result
    }

    /// Update the per-task cumulative token counters and emit an
    /// `AgentEvent::Usage` event so the UI can refresh its display.
    /// `role` is the endpoint that produced the response (used for cost
    /// lookup); `response` carries the token counts and model name.
    async fn record_usage_and_emit(
        &self,
        task_id: &str,
        role: EndpointRole,
        response: &LlmResponse,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) {
        let usage = &response.usage;
        if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
            // No usage reported by the provider 鈥?nothing useful to surface.
            return;
        }

        let router = self.router();
        let step_cost = router
            .compute_cost(role, usage.prompt_tokens, usage.completion_tokens)
            .await;
        let context_window = {
            let cfg = router.config().await;
            Self::context_window_for_role(&cfg, role)
        };

        let (cum_prompt, cum_completion, cum_total, cum_cost_opt, has_cost) = {
            let mut map = self.cumulative_usage.lock().unwrap();
            let entry = map.entry(task_id.to_string()).or_insert_with(|| {
                // Seed from persisted counters when this task was resumed or
                // reopened: the in-memory map is cleared on task completion
                // (and lost on restart), but the DB row keeps the running
                // totals so cumulative stats stay valid across sessions.
                self.db
                    .get_task_usage(task_id)
                    .ok()
                    .flatten()
                    .map(CumulativeUsage::from)
                    .unwrap_or_default()
            });
            entry.prompt_tokens = entry.prompt_tokens.saturating_add(usage.prompt_tokens);
            entry.completion_tokens = entry
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            entry.total_tokens = entry.total_tokens.saturating_add(usage.total_tokens);
            if let Some(c) = step_cost {
                entry.cost_usd += c;
                entry.has_cost = true;
            }
            let cum_cost = if entry.has_cost {
                Some(entry.cost_usd)
            } else {
                None
            };
            (
                entry.prompt_tokens,
                entry.completion_tokens,
                entry.total_tokens,
                cum_cost,
                entry.has_cost,
            )
        };

        // Persist the cumulative counters so a resumed session (after task
        // completion or app restart) restores the correct token-stats display
        // instead of restarting from zero. Run on a blocking thread: the
        // autocommit hits the disk/fsync path and must not stall the agent
        // step loop on the async runtime (it also shares the global DB lock).
        let db = self.db.clone();
        let task_id_for_persist = task_id.to_string();
        let persist = {
            let cum_cost = cum_cost_opt.unwrap_or(0.0);
            tokio::task::spawn_blocking(move || {
                let _ = db.update_task_usage(
                    &task_id_for_persist,
                    cum_prompt,
                    cum_completion,
                    cum_total,
                    cum_cost,
                    has_cost,
                );
            })
        };
        persist.await.ok();

        EventDispatcher::emit_usage_from(
            emitter,
            UsagePayload {
                task_id: task_id.to_string(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cost_usd: step_cost,
                model: response.model.clone().or_else(|| usage.model_name.clone()),
                cumulative_prompt_tokens: cum_prompt,
                cumulative_completion_tokens: cum_completion,
                cumulative_total_tokens: cum_total,
                cumulative_cost_usd: cum_cost_opt,
                context_window,
            },
        )
        .await;
    }

    /// Drop cumulative counters for a finished task so the map stays
    /// bounded across long-running sessions.
    pub fn reset_cumulative_usage(&self, task_id: &str) {
        let mut map = self.cumulative_usage.lock().unwrap();
        map.remove(task_id);
    }

    /// Resolve the model's true context window for the endpoint used by
    /// `role` — explicit `context_window` config, else the builtin catalog
    /// (e.g. 1M for gpt-4.1-nano / Gemini 2.5 Flash), else a 128K default.
    /// This is the real input budget for the token-usage display, not the
    /// per-response output cap (`max_tokens`).
    fn context_window_for_role(
        cfg: &haven_common::config::LlmConfig,
        role: EndpointRole,
    ) -> Option<u32> {
        let ep = match role {
            EndpointRole::SmallModel => &cfg.small_model,
            EndpointRole::DefaultModel => &cfg.default_model,
            EndpointRole::BalancedModel => &cfg.balanced_model,
            EndpointRole::ImageModel => &cfg.image_model,
            EndpointRole::AudioModel => &cfg.audio_model,
            EndpointRole::EmbeddingModel => &cfg.embedding_model,
        };
        Some(haven_llm::registry::context_window_for(ep))
    }

    /// Build a compactor whose context window reflects the *actual* model for
    /// the role that will handle the step (explicit `context_window` config,
    /// else the builtin catalog, else 128K). Re-resolved on every call so a
    /// hot-swapped router config takes effect immediately. `reserve_tokens`
    /// stays fixed at 4096; the ratio dominates the threshold at large
    /// windows.
    async fn context_compactor(&self, role: EndpointRole) -> ContextCompactor {
        let router = self.router();
        let cfg = router.config().await;
        let window = Self::context_window_for_role(&cfg, role).unwrap_or(128_000);
        ContextCompactor::with_ratio(window, 4_096, 0.75)
    }

    /// Check if context compaction is needed before the next LLM call.
    pub async fn maybe_compact(
        &self,
        task_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) {
        if canonical.len() < 4 {
            return;
        }
        // The compaction window must match the endpoint the next step will
        // use (image-routed steps compact against the image model's budget),
        // mirroring choose_agent_role's role selection.
        let router = self.router();
        let role = if canonical.iter().any(|m| {
            m.content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }))
        }) {
            router.vision_role().await
        } else {
            EndpointRole::DefaultModel
        };
        let compactor = self.context_compactor(role).await;
        if !compactor.needs_compaction(canonical) {
            return;
        }
        let router = self.router();
        if let Some(result) = compactor.compact(canonical, &router).await {
            tracing::info!(
                "compaction for task {}: {} tokens 鈫?{} tokens ({} msgs summarized)",
                task_id,
                result.tokens_before,
                result.tokens_after,
                result.summarized_count
            );
            *canonical = result.compacted;
            EventDispatcher::emit_compaction_from(
                emitter,
                task_id,
                &result.summary,
                result.tokens_before,
                result.tokens_after,
            )
            .await;
        }
    }

    /// Emit balanced model activated with per-task deduplication.
    async fn emit_balanced_model(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        reason: &str,
    ) {
        let should_emit = {
            let mut notified = self.balanced_model_notified.lock().unwrap();
            notified.insert(task_id.to_string())
        };
        if should_emit {
            EventDispatcher::emit_balanced_model_activated_from(emitter, task_id, reason).await;
        }
    }

    /// Emit task error and clean up balanced model dedup state.
    async fn emit_error(&self, emitter: &Arc<dyn AgentEventEmitter>, task_id: &str, error: &str) {
        {
            let mut notified = self.balanced_model_notified.lock().unwrap();
            notified.remove(task_id);
        }
        EventDispatcher::emit_task_error_from(emitter, task_id, error).await;
        self.reset_cumulative_usage(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmResponse, LlmRole, StreamChunk, ToolCall};
    use std::pin::Pin;

    struct MockLlm;

    #[async_trait]
    impl LlmClient for MockLlm {
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
            Err(LlmError::Unknown(
                "mock: chat_stream_with_tools not implemented".into(),
            ))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn mock_router() -> LlmRouter {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlm);
        LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        )
    }

    fn text_msg(role: LlmRole, text: &str) -> LlmMessage {
        LlmMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    fn image_msg(role: LlmRole) -> LlmMessage {
        LlmMessage {
            role,
            content: vec![ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    #[tokio::test]
    async fn choose_agent_role_default_without_images() {
        let router = mock_router();
        let messages = vec![
            text_msg(LlmRole::System, "be concise"),
            text_msg(LlmRole::User, "hello"),
        ];
        assert_eq!(
            choose_agent_role(&router, &messages).await,
            EndpointRole::DefaultModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_default_when_image_model_unconfigured() {
        let router = mock_router();
        let messages = vec![image_msg(LlmRole::User)];
        assert_eq!(
            choose_agent_role(&router, &messages).await,
            EndpointRole::DefaultModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_image_model_when_configured() {
        let router = mock_router();
        router
            .force_role_configured(EndpointRole::ImageModel, true)
            .await;
        let messages = vec![image_msg(LlmRole::User)];
        assert_eq!(
            choose_agent_role(&router, &messages).await,
            EndpointRole::ImageModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_default_when_image_routing_disabled() {
        let router = mock_router();
        router
            .force_role_configured(EndpointRole::ImageModel, true)
            .await;
        router.force_routing_flags(true, false).await;
        let messages = vec![image_msg(LlmRole::User)];
        assert_eq!(
            choose_agent_role(&router, &messages).await,
            EndpointRole::DefaultModel
        );
    }

    fn resp(text: &str, tool_calls: Vec<ToolCall>, finish: Option<FinishReason>) -> LlmResponse {
        LlmResponse {
            text: text.to_string(),
            tool_calls,
            finish_reason: finish,
            usage: haven_llm::types::Usage::default(),
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    #[test]
    fn parse_empty_response_no_actions() {
        let r = resp("", vec![], None);
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought, None);
        assert!(actions.is_empty());
    }

    #[test]
    fn parse_text_only_no_finish_reason_keeps_thought_no_action() {
        // step_number=1, Stop finish, but step>0 required for implicit final.
        let r = resp("hello", vec![], Some(FinishReason::Stop));
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 0);
        assert_eq!(thought.as_deref(), Some("hello"));
        assert!(actions.is_empty(), "step 0 must not auto-finalize");
    }

    #[test]
    fn parse_text_with_stop_finish_step_nonzero_auto_finalizes() {
        let r = resp("the answer is 42", vec![], Some(FinishReason::Stop));
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("the answer is 42"));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
        assert_eq!(actions[0].tool_name, "final_answer");
        assert!(actions[0].tool_call_id.is_none());
    }

    #[test]
    fn parse_tool_calls_produce_actions() {
        let tc = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"x.txt"}"#.into(),
        };
        let r = resp("thinking", vec![tc], Some(FinishReason::ToolCalls));
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("thinking"));
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
        assert_eq!(actions[0].tool_name, "read_file");
        assert_eq!(actions[0].tool_input["path"], "x.txt");
        assert_eq!(actions[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn parse_final_answer_tool_call_marked_final() {
        let tc = ToolCall {
            id: "c2".into(),
            name: "final_answer".into(),
            arguments: r#"{"answer":"done"}"#.into(),
        };
        let r = resp("answering", vec![tc], Some(FinishReason::ToolCalls));
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 2);
        assert!(actions[0].is_final);
    }

    #[test]
    fn parse_answer_and_done_aliases_marked_final() {
        for name in ["answer", "done"] {
            let tc = ToolCall {
                id: String::new(),
                name: name.into(),
                arguments: "{}".into(),
            };
            let r = resp("t", vec![tc], Some(FinishReason::ToolCalls));
            let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
            assert!(actions[0].is_final, "{name} should be final");
        }
    }

    #[test]
    fn parse_empty_tool_call_id_gets_generated() {
        let tc = ToolCall {
            id: String::new(),
            name: "read_file".into(),
            arguments: "{}".into(),
        };
        let r = resp("", vec![tc], Some(FinishReason::ToolCalls));
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert!(actions[0].tool_call_id.is_some());
        assert!(!actions[0].tool_call_id.as_ref().unwrap().is_empty());
    }

    #[test]
    fn parse_invalid_json_arguments_become_null() {
        let tc = ToolCall {
            id: "c3".into(),
            name: "read_file".into(),
            arguments: "not json".into(),
        };
        let r = resp("", vec![tc], Some(FinishReason::ToolCalls));
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert!(actions[0].tool_input.is_null());
    }

    #[test]
    fn parse_multiple_tool_calls_preserve_order() {
        let tcs = vec![
            ToolCall {
                id: "a".into(),
                name: "search".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "b".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        ];
        let r = resp("multi", tcs, Some(FinishReason::ToolCalls));
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].tool_name, "search");
        assert_eq!(actions[1].tool_name, "read_file");
    }

    #[test]
    fn parse_tool_calls_take_precedence_over_text_final() {
        // Even with Stop finish + step>0, tool_calls win over implicit final.
        let tc = ToolCall {
            id: "x".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        };
        let r = resp("text", vec![tc], Some(FinishReason::Stop));
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
    }

    #[test]
    fn parse_text_trimmed_for_thought() {
        let r = resp("  spaced thought  ", vec![], Some(FinishReason::Stop));
        let (thought, _) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("spaced thought"));
    }

    fn ask_tool_msg(question: &str) -> CanonicalMessage {
        CanonicalMessage::tool(
            vec![ContentPart::text(format!(
                r#"{{"ask":true,"question":"{question}","awaiting_answer":true,"options":[]}}"#
            ))],
            Some("call_ask".into()),
        )
    }
    #[test]
    fn pending_ask_true_when_ask_result_unanswered() {
        let canonical = vec![
            CanonicalMessage::user_text("help me"),
            ask_tool_msg("which file?"),
        ];
        assert!(ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn pending_ask_false_when_user_message_follows_ask() {
        let canonical = vec![
            CanonicalMessage::user_text("help me"),
            ask_tool_msg("which file?"),
            CanonicalMessage::user_text("Answer to your previous question: the first one"),
        ];
        assert!(!ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn pending_ask_false_when_no_ask_tool_result() {
        let canonical = vec![
            CanonicalMessage::user_text("help me"),
            CanonicalMessage::tool(
                vec![ContentPart::text(r#"{"success":true,"output":"ok"}"#)],
                Some("call_x".into()),
            ),
        ];
        assert!(!ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn pending_ask_false_when_user_message_before_ask() {
        let canonical = vec![
            CanonicalMessage::user_text("first question"),
            ask_tool_msg("second question?"),
        ];
        // The user message precedes the ask result: still pending.
        assert!(ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn extract_pending_ask_question_reads_last_ask() {
        let canonical = vec![ask_tool_msg("first?"), ask_tool_msg("second?")];
        assert_eq!(
            ReActEngine::extract_pending_ask_question(&canonical),
            "second?"
        );
    }

    #[test]
    fn extract_pending_ask_question_falls_back_on_unparseable_output() {
        let canonical = vec![CanonicalMessage::tool(
            vec![ContentPart::text("truncated {\"ask\":true,\"quest")],
            Some("call_ask".into()),
        )];
        assert_eq!(
            ReActEngine::extract_pending_ask_question(&canonical),
            "I have a pending question for you."
        );
    }

    #[test]
    fn looks_cut_off_detects_mid_sentence_endings() {
        assert!(ReActEngine::looks_cut_off("让我先查一下，")); // trailing comma
        assert!(ReActEngine::looks_cut_off("checking the file,"));
        assert!(ReActEngine::looks_cut_off("waiting for result...")); // ellipsis
        assert!(ReActEngine::looks_cut_off("然后需要："));
        assert!(!ReActEngine::looks_cut_off("好的，已经完成了。"));
        assert!(!ReActEngine::looks_cut_off("The answer is 42."));
        assert!(!ReActEngine::looks_cut_off("完成"));
    }

    #[test]
    fn is_suspect_final_trusts_explicit_tool_calls() {
        let explicit = Action {
            tool_name: "final_answer".into(),
            tool_input: serde_json::Value::Null,
            is_final: true,
            tool_call_id: Some("c1".into()),
        };
        let r = resp("done", vec![], Some(FinishReason::ToolCalls));
        assert!(!ReActEngine::is_suspect_final(
            &Some("done".into()),
            &[explicit],
            &r
        ));
    }

    #[test]
    fn is_suspect_final_flags_truncated_finish() {
        for finish in [
            Some(FinishReason::Length),
            Some(FinishReason::ContentFilter),
            None,
        ] {
            let r = resp("partial text", vec![], finish);
            assert!(
                ReActEngine::is_suspect_final(&Some("partial text".into()), &[], &r),
                "finish={finish:?} must be suspect"
            );
        }
    }

    #[test]
    fn is_suspect_final_flags_stop_with_cut_off_text_but_accepts_complete() {
        let r = resp("让我先查一下，", vec![], Some(FinishReason::Stop));
        assert!(ReActEngine::is_suspect_final(
            &Some("让我先查一下，".into()),
            &[],
            &r
        ));
        let r2 = resp("好的，已经完成了。", vec![], Some(FinishReason::Stop));
        assert!(!ReActEngine::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r2
        ));
    }

    #[test]
    fn is_suspect_final_ignores_empty_thought() {
        let r = resp("", vec![], Some(FinishReason::Length));
        assert!(!ReActEngine::is_suspect_final(&None, &[], &r));
    }
}
