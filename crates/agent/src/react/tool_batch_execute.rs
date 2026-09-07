//! Concurrent execution and cancellation for one planned tool batch.
//!
//! This module owns one deterministic batch boundary: validation, safety
//! admission, bounded concurrent execution, cancellation repair, and ordered
//! result commit. Result slots and observation projection live in
//! `tool_batch.rs`; failure policy lives in `tool_batch_policy.rs`.

use super::hooks::{BeforeToolAction, ToolCallIdentity};
use super::snapshot_io::PauseTurnInput;
use super::tool_batch::{
    CompletedTool, MAX_CONCURRENT_TOOL_CALLS, MAX_RUNTIME_TOOL_CALLS_PER_BATCH, ToolBatchGate,
    ToolBatchOutcome, ToolBatchResults, ToolBatchState, execute_tool_action,
};
use super::tool_batch_plan::ToolBatchPlan;
use super::tool_batch_policy::ToolRetryBudget;
use super::*;
use crate::types::{Action, ConfirmPending, ConfirmPendingTool};
use futures_util::StreamExt;
use haven_common::types::RiskLevel;
use haven_memory::repositories::session_steps::ActionStepOutcome;
use haven_tools::{ToolConcurrency, ToolExecutionOutcome};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

fn cancellation_observation(
    planned: &super::tool_batch_plan::PlannedTool,
    was_started: bool,
) -> String {
    if was_started {
        crate::canonical::interrupted_result_text(
            &planned.action.tool_name,
            &planned.action.tool_input,
        )
    } else {
        "tool call cancelled before execution".to_string()
    }
}

fn rejection_observation(tool_name: &str) -> String {
    format!(
        "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
        tool_name
    )
}

fn tool_execution_outcome(outcome: ActionStepOutcome) -> ToolExecutionOutcome {
    match outcome {
        ActionStepOutcome::Completed => ToolExecutionOutcome::Succeeded,
        ActionStepOutcome::Failed => ToolExecutionOutcome::Failed,
        ActionStepOutcome::Cancelled => ToolExecutionOutcome::Cancelled,
        ActionStepOutcome::Unknown => ToolExecutionOutcome::TimedOutUnknown,
    }
}

struct AdmittedTool {
    plan_index: usize,
    pre_confirmed: bool,
    concurrency: ToolConcurrency,
}

struct DeferredAdmissionFailure {
    plan_index: usize,
    error: String,
}

struct ToolBatchAdmission {
    runnable: Vec<AdmittedTool>,
    need_confirm: Vec<ConfirmPendingTool>,
    failures: Vec<DeferredAdmissionFailure>,
    results: ToolBatchResults,
}

struct ToolBatchExecution {
    results: ToolBatchResults,
    cancelled: bool,
}

