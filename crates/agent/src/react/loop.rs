//! ReAct run driver.
//!
//! The driver owns only run-scoped concerns: the step budget, lifecycle
//! checks, cancellation, and the transition between turns. One model sample
//! is implemented by [`super::turn::ReActEngine::run_turn`]; tool execution
//! and persistence live behind the turn boundary. This mirrors the useful
//! shape shared by Codex and Pi: a small outer run, a turn loop, and explicit
//! tool-batch outcomes.

use super::tool_batch::ToolBatchOutcome;
use super::turn::{TurnInput, TurnOutcome};
use super::*;
use crate::types::{BranchPoint, RunBudget, TranscriptRecord};
use haven_common::types::CanonicalMessage;
use std::collections::HashMap;
use std::sync::Arc;
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

struct RunBudgetConfig {
    start_step: u32,
    max_steps: u32,
    effective_max: u32,
}

impl RunBudgetConfig {
    fn new(max_steps: u32, session_max_steps: Option<u32>, start_step: u32) -> Self {
        let per_run_cap = max_steps.max(start_step.saturating_sub(1).saturating_add(max_steps));
        let effective_max = session_max_steps.map_or(per_run_cap, |cap| per_run_cap.min(cap));
        Self {
            start_step,
            max_steps,
            effective_max,
        }
    }

    fn from_engine(engine: &ReActEngine, start_step: u32) -> Self {
        let max_steps = *engine.max_steps.lock().unwrap();
        let session_cap = *engine.session_max_steps.lock().unwrap();
        Self::new(max_steps, session_cap, start_step)
    }

    fn snapshot(&self, session_max_steps: Option<u32>) -> RunBudget {
        RunBudget {
            start_step: self.start_step,
            effective_max: self.effective_max,
            max_steps: self.max_steps,
            session_max_steps,
        }
    }
}

impl ReActEngine {
    /// Run the session one turn at a time. `events` is the authoritative
    /// transcript and `canonical` is its live projection; callers retain both
    /// so the resume/rollback boundary can persist or project them after the
    /// run returns.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_react_loop(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        events: &mut Vec<TranscriptRecord>,
        start_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> anyhow::Result<LoopExit> {
        let budget = RunBudgetConfig::from_engine(self, start_step);
        let session_max_steps = *self.session_max_steps.lock().unwrap();
        self.clear_msg_ids_for_session(session_id);
        let _run_guard = RunMsgIdGuard {
            engine: self,
            session_id: session_id.to_string(),
        };
        self.set_run_budget(session_id, budget.snapshot(session_max_steps));

        tracing::info!(
            session_id,
            run_id,
            start_step = budget.start_step,
            max_steps = budget.max_steps,
            effective_max = budget.effective_max,
            "ReAct run started"
        );

        // The dispatcher promotes Pending before entering the run. A debug
        // assertion catches accidental direct invocation that skips the
        // lifecycle boundary without adding a second promotion path here.
        debug_assert_ne!(
            self.executor.get_session_state(session_id).await,
            Some(SessionStatus::Pending),
            "Pending -> Running must happen before run_react_loop"
        );

        // A confirmation decision wakes a paused run. Finish the already
        // advertised batch before asking the model for another response.
        if self
            .executor
            .get_awaiting_confirm(session_id)
            .await
            .is_some_and(|pending| pending.all_decided())
        {
            match self
                .finish_confirm_batch(
                    session_id,
                    canonical,
                    events,
                    branch_points,
                    &emitter,
                    run_id,
                )
                .await?
            {
                ToolBatchOutcome::Continue => {}
                ToolBatchOutcome::Done(exit) => return Ok(exit),
            }
        }

        let mut cut_off_retries = 0u32;
        let mut last_step = start_step.saturating_sub(1);
        for step_num in budget.start_step..=budget.effective_max {
            last_step = step_num;
            let cancel = self.executor.cancellation_token(session_id).await;
            if cancel.is_cancelled() {
                return Ok(self
                    .exit_cancelled(session_id, events, step_num, branch_points)
                    .await);
            }

            match self
                .run_state_boundary(
                    session_id,
                    events,
                    step_num,
                    branch_points,
                    &emitter,
                    run_id,
                )
                .await
            {
                RunBoundary::Run => {}
                RunBoundary::Exit(exit) => return Ok(exit),
            }

            let outcome = self
                .run_turn(TurnInput {
                    ctx: StepCtx {
                        session_id: session_id.to_string(),
                        step_num,
                        run_id,
                        emitter: emitter.clone(),
                    },
                    canonical,
                    events,
                    branch_points,
                    cancel,
                    max_steps: budget.max_steps,
                    cut_off_retries: &mut cut_off_retries,
                })
                .instrument(tracing::info_span!("turn", session_id, step_num))
                .await?;
            match outcome {
                TurnOutcome::Continue => {}
                TurnOutcome::Done(exit) => return Ok(exit),
            }
        }

        self.pause_turn_budget(session_id, events, last_step + 1, branch_points, &emitter)
            .await?;
        Ok(LoopExit::Paused {
            reason: PauseReason::Budget,
        })
    }

    async fn run_state_boundary(
        &self,
        session_id: &str,
        events: &[TranscriptRecord],
        step_num: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> RunBoundary {
        match self.executor.get_session_state(session_id).await {
            None | Some(SessionStatus::Completed) => RunBoundary::Exit(
                self.exit_with_snapshot(
                    session_id,
                    events,
                    step_num,
                    branch_points,
                    LoopExit::Completed,
                )
                .await,
            ),
            Some(SessionStatus::Error) => {
                self.emit_error(emitter, session_id, "session interrupted")
                    .await;
                RunBoundary::Exit(
                    self.exit_with_snapshot(
                        session_id,
                        events,
                        step_num,
                        branch_points,
                        LoopExit::Error("session interrupted".into()),
                    )
                    .await,
                )
            }
            Some(status) if status.is_paused() => RunBoundary::Exit(
                self.exit_external_pause(
                    session_id,
                    events,
                    step_num,
                    branch_points,
                    emitter,
                    run_id,
                )
                .await,
            ),
            _ => RunBoundary::Run,
        }
    }
}

enum RunBoundary {
    Run,
    Exit(LoopExit),
}

#[cfg(test)]
mod tests {
    use super::RunBudgetConfig;

    #[test]
    fn fresh_run_uses_the_configured_step_budget() {
        let budget = RunBudgetConfig::new(4, None, 1);

        assert_eq!(budget.start_step, 1);
        assert_eq!(budget.max_steps, 4);
        assert_eq!(budget.effective_max, 4);
    }

    #[test]
    fn resumed_run_gets_a_full_budget_but_respects_session_cap() {
        let budget = RunBudgetConfig::new(4, Some(9), 7);

        assert_eq!(budget.start_step, 7);
        assert_eq!(budget.max_steps, 4);
        assert_eq!(budget.effective_max, 9);
    }
}
