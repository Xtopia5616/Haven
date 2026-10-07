//! ReAct run driver.
//!
//! The driver owns only run-scoped concerns: the step budget, lifecycle
//! checks, cancellation, and the transition between turns. One model sample
//! is planned by [`super::turn::TurnEngine`]; this driver applies the
//! resulting [`super::effects::EffectBatch`] under the same turn deadline.
//! This mirrors the useful shape shared by Codex and Pi: a small outer run,
//! a turn loop, and explicit tool-batch outcomes.

use super::effects::TurnControl;
use super::tool_batch::ToolBatchOutcome;
use super::tool_batch_policy::ToolRetryBudget;
use super::turn::TurnInput;
use super::*;
use crate::types::{BranchPoint, TranscriptRecord};
use haven_common::types::CanonicalMessage;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;

/// Why a ReAct run stopped at a cooperative boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// The model produced an answer and no context was queued behind it.
    TurnEnd,
    /// An `ask` tool is waiting for user input.
    Ask,
    /// A tool is waiting for safety confirmation.
    Confirm,
    /// The per-run step budget was exhausted.
    Budget,
    /// The session was paused by another owner.
    External,
}

/// Explicit result of a ReAct run. Hard failures still use `Err`; these
/// variants are normal lifecycle outcomes and let the dispatcher release its
/// slot without guessing from a session status string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopExit {
    Paused { reason: PauseReason },
    Cancelled,
    Completed,
    Error(String),
}

struct ReActStepBudget {
    start_step: u32,
    max_steps_per_run: u32,
    max_allowed_step_number: u32,
}

/// Absolute wall-clock boundary shared by every phase of one model turn.
/// Callers use `remaining` when entering a wait and `ensure_remaining` after
/// phase boundaries so a retry chain cannot quietly extend the turn forever.
#[derive(Clone, Copy)]
pub(super) struct TurnDeadline {
    at: tokio::time::Instant,
}

impl TurnDeadline {
    pub(super) fn from_now(seconds: u64) -> Self {
        Self {
            at: tokio::time::Instant::now() + Duration::from_secs(seconds.max(1)),
        }
    }

    pub(super) fn remaining(self) -> Duration {
        self.at
            .saturating_duration_since(tokio::time::Instant::now())
    }

    pub(super) fn is_expired(self) -> bool {
        self.remaining().is_zero()
    }

    pub(super) fn ensure_remaining(self, phase: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.remaining().is_zero(),
            "turn deadline exceeded during {phase}"
        );
        Ok(())
    }
}

/// Durable replay data submitted to the session actor. The actor constructs
/// the hot projection from this value and owns it for the lifetime of the run.
pub(crate) struct ReActRunReplay {
    pub(crate) events: Vec<TranscriptRecord>,
    pub(crate) canonical: Vec<CanonicalMessage>,
    pub(crate) branch_points: HashMap<u32, BranchPoint>,
}

/// Run metadata passed to the actor-owned loop.
pub(crate) struct ReActRunInput {
    pub(crate) session_id: String,
    pub(crate) start_step: u32,
    pub(crate) emitter: Arc<dyn AgentEventEmitter>,
    pub(crate) run_id: u64,
}

impl ReActStepBudget {
    fn new(max_steps_per_run: u32, max_steps_per_session: Option<u32>, start_step: u32) -> Self {
        let max_step_number_for_run = max_steps_per_run.max(
            start_step
                .saturating_sub(1)
                .saturating_add(max_steps_per_run),
        );
        let max_allowed_step_number = max_steps_per_session
            .map_or(max_step_number_for_run, |cap| {
                max_step_number_for_run.min(cap)
            });
        Self {
            start_step,
            max_steps_per_run,
            max_allowed_step_number,
        }
    }

    fn from_engine(engine: &ReActEngine, start_step: u32) -> Self {
        let max_steps_per_run = *engine.max_steps_per_run.lock().unwrap();
        let max_steps_per_session = *engine.max_steps_per_session.lock().unwrap();
        Self::new(max_steps_per_run, max_steps_per_session, start_step)
    }

    fn allows_tool_retry(&self, step_number: u32) -> bool {
        step_number < self.max_allowed_step_number
    }
}

