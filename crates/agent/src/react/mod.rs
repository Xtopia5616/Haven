use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::session::{SessionStatus, SessionSupervisor};
use haven_common::config::{ContextLimitsConfig, RequestKind};
use haven_common::media::{
    CapabilityProfile, CapabilitySupport, MediaInput, MediaInputStrategy, MediaPlan,
    MediaProjectionMode, build_media_plan, message_attachment_to_media_input,
};
use haven_common::types::MessageAttachment;
use haven_common::types::{CanonicalMessage, ContentPart};
use haven_common::usage::LlmCallKind;
use haven_llm::{FinishReason, LlmResponse, LlmRouter, LlmToolDefinition};
#[cfg(test)]
use haven_memory::Database;
use haven_memory::{MemoryStore, SessionStore};

use crate::compactor::ContextCompactor;
use crate::event::{AgentEvent, AgentEventEmitter, EventDispatcher, UsagePayload};
#[cfg(test)]
use crate::types::TranscriptRecord;
use crate::types::{ToolCall, media_inputs_from_events};

mod committed_ui;
#[cfg(test)]
mod compaction_summary_tests;
mod context;
mod effects;
mod event_boundary;
mod hook_policy;
mod hooks;
pub(crate) mod identity;
mod inject;
mod r#loop;
mod metrics;
mod request_context;
mod response_cycle;
mod response_policy;
pub(crate) mod sidecars;
mod state;
pub(crate) mod stream_step;
mod tool_batch;
mod tool_batch_execute;
pub(crate) mod tool_batch_plan;
mod tool_batch_policy;
mod tool_ports;
mod transcript;
mod turn;
mod turn_end;
mod usage;

use context::ContextSource;
pub(crate) use context::tool_run_result_message_id;
use hooks::{LoopHooksHandle, default_hooks};
pub(crate) use hooks::{MemoryPatchHandle, default_hooks_with_patch};
pub use r#loop::{LoopExit, PauseReason};
pub(crate) use r#loop::{ReActRunInput, ReActRunReplay};
use metrics::{Counter as MetricsCounter, Phase as MetricsPhase, ReActMetrics};
pub use metrics::{MetricsSnapshot, UiMetricsSnapshot};
pub(crate) use request_context::RequestContext;
use sidecars::{ContextWindowCache, PreparedToolDefinitions, ToolDefCache};
pub(crate) use state::{ReActState, RetryNudge};
pub use tool_ports::ToolCatalogPort;
#[cfg(test)]
pub(crate) use tool_ports::ToolsFacadeToolCatalogAdapter;
use transcript::{ObservationCard, TranscriptEvent};
use usage::{UsageRuntime, UsageUpdate};

fn runtime_setting_lock<'a, T>(
    lock: &'a Mutex<T>,
    setting: &'static str,
) -> anyhow::Result<MutexGuard<'a, T>> {
    lock.lock()
        .map_err(|_| anyhow::anyhow!("{setting} runtime setting lock is poisoned"))
}

pub(crate) use event_boundary::DurableEventState;
pub(crate) use event_boundary::set_status_and_emit;
#[cfg(test)]
use tool_batch_policy::FailureKind;

/// Project an ingress attachment using the current user-selected media policy.
/// Attachments first pass through the common media plan and the LLM
/// projection boundary. Ordinary files deliberately do not expose their
/// persisted absolute path to provider-facing text; managed-file resolution
/// is a later trusted-tool stage.
pub(crate) fn attachment_to_content_part_with_strategy(
    att: &MessageAttachment,
    strategy: MediaInputStrategy,
) -> ContentPart {
    let input = message_attachment_to_media_input(att);
    media_input_to_content_part_with_strategy(&input, strategy)
}

/// Project an already normalized media input. Snapshot resume and live
/// ingress both use this helper, so provider-facing content cannot drift based
/// on which path produced the input.
pub(crate) fn media_input_to_content_part_with_strategy(
    input: &MediaInput,
    strategy: MediaInputStrategy,
) -> ContentPart {
    let capabilities = media_capabilities_for_input(input);
    let plan = build_media_plan(std::slice::from_ref(input), &capabilities, strategy);
    if let Ok(mut parts) = haven_llm::media::project_media_plan(&plan, std::slice::from_ref(input))
        && let Some(part) = parts.pop()
    {
        return part;
    }

    let name = input
        .asset
        .filename
        .as_deref()
        .map(|value| haven_common::text::sanitize_prompt_field(value, 120))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "attachment".into());
    ContentPart::text(format!(
        "[Attachment: {name}; no safe representation is available for this request. The file path is not sent to the model.]"
    ))
}

/// Build the same one-input plan used by canonical projection. Keeping this
/// helper here lets ingress persist a structured plan event without inventing
/// a second capability policy just for the event log.
pub(crate) fn media_plan_for_inputs(
    inputs: &[MediaInput],
    strategy: MediaInputStrategy,
) -> MediaPlan {
    let mut plan = MediaPlan {
        strategy,
        projections: Vec::new(),
        notices: Vec::new(),
    };
    for input in inputs {
        let input_plan = build_media_plan(
            std::slice::from_ref(input),
            &media_capabilities_for_input(input),
            strategy,
        );
        plan.projections.extend(input_plan.projections);
        plan.notices.extend(input_plan.notices);
    }
    plan
}

/// Publish a media plan while a provider request is being prepared.
/// This is a request-time signal for both the initial turn and compaction
/// retries. It is not a deferred turn-end effect and it is not a committed
/// `session_events` row, so `event_seq` stays `None`. The durable ingress
/// plan is published by [`committed_ui::CommittedUiPublisher`].
pub(super) async fn emit_media_plan(
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: &str,
    step_number: u32,
    run_id: u64,
    request: RequestKind,
    plan: MediaPlan,
) {
    if plan.is_empty() && plan.notices.is_empty() {
        return;
    }
    if plan.notices.is_empty()
        && plan
            .projections
            .iter()
            .all(|projection| projection.mode == MediaProjectionMode::Raw)
    {
        tracing::debug!(
            session_id,
            step_number,
            request = request.as_str(),
            strategy = plan.strategy.as_str(),
            projections = ?plan.projections,
            "media request plan recorded"
        );
    } else if plan.notices.is_empty() {
        tracing::info!(
            session_id,
            step_number,
            request = request.as_str(),
            strategy = plan.strategy.as_str(),
            projections = ?plan.projections,
            "media request selected a non-raw representation"
        );
    } else {
        tracing::warn!(
            session_id,
            step_number,
            request = request.as_str(),
            strategy = plan.strategy.as_str(),
            projections = ?plan.projections,
            notices = ?plan.notices,
            "media request was downgraded to match the selected adapter capability profile"
        );
    }
    emitter
        .emit(AgentEvent::MediaPlan {
            session_id: session_id.to_string(),
            step_number,
            run_id,
            role: request,
            strategy: plan.strategy,
            projections: plan.projections,
            notices: plan.notices,
            event_seq: None,
        })
        .await;
}

fn media_capabilities_for_input(input: &MediaInput) -> CapabilityProfile {
    CapabilityProfile {
        // These are the current canonical inline parts, not a model-name
        // guess. The selected adapter still validates the final request.
        image: CapabilitySupport::Supported,
        audio: CapabilitySupport::Supported,
        video: CapabilitySupport::Supported,
        // A persisted ordinary attachment is addressable through the trusted
        // `files` tool using its opaque asset id. Attachments without a
        // managed reference stay on the safe text path.
        tools: if matches!(
            input.asset.source,
            haven_common::media::MediaAssetSource::UserAttachment
                | haven_common::media::MediaAssetSource::Generated
                | haven_common::media::MediaAssetSource::ToolOutput
        ) && input.representations.iter().any(|representation| {
            matches!(
                &representation.payload,
                haven_common::media::MediaRepresentationPayload::ManagedFileRef { .. }
            )
        }) {
            CapabilitySupport::Supported
        } else {
            CapabilitySupport::Unknown
        },
        ..CapabilityProfile::default()
    }
}

