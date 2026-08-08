use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use haven_common::config::ContextLimitsConfig;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::types::{LlmMessage, LlmRole};
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition, ToolFunction};
use haven_memory::Database;
use haven_memory::repositories::messages::MessageAttachment;
use haven_task::{TaskExecutor, TaskStatus};
use haven_tools::is_silent_action;

use crate::compactor::{ContextCompactor, estimate_message_tokens};
use crate::event::{AgentEventEmitter, EventDispatcher, UsagePayload};
use crate::types::{Action, BranchPoint, ReActStep};

/// Convert a stored message attachment into a content part for the LLM.
/// Images become vision content parts (base64 payload); non-image file
/// attachments (persisted on disk with a `path`) become a short text
/// reference so the agent knows the file exists and where to read it with
/// the file tool 鈥?the raw bytes are never shipped to the model.
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
            Some(path) => ContentPart::text(format!("[闄勪欢: {name}锛岃矾寰? {path}]")),
            None => ContentPart::text(format!("[闄勪欢: {name}]")),
        }
    }
}

/// True when the canonical carries at least one image content part. Scanned
/// once per step (after `inject_pending_context`) and shared by the compactor
/// window selection and `choose_agent_role`, so the image check is not
/// repeated across every content part on each step.
pub(crate) fn canonical_has_image(messages: &[CanonicalMessage]) -> bool {
    messages.iter().any(|m| {
        m.content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. }))
    })
}

/// Pick the endpoint role for an agent step. Conversations that carry image
/// content parts route through the router's vision role — the dedicated
/// `image_model` (vision-capable) endpoint when configured, otherwise the
/// default model. Everything else uses the default model.
async fn choose_agent_role(router: &LlmRouter, has_image: bool) -> EndpointRole {
    if has_image {
        router.vision_role().await
    } else {
        EndpointRole::DefaultModel
    }
}

/// Interval (in ReAct steps) at which long-running tasks re-run fact
/// inference mid-task, so memory is refreshed before the task
/// ever pauses or completes.
/// Message persisted when a run exhausts its step budget (`max_steps`). The
/// task is intentionally paused as a checkpoint 鈥?the task is NOT finished,
/// and the next user message resumes it with a fresh budget. System notices
/// like this must NOT land in the chat as an assistant bubble; they are
/// surfaced as a notification (in-app toast + Windows) instead.
const BUDGET_EXHAUSTED_TITLE: &str = "任务步骤上限已用尽";
const BUDGET_EXHAUSTED_BODY: &str = "本轮运行的步骤上限已用完，任务已暂停。发一条消息即可继续。";

/// Nudge appended to the retry call when a text-only response looks cut off
/// (truncated generation or text ending mid-sentence). The retry is private
/// to the loop 鈥?the nudge is never persisted into the canonical, so the
/// conversation stream stays clean if the retry succeeds or falls back.
const CUT_OFF_RETRY_NUDGE: &str =
    "Your previous response was cut off before you finished. Please continue and complete it.";

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<TaskExecutor>,
    db: Arc<Database>,
    max_steps: Mutex<u32>,
    context_limits: ContextLimitsConfig,
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
    /// Per-task incremental token-estimate cache (see
    /// `estimate_canonical_tokens`): avoids re-tokenizing the whole canonical
    /// on every step.
    token_estimate_cache: Mutex<HashMap<String, TokenEstimate>>,
    /// Per-role context-window cache keyed by the router instance pointer,
    /// so per-step compactor construction and usage display do not clone the
    /// full LlmConfig on every step (the router only changes via
    /// `replace_router`).
    context_window_cache: Mutex<(usize, HashMap<EndpointRole, u32>)>,
}

/// Borrowed serialization view of a `ReActSnapshot`. Serializing this instead
/// of building an owned `ReActSnapshot` skips the per-step deep copies of
/// canonical/history/branch_points (which accumulate to O(n虏) over a long
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

/// Incremental token estimate for a task's canonical message list (see
/// `ReActEngine::estimate_canonical_tokens`). `tokens` is the estimate at the
/// last full tokenization pass, when the canonical had `msgs_len` messages.
#[derive(Debug, Clone, Default)]
struct TokenEstimate {
    /// canonical length at the last full tokenization pass
    msgs_len: usize,
    /// estimated tokens at that pass
    tokens: u32,
    /// number of estimation calls so far (drives the periodic full pass)
    passes: u32,
}