impl ReActEngine {
    /// Run the session one turn at a time. The shared `ReActState` carries the
    /// authoritative transcript, its live projection, and branch indexes so
    /// every boundary advances one coherent event/projection state.
    pub(crate) async fn run_react_loop(
        &self,
        input: ReActRunInput,
        state: &mut ReActState,
    ) -> anyhow::Result<LoopExit> {
        let ReActRunInput {
            session_id,
            start_step,
            emitter,
            run_id,
        } = input;
        let session_id = session_id.as_str();
        let budget = ReActStepBudget::from_engine(self, start_step);
        tracing::info!(
            session_id,
            run_id,
            start_step = budget.start_step,
            max_steps_per_run = budget.max_steps_per_run,
            max_allowed_step_number = budget.max_allowed_step_number,
            "ReAct run started"
        );

        // The dispatcher promotes Pending before entering the run. A debug
        // assertion catches accidental direct invocation that skips the
        // lifecycle boundary without adding a second promotion path here.
        debug_assert_ne!(
            self.executor.get_active_session_status(session_id).await,
            Some(SessionStatus::Pending),
            "Pending -> Running must happen before run_react_loop"
        );

        // A confirmation decision wakes a paused run. Finish the already
        // advertised batch before asking the model for another response.
        let confirm_requests = self
            .executor
            .interaction_requests(session_id)
            .await
            .into_iter()
            .filter(|request| request.kind == crate::interaction::InteractionKind::Confirm)
            .collect::<Vec<_>>();
        if !confirm_requests.is_empty()
            && confirm_requests
                .iter()
                .all(|request| request.decision().is_some())
        {
            let parent_cancel = self.executor.cancellation_token(session_id).await;
            if parent_cancel.is_cancelled() {
                return Ok(self.exit_cancelled(session_id, state, start_step).await);
            }
            let deadline = TurnDeadline::from_now(self.limits().turn_deadline_secs);
            let confirm_cancel = parent_cancel.child_token();
            let deadline_cancel = confirm_cancel.clone();
            let deadline_task = tokio::spawn(async move {
                tokio::time::sleep_until(deadline_cancel_at(deadline)).await;
                deadline_cancel.cancel();
            });
            state.turn_cancel = Some(confirm_cancel.clone());
            let confirm_result = tokio::time::timeout(
                deadline.remaining(),
                self.finish_confirm_batch(session_id, state, &emitter, run_id, &confirm_cancel),
            )
            .await;
            if confirm_result.is_err() {
                confirm_cancel.cancel();
            }
            deadline_task.abort();
            state.turn_cancel = None;
            let confirm_outcome = confirm_result.map_err(|_| {
                anyhow::anyhow!("turn deadline exceeded during confirmation batch")
            })??;
            deadline.ensure_remaining("confirmation batch")?;
            match confirm_outcome {
                ToolBatchOutcome::Continue => {}
                ToolBatchOutcome::Done(exit) => return Ok(exit),
            }
        }

        let mut incomplete_tool_args_retries = 0u32;
        let mut tool_retry_budget = ToolRetryBudget::default();
        let mut last_step = start_step.saturating_sub(1);
        for step_num in budget.start_step..=budget.max_allowed_step_number {
            last_step = step_num;
            let parent_cancel = self.executor.cancellation_token(session_id).await;
            if parent_cancel.is_cancelled() {
                return Ok(self.exit_cancelled(session_id, state, step_num).await);
            }

            match self
                .run_state_boundary(session_id, state, step_num, &emitter, run_id)
                .await
            {
                ReActRunBoundary::Run => {}
                ReActRunBoundary::Exit(exit) => return Ok(exit),
            }

            let deadline = TurnDeadline::from_now(self.limits().turn_deadline_secs);
            let remaining = deadline.remaining();
            let turn_cancel = parent_cancel.child_token();
            let deadline_cancel = turn_cancel.clone();
            let deadline_task = tokio::spawn(async move {
                tokio::time::sleep_until(deadline_cancel_at(deadline)).await;
                deadline_cancel.cancel();
            });
            state.turn_cancel = Some(turn_cancel.clone());
            let turn_engine = self.turn_engine();
            let turn_result = tokio::time::timeout(remaining, async {
                let batch = turn_engine
                    .run(TurnInput {
                        ctx: StepCtx {
                            session_id: session_id.to_string(),
                            step_num,
                            run_id,
                            emitter: emitter.clone(),
                        },
                        state,
                        cancel: turn_cancel.clone(),
                        deadline,
                        // The turn receives the policy result, not the budget
                        // representation. This keeps tool execution independent
                        // from run accounting and fixes resumed-run boundaries.
                        allow_tool_retry: budget.allows_tool_retry(step_num),
                        tool_retry_budget: &mut tool_retry_budget,
                        incomplete_tool_args_retries: &mut incomplete_tool_args_retries,
                    })
                    .instrument(tracing::info_span!("turn", session_id, step_num))
                    .await?;
                // Tool execution is one effect of the batch, so it shares this
                // absolute deadline and cancellation token with provider work.
                self.apply_effect_batch(
                    &StepCtx {
                        session_id: session_id.to_string(),
                        step_num,
                        run_id,
                        emitter: emitter.clone(),
                    },
                    state,
                    batch,
                    &mut tool_retry_budget,
                )
                .await
            })
            .await;
            if turn_result.is_err() {
                turn_cancel.cancel();
            }
            deadline_task.abort();
            state.turn_cancel = None;
            let outcome = match turn_result {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(error)) => {
                    if !self
                        .ensure_event_boundary(session_id, state, step_num)
                        .await
                    {
                        tracing::error!(
                            session_id,
                            step = step_num,
                            error = %error,
                            "failed to verify ReAct event boundary after turn error"
                        );
                    }
                    self.mark_session_error(session_id).await;
                    return Err(error);
                }
                Err(_) => {
                    let error = anyhow::anyhow!(
                        "turn deadline exceeded at session '{}' step {}",
                        session_id,
                        step_num
                    );
                    if !self
                        .ensure_event_boundary(session_id, state, step_num)
                        .await
                    {
                        tracing::error!(
                            session_id,
                            step = step_num,
                            "failed to verify event boundary after turn deadline"
                        );
                    }
                    self.mark_session_error(session_id).await;
                    return Err(error);
                }
            };
            match outcome {
                TurnControl::Continue => {}
                TurnControl::Done(exit) => return Ok(exit),
            }
        }

