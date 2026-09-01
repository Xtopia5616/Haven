//! Concurrent execution and cancellation for one planned tool batch.
//!
//! This module owns one deterministic batch boundary: validation, safety
//! admission, bounded concurrent execution, cancellation repair, and ordered
//! result commit. Reusable execution primitives and confirmation state live
//! in `tool_batch.rs`; failure policy lives in `tool_batch_policy.rs`.

use super::hooks::{BeforeToolAction, ToolCallIdentity};
use super::snapshot_io::PauseTurnInput;
use super::tool_batch::{
    CompletedTool, MAX_CONCURRENT_TOOL_CALLS, MAX_RUNTIME_TOOL_CALLS_PER_BATCH, ToolBatchGate,
    ToolBatchOutcome, ToolBatchState, execute_tool_action,
};
use super::tool_batch_plan::ToolBatchPlan;
use super::*;
use crate::types::{Action, ConfirmPending, ConfirmPendingTool};
use haven_memory::repositories::session_steps::ActionStepOutcome;
use haven_tools::ToolConcurrency;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

impl ReActEngine {
    /// Execute the non-final actions for one step: emit Action cards, run the
    /// batch (parallel), drain observations, failure nudge, and ask pause.
    /// Behavior-preserving extract from `run_react_loop` (Phase 1 / E2).
    ///
    /// Phase 7 / E5: tool-input validation runs at the tool-batch boundary
    /// before Action cards are emitted — not in the thin loop. Invalid inputs
    /// become failed observations and are never rewritten.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_tool_batch(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_num: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
        actions: &mut [Action],
        thought: &Option<String>,
        response: &haven_llm::LlmResponse,
        cancel_res: &tokio_util::sync::CancellationToken,
        allow_tool_retry: bool,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let validation_failures = if !actions.is_empty() {
            self.validate_tool_inputs(session_id, actions).await
        } else {
            Vec::new()
        };
        if !validation_failures.is_empty() {
            tracing::warn!(
                "ReAct step {} session {} rejected {} invalid tool call(s)",
                step_num,
                session_id,
                validation_failures.len()
            );
        }
        // The model's array is the protocol order. Build one immutable plan
        // before safety gates or futures so canonical calls, UI cards, DB rows
        // and ordered observations all share the same identities.
        let plan = ToolBatchPlan::from_actions(actions);
        let non_final: Vec<&Action> = plan.iter().map(|planned| &planned.action).collect();
        let action_step_ids: Vec<String> =
            plan.iter().map(|planned| planned.step_id.clone()).collect();

