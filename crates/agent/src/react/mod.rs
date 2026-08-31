use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::session::{SessionExecutor, SessionStatus};
use haven_common::config::ContextLimitsConfig;
use haven_common::types::MessageAttachment;
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition};
use haven_memory::Database;

use crate::compactor::ContextCompactor;
use crate::event::{AgentEvent, AgentEventEmitter, EventDispatcher, UsagePayload};
use crate::types::{Action, BranchPoint, TranscriptRecord};
use chrono::Utc;

mod context;
mod hook_policy;
mod hooks;
mod identity;
mod inject;
mod r#loop;
mod retries;
mod sidecars;
mod snapshot_io;
mod state;
pub(crate) mod stream_step;
mod tool_batch;
mod transcript;
mod turn;
mod turn_end;

use context::ContextSource;
pub(crate) use hooks::{InferCallback, MemoryPatchHandle, default_hooks_with_infer_and_patch};
use hooks::{LoopHooksHandle, default_hooks};
use identity::IdentityMap;
pub(crate) use r#loop::RunInput;
pub use r#loop::{LoopExit, PauseReason};
use sidecars::{
    BalancedModelNotifier, ContextWindowCache, CumulativeUsage, LastMsgAtCache, SnapshotBufs,
    TokenEstimateCache, ToolDefCache, UsageTracker,
};
pub(crate) use state::ReActState;
use transcript::{ActionCard, ObservationCard, TranscriptEvent};

pub(crate) use snapshot_io::set_status_and_emit;
#[cfg(test)]
use tool_batch::FailureKind;

/// Convert a stored message attachment into a content part for the LLM.
/// Images become vision content parts (base64 payload); non-image file
/// attachments (persisted on disk with a `path`) become a short text
/// reference so the agent knows the file exists and where to read it with
/// the file tool —the raw bytes are never shipped to the model.
pub(crate) fn attachment_to_content_part(att: &MessageAttachment) -> ContentPart {
    if att.is_image() {
        haven_llm::media::image_part(&att.media_type, att.data.clone())
    } else {
        let name = att.filename.as_deref().unwrap_or("attachment");
        match &att.path {
            Some(path) => ContentPart::text(format!("[附件: {name}，路径: {path}]")),
            None => ContentPart::text(format!("[附件: {name}]")),
        }
    }
}

/// True when the canonical carries at least one image content part. Scanned
/// once per step (after `inject_pending_context`) and shared by the compactor
/// window selection and `choose_agent_role`, so the image check is not
/// repeated across every content part on each step.
pub(crate) fn canonical_has_image(messages: &[CanonicalMessage]) -> bool {
    messages.iter().any(|m| {
        m.content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. }))
    })
}

/// Pick the endpoint role for an agent step. Conversations that carry image
/// content parts route through the router's vision role — the dedicated
/// `image_model` (vision-capable) endpoint when configured, otherwise the
/// default model. Everything else uses the default model.
pub(super) async fn choose_agent_role(router: &LlmRouter, has_image: bool) -> EndpointRole {
    if has_image {
        router.vision_role().await
    } else {
        EndpointRole::DefaultModel
    }
}

/// A type-appropriate placeholder for a required JSON-schema field that has no
/// declared `default`, used to repair tool-call arguments that are missing a
/// required field. Keeps the call deserializable (avoiding a provider 400)
/// without inventing a semantic value the tool would act on.
fn placeholder_for_schema_type(ty: Option<&str>) -> serde_json::Value {
    match ty {
        Some("string") => serde_json::Value::String(String::new()),
        Some("integer") | Some("number") => serde_json::Value::Number(0.into()),
        Some("boolean") => serde_json::Value::Bool(false),
        Some("array") => serde_json::Value::Array(Vec::new()),
        Some("object") => serde_json::Value::Object(Default::default()),
        _ => serde_json::Value::Null,
    }
}

/// The fallback value for a schema property whose field is missing, null, or
/// holds a value that violates the schema: the declared `default`, else the
/// first enum value (enum-constrained discriminators like `action`/`operation`
/// must stay within the enum), else a type-appropriate placeholder.
fn schema_property_fallback(prop: &serde_json::Value) -> serde_json::Value {
    prop.get("default")
        .cloned()
        .or_else(|| {
            prop.get("enum")
                .and_then(|e| e.as_array())
                .and_then(|arr| arr.first().cloned())
        })
        .unwrap_or_else(|| placeholder_for_schema_type(prop.get("type").and_then(|t| t.as_str())))
}

/// Whether a value conforms to a schema property's type/enum constraints.
/// Detects tool-call inputs a strict provider would reject with a 400
/// ("Failed to deserialize the JSON body into the target type: input.<field>")
/// even though the field is present — e.g. an `action` set to a value outside
/// the declared enum, or a number where the schema declares a string.
fn value_conforms_to_prop(prop: &serde_json::Value, value: &serde_json::Value) -> bool {
    if let Some(enum_arr) = prop.get("enum").and_then(|e| e.as_array())
        && !enum_arr.contains(value)
    {
        return false;
    }
    let Some(ty) = prop.get("type") else {
        return true;
    };
    let matches = |t: &str| match t {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        // Unknown schema types (e.g. formats): don't guess, leave the value.
        _ => true,
    };
    match ty {
        serde_json::Value::String(t) => matches(t),
        serde_json::Value::Array(types) => types.iter().filter_map(|t| t.as_str()).any(matches),
        _ => true,
    }
}