/// Failure classification used to shape the post-failure retry nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// The environment cannot run the approach: missing command, wrong shell,
    /// network/proxy trouble, bad paths. The approach itself may be sound.
    Environmental,
    /// The approach/usage itself is flawed (bad params, parse failures).
    Logic,
    /// Cannot tell from the error text.
    Unknown,
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
        context_limits: ContextLimitsConfig,
    ) -> Self {
        Self {
            router: Arc::new(RwLock::new(router)),
            executor,
            db,
            max_steps: Mutex::new(max_steps),
            context_limits,
            balanced_model_notified: Mutex::new(HashSet::new()),
            run_counter: AtomicU64::new(0),
            current_run_id: AtomicU64::new(0),
            cumulative_usage: Mutex::new(HashMap::new()),
            snapshot_buf: Mutex::new(Vec::new()),
            token_estimate_cache: Mutex::new(HashMap::new()),
            context_window_cache: Mutex::new((0, HashMap::new())),
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
    /// content 鈥?a message that arrived while the LLM call was in flight is
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
        // re-budgets on every resume 鈥?a task can run `max_steps` per run,
        // not once per task lifetime (documented in refactor-dedup.md A9).
        let effective_max = max_steps.max(start_step.saturating_sub(1).saturating_add(max_steps));
        let mut last_step = start_step.saturating_sub(1);
        // Guard so an empty or cut-off model response is retried at most once
        // per run. Initialized outside the step loop so the assignment below
        // is read by later iterations (keeps the lint clean).
        let mut retried_empty = false;

        for step_num in start_step..=effective_max {
            last_step = step_num;
            let cancel = self.executor.cancellation_token(task_id).await;
            // Level-triggered status subscription, taken BEFORE the state
            // check: unlike the edge-triggered Notify it replaces, a
            // transition that lands between a state read and the `changed()`
            // wait is never lost — the receiver's stored value moves and
            // `changed()` resolves immediately. No polling timeout is needed.
            let mut status_rx = self.executor.subscribe_status(task_id).await;
            loop {
                // Check cancellation first: end_task / rollback cancel the
                // token, so the loop must exit silently without touching
                // status or emitting events. The state check below would
                // otherwise observe the Error sentinel of a task that
                // end_task already removed from memory and announce a
                // spurious "task interrupted" error.
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    // Task vanished from the working set (end_task / terminal
                    // cleanup): exit silently.
                    None => return Ok(()),
                    Some(TaskStatus::Completed) => return Ok(()),
                    Some(TaskStatus::Error) => {
                        // An external path marked the task Error while the
                        // loop was alive: announce the interruption so the
                        // user sees why it stopped.
                        self.emit_error(&emitter, task_id, "task interrupted")
                            .await;
                        return Ok(());
                    }
                    Some(s) if s.is_paused() => {
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
                // Wait for the next status change or cancellation, then
                // re-evaluate at the loop head. A resume that landed during
                // the snapshot save above resolves `changed()` immediately.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    r = status_rx.changed() => {
                        // `Err` means the sender was dropped (task cleaned
                        // up): the state re-check at the loop head handles it.
                        let _ = r;
                    }
                }
            }

            // Deliver user interjections (supplements, steering) and
            // background-job results as context at the top of each step so
            // they land in the gap between tool calls and the next LLM call.
            self.inject_pending_context(task_id, canonical, step_num, run_id, &emitter)
                .await;

            // Scan for image content once per step; the flag is shared by the
            // compactor window selection and the endpoint role below, so the
            // image check is not repeated over every content part. Compaction
            // may summarize away the last image, so re-scan only when one ran.
            let mut has_image = canonical_has_image(canonical);
            if self
                .maybe_compact(task_id, canonical, has_image, &emitter)
                .await
            {
                has_image = canonical_has_image(canonical);
            }

            // Incremental fact inference on long-running tasks:
            // turns that never pause would otherwise only trigger extraction
            // at the very end. Every `context_limits.fact_infer_interval_steps`
            // steps we re-run inference; the upsert/known-facts machinery makes
            // this idempotent (re-confirmed facts are reinforced, not duplicated).
            if step_num % self.context_limits.fact_infer_interval_steps == 0 {
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
            // Convert once per step; retries below reuse the converted
            // messages (the canonical is only replaced by the compaction
            // path, which re-converts) instead of cloning the whole
            // canonical and re-serializing every tool-call argument again.
            let mut llm_messages = haven_llm::types::convert_to_llm(canonical);
            let role = choose_agent_role(&router, has_image).await;
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
                    &llm_messages,
                    &tools,
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
                            "compacted {} -> {} tokens",
                            result.tokens_before,
                            result.tokens_after
                        );
                        *canonical = result.compacted;
                        // The retry must convert the *compacted* canonical
                        // (the old messages are stale), and the role must be
                        // re-resolved: summarizing away the last image-bearing
                        // turn changes the routing for the retry.
                        llm_messages = haven_llm::types::convert_to_llm(canonical);
                        let retry_role = if canonical_has_image(canonical) {
                            router.vision_role().await
                        } else {
                            EndpointRole::DefaultModel
                        };
                        EventDispatcher::emit_compaction_from(
                            &emitter,
                            task_id,
                            &result.summary,
                            result.tokens_before,
                            result.tokens_after,
                        )
                        .await;
                        // Reset the accumulators: the first attempt's partial
                        // text was based on pre-compaction context and should
                        // not be mixed with the retry's output.
                        partial_thought.lock().unwrap().clear();
                        partial_reasoning.lock().unwrap().clear();
                        match self
                            .stream_llm_step(
                                router.clone(),
                                retry_role,
                                &llm_messages,
                                &tools,
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
                                self.record_usage_and_emit(
                                    task_id, retry_role, &retry_resp, &emitter,
                                )
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
                // cumulative-detection (delta.startsWith(curr) 閳?replace)
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
            // nothing 鈥?otherwise the task would instantly "complete" with a
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
                    .chat_stream_with_tools_aggregated(role, &llm_messages, &tools, |_| {})
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
            // mid-sentence. Anything else 鈥?a truncated generation (Length /
            // ContentFilter / unknown finish) or text cut off mid-thought 鈥?            // must not end the turn presenting a partial answer as final.
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
                let mut retry_messages = llm_messages.clone();
                retry_messages.push(LlmMessage {
                    role: LlmRole::User,
                    content: vec![ContentPart::text(CUT_OFF_RETRY_NUDGE)],
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                    web_search_calls: Vec::new(),
                });
                match router
                    .chat_stream_with_tools_aggregated(role, &retry_messages, &tools, |_| {})
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

            // 鈹€鈹€ Web search round-trip 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
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
                // The text must match what `persist_task_message` stores
                // (trimmed thought) or the resume dedup fails on the leading
                // whitespace and re-seeds the message as a [conversation] line.
                let push_text = thought.as_deref().unwrap_or(&response.text);
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(push_text.to_string())],
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
                // as the search call 鈥?fall through to the final-answer path,
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
                        TaskStatus::PausedAwaitingAnswer,
                        &question,
                        None,
                        infer,
                    )
                    .await?;
                    return Ok(());
                }
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                // Guard against clobbering: when the response carried no text
                // (thought is None after failed retries), `history.last()`
                // points at a PREVIOUS step; only attach the synthesized final
                // to this step's own entry, otherwise the previous step's
                // action/observation is silently overwritten.
                if let Some(last) = history.last_mut().filter(|s| s.step_number == step_num) {
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
                let before_inject_len = canonical.len();
                if self
                    .inject_pending_context(task_id, canonical, step_num, run_id, &emitter)
                    .await
                {
                    self.persist_task_message(task_id, "assistant", &msg, Some("text"))
                        .await;
                    // The injected messages were appended after
                    // `before_inject_len`; the finished answer must sit BEFORE
                    // them so the re-run's LLM call sees the agent's own
                    // completed answer followed by the interjection. Without
                    // this the canonical jumps from the tool results straight
                    // to the steering/user message and the model re-answers
                    // blind, producing a duplicate or context-free follow-up
                    // bubble. This branch is only reachable without
                    // web_search_calls (the search-round path continues above),
                    // so no duplicate assistant push is possible here.
                    canonical.insert(
                        before_inject_len,
                        CanonicalMessage::assistant(
                            vec![ContentPart::text(msg.clone())],
                            None,
                            response.reasoning.clone(),
                            Vec::new(),
                        ),
                    );
                    // Keep a rollback target for the interrupted final step:
                    // the normal pause_turn path saves one, so mirror it here
                    // or rollback to this step restores a stale snapshot.
                    self.save_branch_point(task_id, canonical, history, step_num, branch_points)
                        .await;
                    continue;
                }
                // Mirror the finished answer into the canonical before the
                // pause: the snapshot then carries the complete conversation
                // in the right order. Without this, the pause snapshot ends
                // right after the tool results and the resume re-seed has to
                // re-insert the answer at the transcript head (sys_end),
                // placing it BEFORE the tool results that preceded it.
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(msg.clone())],
                    None,
                    response.reasoning.clone(),
                    Vec::new(),
                ));
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
                    infer,
                )
                .await?;
                return Ok(());
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Task completed.".into());
                // Same clobber guard as the empty-actions branch above.
                if let Some(s) = history.last_mut().filter(|s| s.step_number == step_num) {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                // The response may already have pushed its own assistant
                // message: a web-search round (pushed above with the search
                // context) or a response mixing real tool calls with the final
                // action (pushed with tool_calls below). In those cases the
                // final text is already in the canonical and must not be
                // duplicated.
                let already_pushed = has_web_search || actions.iter().any(|a| !a.is_final);
                // Same mid-turn delivery as the empty-actions branch: a
                // message that arrived during this final LLM call is injected
                // before the turn ends so it influences the answer.
                let before_inject_len = canonical.len();
                if self
                    .inject_pending_context(task_id, canonical, step_num, run_id, &emitter)
                    .await
                {
                    self.persist_task_message(task_id, "assistant", &final_text, Some("text"))
                        .await;
                    // Same rollback target guarantee as the empty-actions
                    // branch: the interrupted final step must retain a branch
                    // point. Insert the finished answer BEFORE the injected
                    // messages (see the empty-actions branch for why).
                    if !already_pushed {
                        canonical.insert(
                            before_inject_len,
                            CanonicalMessage::assistant(
                                vec![ContentPart::text(final_text.clone())],
                                None,
                                response.reasoning.clone(),
                                Vec::new(),
                            ),
                        );
                    }
                    self.save_branch_point(task_id, canonical, history, step_num, branch_points)
                        .await;
                    continue;
                }
                // Mirror the finished answer into the canonical before the
                // pause (same ordering rationale as the empty-actions branch).
                if !already_pushed {
                    canonical.push(CanonicalMessage::assistant(
                        vec![ContentPart::text(final_text.clone())],
                        None,
                        response.reasoning.clone(),
                        Vec::new(),
                    ));
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
                // The tool_calls echoed into the canonical assistant message
                // must exactly match the tool results pushed below, or
                // providers reject the request with a 400. They are built
                // from the ACTIONS (not `response.tool_calls`) so that a
                // retry-replaced response stays consistent: when the empty /
                // cut-off retry produced the tool calls, the original
                // `response.tool_calls` is empty and zipping it with the
                // retried actions would emit an assistant message WITHOUT
                // tool_calls followed by orphaned tool results (silently
                // dropped by sanitize_canonical, losing the observations).
                // The Action side already carries the synthesized UUID for
                // empty provider ids, matching the tool-result side below.
                let tool_calls: Option<Vec<CanonicalToolCall>> = Some(
                    non_final
                        .iter()
                        .map(|a| CanonicalToolCall {
                            id: a.tool_call_id.clone().unwrap_or_default(),
                            name: a.tool_name.clone(),
                            arguments: a.tool_input.clone(),
                        })
                        .collect(),
                );
                // The text must match what `persist_task_message` stores
                // (trimmed thought) so resume dedup cannot fail; a
                // retry-replaced response also must not echo the cut-off
                // original text.
                let push_text = thought.as_deref().unwrap_or(&response.text);
                // A response mixing real tool calls with a web search round
                // carries both: the `web_search_call` items round-trip in the
                // same assistant message so the next request restores the
                // search context alongside the function tool results.
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(push_text.to_string())],
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
                let max_obs = self.context_limits.max_observation_chars;
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
            // Bounded per-step failure evidence (tool name + error tail) used
            // to classify failures as environmental vs logic when composing
            // the retry nudge — a broken proxy or a missing command must not
            // push the model to abandon a sound approach.
            let mut failure_signals: Vec<(String, String)> = Vec::new();
            // If the agent invoked the `ask` tool, the task must pause and
            // wait for the user's reply (delivered as a supplement). Collect
            // every question in the batch so all are surfaced.
            let mut asked_questions: Vec<String> = Vec::new();
            // Drain tool results while remaining responsive to cancellation.
            // Without select!, a cancel arriving mid-batch would only be
            // detected at the next step boundary 閳?after all tools finish.
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
                            if failure_signals.len() < 3 {
                                let cap: String = step_result.chars().take(600).collect();
                                failure_signals.push((tool_name.clone(), cap));
                            }
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
                        // process_input 閳?supplement 閳?Paused閳墾ending resume.
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

                        if let Some(last) = history
                            .last_mut()
                            .filter(|s| s.step_number == step_num && s.action.is_none())
                        {
                            // First tool result of this step: fill the thought
                            // entry pushed at step start.
                            last.action = Some(action.clone());
                            last.observation = Some(display_observation.clone());
                        } else {
                            // A later tool of a multi-tool step, or a tool-only
                            // step (thought was None, so no entry was pushed at
                            // step start): append a fresh entry instead of
                            // overwriting the previous entry. The old behavior
                            // kept only the LAST completed tool per step (and
                            // could clobber the PREVIOUS step's entry when the
                            // response carried no thought), silently dropping
                            // every other tool from the step history — which
                            // also made restore_per_task_tools miss parallel
                            // load_skill/load_mcp registrations on restart.
                            history.push(ReActStep {
                                step_number: step_num,
                                thought: None,
                                action: Some(action.clone()),
                                observation: Some(display_observation),
                            });
                        }

                        canonical.push(CanonicalMessage::tool(
                            vec![ContentPart::text(step_result)],
                            action.tool_call_id.clone(),
                        ));
                    }
                }
            }

            // Skip the retry nudge when the batch asked the user: it would be
            // baked into the paused snapshot ahead of the user's real answer,
            // contradicting the pending question.
            if any_tool_failure && asked_questions.is_empty() && step_num < max_steps - 1 {
                canonical.push(CanonicalMessage::user_text(Self::build_failure_nudge(
                    &failure_signals,
                )));
            }

            // The agent asked the human a question: pause so the user can
            // answer. Their reply arrives as a supplement and resumes the task
            // (Paused 閳?Pending 閳?dispatcher re-enters the loop, injecting the
            // answer as context at the top of the next step).
            if !asked_questions.is_empty() {
                let question = asked_questions.join("\n\n");
                // TOCTOU: a reply that arrived during the drain window went to
                // the steering queue (task was still Running). Convert it to a
                // supplement and, if present, resume immediately as Pending so
                // the answer isn't stranded while the task sits Paused. The
                // steering queue holds only user interjections now 閳?background
                // job results are buffered separately 閳?so `has_answer` truly
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
                    // No reply arrived: the task pauses awaiting the user's
                    // answer (PausedAwaitingAnswer blocks auto-wake by
                    // background-job completions).
                    TaskStatus::PausedAwaitingAnswer
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
                    infer,
                )
                .await?;
                return Ok(());
            }

            let state = self.executor.get_task_state(task_id).await;
            match state {
                Some(s) if s.is_paused() => {
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
                // Task gone (end_task/terminal cleanup) or terminal: exit.
                None | Some(TaskStatus::Error) | Some(TaskStatus::Completed) => return Ok(()),
                _ => {}
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

    /// Persist an assistant message into the task's message stream.
    /// Delegates to the shared `crate::persist_task_message` so this path
    /// cannot drift from the user-turn persistence path (same trim, same
    /// error policy). Persistence failures are logged here instead of being
    /// silently swallowed: a dropped write would make the streamed content
    /// disappear after a reload while the UI keeps showing it.
    async fn persist_task_message(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
    ) {
        if let Err(e) =
            crate::persist_task_message(&self.db, task_id, role, content, message_type, &[], false)
                .await
        {
            tracing::warn!(
                "ReAct: failed to persist {} message for task {} (type={:?}): {}",
                role,
                task_id,
                message_type,
                e
            );
        }
    }

    /// Finalize a turn: persist the assistant text, save the branch point
    /// (when requested), snapshot the ReAct state, then mark the task with
    /// the given status and notify the frontend + inference. Shared by all
    /// pause/complete paths so the persist 鈫?branch-point 鈫?snapshot 鈫?
    /// status 鈫?event ordering cannot drift between them. The snapshot is
    /// taken after the branch point so it includes the newly added entry.
    /// Callers pause with `TaskStatus::Paused` (scheduling) or
    /// `TaskStatus::PausedAwaitingAnswer` (the `ask` tool is blocked on a
    /// human reply — that flavor also blocks background-job auto-wake).
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
        // The status itself carries the awaiting-answer flavor
        // (`PausedAwaitingAnswer`), so the transition is atomic: a
        // background-job completion landing concurrently reads the final
        // state and cannot auto-wake an answer-blocked task.
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
    /// replies 鈥?they are surfaced as a notification (in-app toast +
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
    ///
    /// Scans backward from the tail: the first user message OR ask result
    /// encountered decides. This is equivalent to the old forward scan (the
    /// last ask must be followed only by non-User messages) but resolves in
    /// O(recent window) instead of O(whole canonical) per step.
    fn canonical_has_pending_ask(canonical: &[CanonicalMessage]) -> bool {
        for m in canonical.iter().rev() {
            match m.role {
                CanonicalRole::User => return false,
                CanonicalRole::Tool => {
                    if m.content.iter().any(|p| match p {
                        ContentPart::Text(t) => {
                            t.contains("\"ask\":true") || t.contains("\"ask\": true")
                        }
                        _ => false,
                    }) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
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
    /// (trailing comma/connector/ellipsis 鈥?the generation was interrupted
    /// rather than concluded).
    fn looks_cut_off(text: &str) -> bool {
        text.ends_with("...")
            || text.ends_with("路路路")
            || matches!(
                text.chars().last(),
                Some('，')
                    | Some('：')
                    | Some('！')
                    | Some(',')
                    | Some(';')
                    | Some(':')
                    | Some('…')
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

    /// Compose the retry nudge after a step where tool calls failed. The
    /// failure evidence is classified first: environment-type failures
    /// (missing command, wrong shell syntax, network/proxy, paths) must NOT
    /// push the model to abandon its approach — the correct move is to
    /// diagnose and fix the environment (different shell, different tool,
    /// corrected path) and retry. Logic failures get a fix-and-retry nudge
    /// with an explicit threshold before switching approach. This replaces
    /// the old unconditional "try a completely different approach" nudge,
    /// which repeatedly sent users down wrong paths when the real cause was
    /// environmental (Get-FileHash missing in the chosen shell, a broken
    /// proxy, a different 7z path).
    fn build_failure_nudge(failures: &[(String, String)]) -> String {
        let has_env = failures
            .iter()
            .any(|(t, e)| Self::classify_tool_failure(t, e) == FailureKind::Environmental);
        let has_logic = failures
            .iter()
            .any(|(t, e)| Self::classify_tool_failure(t, e) == FailureKind::Logic);
        if has_env {
            "The tool failures look ENVIRONMENTAL (missing command / wrong shell syntax / network / path), not logic errors. Do NOT abandon your approach. Diagnose the environment first: verify the command exists in the shell you chose (cmd vs PowerShell syntax differs; `&&` only works in cmd), check network/proxy/endpoints, fix paths and prerequisites. Switching tools (e.g. curl -> aria2) or shells is an environment fix, not a change of approach — keep the same approach and retry."
                .into()
        } else if has_logic {
            "The previous approach failed with logic errors. Analyze the exact error, fix the specific mistake, and retry. Only consider a completely different approach if the same method fails again after you fixed it."
                .into()
        } else {
            "The previous approach encountered errors. Diagnose the root cause first: is it an environment problem (missing command, network, path) or a logic problem? Fix the cause and retry; change approach only if the method itself is wrong."
                .into()
        }
    }

    /// Heuristic classification of a tool failure: environment problems (the
    /// user's tools/environment cannot run the approach) vs logic problems
    /// (the approach itself is flawed). Used to shape the retry nudge so
    /// environmental failures do not trigger an unnecessary method switch.
    fn classify_tool_failure(tool_name: &str, err: &str) -> FailureKind {
        // Tool-usage mistakes by the model itself (missing params, invalid
        // input) are logic errors: the schema/validation error names the fix.
        if tool_name == "files"
            && (err.contains("MISSING REQUIRED FIELD")
                || err.contains("old_string")
                || err.contains("not found in file"))
        {
            return FailureKind::Logic;
        }
        let e = err.to_lowercase();
        const ENV_MARKERS: &[&str] = &[
            // command / executable missing
            "not recognized",
            "not recognized as an internal or external command",
            "不是内部或外部命令",
            "command not found",
            "无法识别",
            "not found",
            "cannot be found",
            "cannot find",
            "找不到",
            "no such file",
            "no such directory",
            "spawn",
            "program not found",
            // network / proxy / transport
            "connection",
            "timed out",
            "timeout",
            "refused",
            "reset",
            "proxy",
            "unreachable",
            "resolve",
            "dns",
            "ssl",
            "tls",
            "certificate",
            "failed to connect",
            "tunnel",
            "network",
            // paths / permissions
            "path does not exist",
            "路径不存在",
            "access denied",
            "拒绝访问",
            // PowerShell/7z style environment mismatches
            "无法将",
            "不是有效的",
        ];
        if ENV_MARKERS.iter().any(|m| e.contains(m)) {
            return FailureKind::Environmental;
        }
        const LOGIC_MARKERS: &[&str] = &[
            "validation failed",
            "missing required",
            "parse error",
            "syntax error",
            "unterminated",
            "invalid json",
            "is required for",
        ];
        if LOGIC_MARKERS.iter().any(|m| e.contains(m)) {
            return FailureKind::Logic;
        }
        FailureKind::Unknown
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

    /// Save snapshot including branch points for tree-structured rollback (鎼?).
    ///
    /// Serializes a borrowed view of the ReAct state (no per-step deep copies
    /// of canonical/history/branch_points 鈥?those clones were O(n虏) over a
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

    /// Save a branch point at the current step before tool execution (鎼?).
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
        llm_messages: &[LlmMessage],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        step_num: u32,
        run_id: u64,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<LlmResponse, haven_llm::LlmError> {
        let (chunk_tx, reasoning_tx, consumer_handle) = EventDispatcher::spawn_chunk_consumer_raw(
            emitter,
            self.context_limits.event_chunk_batch_max_bytes,
        );
        let chunk_tx_c = chunk_tx.clone();
        let reasoning_tx_c = reasoning_tx.clone();
        let task_id_c = task_id.to_string();
        let pt = partial_thought.clone();
        let pr = partial_reasoning.clone();
        // Crash/stop recovery: the accumulated thought text is checkpointed
        // into the `partial_messages` scratch table while streaming so a
        // crash, user stop, or app exit does not lose the whole reply. The
        // first chunk checkpoints immediately; afterwards at most every 2s
        // or every `partial_checkpoint_min_chars` new chars, and never while
        // a write is in flight.
        let checkpoint_db = self.db.clone();
        let checkpoint_task = task_id.to_string();
        let checkpoint_inflight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let partial_checkpoint_min_chars = self.context_limits.partial_checkpoint_min_chars;
        let partial_checkpoint_interval =
            std::time::Duration::from_secs(self.context_limits.partial_checkpoint_interval_secs);
        let mut checkpoint_at = std::time::Instant::now() - partial_checkpoint_interval;
        let mut checkpoint_len = 0usize;
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
                &llm_messages,
                &tools,
                move |c: &haven_llm::StreamChunk| {
                    if let Some(t) = &c.text {
                        pt.lock().unwrap().push_str(t);
                        if let Err(e) =
                            chunk_tx_c.try_send((task_id_c.clone(), t.clone(), step_num, run_id))
                        {
                            tracing::warn!("thought chunk channel full, dropping: {}", e);
                        }
                        let now = std::time::Instant::now();
                        let len = pt.lock().unwrap().len();
                        if !checkpoint_inflight.load(std::sync::atomic::Ordering::Relaxed)
                            && (now.duration_since(checkpoint_at) >= partial_checkpoint_interval
                                || len.saturating_sub(checkpoint_len)
                                    >= partial_checkpoint_min_chars)
                        {
                            checkpoint_at = now;
                            checkpoint_len = len;
                            let snapshot = pt.lock().unwrap().clone();
                            let db = checkpoint_db.clone();
                            let tid = checkpoint_task.clone();
                            let flag = checkpoint_inflight.clone();
                            tokio::spawn(async move {
                                let tid_c = tid.clone();
                                let snapshot_c = snapshot.clone();
                                let res = db
                                    .run_blocking(move |db| {
                                        db.upsert_partial_message(&tid_c, &snapshot_c)
                                    })
                                    .await;
                                flag.store(false, std::sync::atomic::Ordering::Relaxed);
                                if let Err(e) = res {
                                    tracing::warn!(
                                        "failed to checkpoint partial stream text for task {}: {}",
                                        tid,
                                        e
                                    );
                                }
                            });
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
            // No usage reported by the provider 閳?nothing useful to surface.
            return;
        }

        let router = self.router();
        let step_cost = router
            .compute_cost(role, usage.prompt_tokens, usage.completion_tokens)
            .await;
        // `context_window_for_role` always yields Some; the cached resolver
        // avoids cloning the full LlmConfig on every step.
        let context_window = Some(self.cached_context_window(role).await);

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

    /// Drop cumulative counters and the token-estimate cache for a finished
    /// task so both maps stay bounded across long-running sessions.
    pub fn reset_cumulative_usage(&self, task_id: &str) {
        let mut map = self.cumulative_usage.lock().unwrap();
        map.remove(task_id);
        drop(map);
        self.reset_token_estimate(task_id);
    }

    /// Resolve the model's true context window for the endpoint used by
    /// `role` 鈥?explicit `context_window` config, else the builtin catalog
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

    /// Resolve the model's true context window for `role` using a per-router
    /// cache. Cloning the full LlmConfig on every step (compactor window +
    /// usage display) is wasteful when the router only changes via
    /// `replace_router`; the cache is keyed by the router instance pointer so
    /// a hot-swapped router invalidates it immediately.
    async fn cached_context_window(&self, role: EndpointRole) -> u32 {
        let router = self.router();
        let ptr = Arc::as_ptr(&router) as usize;
        // Fast path: read the cached window without awaiting the router
        // config. The cache guard is scoped so it never crosses an await
        // (the std Mutex guard is not Send).
        if let Some(window) = {
            let cache = self.context_window_cache.lock().unwrap();
            if cache.0 == ptr {
                cache.1.get(&role).copied()
            } else {
                None
            }
        } {
            return window;
        }
        // Slow path: resolve from the live router config. A concurrent
        // router swap between the fast-path miss and the insert is harmless:
        // the entry is stored under the pointer that was current at read
        // time and recomputed on the next miss.
        let cfg = router.config().await;
        let window = Self::context_window_for_role(&cfg, role)
            .unwrap_or(self.context_limits.default_context_window);
        let mut cache = self.context_window_cache.lock().unwrap();
        if cache.0 != ptr {
            cache.0 = ptr;
            cache.1.clear();
        }
        cache.1.insert(role, window);
        window
    }

    /// Build a compactor whose context window reflects the *actual* model for
    /// the role that will handle the step (explicit `context_window` config,
    /// else the builtin catalog, else `context_limits.default_context_window`).
    /// The window comes from `cached_context_window`, so a hot-swapped router
    /// config takes effect immediately without cloning the full config on
    /// every step. The compaction threshold (ratio and reserve) and the
    /// fallback window come from `context_limits`.
    async fn context_compactor(&self, role: EndpointRole) -> ContextCompactor {
        let window = self.cached_context_window(role).await;
        ContextCompactor::with_ratio(
            window,
            self.context_limits.compaction_reserve_tokens,
            self.context_limits.compaction_ratio,
        )
    }

    /// Incremental token estimate for a task's canonical message list.
    ///
    /// The estimate is cached per task: each step adds only the token count
    /// of the messages appended since the last pass instead of re-tokenizing
    /// the whole history (which is O(n) per step, O(n^2) over a long task).
    /// A full pass re-runs every `FULL_ESTIMATE_PASS_INTERVAL` calls and
    /// whenever the list shrank (sanitize drops, compaction), which bounds
    /// drift from mid-array inserts and from restored snapshots whose length
    /// coincidentally matches the cache. Under-counting by one message's
    /// worth of tokens is acceptable: the forced-compaction 400 retry remains
    /// the safety net for genuine overflow.
    fn estimate_canonical_tokens(&self, task_id: &str, canonical: &[CanonicalMessage]) -> u32 {
        const FULL_ESTIMATE_PASS_INTERVAL: u32 = 8;
        let mut cache = self.token_estimate_cache.lock().unwrap();
        let entry = cache
            .entry(task_id.to_string())
            .or_insert_with(TokenEstimate::default);
        let full_pass = entry.tokens == 0
            || entry.msgs_len > canonical.len()
            || entry.passes % FULL_ESTIMATE_PASS_INTERVAL == 0;
        if full_pass {
            entry.msgs_len = canonical.len();
            entry.tokens = estimate_message_tokens(canonical);
        } else if entry.msgs_len < canonical.len() {
            entry.tokens += estimate_message_tokens(&canonical[entry.msgs_len..]);
            entry.msgs_len = canonical.len();
        }
        entry.passes = entry.passes.saturating_add(1);
        entry.tokens
    }

    /// Drop the per-task token-estimate cache entry (called alongside
    /// `reset_cumulative_usage` on task completion/error).
    pub fn reset_token_estimate(&self, task_id: &str) {
        self.token_estimate_cache.lock().unwrap().remove(task_id);
    }

    /// Check if context compaction is needed before the next LLM call.
    ///
    /// Returns `true` when a compaction actually ran (the caller re-checks
    /// the image flag afterwards, since summarizing away the last image
    /// changes the endpoint routing).
    pub async fn maybe_compact(
        &self,
        task_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        has_image: bool,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) -> bool {
        if canonical.len() < 4 {
            return false;
        }
        // The compaction window must match the endpoint the next step will
        // use (image-routed steps compact against the image model's budget),
        // mirroring choose_agent_role's role selection.
        let router = self.router();
        let role = if has_image {
            router.vision_role().await
        } else {
            EndpointRole::DefaultModel
        };
        let compactor = self.context_compactor(role).await;
        // Compare the incremental estimate against the threshold directly;
        // `needs_compaction` would re-estimate the whole canonical and undo
        // the incremental cache.
        if self.estimate_canonical_tokens(task_id, canonical) <= compactor.threshold_tokens() {
            return false;
        }
        if let Some(result) = compactor.compact(canonical, &router).await {
            tracing::info!(
                "compaction for task {}: {} tokens -> {} tokens ({} msgs summarized)",
                task_id,
                result.tokens_before,
                result.tokens_after,
                result.summarized_count
            );
            *canonical = result.compacted;
            // Compaction replaced the list wholesale: the incremental
            // estimate is stale, drop it so the next step does a full pass.
            self.reset_token_estimate(task_id);
            EventDispatcher::emit_compaction_from(
                emitter,
                task_id,
                &result.summary,
                result.tokens_before,
                result.tokens_after,
            )
            .await;
            true
        } else {
            false
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

    // ── failure classification & retry nudge ──────────────────────────────

    #[test]
    fn classify_environmental_command_missing() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "'Get-FileHash' is not recognized as the name of a cmdlet, function, script file, or operable program"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "'curl' 不是内部或外部命令，也不是可运行的程序或批处理文件"
            ),
            FailureKind::Environmental
        );
    }

    #[test]
    fn classify_environmental_network() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "network",
                "tcp connect error: A connection attempt failed because the connected party did not properly respond"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "curl: (7) Failed to connect to host port 443: Connection refused"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "download timed out after 60s"),
            FailureKind::Environmental
        );
    }

    #[test]
    fn classify_environmental_paths() {
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "7z: cannot find archive path"),
            FailureKind::Environmental
        );
    }

    #[test]
    fn classify_logic_usage_errors() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "files",
                "input validation failed for 'files': MISSING REQUIRED FIELD(S): operation"
            ),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "files",
                "'old_string' is required for edit operation"
            ),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("files", "old_string not found in file"),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "invalid json in script"),
            FailureKind::Logic
        );
    }

    #[test]
    fn classify_unknown_falls_back() {
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "something odd happened"),
            FailureKind::Unknown
        );
    }

    #[test]
    fn failure_nudge_environmental_keeps_approach() {
        let nudge = ReActEngine::build_failure_nudge(&vec![(
            "shell".into(),
            "curl: (7) Failed to connect: Connection refused".into(),
        )]);
        assert!(
            !nudge.contains("completely different approach"),
            "environmental failures must not force a method switch, got: {nudge}"
        );
        assert!(nudge.contains("ENVIRONMENTAL"), "got: {nudge}");
        assert!(
            nudge.contains("curl"),
            "should mention tool switching, got: {nudge}"
        );
    }

    #[test]
    fn failure_nudge_logic_allows_method_switch_after_fix() {
        let nudge = ReActEngine::build_failure_nudge(&vec![(
            "files".into(),
            "'old_string' is required for edit operation".into(),
        )]);
        assert!(nudge.contains("logic errors"), "got: {nudge}");
        assert!(
            nudge.contains(
                "Only consider a completely different approach if the same method fails again"
            ),
            "method switch must be gated, got: {nudge}"
        );
    }

    #[test]
    fn failure_nudge_empty_falls_back_to_generic() {
        let nudge = ReActEngine::build_failure_nudge(&[]);
        assert!(nudge.contains("Diagnose the root cause"), "got: {nudge}");
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
        let has_image = messages
            .iter()
            .any(|m| m.content.iter().any(|p| matches!(p, ContentPart::Image { .. })));
        assert_eq!(
            choose_agent_role(&router, has_image).await,
            EndpointRole::DefaultModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_default_when_image_model_unconfigured() {
        let router = mock_router();
        let messages = vec![image_msg(LlmRole::User)];
        let has_image = messages
            .iter()
            .any(|m| m.content.iter().any(|p| matches!(p, ContentPart::Image { .. })));
        assert_eq!(
            choose_agent_role(&router, has_image).await,
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
        let has_image = messages
            .iter()
            .any(|m| m.content.iter().any(|p| matches!(p, ContentPart::Image { .. })));
        assert_eq!(
            choose_agent_role(&router, has_image).await,
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
        let has_image = messages
            .iter()
            .any(|m| m.content.iter().any(|p| matches!(p, ContentPart::Image { .. })));
        assert_eq!(
            choose_agent_role(&router, has_image).await,
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
