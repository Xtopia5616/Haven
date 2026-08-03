use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::types::LlmMessage;
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition, ToolFunction};
use haven_memory::Database;
use haven_memory::repositories::messages::MessageAttachment;
use haven_task::{TaskExecutor, TaskStatus};

use crate::compactor::ContextCompactor;
use crate::event::{AgentEventEmitter, EventDispatcher, UsagePayload};
use crate::session::SessionManager;
use crate::types::{Action, BranchPoint, ReActSnapshot, ReActStep};

/// Convert a stored message attachment into an image content part for the LLM.
pub(crate) fn attachment_to_content_part(att: &MessageAttachment) -> ContentPart {
    ContentPart::Image {
        content_type: "image_url".into(),
        media_type: att.media_type.clone(),
        data: att.data.clone(),
    }
}

/// Pick the endpoint role for an agent step. Conversations that carry image
/// content parts route through the router's vision role — the dedicated
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

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<TaskExecutor>,
    db: Arc<Database>,
    compactor: ContextCompactor,
    max_steps: Mutex<u32>,
    max_observation_chars: usize,
    balanced_model_notified: Mutex<HashSet<String>>,
    run_counter: AtomicU64,
    current_run_id: AtomicU64,
    /// Per-task cumulative token usage. Keyed by `task_id` so multiple
    /// parallel tasks each track their own counters. Reset on task
    /// completion to avoid leaking finished-task entries.
    cumulative_usage: Mutex<HashMap<String, CumulativeUsage>>,
}

#[derive(Debug, Clone, Default)]
struct CumulativeUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cost_usd: f64,
    has_cost: bool,
}

