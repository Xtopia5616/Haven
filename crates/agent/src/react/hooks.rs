//! Loop extension hooks (Phase 3 / G1–G2; Phase 5 / G3 after_llm + E3 before_tool).
//!
//! Order contract (documented at the call site in `loop.rs`):
//! `inject_pending_context` → `hooks.before_step` → `sanitize_canonical` → LLM
//! → `hooks.after_llm` (response policy) → tools (`before_tool` per call) / pause.
//!
//! Default hooks own prologue side effects (inbox / compact / interval infer),
//! empty/cut-off classification, confirm pre-check, and pause-time infer.
//! Tests use [`NoopHooks`] so the thin loop can run without messaging or
//! SQLite maintenance.

use std::sync::Arc;

use async_trait::async_trait;
use haven_common::types::CanonicalMessage;
use haven_llm::LlmResponse;
use serde_json::Value;

use haven_common::types::RiskLevel;
use haven_tools::ConfirmationResult;

use super::retries::{AfterLlmAction, ResponsePolicy, ResponsePolicyState};
use super::{Action, PauseReason, ReActEngine, StepCtx, canonical_has_image};

/// Pre-tool gate decision (Phase 5 / E3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BeforeToolAction {
    /// Run the tool now (auto-approved or already confirmed).
    Proceed { confirmed: Option<bool> },
    /// Safety policy blocked the tool — emit a failed observation, do not run.
    Block { error: String },
    /// Needs user confirmation — pause the session (like ask) before running.
    NeedConfirm { risk_level: RiskLevel },
}

/// Extension seam for ReAct domain side effects. Production uses
/// [`DefaultHooks`]; unit tests can install [`NoopHooks`].
#[async_trait]
pub(crate) trait LoopHooks: Send + Sync {
    /// Prologue side effects after inject, before sanitize.
    /// `infer(false)` is time-throttled interval extraction.
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        infer: &(dyn Fn(bool) + Send + Sync),
    );

    /// Classify the parsed LLM response (Phase 5 / G3). Default accepts.
    async fn after_llm(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
        canonical: &[CanonicalMessage],
        state: ResponsePolicyState,
    ) -> AfterLlmAction {
        let _ = (thought, actions, response, canonical, state);
        AfterLlmAction::Accept
    }

    /// Pre-tool safety gate (Phase 5 / E3). Default always proceeds.
    async fn before_tool(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _tool_name: &str,
        _input: &Value,
    ) -> BeforeToolAction {
        BeforeToolAction::Proceed { confirmed: None }
    }

    /// Called after status is set to a pause flavor. Default: no-op.
    /// `infer(true)` bypasses the extraction throttle (fresher transcript).
    async fn on_pause(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _reason: PauseReason,
        _infer: &(dyn Fn(bool) + Send + Sync),
    ) {
    }
}

/// Production hooks: inbox poll, context compaction, interval + pause infer,
/// response policy, and confirm pre-check.
pub(crate) struct DefaultHooks;

#[async_trait]
impl LoopHooks for DefaultHooks {
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        infer: &(dyn Fn(bool) + Send + Sync),
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
            infer(false);
        }
    }

    async fn after_llm(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
        canonical: &[CanonicalMessage],
        state: ResponsePolicyState,
    ) -> AfterLlmAction {
        ResponsePolicy::classify(thought, actions, response, canonical, state)
    }

    async fn before_tool(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        tool_name: &str,
        input: &Value,
    ) -> BeforeToolAction {
        // Resume path: a prior confirm pause already recorded a decision.
        if let Some(decision) = engine
            .executor
            .confirm_decision_for(&ctx.session_id, tool_name, input)
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
            ConfirmationResult::Blocked => BeforeToolAction::Block {
                error: format!(
                    "operation '{}' is blocked by the security policy. Do NOT retry it — ask the user what to do instead or choose a different approach.",
                    tool_name
                ),
            },
            ConfirmationResult::RequiresConfirmation { risk_level, .. } => {
                BeforeToolAction::NeedConfirm { risk_level }
            }
        }
    }

    async fn on_pause(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _reason: PauseReason,
        infer: &(dyn Fn(bool) + Send + Sync),
    ) {
        infer(true);
    }
}

/// No-op hooks for thin-loop tests: never touch inbox / compact / infer /
/// response policy / confirm gate.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct NoopHooks;