/// Media requirements of one provider request. This is deliberately a small
/// internal policy type: content parts remain provider-neutral, while routing
/// decides which configured request should receive them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MediaRequirements {
    pub(crate) image: bool,
    pub(crate) audio: bool,
    pub(crate) video: bool,
}

/// Derived media metadata cached for one canonical request projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CanonicalMediaSummary {
    pub(crate) requirements: MediaRequirements,
    pub(crate) media_part_count: usize,
}

/// Summarize canonical media in one pass. Run state updates this summary on
/// append and replacement so turn preparation does not rescan long histories.
pub(crate) fn canonical_media_summary(messages: &[CanonicalMessage]) -> CanonicalMediaSummary {
    let mut requirements = MediaRequirements::default();
    let mut media_part_count = 0;
    for part in messages.iter().flat_map(|message| &message.content) {
        match part {
            ContentPart::Image { .. } => {
                requirements.image = true;
                media_part_count += 1;
            }
            ContentPart::Audio { .. } => {
                requirements.audio = true;
                media_part_count += 1;
            }
            ContentPart::Video { .. } => {
                requirements.video = true;
                media_part_count += 1;
            }
            ContentPart::Text(_) => {}
        }
    }
    CanonicalMediaSummary {
        requirements,
        media_part_count,
    }
}

/// Pick the request kind for an agent step. Image content routes through the
/// vision request and audio-only content through the audio request; a
/// configured dedicated request is used only when its capability profile can
/// preserve every raw media part. Otherwise the chat request gets the first
/// opportunity to carry the request.
pub(super) async fn choose_agent_request(
    router: &LlmRouter,
    request_context: &RequestContext,
) -> RequestKind {
    let requirements = request_context.media_requirements();
    let preferred = if requirements.image || requirements.video {
        RequestKind::Vision
    } else if requirements.audio {
        RequestKind::AudioChat
    } else {
        RequestKind::Chat
    };
    if preferred == RequestKind::Chat {
        return preferred;
    }
    if !router.is_request_configured(preferred).await {
        return RequestKind::Chat;
    }
    if request_context.raw_media_fits_profile(&router.capability_profile_for_request(preferred)) {
        return preferred;
    }

    let default_request = RequestKind::Chat;
    if request_context
        .raw_media_fits_profile(&router.capability_profile_for_request(default_request))
    {
        tracing::info!(
            preferred_request = preferred.as_str(),
            fallback_request = default_request.as_str(),
            "dedicated media request cannot represent the request; using chat request"
        );
        return default_request;
    }

    preferred
}

/// A tool input that cannot be executed without changing the model's
/// intended semantics. Invalid input is reported as a failed tool result;
/// the agent never invents an enum member, default-like placeholder, or
/// side-effecting discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolInputValidationFailure {
    pub tool_index: u32,
    pub tool_name: String,
    pub details: Vec<String>,
}

impl ToolInputValidationFailure {
    pub fn render(&self) -> String {
        format!(
            "tool input validation failed for '{}' (tool_index={}): {}",
            self.tool_name,
            self.tool_index,
            self.details.join("; ")
        )
    }
}

/// Agent-level interpretation of a provider response for one ReAct step.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAgentResponse {
    pub thought: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<SessionSupervisor>,
    /// Agent-owned read boundary for immutable per-session tool catalogs.
    tool_catalog: Arc<dyn ToolCatalogPort>,
    /// Durable episode-summary writes are owned by Memory's narrow store port.
    memory_store: MemoryStore,
    /// Durable transcript/event boundary. All new transcript records are
    /// appended here before entering the in-memory projection; checkpoint
    /// metadata is written through the same store.
    pub(crate) event_store: SessionStore,
    /// Agent-owned usage persistence and cumulative counters. The runtime
    /// preserves per-session ordering without blocking the session actor.
    usage_runtime: UsageRuntime,
    max_steps_per_run: Mutex<u32>,
    /// Optional Session-lifetime step cap (Phase 8 / J1). `None` = unlimited.
    max_steps_per_session: Mutex<Option<u32>>,
    /// Hot-reloaded via [`Self::set_context_limits`] on settings save.
    context_limits: std::sync::Mutex<ContextLimitsConfig>,
    /// Hot-reloaded provider-facing media projection policy.
    media_strategy: Mutex<MediaInputStrategy>,
    run_counter: AtomicU64,
    /// Queue/inbox source adapter; projection remains in `inject`.
    context_source: ContextSource,
    /// Per-request context-window cache keyed by router instance pointer.
    context_windows: ContextWindowCache,
    /// Per-session provider definitions and serialized schema cost, keyed by
    /// the immutable global/session catalog version.
    tool_definitions: ToolDefCache,
    /// Domain side effects (inbox / compact / infer). Thin loop only calls
    /// `hooks.before_step` / `on_pause` (Phase 3 / G1).
    hooks: LoopHooksHandle,
    /// Optional fact engine for compaction-summary extraction (M3).
    memory_worker: Option<Arc<crate::MemoryWorker>>,
    /// Fixed-size, in-process ReAct baseline metrics. Updates are atomic and
    /// deliberately separate from the durable session/event projection.
    metrics: Arc<ReActMetrics>,
    /// Publishes UI cards from committed transcript rows. Shared with the
    /// process-wide store subscription started from `AgentLayer`.
    committed_ui: Arc<committed_ui::CommittedUiPublisher>,
}

/// Per-step context shared by the ReAct-loop helpers (context injection,
/// streaming, error handling). Bundles the four values every helper needs so
/// signatures stay readable instead of threading 4 parameters through each
/// call.
#[derive(Clone)]
pub(super) struct StepCtx {
    pub(super) session_id: String,
    pub(super) step_num: u32,
    pub(super) run_id: u64,
    pub(super) emitter: Arc<dyn AgentEventEmitter>,
}

/// Result of one step's LLM call (including the compaction retry). The loop
/// dispatches on this instead of inlining ~130 lines of error handling.
pub(super) enum StepCallOutcome {
    /// A usable response (possibly from the post-compaction retry).
    Response(Box<LlmResponse>),
    /// Cancelled mid-call (end_session / rollback): exit silently.
    Cancelled,
    /// Persisted/emitted error already; the loop must propagate it.
    Fatal(String),
}

#[cfg(test)]
pub(crate) fn test_tool_catalog_port(executor: &SessionSupervisor) -> Arc<dyn ToolCatalogPort> {
    executor.tool_catalog_for_test()
}