impl ReActEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<LlmRouter>,
        executor: Arc<TaskExecutor>,
        db: Arc<Database>,
        max_steps: u32,
        max_observation_chars: usize,
    ) -> Self {
        Self {
            router: Arc::new(RwLock::new(router)),
            executor,
            db,
            compactor: ContextCompactor::with_ratio(32_000, 4_096, 0.75),
            max_steps: Mutex::new(max_steps),
            max_observation_chars,
            balanced_model_notified: Mutex::new(HashSet::new()),
            run_counter: AtomicU64::new(0),
            current_run_id: AtomicU64::new(0),
            cumulative_usage: Mutex::new(HashMap::new()),
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
        sessions: &SessionManager,
        infer: &(dyn Fn() + Send + Sync),
        run_id: u64,
    ) -> anyhow::Result<()> {
        let max_steps = *self.max_steps.lock().unwrap();
        let mut last_step = start_step.saturating_sub(1);
        let session_id = sessions.current_session_id();

        for step_num in start_step..=max_steps {
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
                        );
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

            let supplements = self.executor.get_supplements(task_id).await;
            for supplement in &supplements {
                emitter
                    .emit(crate::event::AgentEvent::Supplement {
                        task_id: task_id.into(),
                        additional_context: supplement.text.clone(),
                        step_number: step_num,
                        run_id,
                    })
                    .await;
                let _ = self
                    .db
                    .create_thought_step(task_id, step_num as i32, &supplement.text);
                let mut content = vec![ContentPart::text(format!(
                    "Additional context from user: {}",
                    supplement.text
                ))];
                content.extend(
                    supplement
                        .attachments
                        .iter()
                        .map(attachment_to_content_part),
                );
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content,
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                    reasoning: None,
                });
            }

            let steering = self.executor.get_steering(task_id).await;
            for s in &steering {
                emitter
                    .emit(crate::event::AgentEvent::Supplement {
                        task_id: task_id.into(),
                        additional_context: s.text.clone(),
                        step_number: step_num,
                        run_id,
                    })
                    .await;
                let mut content = vec![ContentPart::text(format!("Steering: {}", s.text))];
                content.extend(s.attachments.iter().map(attachment_to_content_part));
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content,
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                    reasoning: None,
                });
            }

            // Deliver completed background-job results as context. These are
            // kept separate from the steering queue so job output is never
            // mistaken for a user reply (which would let the `ask` pause path
            // resume the task without the user's answer).
            let job_results = self.executor.drain_job_completions(task_id).await;
            for s in &job_results {
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Steering: {}", s))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                    reasoning: None,
                });
            }

            self.maybe_compact(task_id, canonical, &emitter).await;

            // Rebuild tool definitions each step so that per-task tools
            // registered by `load_skill` / `load_mcp` are visible to the LLM.
            let tools: Vec<ToolDefinition> = self.build_tool_definitions_for_task(task_id).await;

            let (chunk_tx, reasoning_tx, consumer_handle) =
                EventDispatcher::spawn_chunk_consumer_raw(&emitter);
            let chunk_tx_1 = chunk_tx.clone();
            let reasoning_tx_1 = reasoning_tx.clone();
            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
            let role = choose_agent_role(&router, &llm_messages).await;
            let task_id_1 = task_id.to_string();
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
            let pt1 = partial_thought.clone();
            let pr1 = partial_reasoning.clone();
            let response = match router
                .chat_stream_with_tools_aggregated_cancellable(
                    role,
                    llm_messages,
                    tools.to_vec(),
                    move |c: &haven_llm::StreamChunk| {
                        if let Some(t) = &c.text {
                            pt1.lock().unwrap().push_str(t);
                            if let Err(e) = chunk_tx_1.try_send((
                                task_id_1.clone(),
                                t.clone(),
                                step_num,
                                run_id,
                            )) {
                                tracing::warn!("thought chunk channel full, dropping: {}", e);
                            }
                        }
                        if let Some(r) = &c.reasoning {
                            pr1.lock().unwrap().push_str(r);
                            if let Err(e) = reasoning_tx_1.try_send((
                                task_id_1.clone(),
                                r.clone(),
                                step_num,
                                run_id,
                            )) {
                                tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                            }
                        }
                    },
                    cancel_res.clone(),
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
                    if let Some(result) = self.compactor.compact(canonical, &self.router()).await {
                        tracing::debug!(
                            "compacted {} → {} tokens",
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
                        let (chunk_tx2, reasoning_tx2, consumer_handle2) =
                            EventDispatcher::spawn_chunk_consumer_raw(&emitter);
                        let task_id_retry = task_id.to_string();
                        let llm_messages2 = haven_llm::types::convert_to_llm(canonical.clone());
                        // Reset the accumulators: the first attempt's partial
                        // text was based on pre-compaction context and should
                        // not be mixed with the retry's output.
                        partial_thought.lock().unwrap().clear();
                        partial_reasoning.lock().unwrap().clear();
                        let pt2 = partial_thought.clone();
                        let pr2 = partial_reasoning.clone();
                        match router
                            .chat_stream_with_tools_aggregated_cancellable(
                                role,
                                llm_messages2,
                                tools.to_vec(),
                                move |c: &haven_llm::StreamChunk| {
                                    if let Some(t) = &c.text {
                                        pt2.lock().unwrap().push_str(t);
                                        if let Err(e) = chunk_tx2.try_send((
                                            task_id_retry.clone(),
                                            t.clone(),
                                            step_num,
                                            run_id,
                                        )) {
                                            tracing::warn!(
                                                "retry thought chunk channel full, dropping: {}",
                                                e
                                            );
                                        }
                                    }
                                    if let Some(r) = &c.reasoning {
                                        pr2.lock().unwrap().push_str(r);
                                        if let Err(e) = reasoning_tx2.try_send((
                                            task_id_retry.clone(),
                                            r.clone(),
                                            step_num,
                                            run_id,
                                        )) {
                                            tracing::warn!(
                                                "retry reasoning chunk channel full, dropping: {}",
                                                e
                                            );
                                        }
                                    }
                                },
                                cancel_res.clone(),
                            )
                            .await
                        {
                            Ok(retry_resp) => {
                                if let Some(handle) = consumer_handle2 {
                                    let _ = handle.await;
                                }
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
                                    sessions,
                                    &session_id,
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
                            sessions,
                            &session_id,
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
                        sessions,
                        &session_id,
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

            drop(chunk_tx);
            drop(reasoning_tx);
            if let Some(handle) = consumer_handle {
                let _ = handle.await;
            }

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
                sessions.persist_message("assistant", reasoning, Some("reasoning"));
                // Reconcile the frontend's streamed reasoning with the
                // authoritative complete text. The frontend builds reasoning
                // only from batched deltas, so a dropped/delayed final chunk
                // would permanently lose trailing characters. Emitting the
                // complete reasoning as a final delta lets the frontend's
                // cumulative-detection (delta.startsWith(curr) → replace)
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

            let (thought, actions) = Self::parse_default_model_response(&response, step_num);
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

            if actions.is_empty() {
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
                self.executor
                    .update_task_status(task_id, TaskStatus::Paused)
                    .await?;
                emitter
                    .emit(crate::event::AgentEvent::TaskUpdated {
                        task_id: task_id.into(),
                        status: "paused".into(),
                    })
                    .await;
                sessions.persist_message("assistant", &msg, Some("text"));
                infer();
                self.save_branch_point(
                    task_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                    &session_id,
                );
                self.save_snapshot_with_branches(
                    task_id,
                    canonical,
                    history,
                    last_step + 1,
                    branch_points,
                );
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
                self.executor
                    .update_task_status(task_id, TaskStatus::Paused)
                    .await?;
                emitter
                    .emit(crate::event::AgentEvent::TaskUpdated {
                        task_id: task_id.into(),
                        status: "paused".into(),
                    })
                    .await;
                sessions.persist_message("assistant", &final_text, Some("text"));
                infer();
                self.save_branch_point(
                    task_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                    &session_id,
                );
                self.save_snapshot_with_branches(
                    task_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                );
                return Ok(());
            }

            if let Some(ref t) = thought {
                let text = t.trim();
                if !text.is_empty() {
                    sessions.persist_message("assistant", text, Some("text"));
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
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::Assistant,
                    content: vec![ContentPart::text(response.text.clone())],
                    tool_calls,
                    tool_call_id: None,
                    parent_message_id: None,
                    reasoning: response.reasoning.clone(),
                });
            }

            self.save_branch_point(
                task_id,
                canonical,
                history,
                step_num,
                branch_points,
                &session_id,
            );

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
                                let _ = db.record_tool_usage(&tool_name, &tool_input, r.success);
                                let text = if r.success {
                                    serde_json::to_string(&r.output)
                                        .unwrap_or_else(|_| "success".into())
                                } else {
                                    r.error.unwrap_or_else(|| "unknown failure".into())
                                };
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
                                    let question = r
                                        .output
                                        .get("question")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    let options = r
                                        .output
                                        .get("options")
                                        .and_then(|v| v.as_array())
                                        .map(|arr| {
                                            arr.iter()
                                                .filter_map(|o| o.as_str().map(|s| s.to_string()))
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    (question, options)
                                } else {
                                    (None, Vec::new())
                                };
                                // The notify signal (from the `notify` tool) is also
                                // read from the structured output BEFORE truncation,
                                // mirroring the ask handling above.
                                let (notify_title, notify_body) = if tool_name == "notify"
                                    && r.success
                                    && r.output.get("notify").and_then(|v| v.as_bool())
                                        == Some(true)
                                {
                                    let title = r
                                        .output
                                        .get("title")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Haven")
                                        .to_string();
                                    let body = r
                                        .output
                                        .get("body")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default()
                                        .to_string();
                                    (Some(title), Some(body))
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
            // detected at the next step boundary — after all tools finish.
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
                        // process_input → supplement → Paused→Pending resume.
                        if let Some(q) = &ask_question {
                            asked_questions.push(q.clone());
                        }
                        // `ask` must never be silent: hiding the question
                        // while the task pauses for an answer would leave the
                        // user waiting on a question they can't see.
                        let silent = tool_name != "ask"
                            && action
                                .tool_input
                                .get("silent")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                        // For `ask`, the chat/review bubble shows the readable
                        // question text; the canonical (model) context keeps
                        // the raw JSON so the model can still parse the flag.
                        // Same for `notify`: show a readable confirmation
                        // instead of the raw signal JSON.
                        let display_observation = if tool_name == "ask" {
                            ask_question.unwrap_or_else(|| step_result.clone())
                        } else if tool_name == "notify" {
                            let title = notify_title.clone().unwrap_or_else(|| "Haven".into());
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

                        canonical.push(CanonicalMessage {
                            role: CanonicalRole::Tool,
                            content: vec![ContentPart::text(step_result)],
                            tool_calls: None,
                            tool_call_id: action.tool_call_id.clone(),
                            parent_message_id: None,
                            reasoning: None,
                        });
                    }
                }
            }

            // Skip the "try a different approach" nudge when the batch asked
            // the user: it would be baked into the paused snapshot ahead of the
            // user's real answer, contradicting the pending question.
            if any_tool_failure && asked_questions.is_empty() && step_num < max_steps - 1 {
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(
                        "The previous approach encountered errors. Please try a completely different approach this time."
                    )],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                    reasoning: None,
                });
            }

            // The agent asked the human a question: pause so the user can
            // answer. Their reply arrives as a supplement and resumes the task
            // (Paused → Pending → dispatcher re-enters the loop, injecting the
            // answer as context at the top of the next step).
            if !asked_questions.is_empty() {
                let question = asked_questions.join("\n\n");
                sessions.persist_message("assistant", &question, Some("text"));
                // Save the snapshot at step_num+1 (matching the other pause
                // paths) so the resumed turn gets a fresh step number.
                self.save_snapshot_with_branches(
                    task_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                );
                // Mark the task as awaiting a human answer BEFORE setting the
                // status, so a background-job completion landing concurrently
                // cannot auto-wake it (the consumer checks this gate). The flag
                // is cleared centrally by `update_task_status` on reactivation.
                self.executor.set_awaiting_answer(task_id, true).await;
                // TOCTOU: a reply that arrived during the drain window went to
                // the steering queue (task was still Running). Convert it to a
                // supplement and, if present, resume immediately as Pending so
                // the answer isn't stranded while the task sits Paused. The
                // steering queue holds only user interjections now — background
                // job results are buffered separately — so `has_answer` truly
                // reflects a human reply.
                let steering = self.executor.get_steering(task_id).await;
                let has_answer = !steering.is_empty();
                for s in &steering {
                    let _ = self
                        .executor
                        .add_supplement_with_attachments(task_id, &s.text, &s.attachments)
                        .await;
                }
                let status = if has_answer {
                    TaskStatus::Pending
                } else {
                    TaskStatus::Paused
                };
                let status_str = status.as_str().to_string();
                self.executor.update_task_status(task_id, status).await?;
                emitter
                    .emit(crate::event::AgentEvent::TaskUpdated {
                        task_id: task_id.into(),
                        status: status_str,
                    })
                    .await;
                infer();
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
                );
                return Ok(());
            }
            if state == TaskStatus::Error || state == TaskStatus::Completed {
                return Ok(());
            }
        }

        self.executor
            .update_task_status(task_id, TaskStatus::Paused)
            .await?;
        emitter
            .emit(crate::event::AgentEvent::TaskUpdated {
                task_id: task_id.into(),
                status: "paused".into(),
            })
            .await;
        sessions.persist_message("assistant", "Task completed.", Some("text"));
        infer();
        self.save_snapshot_with_branches(task_id, canonical, history, last_step + 1, branch_points);
        Ok(())
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

    /// Save snapshot including branch points for tree-structured rollback (§2).
    fn save_snapshot_with_branches(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        let snapshot = ReActSnapshot {
            canonical: canonical.to_vec(),
            history: history.to_vec(),
            step_number,
            branch_points: branch_points.clone(),
        };
        if let Ok(json) = serde_json::to_string(&snapshot) {
            let _ = self.db.save_react_state(task_id, &json);
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
        sessions: &SessionManager,
        session_id: &str,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) {
        // Save a branch point BEFORE persisting the partial output, so
        // last_msg_at captures the timestamp of the last message BEFORE the
        // partial. This lets continue_task / rollback_task precisely delete
        // only the partial output via delete_messages_after(last_msg_at).
        // The canonical/history here represent the state BEFORE the failed
        // LLM call (the response was never pushed to canonical), so resuming
        // will retry the step cleanly.
        self.save_branch_point(
            task_id,
            canonical,
            history,
            step_num,
            branch_points,
            session_id,
        );

        let thought_text = partial_thought.lock().unwrap().clone();
        let reasoning_text = partial_reasoning.lock().unwrap().clone();
        if !reasoning_text.trim().is_empty() {
            sessions.persist_message("assistant", reasoning_text.trim(), Some("reasoning"));
        }
        if !thought_text.trim().is_empty() {
            let text = thought_text.trim();
            sessions.persist_message("assistant", text, Some("text"));
            EventDispatcher::emit_thought_from(emitter, task_id, text, step_num, run_id, &self.db)
                .await;
        }
    }

    /// Save a branch point at the current step before tool execution (§2).
    fn save_branch_point(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        session_id: &str,
    ) {
        let last_msg_at = self.db.get_last_message_created_at(session_id);
        branch_points.insert(
            step_number,
            BranchPoint {
                canonical: canonical.to_vec(),
                history: history.to_vec(),
                step_number,
                last_msg_at,
            },
        );
        self.save_snapshot_with_branches(task_id, canonical, history, step_number, branch_points);
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
            // No usage reported by the provider — nothing useful to surface.
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

        let (cum_prompt, cum_completion, cum_total, cum_cost_opt) = {
            let mut map = self.cumulative_usage.lock().unwrap();
            let entry = map.entry(task_id.to_string()).or_default();
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
            )
        };

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
        };
        // max_tokens is the per-response output cap, not the model's true
        // context window. Providers don't surface the input+output budget
        // here; consumers (UI) typically default to 32K when unknown. Return
        // None so the UI falls back rather than reporting an incorrect cap.
        if ep.max_tokens > 0 {
            Some(ep.max_tokens)
        } else {
            None
        }
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
        if !self.compactor.needs_compaction(canonical) {
            return;
        }
        let router = self.router();
        if let Some(result) = self.compactor.compact(canonical, &router).await {
            tracing::info!(
                "compaction for task {}: {} tokens → {} tokens ({} msgs summarized)",
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
}
