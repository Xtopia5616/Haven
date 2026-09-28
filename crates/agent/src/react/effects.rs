//! Turn effects produced by the ReAct coordinator.
//!
//! `TurnEngine` decides what should happen at a turn boundary; it does not
//! own the durable transcript or the session lifecycle writer.  Effects are
//! intentionally data-shaped so the run driver has one place where durable
//! projection and live UI publication are applied in order.

use super::event_boundary::PauseTurnInput;
use super::tool_batch::ToolBatchOutcome;
use super::tool_batch_policy::ToolRetryBudget;
use super::transcript::TranscriptEvent;
use super::{AgentEvent, LoopExit, PauseReason, ReActEngine, ReActState, SessionStatus, StepCtx};
use crate::types::Action;
use haven_llm::LlmResponse;
use haven_tools::ToolCatalogSnapshot;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One side-effect intent emitted by a turn.
pub(super) enum TurnEffect {
    /// Append and project one canonical transcript event.
    Transcript(TranscriptEvent),
    /// Materialize an assistant row without adding a transcript event in this
    /// effect (for example a UI-only waiting notice).
    ProjectChatMessage {
        role: String,
        content: String,
        message_type: Option<String>,
        tool_call_id: Option<String>,
        message_id: Option<String>,
    },
    /// Persist the branch marker after a search-only round. The marker is a
    /// durable effect because it must be ordered after the transcript event.
    SaveBranchPoint { step_number: u32 },
    /// Run the tool batch after its assistant ToolCall effect has committed.
    /// Tool execution is still an external runtime activity, but the turn
    /// coordinator no longer owns its transcript/UI application boundary.
    ExecuteToolBatch {
        actions: Vec<Action>,
        thought: Option<String>,
        response: LlmResponse,
        catalog: Arc<ToolCatalogSnapshot>,
        cancel: CancellationToken,
        allow_tool_retry: bool,
    },
    /// Publish a live event after all preceding durable effects commit.
    Emit(AgentEvent),
    /// Complete a turn at a durable event boundary and publish its lifecycle
    /// transition.  `pause_turn` remains the single implementation of the
    /// branch/check/status/hook ordering contract.
    Pause {
        boundary_step: u32,
        status: SessionStatus,
        waiting_reason: Option<haven_common::SessionWaitingReason>,
        final_text: String,
        branch_point_step: Option<u32>,
        reason: PauseReason,
    },
    /// Drain process-local context after the final transcript effect.
    /// Injected input keeps the run alive and suppresses every later effect,
    /// including the turn-end pause.
    InjectTurnEnd { step_number: u32 },
    /// Publish a session error only after preceding durable effects commit.
    /// `hard` returns from the run driver so the existing fail-closed path
    /// can still verify the event boundary; a soft failure is a normal
    /// `LoopExit::Error`.
    FailSession { message: String, hard: bool },
    /// Verify the durable event boundary, then finish the run. Boundary
    /// failure replaces the requested exit with an error exit.
    ExitAtBoundary { exit: LoopExit },
}

/// The result of one model turn. The batch is applied by the run driver, not
/// by the code which decides the turn outcome.
pub(super) struct EffectBatch {
    effects: Vec<TurnEffect>,
    control: TurnControl,
}

pub(super) enum TurnControl {
    Continue,
    Done(LoopExit),
}

impl EffectBatch {
    pub(super) fn continue_batch() -> Self {
        Self {
            effects: Vec::new(),
            control: TurnControl::Continue,
        }
    }

    pub(super) fn done(exit: LoopExit) -> Self {
        Self {
            effects: Vec::new(),
            control: TurnControl::Done(exit),
        }
    }

    pub(super) fn with_effects(control: TurnControl, effects: Vec<TurnEffect>) -> Self {
        Self { effects, control }
    }

    pub(super) fn push(&mut self, effect: TurnEffect) {
        self.effects.push(effect);
    }

    pub(super) fn into_effects(self) -> Vec<TurnEffect> {
        self.effects
    }

    pub(super) fn prepend(&mut self, prefix: Self) {
        let mut effects = prefix.effects;
        effects.append(&mut self.effects);
        self.effects = effects;
    }

    pub(super) fn transcript(&mut self, event: TranscriptEvent) {
        self.push(TurnEffect::Transcript(event));
    }