/// RAII guard clearing a session's minted streaming-message ids when the
/// ReAct run exits (every path — early returns, `?` propagation, cancels),
/// so finished sessions never leave stale entries in [`IdentityMap`].
pub(super) struct RunMsgIdGuard<'a> {
    engine: &'a ReActEngine,
    session_id: String,
}

impl Drop for RunMsgIdGuard<'_> {
    fn drop(&mut self) {
        self.engine.clear_msg_ids_for_session(&self.session_id);
        self.engine.clear_run_budget(&self.session_id);
    }
}

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<SessionExecutor>,
    db: Arc<Database>,
    max_steps: Mutex<u32>,
    /// Optional session-lifetime step cap (Phase 8 / J1). `None` = unlimited.
    session_max_steps: Mutex<Option<u32>>,
    /// Hot-reloaded via [`Self::set_context_limits`] on settings save.
    context_limits: std::sync::Mutex<ContextLimitsConfig>,
    run_counter: AtomicU64,
    /// Queue/inbox source adapter; projection remains in `inject`.
    context_source: ContextSource,
    /// Per-session cumulative token usage.
    usage: UsageTracker,
    /// Per-session tool-definition cache (catalog version keyed).
    tool_defs: ToolDefCache,
    /// Newest message `created_at` per session (branch-point cutoff cache).
    last_msg_at: LastMsgAtCache,
    /// Per-session incremental token-estimate cache.
    token_estimates: TokenEstimateCache,
    /// Snapshot serialization buffers.
    snapshot_bufs: SnapshotBufs,
    /// Mid-run DB snapshot throttle (Phase 7 / F3).
    snapshot_store: Mutex<snapshot_io::SnapshotStore>,
    /// Per-role context-window cache keyed by router instance pointer.
    context_windows: ContextWindowCache,
    /// Per-session dedup for balanced-model-activated notifications.
    balanced_model: BalancedModelNotifier,
    /// Minted streaming-message ids (Phase 6 / I3).
    identity: IdentityMap,
    /// Domain side effects (inbox / compact / infer). Thin loop only calls
    /// `hooks.before_step` / `on_pause` (Phase 3 / G1).
    hooks: LoopHooksHandle,
    /// Optional fact engine for compaction-summary extraction (M3).
    inference: Option<Arc<crate::InferenceEngine>>,
    /// Live per-run budget mirrored into snapshots (R4). Cleared when the
    /// run exits so a later pause/resume cannot leak a stale budget.
    run_budgets: Mutex<HashMap<String, crate::types::RunBudget>>,
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