impl ReActEngine {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        router: Arc<LlmRouter>,
        tool_catalog: Arc<dyn ToolCatalogPort>,
        executor: Arc<SessionSupervisor>,
        memory_store: MemoryStore,
        max_steps_per_run: u32,
        context_limits: ContextLimitsConfig,
    ) -> Self {
        let metrics = Arc::new(ReActMetrics::new());
        let event_store = executor.session_store();
        let context_source =
            ContextSource::new(executor.clone(), event_store.clone(), metrics.clone());
        let usage_runtime = UsageRuntime::new(event_store.clone());
        Self {
            router: Arc::new(RwLock::new(router)),
            executor,
            tool_catalog,
            memory_store,
            event_store,
            usage_runtime,
            max_steps_per_run: Mutex::new(max_steps_per_run),
            max_steps_per_session: Mutex::new(None),
            context_limits: std::sync::Mutex::new(context_limits),
            media_strategy: Mutex::new(MediaInputStrategy::Auto),
            run_counter: AtomicU64::new(0),
            context_source,
            context_windows: ContextWindowCache::new(),
            tool_definitions: ToolDefCache::new(),
            hooks: default_hooks(),
            memory_worker: None,
            metrics,
            committed_ui: Arc::new(committed_ui::CommittedUiPublisher::new()),
        }
    }

    /// Subscribe `rx` to publish committed transcript rows as UI events.
    /// The caller must already be inside a Tokio runtime. A second call is
    /// ignored so replacing the emitter does not start a second bridge.
    pub(crate) fn start_committed_ui_bridge(
        &self,
        events: Arc<EventDispatcher>,
        rx: tokio::sync::broadcast::Receiver<haven_memory::SessionEvent>,
    ) {
        if !self.committed_ui.try_mark_started() {
            return;
        }
        let publisher = Arc::clone(&self.committed_ui);
        tokio::spawn(async move {
            publisher.run(rx, events).await;
        });
    }

    /// Return a point-in-time diagnostic snapshot for local diagnostics and
    /// tests. The snapshot is read-only and is not persisted.
    #[allow(dead_code)]
    pub(crate) fn metrics_snapshot(&self) -> metrics::MetricsSnapshot {
        self.metrics.snapshot()
    }

    pub(crate) fn note_tool_run_result_retry(&self) {
        self.metrics.increment(MetricsCounter::ToolRunResultRetries);
    }

    /// Replace loop hooks (production: `default_hooks_with_patch`; tests:
    /// `hooks::NoopHooks` to skip inbox/memory maintenance).
    pub(crate) fn with_hooks(mut self, hooks: LoopHooksHandle) -> Self {
        self.hooks = hooks;
        self
    }

    /// Attach the shared [`crate::MemoryWorker`] for M3 summary→facts.
    pub(crate) fn with_memory_worker(mut self, memory_worker: Arc<crate::MemoryWorker>) -> Self {
        self.memory_worker = Some(memory_worker);
        self
    }

    pub fn replace_router(&self, new_router: Arc<LlmRouter>) -> anyhow::Result<()> {
        *self
            .router
            .write()
            .map_err(|_| anyhow::anyhow!("router runtime setting lock is poisoned"))? = new_router;
        Ok(())
    }

    /// Snapshot of `[context_limits]` (cloned; safe across awaits).
    pub(crate) fn limits(&self) -> ContextLimitsConfig {
        self.context_limits.lock().unwrap().clone()
    }

    /// Hot-reload `[context_limits]` from settings save and drop cached windows
    /// so the next step re-resolves against the new default.
    pub fn set_context_limits(&self, limits: ContextLimitsConfig) -> anyhow::Result<()> {
        *runtime_setting_lock(&self.context_limits, "context limits")? = limits;
        self.context_windows.clear();
        Ok(())
    }

    /// Hot-reload the provider-facing media projection policy.
    pub fn set_media_strategy(&self, strategy: MediaInputStrategy) -> anyhow::Result<()> {
        *runtime_setting_lock(&self.media_strategy, "media strategy")? = strategy;
        Ok(())
    }

    pub(crate) fn media_strategy(&self) -> MediaInputStrategy {
        *self.media_strategy.lock().unwrap()
    }

    pub fn set_max_steps_per_run(&self, max_steps_per_run: u32) -> anyhow::Result<()> {
        *runtime_setting_lock(&self.max_steps_per_run, "max steps per run")? = max_steps_per_run;
        Ok(())
    }

    /// Set optional session-lifetime step cap (`None` = unlimited).
    pub fn set_max_steps_per_session(
        &self,
        max_steps_per_session: Option<u32>,
    ) -> anyhow::Result<()> {
        *runtime_setting_lock(&self.max_steps_per_session, "max steps per session")? =
            max_steps_per_session;
        Ok(())
    }

    pub fn next_run_id(&self) -> u64 {
        self.run_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Live three-way connectivity probe to the default-model endpoint. Used
    /// by the top-right status indicator to show 就绪 / 已断开 / 未配置.
    pub async fn check_connection(&self) -> haven_llm::LlmConnectionReport {
        let router = self.router();
        router.connection_status(RequestKind::Chat).await
    }

    /// Allow the chat request launched by an explicit Continue action to try
    /// again immediately after an endpoint circuit has opened.
    pub(crate) async fn prepare_manual_chat_retry(&self) {
        self.router().prepare_manual_retry(RequestKind::Chat).await;
    }

    pub(super) fn router(&self) -> Arc<LlmRouter> {
        self.router.read().unwrap().clone()
    }

    /// Build the full tool-definition list for a session: global registry tools
    /// plus per-session MCP adapters registered via `load_mcp`.
    /// Capture the immutable catalog used by one model turn.  Provider
    /// definitions are derived from this exact snapshot, so execution cannot
    /// silently switch to a newer registry generation after the LLM request.
    pub(super) async fn build_tool_catalog_for_session(
        &self,
        session_id: &str,
    ) -> Arc<haven_tools::ToolCatalogSnapshot> {
        self.tool_catalog.catalog_snapshot(session_id).await
    }

    /// Reuse provider-facing schemas and their serialized token estimate while
    /// the immutable session catalog version is unchanged.
    pub(super) fn prepare_tool_definitions(
        &self,
        session_id: &str,
        catalog: &haven_tools::ToolCatalogSnapshot,
    ) -> PreparedToolDefinitions {
        let catalog_version = catalog.catalog_version();
        if let Some(prepared) = self
            .tool_definitions
            .get_if_catalog_version(session_id, catalog_version)
        {
            return prepared;
        }
        let definitions = Arc::new(
            catalog
                .provider_definitions()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<LlmToolDefinition>>(),
        );
        let token_estimate = crate::token_budget::estimate_tool_tokens(&definitions);
        let prepared = PreparedToolDefinitions {
            definitions,
            token_estimate,
        };
        tracing::info!(
            session_id,
            catalog_global_version = catalog_version.global_catalog_version,
            catalog_session_version = catalog_version.session_overlay_version,
            provider_tool_count = prepared.definitions.len(),
            tool_schema_token_estimate = prepared.token_estimate,
            "ReAct::prepare_tool_definitions: rebuilt provider tool surface"
        );
        self.tool_definitions
            .insert(session_id, catalog_version, prepared.clone());
        prepared
    }

    /// Validate every non-final tool call without altering its arguments.
    ///
    /// A missing discriminator, an invalid enum member, and a type mismatch
    /// can all change the meaning of a side-effecting call if replaced with a
    /// guessed value. The caller turns each failure into a normal failed tool
    /// observation so the model receives an actionable, structured error.
    #[cfg(test)]
    pub(crate) async fn validate_tool_inputs(
        &self,
        session_id: &str,
        tool_calls: &[ToolCall],
    ) -> Vec<ToolInputValidationFailure> {
        let catalog = self.tool_catalog.catalog_snapshot(session_id).await;
        self.validate_tool_inputs_from_catalog(&catalog, tool_calls)
    }

    pub(crate) fn validate_tool_inputs_from_catalog(
        &self,
        catalog: &haven_tools::ToolCatalogSnapshot,
        tool_calls: &[ToolCall],
    ) -> Vec<ToolInputValidationFailure> {
        self.validate_indexed_tool_inputs_from_catalog(
            catalog,
            tool_calls
                .iter()
                .enumerate()
                .map(|(tool_index, tool_call)| (tool_index as u32, tool_call)),
        )
    }

    pub(crate) fn validate_indexed_tool_inputs_from_catalog<'a>(
        &self,
        catalog: &haven_tools::ToolCatalogSnapshot,
        tool_calls: impl IntoIterator<Item = (u32, &'a ToolCall)>,
    ) -> Vec<ToolInputValidationFailure> {
        let mut failures = Vec::new();
        for (tool_index, tool_call) in tool_calls {
            if tool_call.is_final {
                continue;
            }
            let Some(result) = catalog.validate_input(&tool_call.tool_name, &tool_call.tool_input)
            else {
                continue;
            };
            if let Err(error) = result {
                failures.push(ToolInputValidationFailure {
                    tool_index,
                    tool_name: tool_call.tool_name.clone(),
                    details: vec![error.to_string()],
                });
            }
        }
        failures
    }

    /// Parse LLM response into thought text and tool_calls.
    pub fn parse_default_model_response(
        response: &LlmResponse,
        step_number: u32,
    ) -> ParsedAgentResponse {
        let text = response.text.trim().to_string();

        // Some OpenAI-compatible gateways leak one natural-language token while
        // switching a streamed response into a function call. It is not a useful
        // tool preamble, but would otherwise become a visible standalone bubble.
        // Gate at two characters because gateways commonly split the leaked
        // prefix into two deltas before emitting the tool call.
        let suppress_tool_call_fragment = !response.tool_calls.is_empty()
            && response
                .tool_calls
                .iter()
                .any(|call| call.name != "final_answer")
            && text.chars().count() <= 2;
        let thought = if text.is_empty() || suppress_tool_call_fragment {
            None
        } else {
            Some(text.clone())
        };

        let tool_calls: Vec<ToolCall> = if !response.tool_calls.is_empty() {
            let mut seen_tool_call_ids = HashSet::new();
            response
                .tool_calls
                .iter()
                .map(|tc| {
                    let args = tc.arguments.clone();
                    // `final_answer` is the only name that marks a tool call
                    // as the conversation's final answer.
                    let is_final = tc.name == "final_answer";
                    let provider_id = tc.id.trim();
                    let tool_call_id = if provider_id.is_empty()
                        || !seen_tool_call_ids.insert(provider_id.to_string())
                    {
                        haven_common::types::new_id("call")
                    } else {
                        provider_id.to_string()
                    };
                    ToolCall {
                        tool_name: tc.name.clone(),
                        tool_input: args,
                        is_final,
                        tool_call_id: Some(tool_call_id),
                    }
                })
                .collect()
        } else if !text.is_empty()
            && response.finish_reason == Some(FinishReason::Stop)
            && step_number > 0
        {
            vec![ToolCall {
                tool_name: "final_answer".into(),
                tool_input: serde_json::Value::Null,
                is_final: true,
                tool_call_id: None,
            }]
        } else {
            Vec::new()
        };

        ParsedAgentResponse {
            thought,
            tool_calls,
        }
    }

    /// Mark the session Error without propagating DB failures (the loop is
    /// already unwinding through the Fatal outcome).
    pub(super) async fn mark_session_error(&self, session_id: &str) {
        if let Err(e) = self
            .executor
            .update_session_status_if(session_id, SessionStatus::Running, SessionStatus::Error)
            .await
        {
            tracing::warn!(
                "ReAct: failed to mark session {} Error after a fatal step failure: {}",
                session_id,
                e
            );
        }
    }

    /// Update the per-session cumulative token counters, persist one per-call
    /// usage-detail row, and emit an `AgentEvent::Usage` event so the UI can
    /// refresh its display.
    /// `request` identifies the request policy that produced the response
    /// (used for cost lookup); `response` carries the token counts and model name;
    /// `step_number` is the ReAct step the call served and `duration_ms` its
    /// wall-clock duration, both recorded with the detail row.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_usage_and_emit(
        &self,
        session_id: &str,
        request: RequestKind,
        response: &LlmResponse,
        step_number: i32,
        duration_ms: Option<u64>,
        emitter: &Arc<dyn AgentEventEmitter>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) {
        let usage = response.usage.clone().normalize();
        if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
            // No usage reported by the provider —nothing useful to surface.
            return;
        }

        let router = self.router();
        let step_cost = router.compute_cost(request, &usage).await;
        // `context_window_for_request` always yields Some; the cached resolver
        // avoids cloning the full LlmConfig on every step.
        let context_window = Some(self.cached_context_window(request).await);

        let model = response.model.clone().or_else(|| usage.model_name.clone());
        let call_has_cost = step_cost.is_some();
        let cache_diagnostics = usage.cache_diagnostics.clone();
        let totals = match self
            .usage_runtime
            .record(
                session_id,
                UsageUpdate {
                    call_kind: LlmCallKind::Agent,
                    request,
                    model: model.clone(),
                    step_number,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    total_tokens: usage.total_tokens,
                    cached_tokens: usage.cached_tokens,
                    cache_creation_tokens: usage.cache_creation_tokens,
                    cache_miss_tokens: usage.effective_cache_miss_tokens(),
                    cache_accounting: usage.cache_accounting,
                    cache_diagnostics,
                    cost_usd: step_cost.unwrap_or(0.0),
                    has_cost: call_has_cost,
                    duration_ms,
                    context_tokens: usage.context_tokens(),
                    context_window,
                    cancel,
                },
            )
            .await
        {
            Ok(totals) => totals,
            Err(error) => {
                tracing::warn!(session_id, %error, "failed to record session usage");
                return;
            }
        };
        let cum_prompt = totals.prompt_tokens;
        let cum_completion = totals.completion_tokens;
        let cum_total = totals.total_tokens;
        let cum_cached = totals.cached_tokens;
        let cum_cache_creation = totals.cache_creation_tokens;
        let cum_cache_miss = totals.cache_miss_tokens;
        let cum_cost_opt = totals.cost_usd;

        tracing::debug!(
            "ReAct step {} session {} LLM usage: {}/{}/{} tokens (cache hit {} / write {}), {} ms, model={:?}",
            step_number,
            session_id,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens,
            usage.cached_tokens,
            usage.cache_creation_tokens,
            duration_ms.unwrap_or(0),
            model
        );

        EventDispatcher::emit_usage_from(
            emitter,
            UsagePayload {
                session_id: session_id.to_string(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                cache_miss_tokens: usage.effective_cache_miss_tokens(),
                context_tokens: usage.context_tokens(),
                cache_exclusive: usage.cache_exclusive_of_prompt(),
                cache_accounting: usage.cache_accounting,
                cost_usd: step_cost,
                model,
                cumulative_prompt_tokens: cum_prompt,
                cumulative_completion_tokens: cum_completion,
                cumulative_total_tokens: cum_total,
                cumulative_cached_tokens: cum_cached,
                cumulative_cache_creation_tokens: cum_cache_creation,
                cumulative_cache_miss_tokens: cum_cache_miss,
                cache_diagnostics: usage.cache_diagnostics,
                cumulative_cost_usd: cum_cost_opt,
                context_window,
                step_number: Some(step_number as u32),
                duration_ms,
                request_kind: Some(request),
                call_kind: LlmCallKind::Agent,
                has_cost: call_has_cost,
            },
        )
        .await;
    }

    /// Persist and emit usage for model calls owned by a tool. These
    /// calls are intentionally outside the ReAct cumulative tracker: they are
    /// useful diagnostics and cost data, but they do not occupy the Agent's
    /// next prompt prefix and must not distort its cache-hit rate.
    pub(super) async fn record_tool_usage(
        &self,
        session_id: &str,
        step_number: i32,
        usages: &[haven_tools::ToolLlmUsage],
        emitter: &Arc<dyn AgentEventEmitter>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) {
        struct PendingToolUsage {
            input: haven_memory::LlmUsageRecordInput,
            payload: UsagePayload,
        }

        let mut pending = Vec::with_capacity(usages.len());
        for tool_usage in usages {
            let usage = tool_usage.usage.clone().normalize();
            if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
                continue;
            }
            let request = tool_usage.request;
            let call_kind = tool_usage.call_kind;
            let step_cost = self.router().compute_cost(request, &usage).await;
            let model = tool_usage
                .model
                .clone()
                .or_else(|| usage.model_name.clone());
            let cache_accounting = usage.cache_accounting;
            let cache_diagnostics = usage.cache_diagnostics.clone();
            let payload = UsagePayload {
                session_id: session_id.to_string(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                cache_miss_tokens: usage.effective_cache_miss_tokens(),
                context_tokens: usage.context_tokens(),
                cache_exclusive: usage.cache_exclusive_of_prompt(),
                cache_accounting,
                cost_usd: step_cost,
                model: model.clone(),
                cumulative_prompt_tokens: 0,
                cumulative_completion_tokens: 0,
                cumulative_total_tokens: 0,
                cumulative_cached_tokens: 0,
                cumulative_cache_creation_tokens: 0,
                cumulative_cache_miss_tokens: 0,
                cache_diagnostics: usage.cache_diagnostics.clone(),
                cumulative_cost_usd: None,
                context_window: None,
                step_number: Some(step_number as u32),
                duration_ms: tool_usage.duration_ms,
                request_kind: Some(request),
                call_kind,
                has_cost: step_cost.is_some(),
            };
            pending.push(PendingToolUsage {
                input: haven_memory::LlmUsageRecordInput {
                    step_number: Some(step_number),
                    request_kind: request,
                    call_kind,
                    model,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    total_tokens: usage.total_tokens,
                    cached_tokens: usage.cached_tokens,
                    cache_creation_tokens: usage.cache_creation_tokens,
                    cache_miss_tokens: usage.effective_cache_miss_tokens(),
                    cache_accounting: usage.cache_accounting,
                    cache_diagnostics,
                    cost_usd: step_cost.unwrap_or(0.0),
                    has_cost: step_cost.is_some(),
                    duration_ms: tool_usage.duration_ms,
                    context_tokens: usage.context_tokens(),
                    context_window: None,
                },
                payload,
            });
        }

        if pending.is_empty() {
            return;
        }

        let inputs = pending
            .iter()
            .map(|item| item.input.clone())
            .collect::<Vec<_>>();
        let persisted = self
            .usage_runtime
            .append_tool_usage_batch(session_id, inputs, cancel)
            .await;
        match persisted {
            Ok(_) => {}
            Err(error) => tracing::warn!(
                "ReAct: failed to persist tool usage batch for session {} step {}: {}",
                session_id,
                step_number,
                error
            ),
        }

        for item in pending {
            EventDispatcher::emit_usage_from(emitter, item.payload).await;
        }
    }

    /// Persist and optionally emit usage for a model call owned by a media
    /// capability. Ingress calls have no ReAct step, so `step_number` is
    /// nullable; tool calls pass their enclosing step. Both paths share the
    /// same `call_kind=media` persistence and event contract.
    pub(crate) async fn record_media_usage_at_step(
        &self,
        session_id: &str,
        step_number: Option<i32>,
        usages: &[haven_llm::LlmCallUsage],
        emitter: Option<&Arc<dyn AgentEventEmitter>>,
    ) {
        self.record_usage_at_step(session_id, step_number, usages, LlmCallKind::Media, emitter)
            .await;
    }

    async fn record_usage_at_step(
        &self,
        session_id: &str,
        step_number: Option<i32>,
        usages: &[haven_llm::LlmCallUsage],
        call_kind: LlmCallKind,
        emitter: Option<&Arc<dyn AgentEventEmitter>>,
    ) {
        for tool_usage in usages {
            let usage = tool_usage.usage.clone().normalize();
            if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
                continue;
            }
            let request = tool_usage.request;
            let step_cost = self.router().compute_cost(request, &usage).await;
            let model = tool_usage
                .model
                .clone()
                .or_else(|| usage.model_name.clone());
            let usage_prompt = usage.prompt_tokens;
            let usage_completion = usage.completion_tokens;
            let usage_total = usage.total_tokens;
            let usage_cached = usage.cached_tokens;
            let usage_cache_creation = usage.cache_creation_tokens;
            let usage_cache_miss = usage.effective_cache_miss_tokens();
            let usage_cache_accounting = usage.cache_accounting;
            let cache_diagnostics = usage.cache_diagnostics.clone();
            let cache_diagnostics_for_event = usage.cache_diagnostics.clone();
            let store = self.event_store.clone();
            let session_id_for_persist = session_id.to_string();
            let call_kind_for_persist = call_kind;
            let model_for_persist = model.clone();
            let call_cost = step_cost.unwrap_or(0.0);
            let call_has_cost = step_cost.is_some();
            let duration_ms = tool_usage.duration_ms;
            let usage_context_tokens = usage.context_tokens();
            let persist = tokio::task::spawn_blocking(move || {
                store.append_usage(
                    &session_id_for_persist,
                    &haven_memory::LlmUsageRecordInput {
                        step_number,
                        request_kind: request,
                        call_kind: call_kind_for_persist,
                        model: model_for_persist,
                        prompt_tokens: usage_prompt,
                        completion_tokens: usage_completion,
                        total_tokens: usage_total,
                        cached_tokens: usage_cached,
                        cache_creation_tokens: usage_cache_creation,
                        cache_miss_tokens: usage_cache_miss,
                        cache_accounting: usage_cache_accounting,
                        cache_diagnostics,
                        cost_usd: call_cost,
                        has_cost: call_has_cost,
                        duration_ms,
                        context_tokens: usage_context_tokens,
                        context_window: None,
                    },
                )
            });
            match persist.await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => tracing::warn!(
                    "ReAct: failed to persist {} usage for session {} step {:?}: {}",
                    call_kind.as_str(),
                    session_id,
                    step_number,
                    error
                ),
                Err(error) => tracing::warn!(
                    "ReAct: {} usage persistence task failed for session {} step {:?}: {}",
                    call_kind.as_str(),
                    session_id,
                    step_number,
                    error
                ),
            }

            let Some(emitter) = emitter else {
                continue;
            };
            EventDispatcher::emit_usage_from(
                emitter,
                UsagePayload {
                    session_id: session_id.to_string(),
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    total_tokens: usage.total_tokens,
                    cached_tokens: usage.cached_tokens,
                    cache_creation_tokens: usage.cache_creation_tokens,
                    cache_miss_tokens: usage.effective_cache_miss_tokens(),
                    context_tokens: usage_context_tokens,
                    cache_exclusive: usage.cache_exclusive_of_prompt(),
                    cache_accounting: usage_cache_accounting,
                    cost_usd: step_cost,
                    model,
                    cumulative_prompt_tokens: 0,
                    cumulative_completion_tokens: 0,
                    cumulative_total_tokens: 0,
                    cumulative_cached_tokens: 0,
                    cumulative_cache_creation_tokens: 0,
                    cumulative_cache_miss_tokens: 0,
                    cache_diagnostics: cache_diagnostics_for_event,
                    cumulative_cost_usd: None,
                    context_window: None,
                    step_number: step_number.and_then(|step| u32::try_from(step).ok()),
                    duration_ms,
                    request_kind: Some(request),
                    call_kind,
                    has_cost: call_has_cost,
                },
            )
            .await;
        }
    }

    /// Drop cumulative counters and process-local turn caches for a finished
    /// session. Deletion later closes and joins the UsageRuntime worker.
    pub fn reset_cumulative_usage(&self, session_id: &str) {
        self.context_source.clear_session(session_id);
        self.usage_runtime.reset(session_id);
    }

    /// Reclaim transient session caches and join the detached usage worker
    /// after deletion has quiesced the owning actor.
    pub(crate) async fn forget_deleted_session(&self, session_id: &str) {
        self.context_source.clear_session(session_id);
        self.usage_runtime.remove_session(session_id).await;
    }

    /// Reclaim every known session cache after the history-clear barrier.
    pub(crate) async fn forget_deleted_sessions(&self, session_ids: &[String]) {
        for session_id in session_ids {
            self.context_source.clear_session(session_id);
        }
        self.usage_runtime.remove_all_sessions().await;
    }

    /// After rollback/truncate rebuilt `session_usage` from remaining
    /// `llm_usage` rows: clear in-memory counters and bump the persist epoch
    /// so a late fire-and-forget write from a discarded call cannot re-inflate
    /// the totals. Next live usage event re-seeds from the rebuilt DB row.
    pub fn invalidate_usage_after_truncate(&self, session_id: &str) {
        self.usage_runtime.invalidate_after_truncate(session_id);
    }

    /// Resolve the model's true context window for the request used by
    /// `request`. Explicit `context_window` on the model endpoint takes
    /// precedence; callers fall back to the context-limit default. Prefer
    /// writing the window from provider `/models` metadata into the model slot
    /// when the user picks a model. This is the real input budget for the
    /// token-usage display, not the per-response output cap (`max_tokens`).
    pub(super) fn context_window_for_request(
        cfg: &haven_common::config::RouterConfig,
        request: RequestKind,
    ) -> Option<u32> {
        cfg.route(request)
            .and_then(|model| haven_llm::registry::context_window_for(&model.endpoint))
    }

    /// Resolve the model's true context window for `request` using a per-router
    /// cache. Cloning the full LlmConfig on every step (compactor window +
    /// usage display) is wasteful when the router only changes via
    /// `replace_router`; the cache is keyed by the router instance pointer so
    /// a hot-swapped router invalidates it immediately.
    pub(super) async fn cached_context_window(&self, request: RequestKind) -> u32 {
        let router = self.router();
        let ptr = Arc::as_ptr(&router) as usize;
        // Fast path: read the cached window without awaiting the router
        // config. The cache guard is scoped so it never crosses an await
        // (the std Mutex guard is not Send).
        if let Some(window) = self.context_windows.get(ptr, request) {
            return window;
        }
        // Slow path: resolve from the live router config. A concurrent
        // router swap between the fast-path miss and the insert is harmless:
        // the entry is stored under the pointer that was current at read
        // time and recomputed on the next miss.
        let cfg = router.config().await;
        let window = Self::context_window_for_request(&cfg, request)
            .unwrap_or(self.limits().default_context_window);
        self.context_windows.insert(ptr, request, window);
        window
    }

    /// Build a compactor whose context window reflects the *actual* model for
    /// the request that will handle the step (explicit `context_window` on the
    /// model, else `context_limits.default_context_window`). The window comes
    /// from `cached_context_window`, so a hot-swapped router config takes
    /// effect immediately without cloning the full config on every step. The
    /// compaction threshold (ratio and reserve) and the fallback window come
    /// from `context_limits`.
    pub(super) async fn context_compactor(&self, request: RequestKind) -> ContextCompactor {
        let window = self.cached_context_window(request).await;
        let limits = self.limits();
        ContextCompactor::with_ratio(
            window,
            limits.compaction_reserve_tokens,
            limits.compaction_ratio,
        )
    }

    /// Check if context compaction is needed before the next LLM call.
    ///
    /// Returns `true` when a compaction actually ran (the caller re-checks
    /// the image flag afterwards, since summarizing away the last image
    /// changes the endpoint routing). Compaction goes through
    /// [`TranscriptEvent::CompactSummary`] (Phase 6.1).
    pub(crate) async fn maybe_compact(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        requirements: MediaRequirements,
        tool_token_estimate: u32,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<bool> {
        if state.canonical.len() < 4 {
            return Ok(false);
        }
        // The compaction window must match the endpoint the next step will
        // use, mirroring choose_agent_request's request selection.
        let router = self.router();
        let request = if requirements.image || requirements.video {
            RequestKind::Vision
        } else if requirements.audio {
            RequestKind::AudioChat
        } else {
            RequestKind::Chat
        };
        let compactor = self.context_compactor(request).await;
        // Compare the incremental estimate against the threshold directly;
        // `needs_compaction` would re-estimate the whole canonical and undo
        // the incremental cache.
        let cached_message_tokens = state.estimate_canonical_tokens();
        let request_tokens = crate::token_budget::estimate_provider_request_tokens_with_estimates(
            &state.canonical,
            cached_message_tokens,
            tool_token_estimate,
        );
        if request_tokens <= compactor.threshold_tokens() {
            return Ok(false);
        }
        match compactor
            .compact_with_tool_token_estimate(
                &state.canonical,
                tool_token_estimate,
                &router,
                cancel,
            )
            .await
        {
            Ok(Some(result)) => {
                tracing::info!(
                    session_id = %ctx.session_id,
                    tokens_before = result.tokens_before,
                    tokens_after = result.tokens_after,
                    summarized_count = result.summarized_count,
                    degraded = result.degraded,
                    "compaction completed"
                );
                self.apply_transcript(
                    ctx,
                    TranscriptEvent::CompactSummary {
                        compacted: result.compacted,
                        media_inputs: media_inputs_from_events(&state.events),
                        summary: result.summary,
                        tokens_before: result.tokens_before,
                        tokens_after: result.tokens_after,
                        episode_id: result.episode_id,
                        degraded: result.degraded,
                    },
                    state,
                )
                .await?;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(haven_llm::LlmError::Cancelled) => Ok(false),
            Err(error) => {
                tracing::warn!(
                    session_id = %ctx.session_id,
                    "compaction cancelled or unavailable: {error}"
                );
                Ok(false)
            }
        }
    }

    /// Emit session error and clean up per-session state.
    pub(super) async fn emit_error(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        error: &str,
    ) {
        tracing::error!(
            session_id = %session_id,
            error = %haven_common::error::sanitize_error_text(error),
            "ReAct session failed"
        );
        EventDispatcher::emit_session_error_from(emitter, session_id, error).await;
        self.reset_cumulative_usage(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use haven_common::types::{CanonicalRole, CanonicalToolCall, InjectSource, MessageAttachment};

    #[test]
    fn poisoned_runtime_setting_lock_returns_a_diagnostic() {
        let lock = Arc::new(Mutex::new(()));
        let lock_for_panic = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let _guard = lock_for_panic.lock().unwrap();
            panic!("poison runtime setting lock for test");
        })
        .join();

        let error = runtime_setting_lock(&lock, "session limits").unwrap_err();
        assert!(error.to_string().contains("session limits"));
        assert!(error.to_string().contains("poisoned"));
    }

    #[test]
    fn loop_exit_variants_distinguish_pause_reasons() {
        assert_eq!(
            LoopExit::Paused {
                reason: PauseReason::Ask
            },
            LoopExit::Paused {
                reason: PauseReason::Ask
            }
        );
        assert_ne!(
            LoopExit::Paused {
                reason: PauseReason::Ask
            },
            LoopExit::Paused {
                reason: PauseReason::TurnEnd
            }
        );
        assert_ne!(LoopExit::Cancelled, LoopExit::Completed);
        assert!(matches!(LoopExit::Error("x".into()), LoopExit::Error(_)));
    }
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmResponse, StreamChunk};
    use std::pin::Pin;

    #[tokio::test]
    async fn token_estimate_cache_is_used_directly_without_a_session_actor() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("token-estimate.db"))
                .expect("temporary database"),
        );
        let executor = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(haven_tools::ToolsFacade::new()),
            1,
        ));
        let session_id = "ses-0123456789abcdef0123456789abcdef";
        let mut state = ReActState::new(
            Vec::new(),
            vec![text_msg(CanonicalRole::User, "first message")],
            HashMap::new(),
        );

        assert_eq!(
            state.estimate_canonical_tokens(),
            crate::token_budget::estimate_message_tokens(&state.canonical),
        );
        Arc::make_mut(&mut state.canonical)
            .push(text_msg(CanonicalRole::Assistant, "appended message"));
        state.mark_canonical_append();
        assert_eq!(
            state.estimate_canonical_tokens(),
            crate::token_budget::estimate_message_tokens(&state.canonical),
        );

        assert!(executor.actor_for_now(session_id).is_none());
    }

    struct MockLlm {
        profile: CapabilityProfile,
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        fn capability_profile(&self) -> CapabilityProfile {
            self.profile.clone()
        }

        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: chat not implemented".into()))
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<LlmToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown(
                "mock: chat_with_tools not implemented".into(),
            ))
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "mock: chat_stream not implemented".into(),
            ))
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<LlmToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "mock: chat_stream_with_tools not implemented".into(),
            ))
        }
        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _messages: Arc<[CanonicalMessage]>,
            _tools: Arc<[LlmToolDefinition]>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "mock: chat_stream_with_tools not implemented".into(),
            ))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn mock_router() -> LlmRouter {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlm {
            profile: CapabilityProfile::default(),
        });
        LlmRouter::new_with_test_clients(client.clone(), client.clone(), client.clone(), client)
    }

    fn mock_router_with_profiles(
        default_profile: CapabilityProfile,
        image_profile: CapabilityProfile,
        audio_profile: CapabilityProfile,
    ) -> LlmRouter {
        let small: Arc<dyn LlmClient> = Arc::new(MockLlm {
            profile: default_profile.clone(),
        });
        let default: Arc<dyn LlmClient> = Arc::new(MockLlm {
            profile: default_profile,
        });
        let image: Arc<dyn LlmClient> = Arc::new(MockLlm {
            profile: image_profile,
        });
        let audio: Arc<dyn LlmClient> = Arc::new(MockLlm {
            profile: audio_profile,
        });
        LlmRouter::new_with_test_clients(small, default, image, audio)
    }

    // ── structured failure classes & retry nudge ──────────────────────────

    #[test]
    fn structured_failure_classes_shape_the_nudge_without_text_scanning() {
        assert_eq!(
            tool_batch_policy::failure_kind(haven_tools::ToolErrorClass::Transient),
            FailureKind::Environmental
        );
        assert_eq!(
            tool_batch_policy::failure_kind(haven_tools::ToolErrorClass::Validation),
            FailureKind::Logic
        );
        assert_eq!(
            tool_batch_policy::failure_kind(haven_tools::ToolErrorClass::Other),
            FailureKind::Unknown
        );
    }

    #[test]
    fn failure_nudge_environmental_suggests_relevant_checks() {
        let nudge = ReActEngine::build_failure_nudge(&[(
            "shell".into(),
            haven_tools::ToolErrorClass::Transient,
        )]);
        assert!(
            !nudge.contains("Do NOT abandon"),
            "environmental failures should leave the next step open, got: {nudge}"
        );
        assert!(
            nudge.contains("environmental") && nudge.contains("network"),
            "should name likely environmental causes, got: {nudge}"
        );
    }

    #[test]
    fn failure_nudge_logic_suggests_correction_and_reassessment() {
        let nudge = ReActEngine::build_failure_nudge(&[(
            "files".into(),
            haven_tools::ToolErrorClass::Validation,
        )]);
        assert!(nudge.contains("logic errors"), "got: {nudge}");
        assert!(
            nudge.contains("Correct the specific issue") && nudge.contains("reassess the approach"),
            "should guide correction without locking the approach, got: {nudge}"
        );
    }

    #[test]
    fn failure_nudge_empty_falls_back_to_generic() {
        let nudge = ReActEngine::build_failure_nudge(&[]);
        // Unknown failures receive the concise shared retry guidance.
        assert!(
            nudge.contains(haven_common::prompts::TOOL_FAILURE_DIAGNOSIS),
            "got: {nudge}"
        );
    }

    #[test]
    fn audio_attachment_becomes_inline_content_part() {
        let attachment = MessageAttachment::new("audio/wav", "UklGRg==");
        assert!(matches!(
            attachment_to_content_part_with_strategy(&attachment, MediaInputStrategy::Auto),
            ContentPart::Audio {
                ref media_type,
                ref data,
                ..
            } if media_type == "audio/wav" && data == "UklGRg=="
        ));
    }

    #[test]
    fn media_strategy_controls_raw_attachment_projection() {
        let attachment = MessageAttachment::new("image/png", "iVBORw0KGgo=");
        assert!(matches!(
            attachment_to_content_part_with_strategy(&attachment, MediaInputStrategy::RawPreferred),
            ContentPart::Image { .. }
        ));

        let ContentPart::Text(text) =
            attachment_to_content_part_with_strategy(&attachment, MediaInputStrategy::TextOnlySafe)
        else {
            panic!("text_only_safe must not project raw image bytes");
        };
        assert!(text.contains("no safe representation is available for this request"));
    }

    #[test]
    fn ordinary_file_attachment_uses_opaque_reference_without_path() {
        let mut attachment = MessageAttachment::new("application/pdf", "");
        attachment.filename = Some("report.pdf".into());
        attachment.path = Some(r"C:\Users\olive\uploads\report.pdf".into());
        let ContentPart::Text(text) =
            attachment_to_content_part_with_strategy(&attachment, MediaInputStrategy::Auto)
        else {
            panic!("ordinary files use an opaque managed reference");
        };
        assert!(text.contains("report.pdf"));
        assert!(text.contains("asset_id="));
        assert!(!text.contains(r"C:\Users\olive"));
    }

    #[test]
    fn snapshotted_managed_attachment_exposes_media_plan_handle() {
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some("asset-0123456789abcdef0123456789abcdef".into());
        attachment.path = Some(r"C:\Users\olive\uploads\photo.png".into());
        let canonical = crate::types::project_transcript_with_strategy(
            &[TranscriptRecord::UserInject {
                step_number: 1,
                source: haven_common::types::InjectSource::FollowUp,
                text: "看这张图".into(),
                media_inputs: vec![
                    haven_common::media::message_attachment_to_media_input(&attachment)
                        .for_snapshot(),
                ],
                message_id: None,
            }],
            MediaInputStrategy::Auto,
        )
        .canonical_messages;
        let text = canonical[0]
            .content
            .iter()
            .find_map(|part| match part {
                ContentPart::Text(text) if text.contains("media_plan:") => Some(text),
                _ => None,
            })
            .expect("raw managed media should leave a plan notice");
        assert!(text.contains("asset-0123456789abcdef0123456789abcdef"));
        assert!(text.contains("asset_id=asset-0123456789abcdef0123456789abcdef"));
        assert!(text.contains("managed_file_ref"));
        assert!(text.contains("previous tool result"));
        assert!(text.contains("media(asset_id="));
        assert!(!text.contains(r"C:\Users\olive"));
    }

    #[test]
    fn managed_file_attachment_projects_to_opaque_reference() {
        let mut attachment = MessageAttachment::new("application/pdf", "");
        attachment.asset_id = Some("asset-report".into());
        attachment.filename = Some("report.pdf".into());
        attachment.path = Some(r"C:\Users\olive\uploads\report.pdf".into());
        let ContentPart::Text(text) =
            attachment_to_content_part_with_strategy(&attachment, MediaInputStrategy::Auto)
        else {
            panic!("managed files use a text reference for the files tool");
        };
        assert!(text.contains("asset-report"));
        assert!(text.contains("report.pdf"));
        assert!(!text.contains(r"C:\Users\olive"));
    }

    fn text_msg(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    fn image_msg(role: CanonicalRole) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    fn audio_msg(role: CanonicalRole) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::Audio {
                content_type: "input_audio".into(),
                media_type: "audio/wav".into(),
                data: "UklGRg==".into(),
            }],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    fn request_context(messages: Vec<CanonicalMessage>) -> RequestContext {
        RequestContext::from_state(&ReActState::new(Vec::new(), messages, HashMap::new()), None)
    }

    fn media_request_context(
        message: CanonicalMessage,
        media_type: &str,
        data: &str,
    ) -> RequestContext {
        let input = message_attachment_to_media_input(&MessageAttachment::new(media_type, data));
        RequestContext::from_state(
            &ReActState::new(
                vec![TranscriptRecord::UserInject {
                    step_number: 1,
                    source: InjectSource::FollowUp,
                    text: "media input".into(),
                    media_inputs: vec![input],
                    message_id: None,
                }],
                vec![message],
                HashMap::new(),
            ),
            None,
        )
    }

    #[tokio::test]
    async fn choose_agent_request_default_without_images() {
        let router = mock_router();
        let messages = [
            text_msg(CanonicalRole::System, "be concise"),
            text_msg(CanonicalRole::User, "hello"),
        ];
        let context = request_context(messages.into());
        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::Chat
        );
    }

    #[tokio::test]
    async fn choose_agent_request_default_when_image_model_unconfigured() {
        let router = mock_router();
        let context =
            media_request_context(image_msg(CanonicalRole::User), "image/png", "aGVsbG8=");
        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::Chat
        );
    }

    #[tokio::test]
    async fn choose_agent_request_vision_when_configured() {
        let router = mock_router();
        router
            .set_request_configured_for_test(RequestKind::Vision, true)
            .await;
        let messages = [image_msg(CanonicalRole::User)];
        let context = request_context(messages.into());
        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::Vision
        );
    }

    #[tokio::test]
    async fn choose_agent_request_default_when_vision_routing_disabled() {
        let router = mock_router();
        router
            .set_request_configured_for_test(RequestKind::Vision, true)
            .await;
        router
            .set_request_primary_for_test(RequestKind::Vision, "default_model")
            .await
            .unwrap();
        let messages = [image_msg(CanonicalRole::User)];
        let context = request_context(messages.into());
        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::Chat
        );
    }

    #[tokio::test]
    async fn choose_agent_request_audio_when_configured() {
        let router = mock_router();
        router
            .set_request_configured_for_test(RequestKind::AudioChat, true)
            .await;
        let messages = [audio_msg(CanonicalRole::User)];
        let context = request_context(messages.into());
        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::AudioChat
        );
    }

    #[tokio::test]
    async fn choose_agent_request_falls_back_when_specialized_mime_is_unsupported() {
        let default_profile = CapabilityProfile {
            image: CapabilitySupport::Supported,
            ..CapabilityProfile::default()
        };
        let mut image_profile = default_profile.clone();
        image_profile.accepted_mime_types = vec!["image/jpeg".into()];
        let router =
            mock_router_with_profiles(default_profile, image_profile, CapabilityProfile::default());
        router
            .set_request_configured_for_test(RequestKind::Vision, true)
            .await;
        let context =
            media_request_context(image_msg(CanonicalRole::User), "image/png", "aGVsbG8=");

        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::Chat
        );
    }

    #[tokio::test]
    async fn choose_agent_request_falls_back_when_specialized_size_is_exceeded() {
        let default_profile = CapabilityProfile {
            image: CapabilitySupport::Supported,
            ..CapabilityProfile::default()
        };
        let mut image_profile = default_profile.clone();
        image_profile.max_input_bytes = Some(4);
        let router =
            mock_router_with_profiles(default_profile, image_profile, CapabilityProfile::default());
        router
            .set_request_configured_for_test(RequestKind::Vision, true)
            .await;
        let context =
            media_request_context(image_msg(CanonicalRole::User), "image/png", "aGVsbG8=");

        assert_eq!(
            choose_agent_request(&router, &context).await,
            RequestKind::Chat
        );
    }

    fn resp(
        text: &str,
        tool_calls: Vec<CanonicalToolCall>,
        finish: Option<FinishReason>,
    ) -> LlmResponse {
        LlmResponse {
            text: text.to_string(),
            tool_calls,
            finish_reason: finish,
            usage: haven_llm::types::Usage::default(),
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }
    }

    #[test]
    fn parse_empty_response_no_tool_calls() {
        let r = resp("", vec![], None);
        let ParsedAgentResponse {
            thought,
            tool_calls,
        } = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought, None);
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn parse_text_only_no_finish_reason_keeps_thought_no_action() {
        // step_number=1, Stop finish, but step>0 required for implicit final.
        let r = resp("hello", vec![], Some(FinishReason::Stop));
        let ParsedAgentResponse {
            thought,
            tool_calls,
        } = ReActEngine::parse_default_model_response(&r, 0);
        assert_eq!(thought.as_deref(), Some("hello"));
        assert!(tool_calls.is_empty(), "step 0 must not auto-finalize");
    }

    #[test]
    fn parse_text_with_stop_finish_step_nonzero_auto_finalizes() {
        let r = resp("the answer is 42", vec![], Some(FinishReason::Stop));
        let ParsedAgentResponse {
            thought,
            tool_calls,
        } = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("the answer is 42"));
        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].is_final);
        assert_eq!(tool_calls[0].tool_name, "final_answer");
        assert!(tool_calls[0].tool_call_id.is_none());
    }

    #[test]
    fn parse_tool_calls_produce_tool_calls() {
        let tc = CanonicalToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "x.txt"}),
        };
        let r = resp("thinking", vec![tc], Some(FinishReason::ToolCalls));
        let ParsedAgentResponse {
            thought,
            tool_calls,
        } = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("thinking"));
        assert_eq!(tool_calls.len(), 1);
        assert!(!tool_calls[0].is_final);
        assert_eq!(tool_calls[0].tool_name, "read_file");
        assert_eq!(tool_calls[0].tool_input["path"], "x.txt");
        assert_eq!(tool_calls[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn parse_short_fragment_before_tool_call_is_not_a_thought() {
        let tc = CanonicalToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "x.txt"}),
        };
        for text in ["我", "I", "我先", "Go"] {
            let r = resp(text, vec![tc.clone()], Some(FinishReason::ToolCalls));
            let ParsedAgentResponse {
                thought,
                tool_calls,
            } = ReActEngine::parse_default_model_response(&r, 1);
            assert_eq!(thought, None, "{text:?} must not become a thought bubble");
            assert_eq!(tool_calls.len(), 1);
        }
    }

    #[test]
    fn parse_tool_call_keeps_meaningful_preamble() {
        let tc = CanonicalToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "x.txt"}),
        };
        let r = resp("正在读取文件。", vec![tc], Some(FinishReason::ToolCalls));
        let ParsedAgentResponse {
            thought,
            tool_calls,
        } = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("正在读取文件。"));
        assert_eq!(tool_calls.len(), 1);
    }

    #[test]
    fn parse_final_answer_tool_call_marked_final() {
        let tc = CanonicalToolCall {
            id: "c2".into(),
            name: "final_answer".into(),
            arguments: serde_json::json!({"answer": "done"}),
        };
        let r = resp("answering", vec![tc], Some(FinishReason::ToolCalls));
        let ParsedAgentResponse { tool_calls, .. } =
            ReActEngine::parse_default_model_response(&r, 2);
        assert!(tool_calls[0].is_final);
    }

    #[test]
    fn parse_non_final_tool_calls_are_not_marked_final() {
        // Only `final_answer` marks a tool call as final; provider-specific
        // names like `answer`/`done` are ordinary tool calls now.
        for name in ["answer", "done"] {
            let tc = CanonicalToolCall {
                id: String::new(),
                name: name.into(),
                arguments: serde_json::json!({}),
            };
            let r = resp("t", vec![tc], Some(FinishReason::ToolCalls));
            let ParsedAgentResponse { tool_calls, .. } =
                ReActEngine::parse_default_model_response(&r, 1);
            assert!(!tool_calls[0].is_final, "{name} must not be final");
        }
    }

    #[test]
    fn parse_empty_tool_call_id_gets_generated() {
        let tc = CanonicalToolCall {
            id: String::new(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        let r = resp("", vec![tc], Some(FinishReason::ToolCalls));
        let ParsedAgentResponse { tool_calls, .. } =
            ReActEngine::parse_default_model_response(&r, 1);
        assert!(tool_calls[0].tool_call_id.is_some());
        assert!(!tool_calls[0].tool_call_id.as_ref().unwrap().is_empty());
    }

    #[test]
    fn parse_multiple_tool_calls_preserve_order() {
        let tcs = vec![
            CanonicalToolCall {
                id: "a".into(),
                name: "search".into(),
                arguments: serde_json::json!({}),
            },
            CanonicalToolCall {
                id: "b".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
        ];
        let r = resp("multi", tcs, Some(FinishReason::ToolCalls));
        let ParsedAgentResponse { tool_calls, .. } =
            ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].tool_name, "search");
        assert_eq!(tool_calls[1].tool_name, "read_file");
    }

    #[test]
    fn parse_duplicate_provider_tool_call_ids_get_distinct_local_ids() {
        let tcs = vec![
            CanonicalToolCall {
                id: "same".into(),
                name: "search".into(),
                arguments: serde_json::json!({"q": "x"}),
            },
            CanonicalToolCall {
                id: "same".into(),
                name: "search".into(),
                arguments: serde_json::json!({"q": "x"}),
            },
        ];
        let r = resp("", tcs, Some(FinishReason::ToolCalls));
        let ParsedAgentResponse { tool_calls, .. } =
            ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].tool_call_id.as_deref(), Some("same"));
        assert_ne!(tool_calls[0].tool_call_id, tool_calls[1].tool_call_id);
        assert!(
            tool_calls[1]
                .tool_call_id
                .as_deref()
                .is_some_and(|id| id.starts_with("call-"))
        );
    }

    #[test]
    fn parse_tool_calls_take_precedence_over_text_final() {
        // Even with Stop finish + step>0, tool_calls win over implicit final.
        let tc = CanonicalToolCall {
            id: "x".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        let r = resp("text", vec![tc], Some(FinishReason::Stop));
        let ParsedAgentResponse { tool_calls, .. } =
            ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(tool_calls.len(), 1);
        assert!(!tool_calls[0].is_final);
    }

    #[test]
    fn parse_text_trimmed_for_thought() {
        let r = resp("  spaced thought  ", vec![], Some(FinishReason::Stop));
        let ParsedAgentResponse { thought, .. } = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("spaced thought"));
    }
}
