//! Turn effects produced by the ReAct coordinator.
//!
//! `TurnEngine` decides what should happen at a turn boundary; it does not
//! own the durable transcript or the session lifecycle writer. A plan keeps
//! ordered projection effects separate from the directive that resolves the
//! turn after those effects have been applied.

use super::event_boundary::PauseTurnInput;
use super::tool_batch_policy::ToolRetryBudget;
use super::transcript::TranscriptEvent;
use super::{AgentEvent, LoopExit, PauseReason, ReActEngine, ReActState, StepCtx};
use crate::types::ToolCall;
use haven_common::lifecycle::SessionStatus;
use haven_llm::LlmResponse;
use haven_tools::ToolCatalogSnapshot;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One side-effect intent emitted by a turn.
pub(super) enum TurnEffect {
    /// Append and project one canonical transcript event.
    Transcript(TranscriptEvent),
    /// Persist the branch marker after a search-only round. The marker is a
    /// durable effect because it must be ordered after the transcript event.
    SaveBranchPoint { step_number: u32 },
    /// Publish a live event after all preceding durable effects commit.
    Emit(AgentEvent),
}

/// The action that resolves a turn after its ordered projection effects.
///
/// These are plans, not results: applying one produces a [`RunControl`],
/// which is the single result consumed by the run loop. Keeping conditional
/// turn-end behavior here prevents it from silently overriding another
/// control result halfway through an effects list.
pub(super) enum TurnDirective {
    Continue,
    Exit(LoopExit),
    /// Verify the durable event boundary, then finish the run. Boundary
    /// failure replaces the requested exit with an error exit.
    ExitAtBoundary {
        exit: LoopExit,
    },
    /// Complete a turn at a durable event boundary and publish its lifecycle
    /// transition. `pause_turn` remains the single implementation of the
    /// branch/check/status/hook ordering contract.
    Pause {
        boundary_step: u32,
        status: SessionStatus,
        waiting_reason: Option<haven_common::lifecycle::SessionWaitingReason>,
        branch_point_step: Option<u32>,
        reason: PauseReason,
    },
    /// Drain process-local context after all projection effects. Injected
    /// input records a branch point and continues; no input pauses the turn.
    TurnEnd {
        step_number: u32,
    },
    /// Publish a session error after all projection effects. `hard` returns
    /// an error so the run driver keeps its fail-closed boundary check; a soft
    /// failure resolves to `LoopExit::Error`.
    FailSession {
        message: String,
        hard: bool,
    },
    /// Run the tool batch after its assistant ToolCall effect has committed.
    /// Tool execution is runtime work; its outcome decides whether the run
    /// continues or exits.
    ExecuteToolBatch {
        tool_calls: Vec<ToolCall>,
        thought: Option<String>,
        response: Box<LlmResponse>,
        catalog: Arc<ToolCatalogSnapshot>,
        cancel: CancellationToken,
        allow_tool_retry: bool,
    },
}

/// One planned turn: ordered effects followed by exactly one resolution
/// directive. Effects are applied one at a time. If an effect fails, later
/// effects and the directive are skipped; earlier successful commits remain
/// effective and are not rolled back.
pub(super) struct EffectBatch {
    effects: Vec<TurnEffect>,
    directive: TurnDirective,
}

/// Control returned after applying a turn plan or tool batch and consumed by the run loop.
pub(super) enum RunControl {
    Continue,
    Done(LoopExit),
}

impl EffectBatch {
    pub(super) fn continue_batch() -> Self {
        Self {
            effects: Vec::new(),
            directive: TurnDirective::Continue,
        }
    }

    pub(super) fn done(exit: LoopExit) -> Self {
        Self {
            effects: Vec::new(),
            directive: TurnDirective::Exit(exit),
        }
    }

    pub(super) fn push(&mut self, effect: TurnEffect) {
        self.effects.push(effect);
    }

    #[cfg(test)]
    pub(super) fn into_effects(self) -> Vec<TurnEffect> {
        self.effects
    }

    pub(super) fn prepend(&mut self, mut prefix: Self) {
        assert!(
            matches!(&prefix.directive, TurnDirective::Continue),
            "a prefixed turn plan cannot carry its own resolution directive"
        );
        prefix.effects.append(&mut self.effects);
        self.effects = prefix.effects;
    }

    pub(super) fn transcript(&mut self, event: TranscriptEvent) {
        self.push(TurnEffect::Transcript(event));
    }

    pub(super) fn pause(
        &mut self,
        boundary_step: u32,
        status: SessionStatus,
        waiting_reason: Option<haven_common::lifecycle::SessionWaitingReason>,
        branch_point_step: Option<u32>,
        reason: PauseReason,
    ) {
        self.set_directive(TurnDirective::Pause {
            boundary_step,
            status,
            waiting_reason,
            branch_point_step,
            reason,
        });
    }

