//! Assemble the complete provider tool surface and settle turn context before
//! request projection. This runs after context injection and before the sole
//! late steering poll.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::sidecars::PreparedToolDefinitions;
use super::{ReActEngine, ReActState, StepCtx};

/// Mid-run MEMORY fence refresh (M2): dirty flag lives on [`crate::MemoryWorker`];
/// patch uses [`crate::SystemPromptBuilder::patch_canonical_memory_fence`] only
/// (resume uses full rebuild — X2; do not widen this to tools/skills).
pub(crate) struct MemoryPatchHandle {
    pub(crate) memory_worker: Arc<crate::MemoryWorker>,
    pub(crate) prompt_builder: Arc<crate::SystemPromptBuilder>,
}

/// A complete immutable tool surface plus the optional interval trigger
/// produced while preparing this turn. Tool catalog, definitions, and schema
/// cost are kept together so a partial preparation cannot reach request setup.
pub(crate) struct PreparedTurn {
    pub(super) tool_catalog: Arc<haven_tools::ToolCatalogSnapshot>,
    pub(super) tools: PreparedToolDefinitions,
    pub(super) memory_trigger: Option<crate::memory_trigger::MemoryTriggerPayload>,
}

impl PreparedTurn {
    pub(crate) fn from_catalog(
        engine: &ReActEngine,
        session_id: &str,
        tool_catalog: Arc<haven_tools::ToolCatalogSnapshot>,
        memory_trigger: Option<crate::memory_trigger::MemoryTriggerPayload>,
    ) -> Self {
        let tools = engine.prepare_tool_definitions(session_id, &tool_catalog);
        Self {
            tool_catalog,
            tools,
            memory_trigger,
        }
    }
}

#[async_trait]
pub(crate) trait TurnPreparation: Send + Sync {
    async fn prepare(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        cancel: CancellationToken,
    ) -> anyhow::Result<PreparedTurn>;
}

pub(crate) type TurnPreparationHandle = Arc<dyn TurnPreparation>;

pub(crate) struct DefaultTurnPreparation {
    memory_patch: Option<MemoryPatchHandle>,
}

impl DefaultTurnPreparation {
    pub(crate) fn new() -> Self {
        Self { memory_patch: None }
    }

    pub(crate) fn with_memory_patch(mut self, handle: MemoryPatchHandle) -> Self {
        self.memory_patch = Some(handle);
        self
    }
}

#[async_trait]
impl TurnPreparation for DefaultTurnPreparation {
    async fn prepare(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        cancel: CancellationToken,
    ) -> anyhow::Result<PreparedTurn> {
        // Refresh MEMORY before compaction so the budget decision sees the
        // exact canonical prompt that will be sent to the provider.
        if let Some(ref patch) = self.memory_patch
            && patch
                .memory_worker
                .take_memory_dirty_throttled(&ctx.session_id)
        {
            let description = match engine.executor.get_runtime_session(&ctx.session_id).await {
                Some(session) if !session.summary.is_empty() => session.summary,
                Some(session) => session.input,
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
        let tool_catalog = engine.build_tool_catalog_for_session(&ctx.session_id).await;
        let mut prepared = PreparedTurn::from_catalog(engine, &ctx.session_id, tool_catalog, None);

        engine
            .maybe_compact(
                ctx,
                state,
                media_requirements,
                prepared.tools.token_estimate,
                cancel,
            )
            .instrument(tracing::info_span!(
                "compact",
                session_id = %ctx.session_id,
                step_number = ctx.step_number
            ))
            .await?;

        prepared.memory_trigger = interval_memory_trigger(
            ctx.run_id,
            ctx.step_number,
            engine.limits().fact_infer_interval_steps,
        );
        Ok(prepared)
    }
}

pub(crate) fn default_turn_preparation() -> TurnPreparationHandle {
    Arc::new(DefaultTurnPreparation::new())
}

pub(crate) fn default_turn_preparation_with_patch(
    memory_patch: MemoryPatchHandle,
) -> TurnPreparationHandle {
    Arc::new(DefaultTurnPreparation::new().with_memory_patch(memory_patch))
}

fn interval_memory_trigger(
    run_id: u64,
    step_number: u32,
    interval: u32,
) -> Option<crate::memory_trigger::MemoryTriggerPayload> {
    (step_number > 0 && interval > 0 && step_number.is_multiple_of(interval))
        .then(|| crate::memory_trigger::MemoryTriggerPayload::step_interval(run_id, step_number))
}

#[cfg(test)]
pub(crate) struct NoopTurnPreparation;

#[cfg(test)]
#[async_trait]
impl TurnPreparation for NoopTurnPreparation {
    async fn prepare(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        _state: &mut ReActState,
        _cancel: CancellationToken,
    ) -> anyhow::Result<PreparedTurn> {
        let catalog = engine.build_tool_catalog_for_session(&ctx.session_id).await;
        Ok(PreparedTurn::from_catalog(
            engine,
            &ctx.session_id,
            catalog,
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_trigger_is_created_only_for_positive_matching_steps() {
        assert_eq!(interval_memory_trigger(7, 0, 5), None);
        assert_eq!(interval_memory_trigger(7, 25, 0), None);
        assert_eq!(interval_memory_trigger(7, 24, 5), None);
        assert_eq!(
            interval_memory_trigger(7, 25, 5),
            Some(crate::memory_trigger::MemoryTriggerPayload::step_interval(
                7, 25
            ))
        );
    }
}
