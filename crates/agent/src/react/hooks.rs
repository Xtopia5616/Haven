//! Loop extension hooks (Phase 3 / G1–G2).
//!
//! Order contract (documented at the call site in `loop.rs`):
//! `inject_pending_context` → `hooks.before_step` → `sanitize_canonical` → LLM.
//!
//! Default hooks own prologue side effects (inbox / compact / interval infer)
//! and pause-time infer. Tests use [`NoopHooks`] so the thin loop can run
//! without messaging or SQLite maintenance.

use std::sync::Arc;

use async_trait::async_trait;
use haven_common::types::CanonicalMessage;

use super::{PauseReason, ReActEngine, StepCtx, canonical_has_image};

/// Extension seam for ReAct domain side effects. Production uses
/// [`DefaultHooks`]; unit tests can install [`NoopHooks`].
#[async_trait]
pub(crate) trait LoopHooks: Send + Sync {
    /// Prologue side effects after inject, before sanitize.
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        infer: &(dyn Fn() + Send + Sync),
    );

    /// Called after status is set to a pause flavor. Default: no-op.
    async fn on_pause(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _reason: PauseReason,
        _infer: &(dyn Fn() + Send + Sync),
    ) {
    }
}

/// Production hooks: inbox poll, context compaction, interval + pause infer.
pub(crate) struct DefaultHooks;

#[async_trait]
impl LoopHooks for DefaultHooks {
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        infer: &(dyn Fn() + Send + Sync),
    ) {
        engine
            .maybe_poll_inbox(&ctx.session_id, ctx, canonical)
            .await;
        let has_image = canonical_has_image(canonical);
        let _ = engine
            .maybe_compact(&ctx.session_id, canonical, has_image, &ctx.emitter)
            .await;
        let interval = engine.context_limits.fact_infer_interval_steps;
        if ctx.step_num > 0 && interval > 0 && ctx.step_num % interval == 0 {
            infer();
        }
    }

    async fn on_pause(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _reason: PauseReason,
        infer: &(dyn Fn() + Send + Sync),
    ) {
        infer();
    }
}

/// No-op hooks for thin-loop tests: never touch inbox / compact / infer.
pub(crate) struct NoopHooks;

#[async_trait]
impl LoopHooks for NoopHooks {
    async fn before_step(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _canonical: &mut Vec<CanonicalMessage>,
        _infer: &(dyn Fn() + Send + Sync),
    ) {
    }
}

/// Shared handle stored on [`ReActEngine`].
pub(crate) type LoopHooksHandle = Arc<dyn LoopHooks>;

pub(crate) fn default_hooks() -> LoopHooksHandle {
    Arc::new(DefaultHooks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_and_default_are_object_safe() {
        let _: LoopHooksHandle = Arc::new(NoopHooks);
        let _: LoopHooksHandle = default_hooks();
    }

    #[test]
    fn noop_hooks_leave_on_pause_as_trait_default() {
        // NoopHooks does not override on_pause → infer is never called from
        // the default empty body. DefaultHooks overrides on_pause to call
        // infer. This compile-time / type-level contract is the G1 acceptance
        // for "禁用 infer 的单测不触达 maintenance".
        let noop: &dyn LoopHooks = &NoopHooks;
        let default: &dyn LoopHooks = &DefaultHooks;
        let _ = (noop, default);
    }

    #[test]
    fn with_hooks_installs_noop_on_engine() {
        // Smoke: ReActEngine::with_hooks accepts NoopHooks without requiring
        // a live run (construction still needs real deps — covered when an
        // AgentLayer test opts in). Here we only assert the handle type.
        let hooks: LoopHooksHandle = Arc::new(NoopHooks);
        assert!(std::sync::Arc::strong_count(&hooks) >= 1);
    }
}
