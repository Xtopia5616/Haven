//! Loop extension hooks (Phase 3 / G1–G2; Phase 5 / G3 after_llm + E3 before_tool;
//! Phase 7 / G6 infer owned by hooks).
//!
//! Order contract (documented at the call site in `loop.rs`):
//! `inject_pending_context` → `hooks.before_step` → `RequestContext` → LLM
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
use tokio_util::sync::CancellationToken;

use haven_common::types::RiskLevel;

#[cfg(test)]
pub(crate) use super::hook_policy::{DefaultHooks, default_hooks_with_infer};
pub(crate) use super::hook_policy::{default_hooks, default_hooks_with_infer_and_patch};
use super::retries::{AfterLlmAction, ResponsePolicyState};
use super::{Action, PauseReason, ReActEngine, ReActState, StepCtx};

/// Fact-inference callback: `(session_id, bypass_throttle)`.
/// `bypass_throttle=true` for pause-path infer so interval extract cannot starve
/// the fresher post-pause pass. Installed once on [`DefaultHooks`]; the thin
/// loop never threads this (Phase 7 / G6).
pub(crate) type InferCallback = Arc<dyn Fn(&str, bool) + Send + Sync>;

/// Mid-run MEMORY fence refresh (M2): dirty flag lives on [`crate::InferenceEngine`];
/// patch uses [`crate::SystemPromptBuilder::patch_canonical_memory_fence`] only
/// (resume uses full rebuild — X2; do not widen this to tools/skills).
pub(crate) struct MemoryPatchHandle {
    pub inference: Arc<crate::InferenceEngine>,
    pub prompt_builder: Arc<crate::SystemPromptBuilder>,
}

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

/// Stable identity of one tool call within a ReAct step. Gate decisions must
/// use this identity; tool names and JSON arguments are not unique.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolCallIdentity<'a> {
    pub step_id: &'a str,
    pub action_index: u32,
    pub tool_call_id: Option<&'a str>,
}

/// Inputs needed to classify a completed LLM response. Grouping these
/// immutable step values keeps the hook boundary explicit as it evolves.
pub(crate) struct AfterLlmInput<'a> {
    pub thought: &'a Option<String>,
    pub actions: &'a [Action],
    pub response: &'a LlmResponse,
    pub canonical: &'a [CanonicalMessage],
    pub state: ResponsePolicyState,
}

/// Extension seam for ReAct domain side effects. Production uses
/// [`DefaultHooks`]; unit tests can install [`NoopHooks`].
#[async_trait]
pub(crate) trait LoopHooks: Send + Sync {
    /// Prologue side effects after inject, before sanitize.
    /// Interval infer (`infer(session, false)`) is time-throttled extraction.
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        cancel: CancellationToken,
    );

    /// Classify the parsed LLM response (Phase 5 / G3). Default accepts.
    async fn after_llm(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        input: AfterLlmInput<'_>,
    ) -> AfterLlmAction {
        let _ = input;
        AfterLlmAction::Accept
    }

    /// Pre-tool safety gate (Phase 5 / E3). Default always proceeds.
    async fn before_tool(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _identity: ToolCallIdentity<'_>,
        _tool_name: &str,
        _input: &Value,
    ) -> BeforeToolAction {
        BeforeToolAction::Proceed { confirmed: None }
    }

    /// Called after status is set to a pause flavor. Default: no-op.
    /// Pause infer (`infer(session, true)`) bypasses the extraction throttle.
    async fn on_pause(&self, _engine: &ReActEngine, _ctx: &StepCtx, _reason: PauseReason) {}
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
        _state: &mut ReActState,
        _cancel: CancellationToken,
    ) {
    }
}

/// Shared handle stored on [`ReActEngine`].
pub(crate) type LoopHooksHandle = Arc<dyn LoopHooks>;

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
        let default: &dyn LoopHooks = &DefaultHooks::new(None);
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
            async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
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
            client,
        ));
        let limits = haven_common::config::ContextLimitsConfig::default();
        let engine = ReActEngine::new(
            router.clone(),
            executor.clone(),
            db.clone(),
            10,
            limits.clone(),
        )
        .with_hooks(Arc::new(NoopHooks));

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_infer = calls.clone();
        let infer: InferCallback = Arc::new(move |_: &str, _: bool| {
            calls_infer.fetch_add(1, Ordering::SeqCst);
        });
        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(SilentEmitter);
        let ctx = StepCtx {
            session_id: "ses-test".into(),
            step_num: 25,
            run_id: 1,
            emitter: emitter.clone(),
        };
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .hooks
            .before_step(&engine, &ctx, &mut state, CancellationToken::new())
            .await;
        engine
            .hooks
            .on_pause(&engine, &ctx, PauseReason::TurnEnd)
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "NoopHooks must not invoke infer (G1 acceptance)"
        );

        // DefaultHooks::on_pause must invoke infer(session, true) when wired.
        let default_engine = ReActEngine::new(router, executor, db, 10, limits)
            .with_hooks(default_hooks_with_infer(infer));
        default_engine
            .hooks
            .on_pause(&default_engine, &ctx, PauseReason::TurnEnd)
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
        let hooks = DefaultHooks::new(None);
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
                async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
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
                    AfterLlmInput {
                        thought: &Some("让我先查一下，".into()),
                        actions: &[],
                        response: &response,
                        canonical: &[],
                        state,
                    },
                )
                .await
        };
        assert!(matches!(action, AfterLlmAction::RetryCutOff { .. }));
    }
}
