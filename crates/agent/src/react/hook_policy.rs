//! Production [`LoopHooks`] policy.
//!
//! `hooks.rs` owns the stable extension contract and test double. This module
//! owns the production composition of compaction, inference, response
//! classification and the safety gate. Turn-start context assembly owns inbox
//! polling before these hooks run. Keeping the policy separate
//! makes it possible to exercise the loop with a deliberately inert hook
//! implementation without importing production side effects.

use async_trait::async_trait;
use haven_tools::ConfirmationResult;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::hooks::{
    AfterLlmInput, BeforeToolAction, InferCallback, LoopHooks, MemoryPatchHandle, ToolCallIdentity,
};
use super::retries::{AfterLlmAction, ResponsePolicy};
use super::{PauseReason, ReActEngine, ReActState, StepCtx, canonical_has_image};

/// Production hooks: context compaction, interval + pause infer, throttled
/// MEMORY fence refresh (M2), response policy, and confirm pre-check.
pub(crate) struct DefaultHooks {
    /// Optional session-scoped fact inference. `None` in unit tests that
    /// construct an engine without an [`crate::InferenceEngine`].
    infer: Option<InferCallback>,
    /// Optional mid-run MEMORY patch after outbox fact writes (M2).
    memory_patch: Option<MemoryPatchHandle>,
}

impl DefaultHooks {
    pub(crate) fn new(infer: Option<InferCallback>) -> Self {
        Self {
            infer,
            memory_patch: None,
        }
    }

    pub(crate) fn with_memory_patch(mut self, handle: MemoryPatchHandle) -> Self {
        self.memory_patch = Some(handle);
        self
    }

    fn call_infer(&self, session_id: &str, bypass_throttle: bool) {
        if let Some(ref infer) = self.infer {
            infer(session_id, bypass_throttle);
        }
    }
}

#[async_trait]
impl LoopHooks for DefaultHooks {
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        cancel: CancellationToken,
    ) {
        // M2: after outbox fact writes, surgically refresh MEMORY fence
        // (throttled). It must run before compaction so the budget decision
        // sees the exact system prompt that will be sent to the provider.
        // Never rebuild tools/skills/MCP short index.
        if let Some(ref patch) = self.memory_patch
            && patch.inference.take_memory_dirty_throttled(&ctx.session_id)
        {
            let description = match engine.executor.get_session(&ctx.session_id).await {
                Some(s) if !s.summary.is_empty() => s.summary,
                Some(s) => s.input,
                None => String::new(),
            };
            patch
                .prompt_builder
                .patch_canonical_memory_fence(&ctx.session_id, &description, &mut state.canonical)
                .await;
        }
        let has_image = canonical_has_image(&state.canonical);
        // Resolve the exact per-session tool projection before compaction so
        // schema tokens participate in the context decision. The turn reuses
        // the same cached Arc immediately afterwards.
        let tool_defs = engine
            .build_tool_definitions_for_session(&ctx.session_id)
            .await;
        // Phase 7 / I2: compact is a nested phase under before_step. It is
        // deliberately after all context sources and prompt patches have
        // settled, so compaction and the following RequestContext snapshot
        // observe one coherent canonical projection.
        let _ = engine
            .maybe_compact(ctx, state, has_image, &tool_defs, cancel)
            .instrument(tracing::info_span!(
                "compact",
                session_id = %ctx.session_id,
                step_num = ctx.step_num
            ))
            .await;
        let interval = engine.limits().fact_infer_interval_steps;
        if ctx.step_num > 0 && interval > 0 && ctx.step_num.is_multiple_of(interval) {
            self.call_infer(&ctx.session_id, false);
        }
    }

    async fn after_llm(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        input: AfterLlmInput<'_>,
    ) -> AfterLlmAction {
        ResponsePolicy::classify(
            input.thought,
            input.actions,
            input.response,
            input.canonical,
            input.state,
        )
    }

    async fn before_tool(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        identity: ToolCallIdentity<'_>,
        tool_name: &str,
        input: &Value,
    ) -> BeforeToolAction {
        // Resume path: a prior confirm pause already recorded a decision.
        if let Some(decision) = engine
            .executor
            .confirm_decision_for(
                &ctx.session_id,
                identity.step_id,
                identity.action_index,
                identity.tool_call_id,
            )
            .await
        {
            return if decision {
                BeforeToolAction::Proceed {
                    confirmed: Some(true),
                }
            } else {
                BeforeToolAction::Block {
                    error: format!(
                        "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                        tool_name
                    ),
                }
            };
        }

        match engine
            .executor
            .check_tool_gate(&ctx.session_id, tool_name, input)
            .await
        {
            ConfirmationResult::AutoApproved => BeforeToolAction::Proceed { confirmed: None },
            ConfirmationResult::Blocked { reason } => BeforeToolAction::Block {
                error: format!(
                    "operation '{}' is blocked by the security policy ({reason}). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                    tool_name
                ),
            },
            ConfirmationResult::RequiresConfirmation { risk_level, .. } => {
                BeforeToolAction::NeedConfirm { risk_level }
            }
        }
    }

    async fn on_pause(&self, _engine: &ReActEngine, ctx: &StepCtx, _reason: PauseReason) {
        self.call_infer(&ctx.session_id, true);
    }
}

pub(crate) fn default_hooks() -> super::hooks::LoopHooksHandle {
    std::sync::Arc::new(DefaultHooks::new(None))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn default_hooks_with_infer(infer: InferCallback) -> super::hooks::LoopHooksHandle {
    std::sync::Arc::new(DefaultHooks::new(Some(infer)))
}

pub(crate) fn default_hooks_with_infer_and_patch(
    infer: InferCallback,
    memory_patch: MemoryPatchHandle,
) -> super::hooks::LoopHooksHandle {
    std::sync::Arc::new(DefaultHooks::new(Some(infer)).with_memory_patch(memory_patch))
}
