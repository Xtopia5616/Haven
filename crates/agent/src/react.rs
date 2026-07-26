use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition};
use haven_memory::Database;
use haven_task::{TaskExecutor, TaskStatus};

use crate::compactor::ContextCompactor;
use crate::event::{AgentEventEmitter, EventDispatcher};
use crate::session::SessionManager;
use crate::types::{Action, BranchPoint, ReActSnapshot, ReActStep};

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<TaskExecutor>,
    db: Arc<Database>,
    compactor: ContextCompactor,
    max_steps: Mutex<u32>,
    max_observation_chars: usize,
    fallback_notified: Mutex<HashSet<String>>,
    run_counter: AtomicU64,
    current_run_id: AtomicU64,
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
            compactor: ContextCompactor::new(32_000, 4_096),
            max_steps: Mutex::new(max_steps),
            max_observation_chars,
            fallback_notified: Mutex::new(HashSet::new()),
            run_counter: AtomicU64::new(0),
            current_run_id: AtomicU64::new(0),
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

    fn router(&self) -> Arc<LlmRouter> {
        self.router.read().unwrap().clone()
    }

    /// Shared ReAct loop body. Runs from `start_step` through `max_steps`.
    /// Called by both `run_task` (fresh) and `run_task_resumed` (resumed from
    /// snapshot).
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
        tools: &[ToolDefinition],
        infer: &(dyn Fn() + Send + Sync),
    ) -> anyhow::Result<()> {
        let max_steps = *self.max_steps.lock().unwrap();
        let mut last_step = start_step.saturating_sub(1);

        for step_num in start_step..=max_steps {
            last_step = step_num;
            loop {
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    TaskStatus::Cancelled => {
                        return Ok(());
                    }
                    TaskStatus::Error | TaskStatus::Completed => {
                        if state != TaskStatus::Completed {
                            self.emit_error(&emitter, task_id, "task interrupted").await;
                        }
                        return Ok(());
                    }
                    TaskStatus::Paused => {
                        self.save_snapshot_with_branches(task_id, canonical, history, step_num, branch_points);
                    }
                    _ => break,
                }
                let cancel = self.executor.cancellation_token(task_id).await;
                if cancel.is_cancelled() {
                    return Ok(());
                }
                if self.executor.get_task_state(task_id).await == TaskStatus::Paused {
                    self.executor.status_notifier(task_id).await.notified().await;
                }
            }

            let run_id = self.current_run_id.load(Ordering::SeqCst);
            let supplements = self.executor.get_supplements(task_id).await;
            for supplement in &supplements {
                emitter.emit(crate::event::AgentEvent::Supplement {
                    task_id: task_id.into(),
                    additional_context: supplement.clone(),
                    step_number: step_num,
                    run_id,
                }).await;
                let _ = self.db.create_thought_step(
                    task_id,
                    history.len() as i32,
                    supplement,
                );
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!(
                        "Additional context from user: {}",
                        supplement
                    ))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            let steering = self.executor.get_steering(task_id).await;
            for s in &steering {
                emitter.emit(crate::event::AgentEvent::Supplement {
                    task_id: task_id.into(),
                    additional_context: s.clone(),
                    step_number: step_num,
                    run_id,
                }).await;
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Steering: {}", s))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            self.maybe_compact(task_id, canonical, &emitter).await;

            let (chunk_tx, reasoning_tx, consumer_handle) = EventDispatcher::spawn_chunk_consumer_raw(&emitter);
            let chunk_tx_1 = chunk_tx.clone();
            let reasoning_tx_1 = reasoning_tx.clone();
            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
            let task_id_1 = task_id.to_string();
            tracing::info!("ReAct step {} calling LLM, messages count: {}", step_num, llm_messages.len());
            let response = match router
                .chat_stream_with_tools_aggregated_cancellable(
                    EndpointRole::DefaultModel,
                    llm_messages,
                    tools.to_vec(),
                    move |c: &haven_llm::StreamChunk| {
                        if let Some(t) = &c.text
                            && let Err(e) = chunk_tx_1.try_send((task_id_1.clone(), t.clone(), step_num, run_id))
                        {
                            tracing::warn!("thought chunk channel full, dropping: {}", e);
                        }
                        if let Some(r) = &c.reasoning
                            && let Err(e) = reasoning_tx_1.try_send((task_id_1.clone(), r.clone(), step_num, run_id))
                        {
                            tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                        }
                    },
                    cancel_res.clone(),
                )
                .await
            {
                Ok(resp) => {
                    if router.fallback_active() {
                        EventDispatcher::emit_fallback_activated_from(&emitter, task_id, "switching to fallback model").await;
                    }
                    resp
                }
                Err(haven_llm::LlmError::ContextLengthExceeded) => {
                    tracing::warn!("context length exceeded for task {}, forcing compaction", task_id);
                    if let Some(result) = self.compactor.compact(canonical, &self.router()).await {
                        tracing::info!("compacted {} → {} tokens", result.tokens_before, result.tokens_after);
                        *canonical = result.compacted;
                        EventDispatcher::emit_compaction_from(&emitter, task_id, &result.summary, result.tokens_before, result.tokens_after).await;
                        let (chunk_tx2, reasoning_tx2, consumer_handle2) = EventDispatcher::spawn_chunk_consumer_raw(&emitter);
                        let task_id_retry = task_id.to_string();
                        let llm_messages2 = haven_llm::types::convert_to_llm(canonical.clone());
                        match router
                            .chat_stream_with_tools_aggregated_cancellable(
                                EndpointRole::DefaultModel,
                                llm_messages2,
                                tools.to_vec(),
                                move |c: &haven_llm::StreamChunk| {
                                    if let Some(t) = &c.text
                                        && let Err(e) = chunk_tx2.try_send((task_id_retry.clone(), t.clone(), step_num, run_id))
                                    {
                                        tracing::warn!("retry thought chunk channel full, dropping: {}", e);
                                    }
                                    if let Some(r) = &c.reasoning
                                        && let Err(e) = reasoning_tx2.try_send((task_id_retry.clone(), r.clone(), step_num, run_id))
                                    {
                                        tracing::warn!("retry reasoning chunk channel full, dropping: {}", e);
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
                                retry_resp
                            }
                            Err(haven_llm::LlmError::Cancelled) => {
                                return Ok(());
                            }
                            Err(e2) => {
                                let err_msg = format!("Compaction retry also failed: {}", e2);
                                self.emit_error(&emitter, task_id, &err_msg).await;
                                self.executor.update_task_status(task_id, TaskStatus::Error).await?;
                                return Err(anyhow::anyhow!("{}", err_msg));
                            }
                        }
                    } else {
                        let err_msg = "context length exceeded but compaction failed".to_string();
                        EventDispatcher::emit_task_error_from(&emitter, task_id, &err_msg).await;
                        self.executor.update_task_status(task_id, TaskStatus::Error).await?;
                        return Err(anyhow::anyhow!("{}", err_msg));
                    }
                }
                Err(haven_llm::LlmError::Cancelled) => {
                    return Ok(());
                }
                Err(e) => {
                    let err_msg = format!("Both reasoner and fallback failed: {}", e);
                    EventDispatcher::emit_task_error_from(&emitter, task_id, &err_msg).await;
                    self.executor.update_task_status(task_id, TaskStatus::Error).await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
            };

            drop(chunk_tx);
            drop(reasoning_tx);
            if let Some(handle) = consumer_handle {
                let _ = handle.await;
            }

            if let Some(ref reasoning) = response.reasoning {
                sessions.persist_message("assistant", reasoning, Some("reasoning"));
            }

            let (thought, actions) = Self::parse_reasoner_response(&response, step_num);

            if let Some(ref t) = thought {
                EventDispatcher::emit_thought_from(&emitter, task_id, t, step_num, run_id, &self.db).await;
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
                self.executor.update_task_status(task_id, TaskStatus::Paused).await?;
                emitter.emit(crate::event::AgentEvent::TaskUpdated {
                    task_id: task_id.into(),
                    status: "paused".into(),
                }).await;
                sessions.persist_message("assistant", &msg, Some("text"));
                infer();
                self.save_branch_point(task_id, canonical, history, step_num, branch_points);
                self.save_snapshot_with_branches(task_id, canonical, history, last_step + 1, branch_points);
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
                self.executor.update_task_status(task_id, TaskStatus::Paused).await?;
                emitter.emit(crate::event::AgentEvent::TaskUpdated {
                    task_id: task_id.into(),
                    status: "paused".into(),
                }).await;
                sessions.persist_message("assistant", &final_text, Some("text"));
                infer();
                self.save_branch_point(task_id, canonical, history, step_num, branch_points);
                self.save_snapshot_with_branches(task_id, canonical, history, step_num + 1, branch_points);
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
                emitter.emit(crate::event::AgentEvent::Action {
                    task_id: task_id.into(),
                    tool_name: action.tool_name.clone(),
                    input: action.tool_input.clone(),
                    step_number: step_num,
                    run_id,
                    tool_call_id: action.tool_call_id.clone(),
                }).await;
            }

            if !non_final.is_empty() {
                let tool_calls: Option<Vec<CanonicalToolCall>> = if response.tool_calls.is_empty() {
                    None
                } else {
                    Some(response.tool_calls.iter().map(|tc| CanonicalToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null),
                    }).collect())
                };
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::Assistant,
                    content: vec![ContentPart::text(response.text.clone())],
                    tool_calls,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            self.save_branch_point(task_id, canonical, history, step_num, branch_points);

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
                    let result = executor
                        .execute_step(&task_id, &tool_name, tool_input.clone())
                        .await;
                    let (text, is_error) = match result {
                        Ok(r) => {
                            let _ = db.record_tool_usage(&tool_name, &tool_input, r.success);
                            let text = if r.success {
                                serde_json::to_string(&r.output)
                                    .unwrap_or_else(|_| "success".into())
                            } else {
                                r.error.unwrap_or_else(|| "unknown failure".into())
                            };
                            let text = if text.len() > max_obs {
                                format!("{}[... truncated {} chars omitted]", &text[..max_obs], text.len() - max_obs)
                            } else {
                                text
                            };
                            (text, !r.success)
                        }
                        Err(e) => (e.to_string(), true),
                    };
                    (action, tool_name, text, is_error)
                });
            }

            let mut any_tool_failure = false;
            while let Some((action, tool_name, step_result, is_error)) = tool_futures.next().await {
                if is_error {
                    any_tool_failure = true;
                }
                let silent = action.tool_input.get("silent").and_then(|v| v.as_bool()).unwrap_or(false);
                emitter.emit(crate::event::AgentEvent::Observation {
                    task_id: task_id.into(),
                    observation: step_result.clone(),
                    tool_name: tool_name.clone(),
                    step_number: step_num,
                    run_id,
                    silent,
                    tool_call_id: action.tool_call_id.clone(),
                }).await;

                if let Some(last) = history.last_mut() {
                    last.action = Some(action.clone());
                    last.observation = Some(step_result.clone());
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
                });
            }

            if any_tool_failure && step_num < max_steps - 1 {
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(
                        "The previous approach encountered errors. Please try a completely different approach this time."
                    )],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            let state = self.executor.get_task_state(task_id).await;
            if state == TaskStatus::Paused {
                self.save_snapshot_with_branches(task_id, canonical, history, step_num, branch_points);
                return Ok(());
            }
            if state == TaskStatus::Cancelled || state == TaskStatus::Error || state == TaskStatus::Completed {
                return Ok(());
            }
        }

        self.executor.update_task_status(task_id, TaskStatus::Paused).await?;
        emitter.emit(crate::event::AgentEvent::TaskUpdated {
            task_id: task_id.into(),
            status: "paused".into(),
        }).await;
        sessions.persist_message("assistant", "Task completed.", Some("text"));
        infer();
        self.save_snapshot_with_branches(task_id, canonical, history, last_step + 1, branch_points);
        Ok(())
    }

    /// Parse LLM response into thought text and actions.
    pub fn parse_reasoner_response(
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
                        tool_call_id: Some(
                            if tc.id.is_empty() {
                                uuid::Uuid::new_v4().to_string()
                            } else {
                                tc.id.clone()
                            },
                        ),
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

    /// Save a branch point at the current step before tool execution (§2).
    fn save_branch_point(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
    ) {
        branch_points.insert(step_number, BranchPoint {
            canonical: canonical.to_vec(),
            history: history.to_vec(),
            step_number,
        });
        self.save_snapshot_with_branches(task_id, canonical, history, step_number, branch_points);
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
                task_id, result.tokens_before, result.tokens_after, result.summarized_count
            );
            *canonical = result.compacted;
            EventDispatcher::emit_compaction_from(emitter, task_id, &result.summary, result.tokens_before, result.tokens_after).await;
        }
    }

    /// Emit fallback activated with per-task deduplication.
    async fn emit_fallback(&self, emitter: &Arc<dyn AgentEventEmitter>, task_id: &str, reason: &str) {
        let mut notified = self.fallback_notified.lock().unwrap();
        if !notified.insert(task_id.to_string()) {
            return;
        }
        drop(notified);
        EventDispatcher::emit_fallback_activated_from(emitter, task_id, reason).await;
    }

    /// Emit task error and clean up fallback dedup state.
    async fn emit_error(&self, emitter: &Arc<dyn AgentEventEmitter>, task_id: &str, error: &str) {
        self.fallback_notified.lock().unwrap().remove(task_id);
        EventDispatcher::emit_task_error_from(emitter, task_id, error).await;
    }
}