        self.pause_turn_budget(session_id, state, last_step + 1, &emitter, run_id)
            .await?;
        Ok(LoopExit::Paused {
            reason: PauseReason::Budget,
        })
    }

    async fn run_state_boundary(
        &self,
        session_id: &str,
        state: &ReActState,
        step_num: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> ReActRunBoundary {
        match self.executor.get_active_session_status(session_id).await {
            None | Some(SessionStatus::Completed) => ReActRunBoundary::Exit(
                self.exit_at_boundary(session_id, state, step_num, LoopExit::Completed)
                    .await,
            ),
            Some(SessionStatus::Error) => {
                self.emit_error(emitter, session_id, "session interrupted")
                    .await;
                ReActRunBoundary::Exit(
                    self.exit_at_boundary(
                        session_id,
                        state,
                        step_num,
                        LoopExit::Error("session interrupted".into()),
                    )
                    .await,
                )
            }
            Some(status) if status.is_paused() => ReActRunBoundary::Exit(
                self.exit_external_pause(session_id, state, step_num, emitter, run_id)
                    .await,
            ),
            _ => ReActRunBoundary::Run,
        }
    }
}

fn deadline_cancel_at(deadline: TurnDeadline) -> tokio::time::Instant {
    deadline.at
}

enum ReActRunBoundary {
    Run,
    Exit(LoopExit),
}

#[cfg(test)]
mod tests {
    use super::ReActStepBudget;

    #[test]
    fn fresh_run_uses_the_configured_step_budget() {
        let budget = ReActStepBudget::new(4, None, 1);

        assert_eq!(budget.start_step, 1);
        assert_eq!(budget.max_steps_per_run, 4);
        assert_eq!(budget.max_allowed_step_number, 4);
    }

    #[test]
    fn resumed_run_gets_a_full_budget_but_respects_session_cap() {
        let budget = ReActStepBudget::new(4, Some(9), 7);

        assert_eq!(budget.start_step, 7);
        assert_eq!(budget.max_steps_per_run, 4);
        assert_eq!(budget.max_allowed_step_number, 9);
    }

    #[test]
    fn resumed_run_allows_retry_until_its_absolute_end() {
        let budget = ReActStepBudget::new(4, Some(9), 7);

        assert!(budget.allows_tool_retry(7));
        assert!(budget.allows_tool_retry(8));
        assert!(!budget.allows_tool_retry(9));
    }
}
