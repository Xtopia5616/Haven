use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition, ToolFunction};
use haven_memory::Database;
use haven_task::{TaskExecutor, TaskStatus};

use crate::compactor::ContextCompactor;
use crate::event::{AgentEventEmitter, EventDispatcher};
use crate::session::SessionManager;
use crate::types::{Action, BranchPoint, ReActSnapshot, ReActStep};

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

    /// Build the full tool-definition list for a task: global registry tools
    /// plus per-task skill/MCP adapters registered via `load_skill`/`load_mcp`.
    /// Called each step so freshly loaded tools are immediately visible.
    async fn build_tool_definitions_for_task(&self, task_id: &str) -> Vec<ToolDefinition> {
        let schemas = self.executor.get_tools().list_schemas_for_task(task_id).await;
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
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    TaskStatus::Error | TaskStatus::Completed => {
                        if state != TaskStatus::Completed {
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
                let cancel = self.executor.cancellation_token(task_id).await;
                if cancel.is_cancelled() {
                    return Ok(());
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
                        additional_context: supplement.clone(),
                        step_number: step_num,
                        run_id,
                    })
                    .await;
                let _ = self
                    .db
                    .create_thought_step(task_id, step_num as i32, supplement);
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
                emitter
                    .emit(crate::event::AgentEvent::Supplement {
                        task_id: task_id.into(),
                        additional_context: s.clone(),
                        step_number: step_num,
                        run_id,
                    })
                    .await;
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Steering: {}", s))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            self.maybe_compact(task_id, canonical, &emitter).await;

            // Rebuild tool definitions each step so that per-task tools
            // registered by `load_skill` / `load_mcp` are visible to the LLM.
            let tools: Vec<ToolDefinition> = self
                .build_tool_definitions_for_task(task_id)
                .await;

            let (chunk_tx, reasoning_tx, consumer_handle) =
                EventDispatcher::spawn_chunk_consumer_raw(&emitter);
            let chunk_tx_1 = chunk_tx.clone();
            let reasoning_tx_1 = reasoning_tx.clone();
            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
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
                llm_messages.iter().map(|m| (m.role.clone(), m.content.len())).collect::<Vec<_>>()
            );
            let pt1 = partial_thought.clone();
            let pr1 = partial_reasoning.clone();
            let response = match router
                .chat_stream_with_tools_aggregated_cancellable(
                    EndpointRole::DefaultModel,
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
                            ))
                            {
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
                            ))
                            {
                                tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                            }
                        }
                    },
                    cancel_res.clone(),
                )
                .await
            {
                Ok(resp) => {
                    if router.fallback_active() {
                        self.emit_fallback(&emitter, task_id, "switching to fallback model")
                            .await;
                    }
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
                                EndpointRole::DefaultModel,
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
                                        ))
                                        {
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
                                        ))
                                        {
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
                                retry_resp
                            }
                            Err(haven_llm::LlmError::Cancelled) => {
                                return Ok(());
                            }
                            Err(e2) => {
                                let err_msg = format!("Compaction retry also failed: {}", e2);
                                self.persist_partial_on_error(
                                    task_id, step_num, run_id,
                                    &partial_thought, &partial_reasoning,
                                    canonical, history, branch_points,
                                    sessions, &session_id, &emitter,
                                ).await;
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
                            task_id, step_num, run_id,
                            &partial_thought, &partial_reasoning,
                            canonical, history, branch_points,
                            sessions, &session_id, &emitter,
                        ).await;
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
                    let err_msg = format!("Both reasoner and fallback failed: {}", e);
                    self.persist_partial_on_error(
                        task_id, step_num, run_id,
                        &partial_thought, &partial_reasoning,
                        canonical, history, branch_points,
                        sessions, &session_id, &emitter,
                    ).await;
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
            }

            let (thought, actions) = Self::parse_reasoner_response(&response, step_num);
            tracing::trace!(
                "ReAct step {} parsed: thought={}, actions={}",
                step_num,
                thought.as_ref().map(|t| format!("{} chars", t.len())).unwrap_or_else(|| "none".into()),
                actions.iter().map(|a| a.tool_name.as_str()).collect::<Vec<_>>().join(", ")
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
                self.save_branch_point(task_id, canonical, history, step_num, branch_points, &session_id);
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
                self.save_branch_point(task_id, canonical, history, step_num, branch_points, &session_id);
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
                });
            }

            self.save_branch_point(task_id, canonical, history, step_num, branch_points, &session_id);

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
                        tool_input.as_object().map(|o| o.keys().collect::<Vec<_>>()).unwrap_or_default()
                    );
                    let result = executor
                        .execute_step(&task_id, &tool_name, tool_input.clone(), step_num)
                        .await;
                    let (text, is_error) = match result {
                        Ok(r) => {
                            tracing::debug!(
                                "tool '{}' at step {} completed: success={}, {} chars",
                                tool_name, step_num, r.success,
                                serde_json::to_string(&r.output).map(|s| s.len()).unwrap_or(0)
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
                            (text, !r.success)
                        }
                        Err(e) => {
                            tracing::debug!("tool '{}' at step {} failed: {}", tool_name, step_num, e);
                            (e.to_string(), true)
                        }
                    };
                    (action, tool_name, text, is_error)
                });
            }

            let mut any_tool_failure = false;
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
                        let Some((action, tool_name, step_result, is_error)) = item else { break; };
                        if is_error {
                            any_tool_failure = true;
                        }
                        let silent = action
                            .tool_input
                            .get("silent")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        emitter
                            .emit(crate::event::AgentEvent::Observation {
                                task_id: task_id.into(),
                                observation: step_result.clone(),
                                tool_name: tool_name.clone(),
                                step_number: step_num,
                                run_id,
                                silent,
                                tool_call_id: action.tool_call_id.clone(),
                            })
                            .await;

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
                }
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
                self.save_snapshot_with_branches(
                    task_id,
                    canonical,
                    history,
                    step_num,
                    branch_points,
                );
                return Ok(());
            }
            if state == TaskStatus::Error
                || state == TaskStatus::Completed
            {
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
        self.save_branch_point(task_id, canonical, history, step_num, branch_points, session_id);

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

    /// Emit fallback activated with per-task deduplication.
    async fn emit_fallback(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        reason: &str,
    ) {
        let should_emit = {
            let mut notified = self.fallback_notified.lock().unwrap();
            notified.insert(task_id.to_string())
        };
        if should_emit {
            EventDispatcher::emit_fallback_activated_from(emitter, task_id, reason).await;
        }
    }

    /// Emit task error and clean up fallback dedup state.
    async fn emit_error(&self, emitter: &Arc<dyn AgentEventEmitter>, task_id: &str, error: &str) {
        {
            let mut notified = self.fallback_notified.lock().unwrap();
            notified.remove(task_id);
        }
        EventDispatcher::emit_task_error_from(emitter, task_id, error).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_llm::types::{FinishReason, LlmResponse, ToolCall};

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
        let (thought, actions) = ReActEngine::parse_reasoner_response(&r, 1);
        assert_eq!(thought, None);
        assert!(actions.is_empty());
    }

    #[test]
    fn parse_text_only_no_finish_reason_keeps_thought_no_action() {
        // step_number=1, Stop finish, but step>0 required for implicit final.
        let r = resp("hello", vec![], Some(FinishReason::Stop));
        let (thought, actions) = ReActEngine::parse_reasoner_response(&r, 0);
        assert_eq!(thought.as_deref(), Some("hello"));
        assert!(actions.is_empty(), "step 0 must not auto-finalize");
    }

    #[test]
    fn parse_text_with_stop_finish_step_nonzero_auto_finalizes() {
        let r = resp("the answer is 42", vec![], Some(FinishReason::Stop));
        let (thought, actions) = ReActEngine::parse_reasoner_response(&r, 1);
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
        let (thought, actions) = ReActEngine::parse_reasoner_response(&r, 1);
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
        let (_, actions) = ReActEngine::parse_reasoner_response(&r, 2);
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
            let (_, actions) = ReActEngine::parse_reasoner_response(&r, 1);
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
        let (_, actions) = ReActEngine::parse_reasoner_response(&r, 1);
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
        let (_, actions) = ReActEngine::parse_reasoner_response(&r, 1);
        assert!(actions[0].tool_input.is_null());
    }

    #[test]
    fn parse_multiple_tool_calls_preserve_order() {
        let tcs = vec![
            ToolCall { id: "a".into(), name: "search".into(), arguments: "{}".into() },
            ToolCall { id: "b".into(), name: "read_file".into(), arguments: "{}".into() },
        ];
        let r = resp("multi", tcs, Some(FinishReason::ToolCalls));
        let (_, actions) = ReActEngine::parse_reasoner_response(&r, 1);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].tool_name, "search");
        assert_eq!(actions[1].tool_name, "read_file");
    }

    #[test]
    fn parse_tool_calls_take_precedence_over_text_final() {
        // Even with Stop finish + step>0, tool_calls win over implicit final.
        let tc = ToolCall { id: "x".into(), name: "read_file".into(), arguments: "{}".into() };
        let r = resp("text", vec![tc], Some(FinishReason::Stop));
        let (_, actions) = ReActEngine::parse_reasoner_response(&r, 1);
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
    }

    #[test]
    fn parse_text_trimmed_for_thought() {
        let r = resp("  spaced thought  ", vec![], Some(FinishReason::Stop));
        let (thought, _) = ReActEngine::parse_reasoner_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("spaced thought"));
    }
}