    pub(super) fn turn_end(&mut self, step_number: u32) {
        self.set_directive(TurnDirective::TurnEnd { step_number });
    }

    /// Finish this batch at a lifecycle boundary after the effects already
    /// queued. The run driver performs the durable check.
    pub(super) fn with_exit(mut self, exit: LoopExit) -> Self {
        self.set_directive(TurnDirective::ExitAtBoundary { exit });
        self
    }

    /// Record a session error after effects already queued. `hard` makes
    /// application return `Err` so the run driver keeps its fail-closed
    /// boundary check; otherwise the plan resolves to `LoopExit::Error`.
    pub(super) fn fail_session(mut self, message: impl Into<String>, hard: bool) -> Self {
        self.set_directive(TurnDirective::FailSession {
            message: message.into(),
            hard,
        });
        self
    }

    pub(super) fn execute_tool_batch(
        &mut self,
        tool_calls: Vec<ToolCall>,
        thought: Option<String>,
        response: LlmResponse,
        catalog: Arc<ToolCatalogSnapshot>,
        cancel: CancellationToken,
        allow_tool_retry: bool,
    ) {
        self.set_directive(TurnDirective::ExecuteToolBatch {
            tool_calls,
            thought,
            response: Box::new(response),
            catalog,
            cancel,
            allow_tool_retry,
        });
    }

    fn set_directive(&mut self, directive: TurnDirective) {
        assert!(
            matches!(&self.directive, TurnDirective::Continue),
            "turn plan already has a resolution directive"
        );
        self.directive = directive;
    }

    pub(super) async fn apply(
        self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        tool_retry_budget: &mut ToolRetryBudget,
    ) -> anyhow::Result<RunControl> {
        self.apply_with_tool_budget(engine, ctx, state, Some(tool_retry_budget))
            .await
    }

    async fn apply_with_tool_budget(
        self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        tool_retry_budget: Option<&mut ToolRetryBudget>,
    ) -> anyhow::Result<RunControl> {
        // Each operation commits before the next starts. An error stops the
        // sequence but does not undo completed transcript/lifecycle writes.
        for effect in self.effects {
            apply_projection_effect(engine, ctx, state, effect).await?;
        }

        match self.directive {
            TurnDirective::ExecuteToolBatch {
                tool_calls,
                thought,
                response,
                catalog,
                cancel,
                allow_tool_retry,
            } => {
                let tool_retry_budget = tool_retry_budget.ok_or_else(|| {
                    anyhow::anyhow!("committed effect batch cannot execute tools")
                })?;
                let outcome = engine
                    .execute_tool_batch(
                        &ctx.session_id,
                        state,
                        ctx.step_number,
                        &ctx.emitter,
                        ctx.run_id,
                        &tool_calls,
                        &thought,
                        &response,
                        catalog,
                        &cancel,
                        allow_tool_retry,
                        tool_retry_budget,
                    )
                    .await?;
                Ok(outcome)
            }
            directive => apply_committed_directive(engine, ctx, state, directive).await,
        }
    }
}

/// Apply a non-recursive terminal directive. Keeping tool execution outside
/// this function lets its outcome commit projection batches without creating
/// a recursive async future.
async fn apply_committed_directive(
    engine: &ReActEngine,
    ctx: &StepCtx,
    state: &mut ReActState,
    directive: TurnDirective,
) -> anyhow::Result<RunControl> {
    match directive {
        TurnDirective::Continue => Ok(RunControl::Continue),
        TurnDirective::Exit(exit) => Ok(RunControl::Done(exit)),
        TurnDirective::ExitAtBoundary { exit } => {
            let exit = engine
                .exit_at_boundary(&ctx.session_id, state, ctx.step_number, exit)
                .await;
            Ok(RunControl::Done(exit))
        }
        TurnDirective::Pause {
            boundary_step,
            status,
            waiting_reason,
            branch_point_step,
            reason,
        } => {
            engine
                .pause_turn(PauseTurnInput {
                    session_id: &ctx.session_id,
                    state,
                    boundary_step,
                    emitter: &ctx.emitter,
                    status,
                    waiting_reason,
                    branch_point_step,
                    run_id: ctx.run_id,
                    reason,
                })
                .await?;
            Ok(RunControl::Done(LoopExit::Paused { reason }))
        }
        TurnDirective::TurnEnd { step_number } => {
            if engine.inject_turn_end_context(ctx, state).await? {
                engine
                    .save_branch_point(&ctx.session_id, state, step_number)
                    .await?;
                return Ok(RunControl::Continue);
            }
            engine
                .pause_turn(PauseTurnInput {
                    session_id: &ctx.session_id,
                    state,
                    boundary_step: step_number + 1,
                    emitter: &ctx.emitter,
                    status: SessionStatus::Paused,
                    waiting_reason: None,
                    branch_point_step: Some(step_number),
                    run_id: ctx.run_id,
                    reason: PauseReason::TurnEnd,
                })
                .await?;
            Ok(RunControl::Done(LoopExit::Paused {
                reason: PauseReason::TurnEnd,
            }))
        }
        TurnDirective::FailSession { message, hard } => {
            engine
                .emit_error(&ctx.emitter, &ctx.session_id, &message)
                .await;
            engine
                .executor
                .update_session_status_if(
                    &ctx.session_id,
                    SessionStatus::Running,
                    SessionStatus::Error,
                )
                .await?;
            if hard {
                return Err(anyhow::anyhow!(message));
            }
            Ok(RunControl::Done(LoopExit::Error(message)))
        }
        TurnDirective::ExecuteToolBatch { .. } => {
            anyhow::bail!("committed effect batch cannot execute tools")
        }
    }
}

