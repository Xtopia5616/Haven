//! Production [`LoopHooks`] policy.
//!
//! `hooks.rs` owns the stable extension contract and test double. This module
//! owns the production composition of compaction, memory worker, response
//! classification and the safety gate. Turn-start context assembly owns inbox
//! polling before these hooks run. Keeping the policy separate
//! makes it possible to exercise the loop with a deliberately inert hook
//! implementation without importing production side effects.

use async_trait::async_trait;
use haven_tools::AuthorizationDecision;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::hooks::{
    AfterLlmInput, BeforeStepOutput, BeforeToolCallDecision, BeforeToolRequest, LoopHooks,
    MemoryPatchHandle,
};
use super::retries::{AfterLlmAction, ResponsePolicy};
use super::{PauseReason, ReActEngine, ReActState, StepCtx};
use crate::memory_trigger::MemoryTriggerPayload;

/// Production hooks: context compaction, interval + pause trigger intent, throttled
/// MEMORY fence refresh (M2), response policy, and confirm pre-check.
pub(crate) struct DefaultHooks {
    /// Optional mid-run MEMORY patch after outbox fact writes (M2).
    memory_patch: Option<MemoryPatchHandle>,
}

impl DefaultHooks {
    pub(crate) fn new() -> Self {
        Self { memory_patch: None }
    }

    pub(crate) fn with_memory_patch(mut self, handle: MemoryPatchHandle) -> Self {
        self.memory_patch = Some(handle);
        self
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
    ) -> anyhow::Result<BeforeStepOutput> {
        // M2: after outbox fact writes, surgically refresh MEMORY fence
        // (throttled). It must run before compaction so the budget decision
        // sees the exact system prompt that will be sent to the provider.
        // Never rebuild tools/skills/MCP short index.
        if let Some(ref patch) = self.memory_patch
            && patch
                .memory_worker
                .take_memory_dirty_throttled(&ctx.session_id)
        {
            let description = match engine.executor.get_session(&ctx.session_id).await {
                Some(s) if !s.summary.is_empty() => s.summary,
                Some(s) => s.input,
                None => String::new(),
            };
            let changed = patch
                .prompt_builder
                .patch_canonical_memory_fence(
                    &ctx.session_id,
                    &description,
                    Arc::make_mut(&mut state.canonical).as_mut_slice(),
                )
                .await;
            if changed {
                state.mark_canonical_changed();
            }
        }
        let media_requirements = state.media_requirements();
        // Resolve the exact per-session tool projection before compaction so
        // schema tokens participate in the context decision. The turn reuses
        // the same cached Arc immediately afterwards.
        let tool_catalog = engine.build_tool_catalog_for_session(&ctx.session_id).await;
        let prepared_tools = engine.prepare_tool_definitions(&ctx.session_id, &tool_catalog);
        // Phase 7 / I2: compact is a nested phase under before_step. It is
        // deliberately after all context sources and prompt patches have
        // settled, so compaction and the following RequestContext snapshot
        // observe one coherent canonical projection.
        engine
            .maybe_compact(
                ctx,
                state,
                media_requirements,
                prepared_tools.token_estimate,
                cancel,
            )
            .instrument(tracing::info_span!(
                "compact",
                session_id = %ctx.session_id,
                step_num = ctx.step_num
            ))
            .await?;
        let interval = engine.limits().fact_infer_interval_steps;
        let memory_trigger =
            (ctx.step_num > 0 && interval > 0 && ctx.step_num.is_multiple_of(interval))
                .then(|| MemoryTriggerPayload::step_interval(ctx.run_id, ctx.step_num));
        Ok(BeforeStepOutput {
            tool_definitions: Some(prepared_tools.definitions),
            tool_token_estimate: Some(prepared_tools.token_estimate),
            tool_catalog: Some(tool_catalog),
            memory_trigger,
        })
    }

    async fn after_llm(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        input: AfterLlmInput<'_>,
    ) -> AfterLlmAction {
        ResponsePolicy::classify(input.thought, input.tool_calls, input.response, input.state)
    }

    fn before_tool(
        &self,
        request: BeforeToolRequest,
    ) -> futures_util::future::BoxFuture<'static, BeforeToolCallDecision> {
        Box::pin(async move {
            // Resume path: a prior confirm pause already recorded a decision.
            if let Some((decision, receipt)) = request
                .executor
                .confirm_decision_for(
                    &request.session_id,
                    &request.identity.step_id,
                    request.identity.tool_index,
                    request.identity.tool_call_id.as_deref(),
                )
                .await
            {
                return if decision {
                    BeforeToolCallDecision::Proceed { receipt }
                } else {
                    BeforeToolCallDecision::Block {
                        error: format!(
                            "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                            request.tool_name
                        ),
                    }
                };
            }

            match request
                .executor
                .check_tool_gate_with_catalog(
                    &request.session_id,
                    &request.tool_name,
                    &request.input,
                    &request.catalog,
                )
                .await
            {
                AuthorizationDecision::AutoApproved => {
                    BeforeToolCallDecision::Proceed { receipt: None }
                }
                AuthorizationDecision::Blocked { reason, .. } => BeforeToolCallDecision::Block {
                    error: format!(
                        "operation '{}' is blocked by the security policy ({reason}). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                        request.tool_name
                    ),
                },
                AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
                    BeforeToolCallDecision::NeedConfirm { receipt }
                }
            }
        })
    }

    async fn on_pause(
        &self,
        _engine: &ReActEngine,
        ctx: &StepCtx,
        reason: PauseReason,
    ) -> Option<MemoryTriggerPayload> {
        let pause_reason = match reason {
            PauseReason::TurnEnd => "turn_end",
            PauseReason::Ask => "ask",
            PauseReason::Confirm => "confirm",
            PauseReason::Budget => "budget",
            PauseReason::External => "external",
        };
        Some(MemoryTriggerPayload::pause(
            ctx.run_id,
            ctx.step_num,
            pause_reason,
        ))
    }
}

pub(crate) fn default_hooks() -> super::hooks::LoopHooksHandle {
    std::sync::Arc::new(DefaultHooks::new())
}

pub(crate) fn default_hooks_with_patch(
    memory_patch: MemoryPatchHandle,
) -> super::hooks::LoopHooksHandle {
    std::sync::Arc::new(DefaultHooks::new().with_memory_patch(memory_patch))
}
