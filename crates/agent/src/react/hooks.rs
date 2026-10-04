//! Loop extension hooks (Phase 3 / G1–G2; Phase 5 / G3 after_llm + E3 before_tool;
//! Phase 7 / G6 infer owned by hooks).
//!
//! Order contract (documented at the call site in `loop.rs`):
//! `inject_pending_context` → `hooks.before_step` → `RequestContext` → LLM
//! → `hooks.after_llm` (response policy) → tools (`before_tool` per call) / pause.
//!
//! Default hooks own prologue side effects (inbox / compact / interval intent),
//! incomplete tool-call classification, confirm pre-check, and pause-time intent.
//! Tests use [`NoopHooks`] so the thin loop can run without messaging or
//! SQLite maintenance.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use haven_llm::{LlmResponse, ToolDefinition};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
pub(crate) use super::hook_policy::DefaultHooks;
pub(crate) use super::hook_policy::{default_hooks, default_hooks_with_patch};
use super::retries::{AfterLlmAction, ResponsePolicyState};
use super::{Action, PauseReason, ReActEngine, ReActState, StepCtx};

/// Mid-run MEMORY fence refresh (M2): dirty flag lives on [`crate::MemoryWorker`];
/// patch uses [`crate::SystemPromptBuilder::patch_canonical_memory_fence`] only
/// (resume uses full rebuild — X2; do not widen this to tools/skills).
pub(crate) struct MemoryPatchHandle {
    pub memory_worker: Arc<crate::MemoryWorker>,
    pub prompt_builder: Arc<crate::SystemPromptBuilder>,
}

/// Pre-tool gate decision (Phase 5 / E3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BeforeToolAction {
    /// Run the tool now (auto-approved or already confirmed).
    Proceed {
        receipt: Option<haven_tools::ConfirmationReceipt>,
    },
    /// Safety policy blocked the tool — emit a failed observation, do not run.
    Block { error: String },
    /// Needs user confirmation — pause the session (like ask) before running.
    NeedConfirm {
        receipt: haven_tools::ConfirmationReceipt,
    },
}

/// Stable identity of one tool call within a ReAct step. Gate decisions must
/// use this identity; tool names and JSON arguments are not unique.
#[derive(Debug, Clone)]
pub(crate) struct ToolCallIdentity {
    pub step_id: String,
    pub action_index: u32,
    pub tool_call_id: Option<String>,
}

/// Owned inputs let pre-tool gates run concurrently without retaining borrows
/// across async trait calls.
pub(crate) struct BeforeToolRequest {
    pub executor: Arc<crate::session::SessionSupervisor>,
    pub session_id: String,
    pub catalog: haven_tools::ToolCatalogSnapshot,
    pub identity: ToolCallIdentity,
    pub tool_name: String,
    pub input: Value,
}

/// Inputs needed to classify a completed LLM response. Grouping these
/// immutable step values keeps the hook boundary explicit as it evolves.
pub(crate) struct AfterLlmInput<'a> {
    pub thought: &'a Option<String>,
    pub actions: &'a [Action],
    pub response: &'a LlmResponse,
    pub state: ResponsePolicyState,
}

/// Values prepared by the prologue for the rest of the turn.
///
/// Production hooks already resolve the session tool surface before deciding
/// whether compaction is needed. Returning that immutable `Arc` avoids a
/// second catalog-version lookup on the hot path; test hooks may leave it
/// empty and let the turn build the surface itself.
#[derive(Default)]
pub(crate) struct BeforeStepOutput {
    pub(crate) tool_definitions: Option<Arc<Vec<ToolDefinition>>>,
    pub(crate) tool_token_estimate: Option<u32>,
    pub(crate) tool_catalog: Option<Arc<haven_tools::ToolCatalogSnapshot>>,
    pub(crate) memory_trigger: Option<crate::memory_trigger::MemoryTriggerPayload>,
}

/// Extension seam for ReAct domain side effects. Production uses
/// [`DefaultHooks`]; unit tests can install [`NoopHooks`].
#[async_trait]
pub(crate) trait LoopHooks: Send + Sync {
    /// Prologue side effects after inject, before sanitize.
    /// Returns any interval memory-trigger intent for the turn boundary to
    /// append after the hook succeeds.
    async fn before_step(
        &self,
        engine: &ReActEngine,
        ctx: &StepCtx,
        state: &mut ReActState,
        cancel: CancellationToken,
    ) -> anyhow::Result<BeforeStepOutput>;

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
    fn before_tool(&self, _request: BeforeToolRequest) -> BoxFuture<'static, BeforeToolAction> {
        Box::pin(async { BeforeToolAction::Proceed { receipt: None } })
    }

    /// Called after status is set to a pause flavor. The returned intent is
    /// appended only after the caller has verified the durable event boundary.
    async fn on_pause(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _reason: PauseReason,
    ) -> Option<crate::memory_trigger::MemoryTriggerPayload> {
        None
    }
}

/// No-op hooks for thin-loop tests: never touch inbox / compact / infer /
/// response policy / confirm gate.
#[cfg(test)]
pub(crate) struct NoopHooks;

#[cfg(test)]
#[async_trait]
impl LoopHooks for NoopHooks {
    async fn before_step(
        &self,
        _engine: &ReActEngine,
        _ctx: &StepCtx,
        _state: &mut ReActState,
        _cancel: CancellationToken,
    ) -> anyhow::Result<BeforeStepOutput> {
        Ok(BeforeStepOutput::default())
    }
}

