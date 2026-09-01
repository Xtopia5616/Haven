//! Turn-end orchestration for the ReAct loop.
//!
//! Turn completion is intentionally separate from turn-start context
//! assembly: the former owns the final transcript event, branch checkpoint
//! and pause transition; its late-input path only drains process-local queues.
//! Keeping this boundary explicit prevents a second inbox claim from silently
//! changing the turn-end persistence contract.

use serde_json::Value;

use super::snapshot_io::PauseTurnInput;
use super::transcript::TranscriptEvent;
use super::*;

/// Inputs for the shared turn-end path used by both text-only responses and
/// explicit `final_answer` responses.
///
/// The mutable transcript and branch state are grouped here so callers do
/// not grow another positional-argument list when turn-end behavior evolves.
pub(super) struct TurnEndInput<'a> {
    pub(super) ctx: &'a StepCtx,
    pub(super) state: &'a mut ReActState,
    pub(super) final_text: &'a str,
    pub(super) reasoning: Option<String>,
    pub(super) thinking_blocks: Vec<Value>,
    pub(super) already_pushed: bool,
}

impl ReActEngine {
    /// Apply the final assistant content, deliver local context that arrived
    /// while the model was running, then either continue or persist a paused
    /// turn. Cross-session inbox collection is turn-start-only.
    ///
    /// X12 remains authoritative here: final content is applied through
    /// [`ReActEngine::apply_transcript`], and `pause_turn` only records the
    /// resulting state/checkpoint. The helper deliberately does not read
    /// queues directly; [`super::inject`] owns context collection.
    pub(super) async fn finish_turn_end(
        &self,
        input: TurnEndInput<'_>,
    ) -> anyhow::Result<TurnEndOutcome> {
        let TurnEndInput {
            ctx,
            state,
            final_text,
            reasoning,
            thinking_blocks,
            already_pushed,
        } = input;

        let thought_projected = state.events.iter().any(|e| {
            matches!(
                e,
                TranscriptRecord::Thought { step_number, .. }
                    if *step_number == ctx.step_num
            )
        });
        // Prefer thinking_blocks over a plain reasoning string when both exist.
        let reasoning = if thinking_blocks.is_empty() {
            reasoning
        } else {
            None
        };
        // Thought apply already projected under the thought id — only project
        // again when there was no Thought for this step (synthetic finals).
        let persist_text_id = if thought_projected {
            None
        } else {
            Some(self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought"))
        };

        if !already_pushed {
            self.apply_transcript(
                ctx,
                TranscriptEvent::ToolCall {
                    text: final_text.to_string(),
                    tool_calls: Vec::new(),
                    reasoning,
                    web_search_calls: Vec::new(),
                    thinking_blocks,
                    action_cards: Vec::new(),
                    persist_text_id,
                },
                state,
            )
            .await;
        } else if let Some(ref mid) = persist_text_id {
            // Search context already pushed the ToolCall event; still need the
            // messages projection when Thought did not land one.
            self.project_chat_message(
                &ctx.session_id,
                "assistant",
                final_text,
                Some("text"),
                None,
                Some(mid),
            )
            .await;
        }

        // Inject AFTER the final so canonical/events order is final → injects.
        // Only local queues are drained here; the inbox was claimed once by
        // the turn-start assembly.
        if self.inject_turn_end_context(ctx, state).await {
            self.save_branch_point(&ctx.session_id, state, ctx.step_num, false)
                .await;
            return Ok(TurnEndOutcome::Continue);
        }

        self.pause_turn(PauseTurnInput {
            session_id: &ctx.session_id,
            state,
            snapshot_step: ctx.step_num + 1,
            emitter: &ctx.emitter,
            status: SessionStatus::Paused,
            final_text,
            branch_point_step: Some(ctx.step_num),
        })
        .await?;
        Ok(TurnEndOutcome::Done(LoopExit::Paused {
            reason: PauseReason::TurnEnd,
        }))
    }
}

/// Outcome of the shared turn-end helper.
#[derive(Debug)]
pub(crate) enum TurnEndOutcome {
    /// Pending context injected mid-final; loop should continue.
    Continue,
    /// Turn paused (`PauseReason::TurnEnd`).
    Done(LoopExit),
}