        if !plan.is_empty() {
            let tool_calls = plan.canonical_calls();
            // Text matches Thought projection (trimmed) so review/resume
            // share one id/content; a retry-replaced response must not echo
            // the cut-off original text.
            // `parse_default_model_response` intentionally drops leaked
            // one-character tool-call fragments. Do not reintroduce the raw
            // response text when projecting the assistant/tool-call record.
            let push_text = thought.as_deref().unwrap_or("");
            let suppress_streamed_thought = thought.is_none() && !response.text.trim().is_empty();
            // A response mixing real tool calls with a web search round
            // carries both: the `web_search_call` items round-trip in the
            // same assistant message so the next request restores the
            // search context alongside the function tool results.
            // Phase 6.1 + X12: Action cards + pending rows + canonical via apply.
            // Thought already projected the messages row — no persist_text_id.
            let action_cards = plan.action_cards(suppress_streamed_thought);
            let step_ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num,
                run_id,
                emitter: emitter.clone(),
            };
            self.apply_transcript(
                &step_ctx,
                TranscriptEvent::ToolCall {
                    text: push_text.to_string(),
                    tool_calls,
                    reasoning: if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    web_search_calls: response.web_search_calls.clone(),
                    thinking_blocks: response.thinking_blocks.clone(),
                    action_cards,
                    persist_text_id: None,
                },
                state,
            )
            .await;
        }

        self.save_branch_point(session_id, state, step_num, false)
            .await;

        use futures_util::StreamExt;

        // Phase 5 / E3: pre-check every non-final action before spawning.
        // Proceed tools run in parallel; blocked calls become immediate
        // results; NeedConfirm is collected and pauses after the drain.
        let gate_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let mut need_confirm: Vec<ConfirmPendingTool> = Vec::new();
        let mut proceed: Vec<(usize, Action, Option<bool>, ToolConcurrency)> = Vec::new();
        let mut completed_results: Vec<Option<CompletedTool>> =
            (0..non_final.len()).map(|_| None).collect();
        // Accumulates result-derived control signals while keeping the
        // projection itself ordered by the assistant's tool-call list.
        let mut batch_state = ToolBatchState::default();

        for (idx, action) in non_final.iter().enumerate() {
            if idx >= MAX_RUNTIME_TOOL_CALLS_PER_BATCH {
                let step_id = action_step_ids[idx].clone();
                let error = format!(
                    "runtime tool-call limit ({MAX_RUNTIME_TOOL_CALLS_PER_BATCH}) exceeded; call was not executed"
                );
                self.executor
                    .finish_step_with_outcome(
                        session_id,
                        &action.tool_name,
                        &action.tool_input,
                        step_num,
                        idx as u32,
                        action.tool_call_id.as_deref(),
                        &step_id,
                        &error,
                        ActionStepOutcome::Failed,
                    )
                    .await;
                completed_results[idx] = Some(CompletedTool::failed(
                    (*action).clone(),
                    step_id,
                    idx as u32,
                    error,
                ));
                continue;
            }
            if let Some(failure) = validation_failures
                .iter()
                .find(|failure| failure.action_index == idx as u32)
            {
                let error = failure.render();
                let step_id = action_step_ids[idx].clone();
                self.executor
                    .finish_interrupted_step_with_identity(
                        session_id,
                        &action.tool_name,
                        &action.tool_input,
                        step_num,
                        idx as u32,
                        action.tool_call_id.as_deref(),
                        &step_id,
                        &error,
                    )
                    .await;
                completed_results[idx] = Some(CompletedTool::failed(
                    (*action).clone(),
                    step_id,
                    idx as u32,
                    error,
                ));
                continue;
            }
            match self
                .hooks
                .before_tool(
                    self,
                    &gate_ctx,
                    ToolCallIdentity {
                        step_id: &action_step_ids[idx],
                        action_index: idx as u32,
                        tool_call_id: action.tool_call_id.as_deref(),
                    },
                    &action.tool_name,
                    &action.tool_input,
                )
                .await
            {
                BeforeToolAction::Proceed { confirmed } => {
                    let concurrency = self
                        .executor
                        .tool_concurrency(session_id, &action.tool_name, &action.tool_input)
                        .await;
                    proceed.push((idx, (*action).clone(), confirmed, concurrency));
                }
                BeforeToolAction::Block { error } => {
                    let step_id = action_step_ids[idx].clone();
                    self.executor
                        .finish_interrupted_step_with_identity(
                            session_id,
                            &action.tool_name,
                            &action.tool_input,
                            step_num,
                            idx as u32,
                            action.tool_call_id.as_deref(),
                            &step_id,
                            &error,
                        )
                        .await;
                    completed_results[idx] = Some(CompletedTool::failed(
                        (*action).clone(),
                        step_id,
                        idx as u32,
                        error,
                    ));
                }
                BeforeToolAction::NeedConfirm { risk_level } => {
                    need_confirm.push(ConfirmPendingTool {
                        confirm_id: haven_common::types::new_id("conf"),
                        tool_name: action.tool_name.clone(),
                        tool_input: action.tool_input.clone(),
                        tool_call_id: action.tool_call_id.clone().unwrap_or_default(),
                        step_id: action_step_ids[idx].clone(),
                        action_index: idx as u32,
                        risk_level,
                        decision: None,
                    });
                }
            }
        }

        let gate = Arc::new(ToolBatchGate {
            all: Arc::new(RwLock::new(())),
            resources: AsyncMutex::new(HashMap::new()),
        });
        let started = Arc::new(
            (0..non_final.len())
                .map(|_| AtomicBool::new(false))
                .collect::<Vec<_>>(),
        );
        let mut tool_futures = futures_util::stream::iter(proceed)
            .map(|(idx, action, confirmed, concurrency)| {
                let session_id = session_id.to_string();
                let executor = self.executor.clone();
                let gate = gate.clone();
                let started = started.clone();
                // The same step id minted at Action-emit time keys the step
                // row execute_step creates, so the live card id, the DB badge
                // id and this step id are identical everywhere.
                let step_id = action_step_ids[idx].clone();
                let pre_confirmed = confirmed == Some(true);
                async move {
                    let _permit = gate.acquire(&concurrency).await;
                    // The call is considered in-flight only after its
                    // resource permit is acquired. A future waiting behind a
                    // conflicting write can therefore be cancelled as
                    // `cancelled`, not conservatively misreported unknown.
                    started[idx].store(true, Ordering::Release);
                    let result = execute_tool_action(
                        executor,
                        session_id,
                        action,
                        step_num,
                        idx as u32,
                        step_id,
                        pre_confirmed,
                    )
                    .await;
                    (idx, result)
                }
            })
            .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);

        // Drain tool results while remaining responsive to cancellation.
        // Without select!, a cancel arriving mid-batch would only be
        // detected at the next step boundary —after all tools finish.
        loop {
            tokio::select! {
                biased;
                _ = cancel_res.cancelled() => {
                    tracing::info!("ReAct loop cancelled during tool batch at step {}", step_num);
                    // Tool calls still in flight were cut off, not skipped:
                    // repair EACH one with an "Interrupted" result so the
                    // model sees the tool was attempted (and may retry it),
                    // and surface it in the UI as an interrupted
                    // observation card rather than leaving a silent gap.
                    for (idx, action) in non_final.iter().enumerate() {
                        if completed_results[idx].is_some() {
                            continue;
                        }
                        let was_started = started[idx].load(Ordering::Acquire);
                        let interrupted_text = if was_started {
                            crate::canonical::interrupted_result_text(
                                &action.tool_name,
                                &action.tool_input,
                            )
                        } else {
                            "tool call cancelled before execution".to_string()
                        };
                        let outcome = if was_started {
                            ActionStepOutcome::Unknown
                        } else {
                            ActionStepOutcome::Cancelled
                        };
                        let step_id = action_step_ids[idx].clone();
                        self.executor
                            .finish_step_with_outcome(
                                session_id,
                                &action.tool_name,
                                &action.tool_input,
                                step_num,
                                idx as u32,
                                action.tool_call_id.as_deref(),
                                &step_id,
                                &interrupted_text,
                                outcome,
                            )
                            .await;
                        completed_results[idx] = Some(CompletedTool::failed(
                            (*action).clone(),
                            step_id,
                            idx as u32,
                            interrupted_text,
                        ));
                    }
                    for result in completed_results.into_iter().flatten() {
                        batch_state
                            .commit_tool_result(self, &gate_ctx, result, state)
                            .await;
                    }
                    // A rollback that lands mid-batch must find the DB row
                    // at the pre-batch branch point (the response and
                    // partial tool results are discarded by the exit).
                    return Ok(ToolBatchOutcome::Done(
                        self.exit_cancelled(session_id, state, step_num)
                        .await,
                    ));
                }
                item = tool_futures.next() => {
                    let Some((idx, result)) = item else {
                        break;
                    };
                    completed_results[idx] = Some(result);
                }
            }
        }

        // Futures finish nondeterministically, but canonical tool messages are
        // an ordered protocol: each observation follows the corresponding
        // assistant call. Buffering only the projection keeps parallel tools
        // fast without making the next provider request depend on completion
        // order.
        for result in completed_results.into_iter().flatten() {
            batch_state
                .commit_tool_result(self, &gate_ctx, result, state)
                .await;
        }

        // Skip the retry nudge when the batch asked the user or is about to
        // pause for confirm: it would be baked into the paused snapshot ahead
        // of the user's real answer / decision. Phase 7 / G5: append onto the
        // last failed tool observation — never a synthetic User message.
        if batch_state.any_tool_failure
            && batch_state.asked_questions.is_empty()
            && need_confirm.is_empty()
            && allow_tool_retry
        {
            let nudge = Self::build_failure_nudge(&batch_state.failure_signals);
            if let Some(tool_call_id) = batch_state.last_failed_tool_call_id.clone() {
                state.stage_retry_nudge(tool_call_id, nudge);
            }
        }

        // Phase 5 / E3: confirm before ask when both appear in one batch.
        // Ask pause used to return first and drop NeedConfirm tools (Action
        // cards + assistant tool_calls with no results → Interrupted repair).
        // Prefer confirm pause; stash ask pending so finish_confirm_batch's
        // next turn still surfaces the question.
        if !need_confirm.is_empty() {
            if !batch_state.asked_questions.is_empty() {
                // Ask question rows were projected inside apply(ToolResult).
                let question = batch_state.asked_questions.join("\n\n");
                self.executor
                    .set_awaiting_answer(
                        session_id,
                        Some(crate::types::AskPending {
                            question,
                            step_ids: batch_state.ask_step_ids.clone(),
                        }),
                    )
                    .await;
            }
            let pending = ConfirmPending {
                step_number: step_num,
                tools: need_confirm,
            };
            self.executor
                .request_confirm_batch(session_id, pending)
                .await;
            // UI-only waiting notice in `messages` (not an LLM event — must
            // not enter `react_state.events` or resume would re-feed it).
            let notice = "Waiting for confirmation…";
            self.project_chat_message(session_id, "assistant", notice, Some("text"), None, None)
                .await;
            self.pause_turn(PauseTurnInput {
                session_id,
                state,
                snapshot_step: step_num + 1,
                emitter,
                status: SessionStatus::PausedAwaitingConfirm,
                final_text: notice,
                branch_point_step: None,
            })
            .await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Confirm,
            }));
        }

        // The agent asked the human a question: pause so the user can
        // answer. Their reply arrives as a supplement and resumes the session
        // (Paused —Pending —dispatcher re-enters the loop, injecting the
        // answer as context at the top of the next step).
        if !batch_state.asked_questions.is_empty() {
            let question = batch_state.asked_questions.join("\n\n");
            return self
                .pause_for_ask(
                    session_id,
                    state,
                    step_num,
                    emitter,
                    crate::types::AskPending {
                        question,
                        step_ids: batch_state.ask_step_ids.clone(),
                    },
                )
                .await;
        }

        let session_state = self.executor.get_session_state(session_id).await;
        match session_state {
            Some(s) if s.is_paused() => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_external_pause(session_id, state, step_num, emitter, run_id)
                        .await,
                ));
            }
            Some(SessionStatus::Error) => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_with_snapshot(
                        session_id,
                        state,
                        step_num,
                        LoopExit::Error("session interrupted".into()),
                    )
                    .await,
                ));
            }
            // Session gone (end_session/terminal cleanup) or completed: exit.
            None | Some(SessionStatus::Completed) => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_with_snapshot(session_id, state, step_num, LoopExit::Completed)
                        .await,
                ));
            }
            _ => {}
        }

        Ok(ToolBatchOutcome::Continue)
    }
}