/// Shared handle stored on [`ReActEngine`].
pub(crate) type LoopHooksHandle = Arc<dyn LoopHooks>;

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::CanonicalMessage;

    #[test]
    fn noop_and_default_are_object_safe() {
        let _: LoopHooksHandle = Arc::new(NoopHooks);
        let _: LoopHooksHandle = default_hooks();
    }

    #[test]
    fn noop_hooks_leave_on_pause_as_trait_default() {
        // NoopHooks does not override on_pause, so it never produces a
        // memory-trigger intent. This keeps thin-loop tests free of memory
        // maintenance side effects.
        let noop: &dyn LoopHooks = &NoopHooks;
        let default: &dyn LoopHooks = &DefaultHooks::new();
        let _ = (noop, default);
    }

    #[tokio::test]
    async fn with_hooks_noop_skips_memory_trigger_on_before_step_and_on_pause() {
        use crate::event::AgentEventEmitter;
        use crate::session::SessionSupervisor;
        use async_trait::async_trait;
        use haven_llm::client::LlmClient;
        use haven_llm::router::LlmRouter;
        use haven_llm::types::{LlmError, LlmResponse, StreamChunk, ToolDefinition};
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
            async fn chat_stream_with_tools_output_cap_shared(
                &self,
                _messages: std::sync::Arc<[CanonicalMessage]>,
                _tools: std::sync::Arc<[ToolDefinition]>,
                _max_output_tokens: Option<u32>,
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
        let executor = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 1));
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
            crate::react::test_tool_catalog_port(&executor),
            executor.clone(),
            haven_memory::MemoryStore::new(db.clone()),
            10,
            limits.clone(),
        )
        .with_hooks(Arc::new(NoopHooks));

        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(SilentEmitter);
        let ctx = StepCtx {
            session_id: "ses-test".into(),
            step_num: 25,
            run_id: 1,
            emitter: emitter.clone(),
        };
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let before_step = engine
            .hooks
            .before_step(&engine, &ctx, &mut state, CancellationToken::new())
            .await
            .unwrap();
        assert!(before_step.memory_trigger.is_none());
        assert!(
            engine
                .hooks
                .on_pause(&engine, &ctx, PauseReason::TurnEnd)
                .await
                .is_none()
        );

        // DefaultHooks emits a typed pause intent; the boundary owns durable
        // persistence, so the hook itself has no worker callback to invoke.
        let default_engine = ReActEngine::new(
            router,
            crate::react::test_tool_catalog_port(&executor),
            executor,
            haven_memory::MemoryStore::new(db.clone()),
            10,
            limits,
        )
        .with_hooks(default_hooks());
        let mut default_state =
            ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let before_step = default_engine
            .hooks
            .before_step(
                &default_engine,
                &ctx,
                &mut default_state,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            before_step.memory_trigger,
            Some(crate::memory_trigger::MemoryTriggerPayload::step_interval(
                1, 25
            )),
            "interval extraction should be represented as a typed intent"
        );
        let pause_trigger = default_engine
            .hooks
            .on_pause(&default_engine, &ctx, PauseReason::TurnEnd)
            .await
            .expect("default hooks should produce a pause trigger");
        assert_eq!(
            pause_trigger,
            crate::memory_trigger::MemoryTriggerPayload::pause(1, 25, "turn_end")
        );

        for (reason, wire_reason) in [
            (PauseReason::Ask, "ask"),
            (PauseReason::Confirm, "confirm"),
            (PauseReason::Budget, "budget"),
            (PauseReason::External, "external"),
        ] {
            let trigger = default_engine
                .hooks
                .on_pause(&default_engine, &ctx, reason)
                .await
                .expect("default hooks should produce every pause trigger");
            assert_eq!(
                trigger,
                crate::memory_trigger::MemoryTriggerPayload::pause(1, 25, wire_reason)
            );
        }
    }

    #[tokio::test]
    async fn default_after_llm_uses_response_policy() {
        use haven_llm::types::FinishReason;

        let response = LlmResponse {
            text: "partial text".into(),
            tool_calls: Vec::new(),
            finish_reason: Some(FinishReason::Length),
            usage: haven_llm::types::Usage::default(),
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let state = ResponsePolicyState {
            incomplete_tool_args_retries_used: 0,
            incomplete_tool_args_retries_max: 2,
            pending_ask: false,
        };
        let hooks = DefaultHooks::new();
        // after_llm does not need a real engine for classification.
        let action = {
            // Build a minimal engine only to satisfy the trait signature.
            use crate::event::AgentEventEmitter;
            use crate::session::SessionSupervisor;
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
                async fn chat_stream_with_tools_output_cap_shared(
                    &self,
                    _messages: std::sync::Arc<[CanonicalMessage]>,
                    _tools: std::sync::Arc<[ToolDefinition]>,
                    _max_output_tokens: Option<u32>,
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
            let executor = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 1));
            let client = Arc::new(SilentLlm) as Arc<dyn LlmClient>;
            let router = Arc::new(LlmRouter::new_with_clients(
                client.clone(),
                client.clone(),
                client.clone(),
                client,
            ));
            let limits = haven_common::config::ContextLimitsConfig::default();
            let engine = ReActEngine::new(
                router,
                crate::react::test_tool_catalog_port(&executor),
                executor,
                haven_memory::MemoryStore::new(db.clone()),
                10,
                limits,
            );
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
                        state,
                    },
                )
                .await
        };
        assert!(matches!(action, AfterLlmAction::Fail { .. }));
    }
}