/// A committed projection effect never re-enters [`EffectBatch::apply`]. Tool
/// execution invokes the non-recursive committed-batch applier, which shares
/// this one-effect function.
async fn apply_projection_effect(
    engine: &ReActEngine,
    ctx: &StepCtx,
    state: &mut ReActState,
    effect: TurnEffect,
) -> anyhow::Result<()> {
    match effect {
        TurnEffect::Transcript(event) => engine.apply_transcript(ctx, event, state).await?,
        TurnEffect::SaveBranchPoint { step_number } => {
            engine
                .save_branch_point(&ctx.session_id, state, step_number)
                .await?;
        }
        TurnEffect::Emit(event) => ctx.emitter.emit(event).await,
    }
    Ok(())
}

impl ReActEngine {
    /// Apply the ordered projection effects, then resolve the turn directive.
    pub(super) async fn apply_effect_batch(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        batch: EffectBatch,
        tool_retry_budget: &mut ToolRetryBudget,
    ) -> anyhow::Result<RunControl> {
        batch.apply(self, ctx, state, tool_retry_budget).await
    }

    /// Apply projection effects for tool results and lifecycle transitions.
    /// This path has no tool budget, so it cannot recursively execute a tool
    /// batch while one is already being committed.
    pub(super) async fn apply_committed_batch(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        batch: EffectBatch,
    ) -> anyhow::Result<RunControl> {
        for effect in batch.effects {
            apply_projection_effect(self, ctx, state, effect).await?;
        }
        apply_committed_directive(self, ctx, state, batch.directive).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batches_have_one_explicit_directive() {
        assert!(matches!(
            EffectBatch::continue_batch().directive,
            TurnDirective::Continue
        ));
        assert!(matches!(
            EffectBatch::done(LoopExit::Cancelled).directive,
            TurnDirective::Exit(LoopExit::Cancelled)
        ));
    }

    #[test]
    fn effects_are_appended_in_declaration_order() {
        let mut batch = EffectBatch::continue_batch();
        batch.transcript(TranscriptEvent::Thought {
            text: "one".into(),
            message_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
        });
        assert_eq!(batch.effects.len(), 1);
        assert!(matches!(batch.effects[0], TurnEffect::Transcript(_)));
    }

    #[test]
    fn prepend_keeps_prefix_effects_before_end_directive() {
        let mut prefix = EffectBatch::continue_batch();
        prefix.transcript(TranscriptEvent::Thought {
            text: "thought".into(),
            message_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
        });
        let mut end = EffectBatch::continue_batch();
        end.transcript(TranscriptEvent::ToolCall {
            text: "final".into(),
            tool_calls: Vec::new(),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            tool_call_cards: Vec::new(),
            persist_text_id: None,
        });
        end.turn_end(1);
        end.prepend(prefix);

        assert!(matches!(end.effects[0], TurnEffect::Transcript(_)));
        assert!(matches!(end.effects[1], TurnEffect::Transcript(_)));
        assert!(matches!(
            end.directive,
            TurnDirective::TurnEnd { step_number: 1 }
        ));
    }

    #[test]
    #[should_panic(expected = "turn plan already has a resolution directive")]
    fn turn_plan_cannot_replace_its_resolution_directive() {
        let mut batch = EffectBatch::continue_batch();
        batch.turn_end(1);
        batch.pause(
            2,
            SessionStatus::Paused,
            None,
            Some(1),
            PauseReason::TurnEnd,
        );
    }
}