    pub(super) fn pause(
        &mut self,
        boundary_step: u32,
        status: SessionStatus,
        waiting_reason: Option<haven_common::SessionWaitingReason>,
        final_text: impl Into<String>,
        branch_point_step: Option<u32>,
        reason: PauseReason,
    ) {
        self.push(TurnEffect::Pause {
            boundary_step,
            status,
            waiting_reason,
            final_text: final_text.into(),
            branch_point_step,
            reason,
        });
    }

    /// Finish this batch at a lifecycle boundary after the effects already
    /// queued. The run driver performs the durable check.
    pub(super) fn with_exit(mut self, exit: LoopExit) -> Self {
        self.effects
            .push(TurnEffect::ExitAtBoundary { exit: exit.clone() });
        self.control = TurnControl::Done(exit);
        self
    }

    /// Record a session error after effects already queued. `hard` makes
    /// application return `Err` so the run driver keeps its fail-closed
    /// boundary check; otherwise the batch completes with `LoopExit::Error`.
    pub(super) fn fail_session(mut self, message: impl Into<String>, hard: bool) -> Self {
        let message = message.into();
        self.effects.push(TurnEffect::FailSession {
            message: message.clone(),
            hard,
        });
        if !hard {
            self.control = TurnControl::Done(LoopExit::Error(message));
        }
        self
    }

    pub(super) async fn apply(
        self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        tool_retry_budget: &mut ToolRetryBudget,
    ) -> anyhow::Result<TurnControl> {
        let mut control = self.control;
        for effect in self.effects {
            control = match effect {
                TurnEffect::ExecuteToolBatch {
                    actions,
                    thought,
                    response,
                    catalog,
                    cancel,
                    allow_tool_retry,
                } => {
                    let outcome = engine
                        .execute_tool_batch(
                            &ctx.session_id,
                            state,
                            ctx.step_num,
                            &ctx.emitter,
                            ctx.run_id,
                            &actions,
                            &thought,
                            &response,
                            catalog,
                            &cancel,
                            allow_tool_retry,
                            tool_retry_budget,
                        )
                        .await?;
                    match outcome {
                        ToolBatchOutcome::Continue => TurnControl::Continue,
                        ToolBatchOutcome::Done(exit) => TurnControl::Done(exit),
                    }
                }
                effect => match apply_projection_effect(engine, ctx, state, effect, control).await?
                {
                    ProjectionProgress::Continue(control) => control,
                    ProjectionProgress::Stop(control) => return Ok(control),
                },
            };
        }
        Ok(control)
    }
}

/// Projection commits must not re-enter [`EffectBatch::apply`]. Tool execution
/// calls back into this applier, and sharing one async function makes the
/// state machine recursive (`E0733`).
enum ProjectionProgress {
    Continue(TurnControl),
    Stop(TurnControl),
}

async fn apply_projection_effect(
    engine: &ReActEngine,
    ctx: &StepCtx,
    state: &mut ReActState,
    effect: TurnEffect,
    control: TurnControl,
) -> anyhow::Result<ProjectionProgress> {
    match effect {
        TurnEffect::ExecuteToolBatch { .. } => {
            anyhow::bail!("committed effect batch cannot execute another tool batch");
        }
        TurnEffect::Transcript(event) => {
            engine.apply_transcript(ctx, event, state).await?;
        }
        TurnEffect::ProjectChatMessage {
            role,
            content,
            message_type,
            tool_call_id,
            message_id,
        } => {
            engine
                .project_chat_message(
                    &ctx.session_id,
                    &role,
                    &content,
                    message_type.as_deref(),
                    tool_call_id.as_deref(),
                    message_id.as_deref(),
                )
                .await?;
        }
        TurnEffect::SaveBranchPoint { step_number } => {
            engine
                .save_branch_point(&ctx.session_id, state, step_number, false)
                .await?;
        }
        TurnEffect::Emit(event) => {
            ctx.emitter.emit(event).await;
        }
        TurnEffect::Pause {
            boundary_step,
            status,
            waiting_reason,
            final_text,
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
                    final_text: &final_text,
                    branch_point_step,
                    run_id: ctx.run_id,
                    reason,
                })
                .await?;
        }
        TurnEffect::InjectTurnEnd { step_number } => {
            if engine.inject_turn_end_context(ctx, state).await? {
                engine
                    .save_branch_point(&ctx.session_id, state, step_number, false)
                    .await?;
                return Ok(ProjectionProgress::Stop(TurnControl::Continue));
            }
        }
        TurnEffect::FailSession { message, hard } => {
            engine
                .emit_error(&ctx.emitter, &ctx.session_id, &message)
                .await;
            engine
                .executor
                .update_session_status(&ctx.session_id, SessionStatus::Error)
                .await?;
            if hard {
                return Err(anyhow::anyhow!(message));
            }
            return Ok(ProjectionProgress::Stop(TurnControl::Done(
                LoopExit::Error(message),
            )));
        }
        TurnEffect::ExitAtBoundary { exit } => {
            let exit = engine
                .exit_at_boundary(&ctx.session_id, state, ctx.step_num, exit)
                .await;
            return Ok(ProjectionProgress::Stop(TurnControl::Done(exit)));
        }
    }
    Ok(ProjectionProgress::Continue(control))
}

