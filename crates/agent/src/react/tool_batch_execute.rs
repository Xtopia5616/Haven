//! Concurrent execution and cancellation for one planned tool batch.
//!
//! This module owns one deterministic batch boundary: validation, safety
//! admission, bounded concurrent execution, cancellation repair, and ordered
//! result commit. Result slots and observation projection live in
//! `tool_batch.rs`; failure policy lives in `tool_batch_policy.rs`.

use super::hooks::{BeforeToolCallDecision, BeforeToolRequest, ToolCallIdentity};
use super::tool_batch::{
    CompletedTool, MAX_CONCURRENT_TOOL_CALLS, MAX_RUNTIME_TOOL_CALLS_PER_BATCH, ToolBatchGate,
    ToolBatchOutcome, ToolBatchResults, ToolBatchState, ToolCallRequest, execute_tool_call,
    tool_step_metadata,
};
use super::tool_batch_plan::ToolBatchPlan;
use super::tool_batch_policy::ToolRetryBudget;
use super::*;
use crate::types::{ToolCall, TranscriptRecord};
use futures_util::StreamExt;
use haven_memory::ToolStepOutcome;
use haven_tools::{ToolConcurrency, ToolExecutionOutcome};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

fn cancellation_observation(
    planned: &super::tool_batch_plan::PlannedTool,
    was_started: bool,
) -> String {
    if was_started {
        crate::canonical::interrupted_result_text(
            &planned.tool_call.tool_name,
            &planned.tool_call.tool_input,
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

fn tool_execution_outcome(outcome: ToolStepOutcome) -> ToolExecutionOutcome {
    match outcome {
        ToolStepOutcome::Completed => ToolExecutionOutcome::Succeeded,
        ToolStepOutcome::Failed => ToolExecutionOutcome::Failed,
        ToolStepOutcome::Cancelled => ToolExecutionOutcome::Cancelled,
        ToolStepOutcome::Unknown => ToolExecutionOutcome::TimedOutUnknown,
    }
}

struct AdmittedTool {
    plan_index: usize,
    receipt: Option<haven_tools::ConfirmationReceipt>,
    concurrency: ToolConcurrency,
}

struct DeferredAdmissionFailure {
    plan_index: usize,
    error: String,
}

struct ToolBatchAdmission {
    runnable: Vec<AdmittedTool>,
    need_confirm: Vec<crate::interaction::InteractionRequest>,
    failures: Vec<DeferredAdmissionFailure>,
    results: ToolBatchResults,
}

struct ToolBatchExecution {
    results: ToolBatchResults,
    cancelled: bool,
}

struct AdmittedToolExecutionRequest<'a> {
    session_id: &'a str,
    step_num: u32,
    ctx: &'a StepCtx,
    plan: &'a ToolBatchPlan,
    catalog: Arc<haven_tools::ToolCatalogSnapshot>,
    runnable: Vec<AdmittedTool>,
    results: ToolBatchResults,
    batch_state: &'a mut ToolBatchState,
    state: &'a mut ReActState,
    cancel_res: &'a tokio_util::sync::CancellationToken,
}

async fn collect_bounded_admission_checks<I, Fut, Output>(checks: I) -> Vec<Output>
where
    I: IntoIterator<Item = Fut>,
    Fut: Future<Output = Output>,
{
    futures_util::stream::iter(checks)
        .buffered(MAX_CONCURRENT_TOOL_CALLS)
        .collect()
        .await
}

#[cfg(test)]
async fn execute_after_admission<Admission, Execute, Execution>(
    admission: Admission,
    execute: Execute,
) -> Execution::Output
where
    Admission: Future,
    Execute: FnOnce(Admission::Output) -> Execution,
    Execution: Future,
{
    execute(admission.await).await
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
        catalog: &haven_tools::ToolCatalogSnapshot,
        plan: &ToolBatchPlan,
        validation_failures: &[ToolInputValidationFailure],
    ) -> ToolBatchAdmission {
        let mut admission = ToolBatchAdmission {
            runnable: Vec::new(),
            need_confirm: Vec::new(),
            failures: Vec::new(),
            results: ToolBatchResults::new(plan.len()),
        };

        // The production pre-tool hook only reads the pending interaction and
        // evaluates the current authorization policy. Bound these independent
        // checks like execution, while `buffered` keeps their outputs in plan
        // order. Collect every decision before returning: the caller starts no
        // tool execution until this whole admission barrier has completed.
        type AdmissionCheck = Result<(usize, BeforeToolCallDecision), (usize, String)>;
        type AdmissionCheckFuture = futures_util::future::BoxFuture<'static, AdmissionCheck>;
        let checks: Vec<AdmissionCheckFuture> =
            plan.iter().enumerate().map(|(plan_index, planned)| {
            let step_id = planned.step_id.clone();
            let tool_index = planned.tool_index;
            let tool_call_id = planned.tool_call.tool_call_id.clone();
            let tool_name = planned.tool_call.tool_name.clone();
            let tool_input = planned.tool_call.tool_input.clone();
            let validation_failure = validation_failures
                .iter()
                .find(|failure| failure.tool_index == tool_index)
                .map(|failure| failure.render());
            if plan_index >= MAX_RUNTIME_TOOL_CALLS_PER_BATCH {
                return Box::pin(async move {
                    Err((
                        plan_index,
                        format!(
                            "runtime tool-call limit ({MAX_RUNTIME_TOOL_CALLS_PER_BATCH}) exceeded; call was not executed"
                        ),
                    ))
                }) as AdmissionCheckFuture;
            }
            if let Some(error) = validation_failure {
                return Box::pin(async move { Err((plan_index, error)) }) as AdmissionCheckFuture;
            }

            let gate = self.hooks.before_tool(BeforeToolRequest {
                executor: Arc::clone(&self.executor),
                session_id: gate_ctx.session_id.clone(),
                catalog: catalog.clone(),
                identity: ToolCallIdentity {
                    step_id,
                    tool_index,
                    tool_call_id,
                },
                tool_name,
                input: tool_input,
            });
            Box::pin(async move { Ok((plan_index, gate.await)) }) as AdmissionCheckFuture
        }).collect();
        let checks = collect_bounded_admission_checks(checks).await;

        for check in checks {
            let (plan_index, decision) = match check {
                Ok(decision) => decision,
                Err((plan_index, error)) => {
                    admission
                        .failures
                        .push(DeferredAdmissionFailure { plan_index, error });
                    continue;
                }
            };
            let planned = plan
                .get(plan_index)
                .expect("admission decision must reference a plan entry");
            match decision {
                BeforeToolCallDecision::Proceed { receipt } => {
                    let concurrency = catalog
                        .operation_policy(
                            &planned.tool_call.tool_name,
                            &planned.tool_call.tool_input,
                        )
                        .concurrency;
                    admission.runnable.push(AdmittedTool {
                        plan_index,
                        receipt,
                        concurrency,
                    });
                }
                BeforeToolCallDecision::Block { error } => {
                    admission
                        .failures
                        .push(DeferredAdmissionFailure { plan_index, error });
                }
                BeforeToolCallDecision::NeedConfirm { receipt } => {
                    admission
                        .need_confirm
                        .push(crate::interaction::InteractionRequest::confirm(
                            session_id,
                            step_num,
                            planned.tool_call.tool_name.clone(),
                            planned.tool_call.tool_input.clone(),
                            planned.tool_call.tool_call_id.clone().unwrap_or_default(),
                            planned.step_id.clone(),
                            planned.tool_index,
                            receipt.effective_risk,
                            Some(receipt),
                        ));
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
                    self.failed_admission_tool(
                        session_id,
                        step_num,
                        planned,
                        catalog,
                        failure.error,
                    )
                    .await,
                );
            }
        } else {
            // A confirmation is a barrier for the whole assistant batch. Do
            // not execute any sibling before the user decides. Persist the
            // complete ordered plan separately; only real permission gates
            // enter the interaction owner registry.
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
        catalog: &haven_tools::ToolCatalogSnapshot,
        error: String,
    ) -> CompletedTool {
        self.executor
            .finish_interrupted_step_with_identity_and_metadata(
                session_id,
                &planned.tool_call.tool_name,
                &planned.tool_call.tool_input,
                step_num,
                planned.tool_index,
                planned.tool_call.tool_call_id.as_deref(),
                &planned.step_id,
                &error,
                tool_step_metadata(
                    catalog,
                    &planned.tool_call.tool_name,
                    &planned.tool_call.tool_input,
                ),
            )
            .await;
        CompletedTool::from_observation(
            planned.tool_call.clone(),
            planned.step_id.clone(),
            planned.tool_index,
            error,
            ToolExecutionOutcome::Failed,
        )
    }

    /// Execute admitted calls concurrently and commit each completed result
    /// immediately. Cancellation repairs every slot that did not produce a
    /// normal result before returning to the shared ordered projector.
    async fn execute_admitted_tools(
        &self,
        request: AdmittedToolExecutionRequest<'_>,
    ) -> anyhow::Result<ToolBatchExecution> {
        let AdmittedToolExecutionRequest {
            session_id,
            step_num,
            ctx,
            plan,
            catalog,
            runnable,
            mut results,
            batch_state,
            state,
            cancel_res,
        } = request;
        self.commit_ready_tool_results(ctx, &mut results, batch_state, state)
            .await?;
        let gate = Arc::new(ToolBatchGate {
            all: Arc::new(RwLock::new(())),
            resources: AsyncMutex::new(HashMap::new()),
        });
        let started = Arc::new(
            (0..plan.len())
                .map(|_| AtomicBool::new(false))
                .collect::<Vec<_>>(),
        );
        let cancel = cancel_res.clone();
        let committed_ui = Arc::clone(&self.committed_ui);
        let emitter = ctx.emitter.clone();
        let mut tool_futures = futures_util::stream::iter(runnable)
            .map(|admitted| {
                let planned = plan
                    .get(admitted.plan_index)
                    .expect("admission must reference a plan entry");
                let tool_call = planned.tool_call.clone();
                let tool_index = planned.tool_index;
                let step_id = planned.step_id.clone();
                let session_id = session_id.to_string();
                let metric_session_id = session_id.clone();
                let executor = self.executor.clone();
                let catalog = catalog.clone();
                let gate = gate.clone();
                let started = started.clone();
                let metrics = self.metrics.clone();
                let run_id = ctx.run_id;
                let cancel = cancel.clone();
                let committed_ui = Arc::clone(&committed_ui);
                let emitter = emitter.clone();
                async move {
                    let _permit = gate.acquire(&admitted.concurrency).await;
                    committed_ui
                        .publish_tool_call(&emitter, &session_id, &step_id)
                        .await;
                    started[admitted.plan_index].store(true, Ordering::Release);
                    let _timer = metrics.start(
                        MetricsPhase::ToolExecution,
                        &metric_session_id,
                        run_id,
                        step_num,
                    );
                    let result = execute_tool_call(ToolCallRequest {
                        executor,
                        catalog,
                        session_id,
                        tool_call,
                        step_num,
                        tool_index,
                        step_id,
                        receipt: admitted.receipt,
                        cancel,
                    })
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
                    self.repair_cancelled_results(
                        session_id,
                        step_num,
                        plan,
                        catalog.as_ref(),
                        &started,
                        &mut results,
                    )
                    .await;
                    self.commit_ready_tool_results(ctx, &mut results, batch_state, state)
                        .await?;
                    return Ok(ToolBatchExecution { results, cancelled: true });
                }
                item = tool_futures.next() => {
                    let Some((plan_index, result)) = item else {
                        break;
                    };
                    results.set(plan_index, result);
                    self
                        .commit_ready_tool_results(ctx, &mut results, batch_state, state)
                        .await?;
                }
            }
        }

        if cancel_res.is_cancelled() {
            self.repair_cancelled_results(
                session_id,
                step_num,
                plan,
                catalog.as_ref(),
                &started,
                &mut results,
            )
            .await;
            self.commit_ready_tool_results(ctx, &mut results, batch_state, state)
                .await?;
            return Ok(ToolBatchExecution {
                results,
                cancelled: true,
            });
        }

        self.commit_ready_tool_results(ctx, &mut results, batch_state, state)
            .await?;

        Ok(ToolBatchExecution {
            results,
            cancelled: false,
        })
    }

    async fn commit_ready_tool_results(
        &self,
        ctx: &StepCtx,
        results: &mut ToolBatchResults,
        batch_state: &mut ToolBatchState,
        state: &mut ReActState,
    ) -> anyhow::Result<()> {
        for index in 0..results.len() {
            let Some(result) = results.take_completed(index) else {
                continue;
            };
            self.committed_ui
                .publish_tool_call(&ctx.emitter, &ctx.session_id, &result.step_id)
                .await;
            let event = batch_state
                .commit_tool_result(self, ctx, result, state)
                .await?;
            results.set_committed(index, event);
        }
        Ok(())
    }

    async fn repair_cancelled_results(
        &self,
        session_id: &str,
        step_num: u32,
        plan: &ToolBatchPlan,
        catalog: &haven_tools::ToolCatalogSnapshot,
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
                ToolStepOutcome::Unknown
            } else {
                ToolStepOutcome::Cancelled
            };
            self.executor
                .finish_step_with_outcome_and_metadata(
                    session_id,
                    &planned.tool_call.tool_name,
                    &planned.tool_call.tool_input,
                    step_num,
                    planned.tool_index,
                    planned.tool_call.tool_call_id.as_deref(),
                    &planned.step_id,
                    &interrupted_text,
                    outcome,
                    tool_step_metadata(
                        catalog,
                        &planned.tool_call.tool_name,
                        &planned.tool_call.tool_input,
                    ),
                )
                .await;
            results.set(
                plan_index,
                CompletedTool::from_observation(
                    planned.tool_call.clone(),
                    planned.step_id.clone(),
                    planned.tool_index,
                    interrupted_text,
                    tool_execution_outcome(outcome),
                ),
            );
        }
    }

    /// Execute the non-final tool_calls for one step: emit ToolCall cards, run the
    /// batch (parallel), drain observations, failure nudge, and ask pause.
    /// Behavior-preserving extract from `run_react_loop` (Phase 1 / E2).
    ///
    /// Phase 7 / E5: tool-input validation runs at the tool-batch boundary
    /// before ToolCall cards are emitted — not in the thin loop. Invalid inputs
    /// become failed observations and are never rewritten.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_tool_batch(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_num: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
        tool_calls: &[ToolCall],
        thought: &Option<String>,
        response: &haven_llm::LlmResponse,
        catalog: Arc<haven_tools::ToolCatalogSnapshot>,
        cancel_res: &tokio_util::sync::CancellationToken,
        allow_tool_retry: bool,
        tool_retry_budget: &mut ToolRetryBudget,
    ) -> anyhow::Result<ToolBatchOutcome> {
        // Build the plan first. Every later identity/index lookup is derived
        // from it; validation only reports tool-schema failures against the
        // plan's tool_call indexes and never mints a parallel identity map.
        let plan = ToolBatchPlan::from_tool_calls(tool_calls);
        let validation_failures = if plan.is_empty() {
            Vec::new()
        } else {
            self.validate_tool_inputs_from_catalog(catalog.as_ref(), tool_calls)
        };
        if !validation_failures.is_empty() {
            tracing::warn!(
                "ReAct step {} session {} rejected {} invalid tool call(s)",
                step_num,
                session_id,
                validation_failures.len()
            );
        }
        let step_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        // Assistant tool calls and the branch marker share one ordered commit.
        // An empty plan still records the branch point, and neither write
        // bypasses the effect applier.
        let mut commit = super::effects::EffectBatch::continue_batch();
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
            // Phase 6.1 + X12: ToolCall cards + pending rows + canonical via apply.
            // Thought already projected the messages row — no persist_text_id.
            let tool_call_cards =
                plan.tool_call_cards_with_catalog(suppress_streamed_thought, catalog.as_ref());
            commit.transcript(TranscriptEvent::ToolCall {
                text: push_text.to_string(),
                tool_calls,
                reasoning: if response.thinking_blocks.is_empty() {
                    response.reasoning.clone()
                } else {
                    None
                },
                web_search_calls: response.web_search_calls.clone(),
                thinking_blocks: response.thinking_blocks.clone(),
                tool_call_cards,
                persist_text_id: None,
            });
        }
        commit.push(super::effects::TurnEffect::SaveBranchPoint {
            step_number: step_num,
        });
        self.apply_committed_batch(&step_ctx, state, commit).await?;

        // Phase 5 / E3: pre-check every planned tool_call before spawning.
        // Proceed tools run in parallel; blocked calls become immediate
        // observations; NeedConfirm is collected and pauses after the drain.
        let admission = {
            let _timer =
                self.metrics
                    .start(MetricsPhase::ToolAdmission, session_id, run_id, step_num);
            self.admit_tool_batch(
                session_id,
                step_num,
                &step_ctx,
                catalog.as_ref(),
                &plan,
                &validation_failures,
            )
            .await
        };
        let ToolBatchAdmission {
            runnable,
            need_confirm,
            results,
            ..
        } = admission;
        let mut batch_state = ToolBatchState::default();
        let execution = self
            .execute_admitted_tools(AdmittedToolExecutionRequest {
                session_id,
                step_num,
                ctx: &step_ctx,
                plan: &plan,
                catalog: catalog.clone(),
                runnable,
                results,
                batch_state: &mut batch_state,
                state,
                cancel_res,
            })
            .await?;

        // Futures finish nondeterministically, but canonical tool messages are
        // an ordered protocol: each observation follows the corresponding
        // assistant call. ToolCall cards publish when each call starts, and
        // observations publish as calls finish; only canonical projection
        // waits for the ordered batch.
        if execution.cancelled || need_confirm.is_empty() {
            let _timer =
                self.metrics
                    .start(MetricsPhase::OrderedCommit, session_id, run_id, step_num);
            batch_state
                .project_ordered_results(self, &step_ctx, execution.results, state)
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
                .map(|signal| (signal.tool_name.clone(), signal.error_class))
                .collect();
            let nudge = Self::build_failure_nudge(&failures);
            if let Some(tool_call_id) = admitted_failures
                .last()
                .and_then(|signal| signal.tool_call_id.clone())
                .or_else(|| batch_state.last_retryable_failed_tool_call_id.clone())
            {
                state.stage_retry_nudge(tool_call_id, nudge);
            }
        } else if batch_state.asked_questions.is_empty()
            && need_confirm.is_empty()
            && let Some(signal) = exhausted_failure
            && let Some(tool_call_id) = signal.tool_call_id.clone()
        {
            state.stage_retry_nudge(
                tool_call_id,
                "The automatic retry budget for this exact tool operation and failure kind is exhausted. Do not repeat the same call; change the approach or ask the user for guidance.".into(),
            );
        }

        // A normal batch is checkpointed only after all state that belongs to
        // the next model request (including a retry nudge) is staged. Ask and
        // confirm batches intentionally defer this to their pause snapshot;
        // otherwise a crash between this checkpoint and `pause_for_ask` could
        // lose the explicit ask gate and resume as if the question were done.
        if need_confirm.is_empty()
            && batch_state.asked_questions.is_empty()
            && !self
                .ensure_event_boundary_after_tool_results(session_id, state, step_num + 1)
                .await
        {
            anyhow::bail!(
                "failed to durably checkpoint tool results for session '{}' at step {}",
                session_id,
                step_num
            );
        }

        // Phase 5 / E3: confirm before ask when both appear in one batch.
        // Ask pause used to return first and drop NeedConfirm tools (ToolCall
        // cards + assistant tool_calls with no results → Interrupted repair).
        // Prefer confirm pause; stash ask pending so finish_confirm_batch's
        // next turn still surfaces the question.
        if !need_confirm.is_empty() {
            let confirmation_plan = super::tool_batch_plan::ConfirmationBatchPlan::from_plan(
                step_num,
                &plan,
                &need_confirm,
            )?;
            if !batch_state.asked_questions.is_empty() {
                // Ask question rows were projected inside apply(ToolResult).
                self.executor
                    .request_interaction(crate::interaction::InteractionRequest::ask(
                        session_id,
                        Vec::new(),
                        batch_state.ask_step_ids.clone(),
                    ))
                    .await?;
            }
            for request in &need_confirm {
                if let crate::interaction::InteractionDetails::Confirm { step_id, .. } =
                    &request.details
                {
                    self.committed_ui
                        .publish_tool_call(emitter, session_id, step_id)
                        .await;
                }
            }
            self.executor
                .request_confirm_batch_with_plan(session_id, need_confirm, confirmation_plan)
                .await?;
            let mut pause = super::effects::EffectBatch::continue_batch();
            pause.pause(
                step_num + 1,
                SessionStatus::Paused,
                Some(haven_common::SessionWaitingReason::Confirmation),
                None,
                PauseReason::Confirm,
            );
            self.apply_committed_batch(&step_ctx, state, pause).await?;
            return Ok(ToolBatchOutcome::Done(LoopExit::Paused {
                reason: PauseReason::Confirm,
            }));
        }

        // The agent asked the human a question: pause so the user can
        // answer. Their reply arrives as a supplement and resumes the session
        // (Paused —Pending —dispatcher re-enters the loop, injecting the
        // answer as context at the top of the next step).
        if !batch_state.asked_questions.is_empty() {
            return self
                .pause_for_ask(
                    session_id,
                    state,
                    step_num,
                    emitter,
                    run_id,
                    crate::interaction::InteractionRequest::ask(
                        session_id,
                        Vec::new(),
                        batch_state.ask_step_ids.clone(),
                    ),
                )
                .await;
        }

        let session_state = self.executor.get_active_session_status(session_id).await;
        match session_state {
            Some(s) if s.is_paused() => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_external_pause(session_id, state, step_num, emitter, run_id)
                        .await,
                ));
            }
            Some(SessionStatus::Error) => {
                return Ok(ToolBatchOutcome::Done(
                    self.exit_at_boundary(
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
                    self.exit_at_boundary(session_id, state, step_num, LoopExit::Completed)
                        .await,
                ));
            }
            _ => {}
        }

        Ok(ToolBatchOutcome::Continue)
    }

    /// Resume a confirmation batch using the original plan identities. The
    /// already advertised ToolCall cards are not emitted again; approved calls
    /// use the same admission/execution/result-slot pipeline as a live batch,
    /// while declined calls occupy their plan slot as cancelled observations.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_confirm_batch(
        &self,
        session_id: &str,
        state: &mut ReActState,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolBatchOutcome> {
        let pending = self
            .executor
            .interaction_requests(session_id)
            .await
            .into_iter()
            .filter(|request| request.kind == crate::interaction::InteractionKind::Confirm)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(ToolBatchOutcome::Continue);
        }
        let active_events = self
            .event_store
            .read_active_events_async(session_id)
            .await?;
        let continuation = super::tool_batch_plan::ConfirmationBatchPlan::replay(&active_events)?
            .unwrap_or_else(|| {
                super::tool_batch_plan::ConfirmationBatchPlan::from_confirm_requests(&pending)
            });
        continuation.validate_requests(&pending)?;
        let step_num = continuation.step_number;
        let canonical_calls = state
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                TranscriptRecord::ToolCall {
                    step_number,
                    tool_calls,
                    ..
                } if *step_number == step_num => Some(tool_calls.as_slice()),
                _ => None,
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "confirmation batch at step {} has no durable ToolCall transcript",
                    step_num
                )
            })?;
        let plan = ToolBatchPlan::from_confirmation_batch(&continuation, canonical_calls)?;
        anyhow::ensure!(
            super::tool_batch_plan::ConfirmationBatchPlan::from_plan(step_num, &plan, &pending,)?
                == continuation,
            "confirmation batch plan does not match its durable ToolCall transcript"
        );
        let catalog = self.tool_catalog.catalog_snapshot(session_id).await;
        let proj_ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num,
            run_id,
            emitter: emitter.clone(),
        };
        let mut runnable = Vec::new();
        let mut results = ToolBatchResults::new(plan.len());
        let mut batch_state = ToolBatchState::default();
        let validation_failures = self
            .validate_indexed_tool_inputs_from_catalog(catalog.as_ref(), plan.indexed_tool_calls());
        let requests_by_id = pending
            .iter()
            .map(|request| (request.id.as_str(), request))
            .collect::<HashMap<_, _>>();
        let confirmation_ids = pending
            .iter()
            .map(|request| request.id.clone())
            .collect::<Vec<_>>();
        batch_state.set_confirmation_completion(
            confirmation_ids,
            plan.iter()
                .last()
                .expect("a confirmation batch must contain a tool")
                .tool_index,
        );

        for (plan_index, (planned, durable_tool)) in
            plan.iter().zip(continuation.tools.iter()).enumerate()
        {
            let pending_request = match durable_tool.confirmation_request_id.as_deref() {
                Some(request_id) => Some(*requests_by_id.get(request_id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "confirmation request '{}' is missing from its active batch",
                        request_id
                    )
                })?),
                None => None,
            };
            let decision = if let Some(pending_request) = pending_request {
                let Some(decision) = pending_request.decision() else {
                    tracing::warn!(
                        session_id,
                        step_num,
                        plan_index,
                        "confirm batch resumed before every decision was recorded"
                    );
                    return Ok(ToolBatchOutcome::Continue);
                };
                decision
            } else {
                true
            };
            if plan_index >= MAX_RUNTIME_TOOL_CALLS_PER_BATCH {
                let error = format!(
                    "runtime tool-call limit ({MAX_RUNTIME_TOOL_CALLS_PER_BATCH}) exceeded; call was not executed"
                );
                results.set(
                    plan_index,
                    self.failed_admission_tool(
                        session_id,
                        step_num,
                        planned,
                        catalog.as_ref(),
                        error,
                    )
                    .await,
                );
                continue;
            }
            if let Some(failure) = validation_failures
                .iter()
                .find(|failure| failure.tool_index == planned.tool_index)
            {
                results.set(
                    plan_index,
                    self.failed_admission_tool(
                        session_id,
                        step_num,
                        planned,
                        catalog.as_ref(),
                        failure.render(),
                    )
                    .await,
                );
                continue;
            };
            if decision {
                let concurrency = catalog
                    .operation_policy(&planned.tool_call.tool_name, &planned.tool_call.tool_input)
                    .concurrency;
                runnable.push(AdmittedTool {
                    plan_index,
                    // Only calls that actually crossed the confirmation
                    // gate may bypass it on resume. Siblings that were safe
                    // at admission have no receipt and are rechecked normally.
                    receipt: pending_request.and_then(|request| match &request.details {
                        crate::interaction::InteractionDetails::Confirm { receipt, .. } => {
                            receipt.clone()
                        }
                        _ => None,
                    }),
                    concurrency,
                });
            } else {
                let error = if pending_request.is_some_and(|request| {
                    request.status == crate::interaction::InteractionStatus::Expired
                }) {
                    format!(
                        "The operation '{}' was not executed because confirmation timed out. Do not retry it; ask the user to confirm again if it is still needed.",
                        planned.tool_call.tool_name
                    )
                } else {
                    rejection_observation(&planned.tool_call.tool_name)
                };
                self.executor
                    .finish_step_with_outcome_and_metadata(
                        session_id,
                        &planned.tool_call.tool_name,
                        &planned.tool_call.tool_input,
                        step_num,
                        planned.tool_index,
                        planned.tool_call.tool_call_id.as_deref(),
                        &planned.step_id,
                        &error,
                        ToolStepOutcome::Cancelled,
                        tool_step_metadata(
                            catalog.as_ref(),
                            &planned.tool_call.tool_name,
                            &planned.tool_call.tool_input,
                        ),
                    )
                    .await;
                results.set(
                    plan_index,
                    CompletedTool::from_observation(
                        planned.tool_call.clone(),
                        planned.step_id.clone(),
                        planned.tool_index,
                        error,
                        ToolExecutionOutcome::Cancelled,
                    ),
                );
            }
        }

        let execution = self
            .execute_admitted_tools(AdmittedToolExecutionRequest {
                session_id,
                step_num,
                ctx: &proj_ctx,
                plan: &plan,
                catalog,
                runnable,
                results,
                batch_state: &mut batch_state,
                state,
                cancel_res: cancel,
            })
            .await?;
        batch_state
            .project_ordered_results(self, &proj_ctx, execution.results, state)
            .await?;
        self.executor
            .forget_interactions(session_id, &batch_state.completed_confirmation_ids())
            .await?;

        if execution.cancelled {
            return Ok(ToolBatchOutcome::Done(
                self.exit_cancelled(session_id, state, step_num).await,
            ));
        }

        if !self
            .ensure_event_boundary_after_confirm_results(session_id, state, step_num + 1)
            .await
        {
            anyhow::bail!(
                "failed to durably checkpoint confirmed tool results for session '{}' at step {}",
                session_id,
                step_num
            );
        }
        let pending_ask = if !batch_state.asked_questions.is_empty() {
            Some(crate::interaction::InteractionRequest::ask(
                session_id,
                Vec::new(),
                batch_state.ask_step_ids.clone(),
            ))
        } else {
            // Same-batch ask was stashed while confirm paused first: surface
            // it now.
            self.executor
                .pending_interactions(session_id, crate::interaction::InteractionKind::Ask)
                .await
                .into_iter()
                .next()
        };
        if let Some(pending) = pending_ask {
            return self
                .pause_for_ask(session_id, state, step_num, emitter, run_id, pending)
                .await;
        }

        Ok(ToolBatchOutcome::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::{mpsc, oneshot};

    fn planned_tool() -> super::super::tool_batch_plan::PlannedTool {
        super::super::tool_batch_plan::PlannedTool {
            tool_call: ToolCall {
                tool_name: "write".into(),
                tool_input: serde_json::json!({"path": "a.txt"}),
                is_final: false,
                tool_call_id: Some("call-write".into()),
            },
            step_id: "step-write".into(),
            tool_index: 4,
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

    #[tokio::test]
    async fn admission_checks_overlap_with_a_limit_and_keep_plan_order() {
        let count = MAX_CONCURRENT_TOOL_CALLS * 2;
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut releases = Vec::with_capacity(count);

        let checks = (0..count)
            .map(|plan_index| {
                let (release_tx, release_rx) = oneshot::channel();
                releases.push(Some(release_tx));
                let started_tx = started_tx.clone();
                let finished_tx = finished_tx.clone();
                let active = active.clone();
                let max_active = max_active.clone();
                async move {
                    let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now_active, Ordering::SeqCst);
                    started_tx.send(plan_index).unwrap();
                    release_rx.await.unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                    finished_tx.send(plan_index).unwrap();
                    plan_index
                }
            })
            .collect::<Vec<_>>();
        drop(started_tx);
        drop(finished_tx);

        let collection = tokio::spawn(collect_bounded_admission_checks(checks));
        for expected in 0..MAX_CONCURRENT_TOOL_CALLS {
            assert_eq!(started_rx.recv().await, Some(expected));
        }
        assert!(matches!(
            started_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        // Complete each window backwards. The second window must not start
        // until the first has drained, and collection must still be ordered.
        for plan_index in (1..MAX_CONCURRENT_TOOL_CALLS).rev() {
            releases[plan_index].take().unwrap().send(()).unwrap();
            assert_eq!(finished_rx.recv().await, Some(plan_index));
        }
        releases[0].take().unwrap().send(()).unwrap();
        assert_eq!(finished_rx.recv().await, Some(0));

        for expected in MAX_CONCURRENT_TOOL_CALLS..count {
            assert_eq!(started_rx.recv().await, Some(expected));
        }
        for plan_index in (MAX_CONCURRENT_TOOL_CALLS..count).rev() {
            releases[plan_index].take().unwrap().send(()).unwrap();
            assert_eq!(finished_rx.recv().await, Some(plan_index));
        }

        assert_eq!(collection.await.unwrap(), (0..count).collect::<Vec<_>>());
        assert_eq!(max_active.load(Ordering::SeqCst), MAX_CONCURRENT_TOOL_CALLS);
    }

    #[tokio::test]
    async fn execution_starts_only_after_every_admission_decision_resolves() {
        let count = 3;
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
        let mut releases = Vec::with_capacity(count);
        let checks = (0..count)
            .map(|plan_index| {
                let (release_tx, release_rx) = oneshot::channel();
                releases.push(Some(release_tx));
                let started_tx = started_tx.clone();
                let finished_tx = finished_tx.clone();
                async move {
                    started_tx.send(plan_index).unwrap();
                    release_rx.await.unwrap();
                    finished_tx.send(plan_index).unwrap();
                    plan_index
                }
            })
            .collect::<Vec<_>>();
        drop(started_tx);
        drop(finished_tx);

        let execution_started = Arc::new(AtomicBool::new(false));
        let execution_marker = execution_started.clone();
        let execution = tokio::spawn(execute_after_admission(
            collect_bounded_admission_checks(checks),
            move |decisions| {
                execution_marker.store(true, Ordering::SeqCst);
                async move { decisions }
            },
        ));

        for expected in 0..count {
            assert_eq!(started_rx.recv().await, Some(expected));
        }
        for plan_index in (1..count).rev() {
            releases[plan_index].take().unwrap().send(()).unwrap();
            assert_eq!(finished_rx.recv().await, Some(plan_index));
            assert!(!execution_started.load(Ordering::SeqCst));
        }

        releases[0].take().unwrap().send(()).unwrap();
        assert_eq!(finished_rx.recv().await, Some(0));
        assert_eq!(execution.await.unwrap(), vec![0, 1, 2]);
        assert!(execution_started.load(Ordering::SeqCst));
    }
}