#[async_trait]
impl LoopHooks for NoopHooks {
    async fn before_step(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _canonical: &mut Vec<CanonicalMessage>,
        _infer: &(dyn Fn(bool) + Send + Sync),
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

    #[tokio::test]
    async fn with_hooks_noop_skips_infer_on_before_step_and_on_pause() {
        use crate::event::AgentEventEmitter;
        use crate::session::SessionExecutor;
        use async_trait::async_trait;
        use haven_llm::client::LlmClient;
        use haven_llm::router::LlmRouter;
        use haven_llm::types::{LlmError, LlmResponse, StreamChunk, ToolDefinition};
        use haven_memory::Database;
        use haven_tools::ToolsManager;
        use std::pin::Pin;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct SilentLlm;
        #[async_trait]
        impl LlmClient for SilentLlm {
            async fn chat(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<LlmResponse, LlmError> {
                Err(LlmError::Unknown("silent".into()))
            }
            async fn chat_with_tools(
                &self,
                _: Vec<CanonicalMessage>,
                _: Vec<ToolDefinition>,
            ) -> Result<LlmResponse, LlmError> {
                Err(LlmError::Unknown("silent".into()))
            }
            async fn chat_stream(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                Err(LlmError::Unknown("silent".into()))
            }
            async fn chat_stream_with_tools(
                &self,
                _: Vec<CanonicalMessage>,
                _: Vec<ToolDefinition>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                Err(LlmError::Unknown("silent".into()))
            }
            async fn health_check(&self) -> Result<(), LlmError> {
                Ok(())
            }
        }

        struct SilentEmitter;
        #[async_trait]
        impl AgentEventEmitter for SilentEmitter {
            async fn emit(&self, _: crate::event::AgentEvent) {}
        }

        let mut p = std::env::temp_dir();
        p.push(format!("haven_noop_hooks_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(SilentLlm) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let limits = haven_common::config::ContextLimitsConfig::default();
        let engine =
            ReActEngine::new(router.clone(), executor.clone(), db.clone(), 10, limits.clone())
                .with_hooks(Arc::new(NoopHooks));

        let calls = AtomicUsize::new(0);
        let infer = |_: bool| {
            calls.fetch_add(1, Ordering::SeqCst);
        };
        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(SilentEmitter);
        let ctx = StepCtx {
            session_id: "ses-test".into(),
            step_num: 25,
            run_id: 1,
            emitter: emitter.clone(),
        };
        let mut canonical = Vec::new();
        engine
            .hooks
            .before_step(&engine, &ctx, &mut canonical, &infer)
            .await;
        engine
            .hooks
            .on_pause(&engine, &ctx, PauseReason::TurnEnd, &infer)
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "NoopHooks must not invoke infer (G1 acceptance)"
        );

        // DefaultHooks::on_pause must invoke infer(true).
        let default_engine = ReActEngine::new(router, executor, db, 10, limits);
        default_engine
            .hooks
            .on_pause(&default_engine, &ctx, PauseReason::TurnEnd, &infer)
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "DefaultHooks::on_pause must invoke infer"
        );
    }

    #[tokio::test]
    async fn default_after_llm_uses_response_policy() {
        use haven_llm::types::FinishReason;

        let response = LlmResponse {
            text: "让我先查一下，".into(),
            tool_calls: Vec::new(),
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::types::Usage::default(),
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let state = ResponsePolicyState {
            empty_retries_remaining: 0,
            empty_retry_delay_ms: 0,
            cut_off_retries_used: 0,
            cut_off_retries_max: 2,
            pending_ask: false,
        };
        let hooks = DefaultHooks;
        // after_llm does not need a real engine for classification.
        let action = {
            // Build a minimal engine only to satisfy the trait signature.
            use crate::event::AgentEventEmitter;
            use crate::session::SessionExecutor;
            use async_trait::async_trait;
            use haven_llm::client::LlmClient;
            use haven_llm::router::LlmRouter;
            use haven_llm::types::{LlmError, StreamChunk, ToolDefinition};
            use haven_memory::Database;
            use haven_tools::ToolsManager;
            use std::pin::Pin;

            struct SilentLlm;
            #[async_trait]
            impl LlmClient for SilentLlm {
                async fn chat(
                    &self,
                    _: Vec<CanonicalMessage>,
                ) -> Result<LlmResponse, LlmError> {
                    Err(LlmError::Unknown("silent".into()))
                }
                async fn chat_with_tools(
                    &self,
                    _: Vec<CanonicalMessage>,
                    _: Vec<ToolDefinition>,
                ) -> Result<LlmResponse, LlmError> {
                    Err(LlmError::Unknown("silent".into()))
                }
                async fn chat_stream(
                    &self,
                    _: Vec<CanonicalMessage>,
                ) -> Result<
                    Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                    LlmError,
                > {
                    Err(LlmError::Unknown("silent".into()))
                }
                async fn chat_stream_with_tools(
                    &self,
                    _: Vec<CanonicalMessage>,
                    _: Vec<ToolDefinition>,
                ) -> Result<
                    Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                    LlmError,
                > {
                    Err(LlmError::Unknown("silent".into()))
                }
                async fn health_check(&self) -> Result<(), LlmError> {
                    Ok(())
                }
            }
            struct SilentEmitter;
            #[async_trait]
            impl AgentEventEmitter for SilentEmitter {
                async fn emit(&self, _: crate::event::AgentEvent) {}
            }

            let mut p = std::env::temp_dir();
            p.push(format!("haven_after_llm_{}.db", uuid::Uuid::new_v4()));
            let db = Arc::new(Database::open(&p).unwrap());
            let tools = Arc::new(ToolsManager::new());
            let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
            let client = Arc::new(SilentLlm) as Arc<dyn LlmClient>;
            let router = Arc::new(LlmRouter::new_with_clients(
                client.clone(),
                client.clone(),
                client.clone(),
                client.clone(),
                client,
            ));
            let limits = haven_common::config::ContextLimitsConfig::default();
            let engine = ReActEngine::new(router, executor, db, 10, limits);
            let emitter: Arc<dyn AgentEventEmitter> = Arc::new(SilentEmitter);
            let ctx = StepCtx {
                session_id: "ses-test".into(),
                step_num: 1,
                run_id: 1,
                emitter,
            };
            hooks
                .after_llm(
                    &engine,
                    &ctx,
                    &Some("让我先查一下，".into()),
                    &[],
                    &response,
                    &[],
                    state,
                )
                .await
        };
        assert!(matches!(action, AfterLlmAction::RetryCutOff { .. }));
    }
}