impl ReActEngine {
    /// Perform all pre-execution decisions against one immutable plan. Failed
    /// admission is normalized into the same observation type as a tool
    /// failure; only approved calls reach the executor.
    async fn admit_tool_batch(
        &self,
        session_id: &str,
        step_num: u32,
        gate_ctx: &StepCtx,
        plan: &ToolBatchPlan,
        validation_failures: &[ToolInputValidationFailure],
    ) -> ToolBatchAdmission {
        let mut admission = ToolBatchAdmission {
            runnable: Vec::new(),
            need_confirm: Vec::new(),
            failures: Vec::new(),
            results: ToolBatchResults::new(plan.len()),
        };

        for (plan_index, planned) in plan.iter().enumerate() {
            if plan_index >= MAX_RUNTIME_TOOL_CALLS_PER_BATCH {
                let error = format!(
                    "runtime tool-call limit ({MAX_RUNTIME_TOOL_CALLS_PER_BATCH}) exceeded; call was not executed"
                );
                admission
                    .failures
                    .push(DeferredAdmissionFailure { plan_index, error });
                continue;
            }
            if let Some(failure) = validation_failures
                .iter()
                .find(|failure| failure.action_index == planned.action_index)
            {
                admission.failures.push(DeferredAdmissionFailure {
                    plan_index,
                    error: failure.render(),
                });
                continue;
            }

            match self
                .hooks
                .before_tool(
                    self,
                    gate_ctx,
                    ToolCallIdentity {
                        step_id: &planned.step_id,
                        action_index: planned.action_index,
                        tool_call_id: planned.action.tool_call_id.as_deref(),
                    },
                    &planned.action.tool_name,
                    &planned.action.tool_input,
                )
                .await
            {
                BeforeToolAction::Proceed { confirmed } => {
                    let concurrency = self
                        .executor
                        .tool_concurrency(
                            session_id,
                            &planned.action.tool_name,
                            &planned.action.tool_input,
                        )
                        .await;
                    admission.runnable.push(AdmittedTool {
                        plan_index,
                        pre_confirmed: confirmed == Some(true),
                        concurrency,
                    });
                }
                BeforeToolAction::Block { error } => {
                    admission
                        .failures
                        .push(DeferredAdmissionFailure { plan_index, error });
                }
                BeforeToolAction::NeedConfirm { risk_level } => {
                    admission.need_confirm.push(ConfirmPendingTool {
                        confirm_id: haven_common::types::new_id("conf"),
                        tool_name: planned.action.tool_name.clone(),
                        tool_input: planned.action.tool_input.clone(),
                        tool_call_id: planned.action.tool_call_id.clone().unwrap_or_default(),
                        step_id: planned.step_id.clone(),
                        action_index: planned.action_index,
                        risk_level,
                        decision: None,
                    });
                }
            }
        }

        if admission.need_confirm.is_empty() {
            for failure in admission.failures.drain(..) {
                let planned = plan
                    .get(failure.plan_index)
                    .expect("admission failure must reference a plan entry");
                admission.results.set(
                    failure.plan_index,
                    self.failed_admission_tool(session_id, step_num, planned, failure.error)
                        .await,
                );
            }
        } else {
            // A confirmation is a barrier for the whole assistant batch. Do
            // not execute any sibling before the user decides: otherwise a
            // later result would need a second durable in-memory batch state
            // to survive the pause. Every plan entry is carried in one
            // pending record and resumed through the same ordered slots.
            let gated_by_index: HashMap<u32, haven_common::types::RiskLevel> = admission
                .need_confirm
                .iter()
                .map(|tool| (tool.action_index, tool.risk_level))
                .collect();
            admission.need_confirm = plan
                .iter()
                .map(|planned| {
                    let risk_level = gated_by_index
                        .get(&planned.action_index)
                        .copied()
                        .unwrap_or(haven_common::types::RiskLevel::Safe);
                    ConfirmPendingTool {
                        confirm_id: haven_common::types::new_id("conf"),
                        tool_name: planned.action.tool_name.clone(),
                        tool_input: planned.action.tool_input.clone(),
                        tool_call_id: planned.action.tool_call_id.clone().unwrap_or_default(),
                        step_id: planned.step_id.clone(),
                        action_index: planned.action_index,
                        risk_level,
                        // Safe, blocked, invalid, and already trusted calls do
                        // not need a user decision. They still wait behind
                        // the same batch barrier and are revalidated on
                        // resume.
                        decision: (!gated_by_index.contains_key(&planned.action_index))
                            .then_some(true),
                    }
                })
                .collect();
            admission.runnable.clear();
            admission.failures.clear();
        }

        admission
    }

    async fn failed_admission_tool(
        &self,
        session_id: &str,
        step_num: u32,
        planned: &super::tool_batch_plan::PlannedTool,
        error: String,
    ) -> CompletedTool {
        self.executor
            .finish_interrupted_step_with_identity(
                session_id,
                &planned.action.tool_name,
                &planned.action.tool_input,
                step_num,
                planned.action_index,
                planned.action.tool_call_id.as_deref(),
                &planned.step_id,
                &error,
            )
            .await;
        CompletedTool::from_observation(
            planned.action.clone(),
            planned.step_id.clone(),
            planned.action_index,
            error,
            ToolExecutionOutcome::Failed,
        )
    }