impl ReActEngine {
    /// Apply the ordered effect boundary for one completed turn.
    pub(super) async fn apply_effect_batch(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        batch: EffectBatch,
        tool_retry_budget: &mut ToolRetryBudget,
    ) -> anyhow::Result<TurnControl> {
        batch.apply(self, ctx, state, tool_retry_budget).await
    }

    /// Apply transcript, projection and lifecycle effects that are not another
    /// tool batch. This path is deliberately not [`EffectBatch::apply`]: tool
    /// execution commits observations while the outer batch is still running.
    pub(super) async fn apply_committed_batch(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        batch: EffectBatch,
    ) -> anyhow::Result<TurnControl> {
        let mut control = batch.control;
        for effect in batch.effects {
            control = match apply_projection_effect(self, ctx, state, effect, control).await? {
                ProjectionProgress::Continue(control) => control,
                ProjectionProgress::Stop(control) => return Ok(control),
            };
        }
        Ok(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batches_keep_explicit_control_flow() {
        assert!(matches!(
            EffectBatch::continue_batch().control,
            TurnControl::Continue
        ));
        assert!(matches!(
            EffectBatch::done(LoopExit::Cancelled).control,
            TurnControl::Done(LoopExit::Cancelled)
        ));
    }

    #[test]
    fn effects_are_appended_in_declaration_order() {
        let mut batch = EffectBatch::continue_batch();
        batch.transcript(TranscriptEvent::Thought {
            text: "one".into(),
            message_id: "step-1".into(),
        });
        assert_eq!(batch.effects.len(), 1);
        assert!(matches!(batch.effects[0], TurnEffect::Transcript(_)));
    }

    #[test]
    fn prepend_keeps_transcript_before_turn_end_control() {
        let mut prefix = EffectBatch::continue_batch();
        prefix.transcript(TranscriptEvent::Thought {
            text: "thought".into(),
            message_id: "step-1".into(),
        });
        let mut end = EffectBatch::continue_batch();
        end.transcript(TranscriptEvent::ToolCall {
            text: "final".into(),
            tool_calls: Vec::new(),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            action_cards: Vec::new(),
            persist_text_id: None,
        });
        end.push(TurnEffect::InjectTurnEnd { step_number: 1 });
        end.pause(
            2,
            SessionStatus::Paused,
            None,
            "final",
            Some(1),
            PauseReason::TurnEnd,
        );
        end = EffectBatch::with_effects(
            TurnControl::Done(LoopExit::Paused {
                reason: super::super::PauseReason::TurnEnd,
            }),
            end.into_effects(),
        );
        end.prepend(prefix);

        assert!(matches!(end.effects[0], TurnEffect::Transcript(_)));
        assert!(matches!(end.effects[1], TurnEffect::Transcript(_)));
        assert!(matches!(
            end.effects[2],
            TurnEffect::InjectTurnEnd { step_number: 1 }
        ));
        assert!(matches!(end.effects[3], TurnEffect::Pause { .. }));
        assert!(matches!(
            end.control,
            TurnControl::Done(LoopExit::Paused {
                reason: super::super::PauseReason::TurnEnd,
            })
        ));
    }
}
