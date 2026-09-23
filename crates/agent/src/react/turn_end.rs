//! Turn-end orchestration for the ReAct loop.
//!
//! Turn completion is intentionally separate from turn-start context
//! assembly: the former assembles the final transcript event, event boundary
//! and pause transition into one effect batch. Its late-input path only
//! drains process-local queues. Keeping this boundary explicit prevents a
//! second inbox claim from silently changing the turn-end persistence contract.

use serde_json::Value;

use super::effects::{EffectBatch, TurnControl, TurnEffect};
use super::transcript::TranscriptEvent;
use super::*;

/// Inputs for the shared turn-end path used by both text-only responses and
/// explicit `final_answer` responses.
///
/// Callers pass the final response view. Transcript mutation happens only
/// when the run driver applies the returned batch.
pub(super) struct TurnEndInput<'a> {
    pub(super) ctx: &'a StepCtx,
    pub(super) final_text: &'a str,
    pub(super) reasoning: Option<String>,
    pub(super) thinking_blocks: Vec<Value>,
    pub(super) already_pushed: bool,
    /// True when this step already has a thought transcript, including one
    /// that is still queued in the caller's effect batch and therefore not
    /// visible on [`ReActState`] yet.
    pub(super) thought_projected: bool,
}

impl ReActEngine {
    /// Assemble the final assistant content, the local turn-end inject, and
    /// the pause boundary as one batch. Cross-session inbox collection stays
    /// turn-start-only. This helper does not write: the run driver applies
    /// the batch, so the final transcript commits before local injects.
    ///
    /// X12 remains authoritative at apply time: final content goes through
    /// [`ReActEngine::apply_transcript`], and `pause_turn` only records the
    /// resulting state/boundary.
    pub(super) async fn finish_turn_end(
        &self,
        input: TurnEndInput<'_>,
    ) -> anyhow::Result<EffectBatch> {
        let TurnEndInput {
            ctx,
            final_text,
            reasoning,
            thinking_blocks,
            already_pushed,
            thought_projected,
        } = input;
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

        let mut effects = EffectBatch::continue_batch();
        if !already_pushed {
            effects.transcript(TranscriptEvent::ToolCall {
                text: final_text.to_string(),
                tool_calls: Vec::new(),
                reasoning,
                web_search_calls: Vec::new(),
                thinking_blocks,
                action_cards: Vec::new(),
                persist_text_id,
            });
        } else if let Some(ref mid) = persist_text_id {
            // Search context already pushed the ToolCall event; still need the
            // messages projection when Thought did not land one.
            effects.push(super::effects::TurnEffect::ProjectChatMessage {
                role: "assistant".into(),
                content: final_text.to_string(),
                message_type: Some("text".into()),
                tool_call_id: None,
                message_id: Some(mid.clone()),
            });
        }

        // Applied after the final transcript, so canonical order stays
        // final → injects. Only local queues are drained here; the inbox was
        // claimed once by the turn-start assembly. A successful inject
        // suppresses the pause below and continues the run.
        effects.push(TurnEffect::InjectTurnEnd {
            step_number: ctx.step_num,
        });

        effects.pause(
            ctx.step_num + 1,
            SessionStatus::Paused,
            None,
            final_text,
            Some(ctx.step_num),
        );
        effects = EffectBatch::with_effects(
            TurnControl::Done(LoopExit::Paused {
                reason: PauseReason::TurnEnd,
            }),
            effects.into_effects(),
        );
        Ok(effects)
    }
}