    /// Execute admitted calls concurrently while preserving the plan-indexed
    /// result slots. Cancellation repairs every slot that did not produce a
    /// normal result before returning to the shared ordered projector.
    async fn execute_admitted_tools(
        &self,
        session_id: &str,
        step_num: u32,
        plan: &ToolBatchPlan,
        runnable: Vec<AdmittedTool>,
        mut results: ToolBatchResults,
        cancel_res: &tokio_util::sync::CancellationToken,
    ) -> ToolBatchExecution {
        let gate = Arc::new(ToolBatchGate {
            all: Arc::new(RwLock::new(())),
            resources: AsyncMutex::new(HashMap::new()),
        });
        let started = Arc::new(
            (0..plan.len())
                .map(|_| AtomicBool::new(false))
                .collect::<Vec<_>>(),
        );
        let mut tool_futures = futures_util::stream::iter(runnable)
            .map(|admitted| {
                let planned = plan
                    .get(admitted.plan_index)
                    .expect("admission must reference a plan entry");
                let action = planned.action.clone();
                let action_index = planned.action_index;
                let step_id = planned.step_id.clone();
                let session_id = session_id.to_string();
                let executor = self.executor.clone();
                let gate = gate.clone();
                let started = started.clone();
                async move {
                    let _permit = gate.acquire(&admitted.concurrency).await;
                    started[admitted.plan_index].store(true, Ordering::Release);
                    let result = execute_tool_action(
                        executor,
                        session_id,
                        action,
                        step_num,
                        action_index,
                        step_id,
                        admitted.pre_confirmed,
                    )
                    .await;
                    (admitted.plan_index, result)
                }
            })
            .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);

        loop {
            tokio::select! {
                biased;
                _ = cancel_res.cancelled() => {
                    tracing::info!("ReAct loop cancelled during tool batch at step {}", step_num);
                    self.repair_cancelled_results(session_id, step_num, plan, &started, &mut results).await;
                    return ToolBatchExecution { results, cancelled: true };
                }
                item = tool_futures.next() => {
                    let Some((plan_index, result)) = item else {
                        break;
                    };
                    results.set(plan_index, result);
                }
            }
        }

