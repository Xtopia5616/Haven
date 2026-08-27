//! Turn-end orchestration for the ReAct loop.
//!
//! Turn completion is intentionally separate from pending-context injection:
//! the former owns the final transcript event, branch checkpoint and pause
//! transition; the latter only drains context sources. Keeping this boundary
//! explicit prevents a new queue or inbox path from silently changing the
//! turn-end persistence contract.

use std::collections::HashMap;

use serde_json::Value;

use super::transcript::TranscriptEvent;
use super::*;
use crate::types::{BranchPoint, TranscriptRecord};

/// Inputs for the shared turn-end path used by both text-only responses and
/// explicit `final_answer` responses.
///
/// The mutable transcript and branch state are grouped here so callers do
/// not grow another positional-argument list when turn-end behavior evolves.
pub(super) struct TurnEndInput<'a> {
    pub(super) ctx: &'a StepCtx,
    pub(super) events: &'a mut Vec<TranscriptRecord>,
    pub(super) canonical: &'a mut Vec<CanonicalMessage>,
    pub(super) branch_points: &'a mut HashMap<u32, BranchPoint>,
    pub(super) final_text: &'a str,
    pub(super) reasoning: Option<String>,
    pub(super) thinking_blocks: Vec<Value>,
    pub(super) already_pushed: bool,
}

impl ReActEngine {
    /// Apply the final assistant content, deliver context that arrived while
    /// the model was running, then either continue or persist a paused turn.
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
            events,
            canonical,
            branch_points,
            final_text,
            reasoning,
            thinking_blocks,
            already_pushed,
        } = input;

        let thought_projected = events.iter().any(|e| {
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
                events,
                canonical,
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

        // Inject AFTER the final so canonical/events order is final → injects
        // (replaces the old insert-before-injects dance).
        if self.inject_pending_context(ctx, events, canonical).await {
            self.save_branch_point(&ctx.session_id, events, ctx.step_num, branch_points, false)
                .await;
            return Ok(TurnEndOutcome::Continue);
        }

        self.pause_turn(
            &ctx.session_id,
            events,
            ctx.step_num + 1,
            branch_points,
            &ctx.emitter,
            SessionStatus::Paused,
            final_text,
            Some(ctx.step_num),
            None,
            true,
        )
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