impl ReActEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<LlmRouter>,
        executor: Arc<SessionExecutor>,
        db: Arc<Database>,
        max_steps: u32,
        context_limits: ContextLimitsConfig,
    ) -> Self {
        let context_source = ContextSource::new(executor.clone(), db.clone());
        Self {
            router: Arc::new(RwLock::new(router)),
            executor,
            db,
            max_steps: Mutex::new(max_steps),
            session_max_steps: Mutex::new(None),
            context_limits: std::sync::Mutex::new(context_limits),
            run_counter: AtomicU64::new(0),
            context_source,
            usage: UsageTracker::new(),
            tool_defs: ToolDefCache::new(),
            last_msg_at: LastMsgAtCache::new(),
            token_estimates: TokenEstimateCache::new(),
            snapshot_bufs: SnapshotBufs::new(),
            snapshot_store: Mutex::new(snapshot_io::SnapshotStore::default()),
            context_windows: ContextWindowCache::new(),
            balanced_model: BalancedModelNotifier::new(),
            identity: IdentityMap::new(),
            hooks: default_hooks(),
            inference: None,
            run_budgets: Mutex::new(HashMap::new()),
        }
    }

    /// Record the live run budget so mid-run / pause snapshots include it (R4).
    pub(super) fn set_run_budget(&self, session_id: &str, budget: crate::types::RunBudget) {
        self.run_budgets
            .lock()
            .unwrap()
            .insert(session_id.to_string(), budget);
    }

    /// Drop the live run budget when the loop exits (any path).
    pub(super) fn clear_run_budget(&self, session_id: &str) {
        self.run_budgets.lock().unwrap().remove(session_id);
    }

    /// Snapshot of the live run budget for `session_id`, if any.
    pub(super) fn current_run_budget(&self, session_id: &str) -> Option<crate::types::RunBudget> {
        self.run_budgets.lock().unwrap().get(session_id).cloned()
    }

    /// Replace loop hooks (production: `default_hooks_with_infer`; tests:
    /// `hooks::NoopHooks` to skip inbox/infer).
    pub(crate) fn with_hooks(mut self, hooks: LoopHooksHandle) -> Self {
        self.hooks = hooks;
        self
    }

    /// Attach the shared [`crate::InferenceEngine`] for M3 summary→facts.
    pub(crate) fn with_inference(mut self, inference: Arc<crate::InferenceEngine>) -> Self {
        self.inference = Some(inference);
        self
    }

    /// Mint (or reuse) the id a streamed thought/reasoning block accumulates
    /// into (Phase 6 / I3 — delegates to [`IdentityMap`]).
    pub(super) fn ensure_msg_id(
        &self,
        session_id: &str,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> String {
        self.identity.ensure_msg_id(session_id, step, run, kind)
    }

    /// The id a streamed block is persisted under (minted or fresh fallback).
    pub(super) fn block_msg_id(
        &self,
        session_id: &str,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> String {
        self.identity.block_msg_id(session_id, step, run, kind)
    }

    /// Drop every minted message id belonging to a session.
    pub(super) fn clear_msg_ids_for_session(&self, session_id: &str) {
        self.identity.clear_for_session(session_id);
    }

    pub fn replace_router(&self, new_router: Arc<LlmRouter>) {
        *self.router.write().unwrap() = new_router;
    }

    /// Snapshot of `[context_limits]` (cloned; safe across awaits).
    pub(crate) fn limits(&self) -> ContextLimitsConfig {
        self.context_limits.lock().unwrap().clone()
    }

    /// Hot-reload `[context_limits]` from settings save and drop cached windows
    /// so the next step re-resolves against the new default.
    pub fn set_context_limits(&self, limits: ContextLimitsConfig) {
        *self.context_limits.lock().unwrap() = limits;
        self.context_windows.clear();
    }

    pub fn set_max_steps(&self, max_steps: u32) {
        *self.max_steps.lock().unwrap() = max_steps;
    }

    /// Set optional session-lifetime step cap (`None` = unlimited).
    pub fn set_session_max_steps(&self, session_max_steps: Option<u32>) {
        *self.session_max_steps.lock().unwrap() = session_max_steps;
    }

    pub fn next_run_id(&self) -> u64 {
        self.run_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Live three-way connectivity probe to the default-model endpoint. Used
    /// by the top-right status indicator to show 就绪 / 已断开 / 未配置.
    pub async fn check_connection(&self) -> haven_llm::LlmConnectionStatus {
        let router = self.router();
        router
            .connection_status(haven_llm::EndpointRole::DefaultModel)
            .await
    }

    pub(super) fn router(&self) -> Arc<LlmRouter> {
        self.router.read().unwrap().clone()
    }

    /// Build the full tool-definition list for a session: global registry tools
    /// plus per-session skill/MCP adapters registered via `load_skill`/`load_mcp`.
    /// Called each step so freshly loaded tools are immediately visible.
    ///
    /// G7 (X2 rethink) — **API `tools[]` is the schema authority.** The
    /// system prompt only embeds a short built-in / installable-skill /
    /// MCP-server **index** (names + one-line descriptions), frozen for the
    /// **current run** (`TOOL_USAGE_NOTES` declares this). Mid-run
    /// `load_skill` / `load_mcp` never rewrite the index; resume fully
    /// rebuilds the system prompt (X2) so catalog drift is picked up between
    /// runs. After `load_skill` / `load_mcp`, new tool schemas appear here on
    /// the next step; they are **not** spliced into the prompt index.
    ///
    /// The result is cached per session against the ToolsManager catalog version:
    /// the definitions only change when a per-session registration
    /// (`load_skill`/`load_mcp`) or a catalog rebuild bumps the version, so
    /// the registry query + JSON mapping is skipped on the vast majority of
    /// steps (the per-session registry query takes the global tools lock and
    /// rebuilds schema JSON on every step otherwise).
    pub(super) async fn build_tool_definitions_for_session(
        &self,
        session_id: &str,
    ) -> Arc<Vec<ToolDefinition>> {
        let version = self.executor.get_tools().catalog_version();
        if let Some(cached) = self.tool_defs.get_if_version(session_id, version) {
            return cached;
        }
        // Structured defs from the manager; the LLM-boundary conversion is a
        // pure `From<ToolDef>` so nothing here re-parses loose schema JSON.
        let defs: Arc<Vec<ToolDefinition>> = Arc::new(
            self.executor
                .get_tools()
                .list_defs_for_session(session_id)
                .await
                .into_iter()
                .map(Into::into)
                .collect(),
        );
        self.tool_defs
            .insert(session_id, version, Arc::clone(&defs));
        defs
    }

    pub fn note_last_msg_at(&self, session_id: &str, created_at: Option<String>) {
        self.last_msg_at.set(session_id, created_at);
    }

    /// Drop the cached newest-message timestamp (rollback / truncate).
    pub fn clear_last_msg_at(&self, session_id: &str) {
        self.last_msg_at.remove(session_id);
    }

    pub(super) async fn refresh_last_msg_at(&self, session_id: &str) -> Option<String> {
        let db = self.db.clone();
        let session_id_owned = session_id.to_string();
        let fetched = db
            .run_blocking(move |db| Ok(db.get_last_message_created_at(&session_id_owned)))
            .await
            .ok()
            .flatten();
        self.note_last_msg_at(session_id, fetched.clone());
        fetched
    }

    /// Supplement missing or invalid fields on a tool call's arguments before
    /// they reach the provider / tool. The model sometimes returns a call whose
    /// `arguments` is valid JSON but omits a field the tool's input schema
    /// marks required (e.g. an `action` discriminator) — most often after an
    /// interrupted/continued generation — or fills it with a value that
    /// violates the schema (wrong type, or not in the declared enum).
    /// Providers reject such a call with a 400 when deserializing the request
    /// body, so the ReAct loop repairs the arguments up front: a missing/null
    /// required field is filled from the schema's `default` when declared,
    /// otherwise from a type-appropriate placeholder; a present but
    /// schema-violating value is replaced the same way. Returns the number of
    /// actions that were repaired.
    pub(crate) async fn supplement_missing_required_fields(
        &self,
        session_id: &str,
        actions: &mut [Action],
    ) -> usize {
        let mut repaired = 0usize;
        for action in actions.iter_mut() {
            if action.is_final {
                continue;
            }
            let Some(tool) = self
                .executor
                .get_tools()
                .get_tool_for_session(Some(session_id), &action.tool_name)
                .await
            else {
                continue;
            };
            let schema = tool.input_schema();
            // A truncated/interrupted generation often yields UNPARSEABLE
            // arguments (parse_default_model_response falls back to Null),
            // so the call arrives without any object to repair. Normalize it
            // to an empty object before the schema checks so every non-object
            // input is covered — otherwise the bare Null reaches
            // validate_input and fails with "MISSING REQUIRED FIELD(S)" for
            // every required field (or a type error when the schema declares
            // none).
            if !action.tool_input.is_object() {
                action.tool_input = serde_json::json!({});
            }
            let required: Vec<&str> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let props = schema.get("properties").and_then(|p| p.as_object());
            let Some(obj) = action.tool_input.as_object_mut() else {
                continue;
            };
            let mut filled = 0usize;
            // First pass: every declared property. Repair present-but-invalid
            // values (wrong type / not in the enum / null for a typed field)
            // so the provider can deserialize the echoed tool_use input — a
            // 400 from a strict provider otherwise fails the whole step.
            // `value_conforms_to_prop` already honors explicit nullability
            // (`"type": ["string", "null"]`), so nulls are judged here, not
            // blanket-skipped.
            if let Some(props) = props {
                for (field, prop) in props {
                    let Some(value) = obj.get(field) else {
                        continue;
                    };
                    if value_conforms_to_prop(prop, value) {
                        continue;
                    }
                    let fallback = schema_property_fallback(prop);
                    tracing::warn!(
                        "repairing invalid value for field '{}' on tool call '{}': {:?} -> {:?}",
                        field,
                        action.tool_name,
                        value,
                        fallback
                    );
                    obj.insert(field.clone(), fallback);
                    filled += 1;
                }
            }
            // Second pass: required fields. A required field that is missing
            // (or present but null — the validator rejects null for typed
            // fields) is filled from the schema default / enum / placeholder.
            for field in required {
                let present = obj.get(field).is_some_and(|v| !v.is_null());
                if present {
                    continue;
                }
                let fallback = props
                    .and_then(|p| p.get(field))
                    .map(schema_property_fallback)
                    .unwrap_or(serde_json::Value::Null);
                tracing::warn!(
                    "supplementing missing required field '{}' on tool call '{}' with {:?}",
                    field,
                    action.tool_name,
                    fallback
                );
                obj.insert(field.to_string(), fallback);
                filled += 1;
            }
            if filled > 0 {
                repaired += 1;
            }
        }
        repaired
    }

    /// Parse LLM response into thought text and actions.
    pub fn parse_default_model_response(
        response: &LlmResponse,
        step_number: u32,
    ) -> (Option<String>, Vec<Action>) {
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

        let actions: Vec<Action> = if !response.tool_calls.is_empty() {
            response
                .tool_calls
                .iter()
                .map(|tc| {
                    let args = tc.arguments.clone();
                    // `final_answer` is the only name that marks a tool call
                    // as the conversation's final answer.
                    let is_final = tc.name == "final_answer";
                    Action {
                        tool_name: tc.name.clone(),
                        tool_input: args,
                        is_final,
                        tool_call_id: Some(if tc.id.is_empty() {
                            haven_common::types::new_id("call")
                        } else {
                            tc.id.clone()
                        }),
                    }
                })
                .collect()
        } else if !text.is_empty()
            && response.finish_reason == Some(FinishReason::Stop)
            && step_number > 0
        {
            vec![Action {
                tool_name: "final_answer".into(),
                tool_input: serde_json::Value::Null,
                is_final: true,
                tool_call_id: None,
            }]
        } else {
            Vec::new()
        };

        (thought, actions)
    }

    /// Mark the session Error without propagating DB failures (the loop is
    /// already unwinding through the Fatal outcome).
    pub(super) async fn mark_session_error(&self, session_id: &str) {
        if let Err(e) = self
            .executor
            .update_session_status(session_id, SessionStatus::Error)
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
    /// `role` is the endpoint that produced the response (used for cost
    /// lookup); `response` carries the token counts and model name;
    /// `step_number` is the ReAct step the call served and `duration_ms` its
    /// wall-clock duration, both recorded with the detail row.
    pub(super) async fn record_usage_and_emit(
        &self,
        session_id: &str,
        role: EndpointRole,
        response: &LlmResponse,
        step_number: i32,
        duration_ms: Option<u64>,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) {
        let usage = response.usage.clone().normalize();
        if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
            // No usage reported by the provider —nothing useful to surface.
            return;
        }

        let router = self.router();
        let step_cost = router.compute_cost(role, &usage).await;
        // `context_window_for_role` always yields Some; the cached resolver
        // avoids cloning the full LlmConfig on every step.
        let context_window = Some(self.cached_context_window(role).await);

        let totals = self.usage.record_with_seed(
            session_id,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens,
            usage.cached_tokens,
            usage.cache_creation_tokens,
            usage.cache_miss_tokens(),
            step_cost,
            || {
                // Seed from persisted counters when this session was resumed or
                // reopened: the in-memory map is cleared on session completion
                // (and lost on restart), but the DB row keeps the running
                // totals so cumulative stats stay valid across sessions.
                self.db
                    .get_session_usage(session_id)
                    .ok()
                    .flatten()
                    .map(CumulativeUsage::from)
                    .unwrap_or_default()
            },
        );
        let cum_prompt = totals.prompt_tokens;
        let cum_completion = totals.completion_tokens;
        let cum_total = totals.total_tokens;
        let cum_cached = totals.cached_tokens;
        let cum_cache_creation = totals.cache_creation_tokens;
        let cum_cache_miss = totals.cache_miss_tokens;
        let cum_cost_opt = totals.cost_usd;

        let model = response.model.clone().or_else(|| usage.model_name.clone());

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

        // Persist one per-call detail row and rebuild `session_usage` from the
        // SUM of remaining detail rows (not the in-memory absolute totals).
        // Await the blocking write before emitting the usage event: a user can
        // pause and immediately reopen a session after seeing that event, and
        // resume must observe the same cumulative counters as the live UI.
        // An epoch captured here is checked inside the task so a rollback that
        // truncates usage after this spawn cannot be undone by a late insert.
        let db = self.db.clone();
        let session_id_for_persist = session_id.to_string();
        let call_cost = step_cost.unwrap_or(0.0);
        let call_has_cost = step_cost.is_some();
        let model_for_persist = model.clone();
        let usage_prompt = usage.prompt_tokens;
        let usage_completion = usage.completion_tokens;
        let usage_total = usage.total_tokens;
        let usage_cached = usage.cached_tokens;
        let usage_cache_creation = usage.cache_creation_tokens;
        let usage_cache_miss = usage.cache_miss_tokens();
        let usage_cache_diagnostics = usage
            .cache_diagnostics
            .as_ref()
            .and_then(|diagnostics| serde_json::to_string(diagnostics).ok());
        let persist_epoch = self.usage.epoch(session_id);
        let epochs = self.usage.epochs_handle();
        let persist = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let epoch_now = || {
                epochs
                    .lock()
                    .unwrap()
                    .get(&session_id_for_persist)
                    .copied()
                    .unwrap_or(0)
            };
            if epoch_now() != persist_epoch {
                return Ok(());
            }
            let rec = db.persist_llm_call_and_refresh_session_usage_with_cache_accounting(
                &session_id_for_persist,
                Some(step_number),
                role.as_str(),
                model_for_persist.as_deref(),
                usage_prompt,
                usage_completion,
                usage_total,
                usage_cached,
                usage_cache_creation,
                usage_cache_miss,
                usage.cache_accounting.as_str(),
                usage_cache_diagnostics.as_deref(),
                call_cost,
                call_has_cost,
                duration_ms,
            )?;
            // Rollback may have truncated between the pre-check and the
            // insert; drop the phantom row and rebuild so totals stay true.
            if epoch_now() != persist_epoch {
                let _ = db.delete_llm_usage_by_id(&rec.id);
                let _ = db.rebuild_session_usage_from_calls(&session_id_for_persist);
            }
            Ok(())
        });
        match persist.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(
                "ReAct: failed to persist usage for session {} step {}: {}",
                session_id,
                step_number,
                e
            ),
            Err(e) => tracing::warn!(
                "ReAct: usage persistence task failed for session {} step {}: {}",
                session_id,
                step_number,
                e
            ),
        }

        EventDispatcher::emit_usage_from(
            emitter,
            UsagePayload {
                session_id: session_id.to_string(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                cache_miss_tokens: usage.cache_miss_tokens(),
                context_tokens: usage.context_tokens(),
                cache_exclusive: usage.cache_exclusive_of_prompt(),
                cache_accounting: usage.cache_accounting.as_str().into(),
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
                role: Some(role.as_str().to_string()),
                has_cost: call_has_cost,
            },
        )
        .await;
    }

    /// Drop cumulative counters, the token-estimate cache, the snapshot
    /// buffer, the snapshot-throttle marker and the tool-definition cache for
    /// a finished session so all per-session maps stay bounded across long-running
    /// sessions.
    pub fn reset_cumulative_usage(&self, session_id: &str) {
        self.usage.reset(session_id);
        self.reset_token_estimate(session_id);
        self.snapshot_store
            .lock()
            .unwrap()
            .clear_session(session_id);
        self.tool_defs.remove(session_id);
        self.last_msg_at.remove(session_id);
        self.snapshot_bufs.remove(session_id);
        self.context_source.clear_session(session_id);
    }

    /// After rollback/truncate rebuilt `session_usage` from remaining
    /// `llm_usage` rows: clear in-memory counters and bump the persist epoch
    /// so a late fire-and-forget write from a discarded call cannot re-inflate
    /// the totals. Next live usage event re-seeds from the rebuilt DB row.
    pub fn invalidate_usage_after_truncate(&self, session_id: &str) {
        self.usage.invalidate_after_truncate(session_id);
    }

    /// Resolve the model's true context window for the endpoint used by
    /// Explicit `context_window` on the role/endpoint when set. Callers fall
    /// back to `context_limits.default_context_window`. Prefer writing the
    /// window from provider `/models` metadata into the role slot when the
    /// user picks a model. This is the real input budget for the token-usage
    /// display, not the per-response output cap (`max_tokens`).
    pub(super) fn context_window_for_role(
        cfg: &haven_common::config::RouterConfig,
        role: EndpointRole,
    ) -> Option<u32> {
        haven_llm::registry::context_window_for(cfg.endpoint(role))
    }

    /// Resolve the model's true context window for `role` using a per-router
    /// cache. Cloning the full LlmConfig on every step (compactor window +
    /// usage display) is wasteful when the router only changes via
    /// `replace_router`; the cache is keyed by the router instance pointer so
    /// a hot-swapped router invalidates it immediately.
    pub(super) async fn cached_context_window(&self, role: EndpointRole) -> u32 {
        let router = self.router();
        let ptr = Arc::as_ptr(&router) as usize;
        // Fast path: read the cached window without awaiting the router
        // config. The cache guard is scoped so it never crosses an await
        // (the std Mutex guard is not Send).
        if let Some(window) = self.context_windows.get(ptr, role) {
            return window;
        }
        // Slow path: resolve from the live router config. A concurrent
        // router swap between the fast-path miss and the insert is harmless:
        // the entry is stored under the pointer that was current at read
        // time and recomputed on the next miss.
        let cfg = router.config().await;
        let window = Self::context_window_for_role(&cfg, role)
            .unwrap_or(self.limits().default_context_window);
        self.context_windows.insert(ptr, role, window);
        window
    }

    /// Build a compactor whose context window reflects the *actual* model for
    /// the role that will handle the step (explicit `context_window` on the
    /// role, else `context_limits.default_context_window`). The window comes
    /// from `cached_context_window`, so a hot-swapped router config takes
    /// effect immediately without cloning the full config on every step. The
    /// compaction threshold (ratio and reserve) and the fallback window come
    /// from `context_limits`.
    pub(super) async fn context_compactor(&self, role: EndpointRole) -> ContextCompactor {
        let window = self.cached_context_window(role).await;
        let limits = self.limits();
        ContextCompactor::with_ratio(
            window,
            limits.compaction_reserve_tokens,
            limits.compaction_ratio,
        )
    }

    /// Incremental token estimate for a session's canonical message list.
    ///
    /// The estimate is cached per session: each step adds only the token count
    /// of the messages appended since the last pass instead of re-tokenizing
    /// the whole history (which is O(n) per step, O(n^2) over a long session).
    /// A full pass re-runs every `FULL_ESTIMATE_PASS_INTERVAL` calls and
    /// whenever the list shrank (sanitize drops, compaction), which bounds
    /// drift from mid-array inserts and from restored snapshots whose length
    /// coincidentally matches the cache. Under-counting by one message's
    /// worth of tokens is acceptable: the forced-compaction 400 retry remains
    /// the safety net for genuine overflow.
    pub(super) fn estimate_canonical_tokens(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
    ) -> u32 {
        self.token_estimates.estimate(session_id, canonical)
    }

    /// Drop the per-session token-estimate cache entry (called alongside
    /// `reset_cumulative_usage` on session completion/error).
    pub fn reset_token_estimate(&self, session_id: &str) {
        self.token_estimates.remove(session_id);
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
        has_image: bool,
    ) -> bool {
        if state.canonical.len() < 4 {
            return false;
        }
        // The compaction window must match the endpoint the next step will
        // use (image-routed steps compact against the image model's budget),
        // mirroring choose_agent_role's role selection.
        let router = self.router();
        let role = if has_image {
            router.vision_role().await
        } else {
            EndpointRole::DefaultModel
        };
        let compactor = self.context_compactor(role).await;
        // Compare the incremental estimate against the threshold directly;
        // `needs_compaction` would re-estimate the whole canonical and undo
        // the incremental cache.
        if self.estimate_canonical_tokens(&ctx.session_id, &state.canonical)
            <= compactor.threshold_tokens()
        {
            return false;
        }
        if let Some(result) = compactor.compact(&state.canonical, &router).await {
            tracing::info!(
                "compaction for session {}: {} tokens -> {} tokens ({} msgs summarized)",
                ctx.session_id,
                result.tokens_before,
                result.tokens_after,
                result.summarized_count
            );
            // Compaction replaced the list wholesale: the incremental
            // estimate is stale, drop it so the next step does a full pass.
            self.reset_token_estimate(&ctx.session_id);
            self.apply_transcript(
                ctx,
                TranscriptEvent::CompactSummary {
                    compacted: result.compacted,
                    summary: result.summary,
                    tokens_before: result.tokens_before,
                    tokens_after: result.tokens_after,
                    episode_id: result.episode_id,
                },
                state,
            )
            .await;
            true
        } else {
            false
        }
    }

    /// Emit balanced model activated with per-session deduplication.
    pub(super) async fn emit_balanced_model(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        reason: &str,
    ) {
        if self.balanced_model.try_mark(session_id) {
            EventDispatcher::emit_balanced_model_activated_from(emitter, session_id, reason).await;
        }
    }

    /// Emit session error and clean up balanced model dedup state.
    pub(super) async fn emit_error(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        error: &str,
    ) {
        tracing::error!("ReAct session {} error: {}", session_id, error);
        self.balanced_model.clear(session_id);
        EventDispatcher::emit_session_error_from(emitter, session_id, error).await;
        self.reset_cumulative_usage(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use haven_common::types::{CanonicalRole, CanonicalToolCall};

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

    struct MockLlm;

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: chat not implemented".into()))
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
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
            _: Vec<ToolDefinition>,
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
        let client: Arc<dyn LlmClient> = Arc::new(MockLlm);
        LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        )
    }

    // ── failure classification & retry nudge (G5: nudge text only; attach
    // onto tool observations is covered in tool_batch::tests) ──────────────

    #[test]
    fn classify_environmental_command_missing() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "'Get-FileHash' is not recognized as the name of a cmdlet, function, script file, or operable program"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "'curl' 不是内部或外部命令，也不是可运行的程序或批处理文件"
            ),
            FailureKind::Environmental
        );
    }

    #[test]
    fn classify_environmental_network() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "http",
                "tcp connect error: A connection attempt failed because the connected party did not properly respond"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "shell",
                "curl: (7) Failed to connect to host port 443: Connection refused"
            ),
            FailureKind::Environmental
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "download timed out after 60s"),
            FailureKind::Environmental
        );
    }

    #[test]
    fn classify_environmental_paths() {
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "7z: cannot find archive path"),
            FailureKind::Environmental
        );
    }

    #[test]
    fn classify_logic_usage_errors() {
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "files",
                "input validation failed for 'files': MISSING REQUIRED FIELD(S): operation"
            ),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure(
                "files",
                "'old_string' is required for edit operation"
            ),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("files", "old_string not found in file"),
            FailureKind::Logic
        );
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "invalid json in script"),
            FailureKind::Logic
        );
    }

    #[test]
    fn classify_unknown_falls_back() {
        assert_eq!(
            ReActEngine::classify_tool_failure("shell", "something odd happened"),
            FailureKind::Unknown
        );
    }

    #[test]
    fn failure_nudge_environmental_keeps_approach() {
        let nudge = ReActEngine::build_failure_nudge(&[(
            "shell".into(),
            "curl: (7) Failed to connect: Connection refused".into(),
        )]);
        assert!(
            !nudge.contains("completely different approach"),
            "environmental failures must not force a method switch, got: {nudge}"
        );
        assert!(nudge.contains("ENVIRONMENTAL"), "got: {nudge}");
        assert!(
            nudge.contains("curl"),
            "should mention tool switching, got: {nudge}"
        );
    }

    #[test]
    fn failure_nudge_logic_allows_method_switch_after_fix() {
        let nudge = ReActEngine::build_failure_nudge(&[(
            "files".into(),
            "'old_string' is required for edit operation".into(),
        )]);
        assert!(nudge.contains("logic errors"), "got: {nudge}");
        assert!(
            nudge.contains(
                "Only consider a completely different approach if the same method fails again"
            ),
            "method switch must be gated, got: {nudge}"
        );
    }

    #[test]
    fn failure_nudge_empty_falls_back_to_generic() {
        let nudge = ReActEngine::build_failure_nudge(&[]);
        // The generic branch reuses the canonical system-prompt guidance.
        assert!(
            nudge.contains(haven_common::prompts::TOOL_FAILURE_DIAGNOSIS),
            "got: {nudge}"
        );
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

    fn msgs_contain_image(messages: &[CanonicalMessage]) -> bool {
        messages.iter().any(|m| {
            m.content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }))
        })
    }

    #[tokio::test]
    async fn choose_agent_role_default_without_images() {
        let router = mock_router();
        let messages = [
            text_msg(CanonicalRole::System, "be concise"),
            text_msg(CanonicalRole::User, "hello"),
        ];
        let has_image = msgs_contain_image(&messages);
        assert_eq!(
            choose_agent_role(&router, has_image).await,
            EndpointRole::DefaultModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_default_when_image_model_unconfigured() {
        let router = mock_router();
        let messages = [image_msg(CanonicalRole::User)];
        let has_image = msgs_contain_image(&messages);
        assert_eq!(
            choose_agent_role(&router, has_image).await,
            EndpointRole::DefaultModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_image_model_when_configured() {
        let router = mock_router();
        router
            .force_role_configured(EndpointRole::ImageModel, true)
            .await;
        let messages = [image_msg(CanonicalRole::User)];
        let has_image = msgs_contain_image(&messages);
        assert_eq!(
            choose_agent_role(&router, has_image).await,
            EndpointRole::ImageModel
        );
    }

    #[tokio::test]
    async fn choose_agent_role_default_when_image_routing_disabled() {
        let router = mock_router();
        router
            .force_role_configured(EndpointRole::ImageModel, true)
            .await;
        router.force_routing_flags(true, false).await;
        let messages = [image_msg(CanonicalRole::User)];
        let has_image = msgs_contain_image(&messages);
        assert_eq!(
            choose_agent_role(&router, has_image).await,
            EndpointRole::DefaultModel
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
    fn parse_empty_response_no_actions() {
        let r = resp("", vec![], None);
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought, None);
        assert!(actions.is_empty());
    }

    #[test]
    fn parse_text_only_no_finish_reason_keeps_thought_no_action() {
        // step_number=1, Stop finish, but step>0 required for implicit final.
        let r = resp("hello", vec![], Some(FinishReason::Stop));
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 0);
        assert_eq!(thought.as_deref(), Some("hello"));
        assert!(actions.is_empty(), "step 0 must not auto-finalize");
    }

    #[test]
    fn parse_text_with_stop_finish_step_nonzero_auto_finalizes() {
        let r = resp("the answer is 42", vec![], Some(FinishReason::Stop));
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("the answer is 42"));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
        assert_eq!(actions[0].tool_name, "final_answer");
        assert!(actions[0].tool_call_id.is_none());
    }

    #[test]
    fn parse_tool_calls_produce_actions() {
        let tc = CanonicalToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "x.txt"}),
        };
        let r = resp("thinking", vec![tc], Some(FinishReason::ToolCalls));
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("thinking"));
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
        assert_eq!(actions[0].tool_name, "read_file");
        assert_eq!(actions[0].tool_input["path"], "x.txt");
        assert_eq!(actions[0].tool_call_id.as_deref(), Some("call_1"));
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
            let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
            assert_eq!(thought, None, "{text:?} must not become a thought bubble");
            assert_eq!(actions.len(), 1);
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
        let (thought, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("正在读取文件。"));
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn parse_final_answer_tool_call_marked_final() {
        let tc = CanonicalToolCall {
            id: "c2".into(),
            name: "final_answer".into(),
            arguments: serde_json::json!({"answer": "done"}),
        };
        let r = resp("answering", vec![tc], Some(FinishReason::ToolCalls));
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 2);
        assert!(actions[0].is_final);
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
            let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
            assert!(!actions[0].is_final, "{name} must not be final");
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
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert!(actions[0].tool_call_id.is_some());
        assert!(!actions[0].tool_call_id.as_ref().unwrap().is_empty());
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
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].tool_name, "search");
        assert_eq!(actions[1].tool_name, "read_file");
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
        let (_, actions) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
    }

    #[test]
    fn parse_text_trimmed_for_thought() {
        let r = resp("  spaced thought  ", vec![], Some(FinishReason::Stop));
        let (thought, _) = ReActEngine::parse_default_model_response(&r, 1);
        assert_eq!(thought.as_deref(), Some("spaced thought"));
    }

    fn ask_tool_msg(question: &str) -> CanonicalMessage {
        CanonicalMessage::tool(
            vec![ContentPart::text(format!(
                r#"{{"ask":true,"question":"{question}","awaiting_answer":true,"options":[]}}"#
            ))],
            Some("call_ask".into()),
        )
    }
    #[test]
    fn pending_ask_true_when_ask_result_unanswered() {
        let canonical = vec![
            CanonicalMessage::user_text("help me"),
            ask_tool_msg("which file?"),
        ];
        assert!(ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn pending_ask_false_when_user_message_follows_ask() {
        let canonical = vec![
            CanonicalMessage::user_text("help me"),
            ask_tool_msg("which file?"),
            CanonicalMessage::user_text("Answer to your previous question: the first one"),
        ];
        assert!(!ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn pending_ask_false_when_no_ask_tool_result() {
        let canonical = vec![
            CanonicalMessage::user_text("help me"),
            CanonicalMessage::tool(
                vec![ContentPart::text(r#"{"success":true,"output":"ok"}"#)],
                Some("call_x".into()),
            ),
        ];
        assert!(!ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn pending_ask_false_when_user_message_before_ask() {
        let canonical = vec![
            CanonicalMessage::user_text("first question"),
            ask_tool_msg("second question?"),
        ];
        // The user message precedes the ask result: still pending.
        assert!(ReActEngine::canonical_has_pending_ask(&canonical));
    }

    #[test]
    fn extract_pending_ask_question_reads_last_ask() {
        let canonical = vec![ask_tool_msg("first?"), ask_tool_msg("second?")];
        assert_eq!(
            ReActEngine::extract_pending_ask_question(&canonical),
            "second?"
        );
    }

    #[test]
    fn extract_pending_ask_question_falls_back_on_unparseable_output() {
        let canonical = vec![CanonicalMessage::tool(
            vec![ContentPart::text("truncated {\"ask\":true,\"quest")],
            Some("call_ask".into()),
        )];
        assert_eq!(
            ReActEngine::extract_pending_ask_question(&canonical),
            "I have a pending question for you."
        );
    }
}