        ToolBatchExecution {
            results,
            cancelled: false,
        }
    }

    async fn repair_cancelled_results(
        &self,
        session_id: &str,
        step_num: u32,
        plan: &ToolBatchPlan,
        started: &[AtomicBool],
        results: &mut ToolBatchResults,
    ) {
        for (plan_index, planned) in plan.iter().enumerate() {
            if results.is_set(plan_index) {
                continue;
            }
            let was_started = started[plan_index].load(Ordering::Acquire);
            let interrupted_text = cancellation_observation(planned, was_started);
            let outcome = if was_started {
                ActionStepOutcome::Unknown
            } else {
                ActionStepOutcome::Cancelled
            };
            self.executor
                .finish_step_with_outcome(
                    session_id,
                    &planned.action.tool_name,
                    &planned.action.tool_input,
                    step_num,
                    planned.action_index,
                    planned.action.tool_call_id.as_deref(),
                    &planned.step_id,
                    &interrupted_text,
                    outcome,
                )
                .await;
            results.set(
                plan_index,
                CompletedTool::from_observation(
                    planned.action.clone(),
                    planned.step_id.clone(),
                    planned.action_index,
                    interrupted_text,
                    tool_execution_outcome(outcome),
                ),
            );
        }
    }

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
        actions: &[Action],
        thought: &Option<String>,
        response: &haven_llm::LlmResponse,
        cancel_res: &tokio_util::sync::CancellationToken,
        allow_tool_retry: bool,
        tool_retry_budget: &mut ToolRetryBudget,
    ) -> anyhow::Result<ToolBatchOutcome> {
        // Build the plan first. Every later identity/index lookup is derived
        // from it; validation only reports tool-schema failures against the
        // plan's action indexes and never mints a parallel identity map.
        let plan = ToolBatchPlan::from_actions(actions);
        let validation_failures = if plan.is_empty() {
            Vec::new()
        } else {
            self.validate_tool_inputs(session_id, actions).await
        };
        if !validation_failures.is_empty() {
            tracing::warn!(
                "ReAct step {} session {} rejected {} invalid tool call(s)",
                step_num,
                session_id,
                validation_failures.len()
            );
        }
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
            .await?;
        }

        self.save_branch_point(session_id, state, step_num, false)
            .await;

        // Phase 5 / E3: pre-check every planned action before spawning.
        // Proceed tools run in parallel; blocked calls become immediate
        // observations; NeedConfirm is collected and pauses after the drain.
        let gate_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let admission = self
            .admit_tool_batch(session_id, step_num, &gate_ctx, &plan, &validation_failures)
            .await;
        let ToolBatchAdmission {
            runnable,
            need_confirm,
            results,
            ..
        } = admission;
        let execution = self
            .execute_admitted_tools(session_id, step_num, &plan, runnable, results, cancel_res)
            .await;

        // Futures finish nondeterministically, but canonical tool messages are
        // an ordered protocol: each observation follows the corresponding
        // assistant call. The shared result buffer makes this true for both
        // size-one and parallel batches.
        let mut batch_state = ToolBatchState::default();
        if execution.cancelled || need_confirm.is_empty() {
            batch_state
                .commit_ordered_results(self, &gate_ctx, execution.results, state)
                .await?;
        }

        if execution.cancelled {
            // A rollback that lands mid-batch must find the DB row at the
            // pre-batch branch point (the response and partial tool results
            // are discarded by the exit).
            return Ok(ToolBatchOutcome::Done(
                self.exit_cancelled(session_id, state, step_num).await,
            ));
        }

        // Skip the retry nudge when the batch asked the user or is about to
        // pause for confirm: it would be baked into the paused snapshot ahead
        // of the user's real answer / decision. Phase 7 / G5: append onto the
        // last failed tool observation — never a synthetic User message.
        let mut admitted_failures = Vec::new();
        let mut exhausted_failure = None;
        if allow_tool_retry {
            for signal in &batch_state.failure_signals {
                if tool_retry_budget.admit(signal) {
                    admitted_failures.push(signal);
                } else if exhausted_failure.is_none() {
                    exhausted_failure = Some(signal);
                }
            }
        }
        if batch_state.retryable_failure
            && batch_state.asked_questions.is_empty()
            && need_confirm.is_empty()
            && !admitted_failures.is_empty()
        {
            let failures: Vec<_> = admitted_failures
                .iter()
                .map(|signal| (signal.tool_name.clone(), signal.error.clone()))
                .collect();
            let nudge = Self::build_failure_nudge(&failures);
            if let Some(tool_call_id) = admitted_failures
                .last()
                .and_then(|signal| signal.tool_call_id.clone())
                .or_else(|| batch_state.last_retryable_failed_tool_call_id.clone())
            {
                state.stage_retry_nudge(tool_call_id, nudge);
            }
        } else if let Some(signal) = exhausted_failure
            && batch_state.asked_questions.is_empty()
            && need_confirm.is_empty()
        {
            if let Some(tool_call_id) = signal.tool_call_id.clone() {
                state.stage_retry_nudge(
                    tool_call_id,
                    "The automatic retry budget for this exact tool operation and failure kind is exhausted. Do not repeat the same call; change the approach or ask the user for guidance.".into(),
                );
            }
        }

        // A normal batch is checkpointed only after all state that belongs to
        // the next model request (including a retry nudge) is staged. Ask and
        // confirm batches intentionally defer this to their pause snapshot;
        // otherwise a crash between this checkpoint and `pause_for_ask` could
        // lose the explicit ask gate and resume as if the question were done.
        if need_confirm.is_empty()
            && batch_state.asked_questions.is_empty()
            && !self
                .save_snapshot_after_tool_results(session_id, state, step_num + 1)
                .await
        {
            anyhow::bail!(
                "failed to durably checkpoint tool results for session '{}' at step {}",
                session_id,
                step_num
            );
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
                .await?;
            // UI-only waiting notice in `messages` (not an LLM event — must
            // not enter `react_state.events` or resume would re-feed it).
            let notice = "Waiting for confirmation…";
            self.project_chat_message(session_id, "assistant", notice, Some("text"), None, None)
                .await?;
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

    /// Resume a confirmation batch using the original plan identities. The
    /// already advertised Action cards are not emitted again; approved calls
    /// use the same admission/execution/result-slot pipeline as a live batch,
    /// while declined calls occupy their plan slot as cancelled observations.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_confirm_batch(
        &self,
        session_id: &str,
        state: &mut ReActState,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let Some(pending) = self.executor.get_awaiting_confirm(session_id).await else {
            return Ok(ToolBatchOutcome::Continue);
        };
        let step_num = pending.step_number;
        let plan = ToolBatchPlan::from_confirm_pending(&pending.tools);
        let proj_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let mut runnable = Vec::new();
        let mut results = ToolBatchResults::new(plan.len());
        let actions: Vec<Action> = plan.iter().map(|planned| planned.action.clone()).collect();
        let validation_failures = self.validate_tool_inputs(session_id, &actions).await;

        for (plan_index, (planned, pending_tool)) in
            plan.iter().zip(pending.tools.iter()).enumerate()
        {
            let Some(decision) = pending_tool.decision else {
                tracing::warn!(
                    session_id,
                    step_num,
                    plan_index,
                    "confirm batch resumed before every decision was recorded"
                );
                return Ok(ToolBatchOutcome::Continue);
            };
            if plan_index >= MAX_RUNTIME_TOOL_CALLS_PER_BATCH {
                let error = format!(
                    "runtime tool-call limit ({MAX_RUNTIME_TOOL_CALLS_PER_BATCH}) exceeded; call was not executed"
                );
                results.set(
                    plan_index,
                    self.failed_admission_tool(session_id, step_num, planned, error)
                        .await,
                );
                continue;
            }
            if let Some(failure) = validation_failures
                .iter()
                .find(|failure| failure.action_index == planned.action_index)
            {
                results.set(
                    plan_index,
                    self.failed_admission_tool(session_id, step_num, planned, failure.render())
                        .await,
                );
                continue;
            };
            if decision {
                let concurrency = self
                    .executor
                    .tool_concurrency(
                        session_id,
                        &planned.action.tool_name,
                        &planned.action.tool_input,
                    )
                    .await;
                runnable.push(AdmittedTool {
                    plan_index,
                    // Only calls that actually crossed the confirmation
                    // gate may bypass it on resume. Siblings that were safe
                    // at admission must be rechecked fail-closed after the
                    // pause; a dynamic policy change must never become an
                    // implicit approval.
                    pre_confirmed: !matches!(pending_tool.risk_level, RiskLevel::Safe),
                    concurrency,
                });
            } else {
                let error = rejection_observation(&planned.action.tool_name);
                self.executor
                    .finish_step_with_outcome(
                        session_id,
                        &planned.action.tool_name,
                        &planned.action.tool_input,
                        step_num,
                        planned.action_index,
                        planned.action.tool_call_id.as_deref(),
                        &planned.step_id,
                        &error,
                        ActionStepOutcome::Cancelled,
                    )
                    .await;
                results.set(
                    plan_index,
                    CompletedTool::from_observation(
                        planned.action.clone(),
                        planned.step_id.clone(),
                        planned.action_index,
                        error,
                        ToolExecutionOutcome::Cancelled,
                    ),
                );
            }
        }

        let cancel = self.executor.cancellation_token(session_id).await;
        let execution = self
            .execute_admitted_tools(session_id, step_num, &plan, runnable, results, &cancel)
            .await;
        let mut batch_state = ToolBatchState::default();
        batch_state
            .commit_ordered_results(self, &proj_ctx, execution.results, state)
            .await?;

        if execution.cancelled {
            self.executor
                .clear_awaiting_confirm_persisted(session_id)
                .await?;
            return Ok(ToolBatchOutcome::Done(
                self.exit_cancelled(session_id, state, step_num).await,
            ));
        }

        if !self
            .save_snapshot_after_confirm_results(session_id, state, step_num + 1)
            .await
        {
            anyhow::bail!(
                "failed to durably checkpoint confirmed tool results for session '{}' at step {}",
                session_id,
                step_num
            );
        }
        self.executor
            .clear_awaiting_confirm_persisted(session_id)
            .await?;

        let pending_ask = if !batch_state.asked_questions.is_empty() {
            Some(crate::types::AskPending {
                question: batch_state.asked_questions.join("\n\n"),
                step_ids: batch_state.ask_step_ids.clone(),
            })
        } else {
            // Same-batch ask was stashed while confirm paused first: surface
            // it now.
            self.executor.get_awaiting_answer(session_id).await
        };
        if let Some(pending) = pending_ask {
            return self
                .pause_for_ask(session_id, state, step_num, emitter, pending)
                .await;
        }

        Ok(ToolBatchOutcome::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned_tool() -> super::super::tool_batch_plan::PlannedTool {
        super::super::tool_batch_plan::PlannedTool {
            action: Action {
                tool_name: "write".into(),
                tool_input: serde_json::json!({"path": "a.txt"}),
                is_final: false,
                tool_call_id: Some("call-write".into()),
            },
            step_id: "step-write".into(),
            action_index: 4,
        }
    }

    #[test]
    fn cancellation_observation_distinguishes_attempted_and_queued_calls() {
        let planned = planned_tool();
        let attempted = cancellation_observation(&planned, true);
        let queued = cancellation_observation(&planned, false);

        assert!(attempted.starts_with("Interrupted:"));
        assert!(attempted.contains("write"));
        assert_eq!(queued, "tool call cancelled before execution");
    }

    #[test]
    fn confirmation_rejection_is_non_retryable() {
        let error = rejection_observation("shell");

        assert!(error.contains("shell"));
        assert!(error.contains("Do NOT retry it"));
    }
}
