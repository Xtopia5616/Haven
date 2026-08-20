use std::collections::HashMap;
use std::collections::HashSet;
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

use crate::compactor::{ContextCompactor, estimate_message_tokens};
use crate::event::{AgentEvent, AgentEventEmitter, EventDispatcher, UsagePayload};
use crate::types::{Action, BranchPoint, ReActStep};
use chrono::Utc;

mod hooks;
mod inject;
mod r#loop;
mod retries;
mod snapshot_io;
mod stream_step;
mod tool_batch;

use hooks::{LoopHooksHandle, default_hooks};

use inject::MessagingState;

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

/// Key identifying one streamed block within a run: session, step number,
/// run id and block kind ("thought" | "reasoning").
pub(super) type StreamBlockKey = (String, u32, u64, &'static str);

/// RAII guard clearing a session's minted streaming-message ids when the
/// ReAct run exits (every path — early returns, `?` propagation, cancels),
/// so finished sessions never leave stale entries in `step_msg_ids`.
pub(super) struct RunMsgIdGuard<'a> {
    engine: &'a ReActEngine,
    session_id: String,
}

impl Drop for RunMsgIdGuard<'_> {
    fn drop(&mut self) {
        self.engine.clear_msg_ids_for_session(&self.session_id);
    }
}

pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<SessionExecutor>,
    db: Arc<Database>,
    max_steps: Mutex<u32>,
    context_limits: ContextLimitsConfig,
    balanced_model_notified: Mutex<HashSet<String>>,
    run_counter: AtomicU64,
    /// Per-session cumulative token usage. Keyed by `session_id` so multiple
    /// parallel sessions each track their own counters. Reset on session
    /// completion to avoid leaking finished-session entries.
    cumulative_usage: Mutex<HashMap<String, CumulativeUsage>>,
    /// Cross-session messaging integration: heartbeat + automatic inbox
    /// polling driven by in-process delivery notifications (see
    /// `maybe_poll_inbox`).
    messaging: Mutex<MessagingState>,
    /// Reusable per-session serialization buffers for ReAct snapshots (see
    /// `save_snapshot_with_branches`): avoids a fresh allocation for every
    /// per-step snapshot write. Keyed by `session_id` so parallel sessions never
    /// contend on one shared buffer (a long session's canonical+history can be
    /// sizable).
    snapshot_bufs: Mutex<HashMap<String, Vec<u8>>>,
    /// Per-session incremental token-estimate cache (see
    /// `estimate_canonical_tokens`): avoids re-tokenizing the whole canonical
    /// on every step.
    token_estimate_cache: Mutex<HashMap<String, TokenEstimate>>,
    /// Per-role context-window cache keyed by the router instance pointer,
    /// so per-step compactor construction and usage display do not clone the
    /// full LlmConfig on every step (the router only changes via
    /// `replace_router`).
    context_window_cache: Mutex<(usize, HashMap<EndpointRole, u32>)>,
    /// Per-session step number of the last DB snapshot write (see
    /// `save_branch_point`): mid-run snapshot writes are throttled to every
    /// `SNAPSHOT_WRITE_INTERVAL` steps; pause/error/final and cancellation
    /// exit paths always write.
    last_snapshot_step: Mutex<HashMap<String, u32>>,
    /// Per-session tool-definition cache keyed by the ToolsManager catalog
    /// version (see `build_tool_definitions_for_session`): the definitions are
    /// rebuilt only when a skill/MCP per-session registration or a catalog
    /// rebuild bumps the version, instead of re-querying the registry on
    /// every step.
    tool_def_cache: Mutex<HashMap<String, (u64, Vec<ToolDefinition>)>>,
    /// Minted streaming-message ids: a `StreamBlockKey` → the
    /// `msg-*` id a streamed thinking/reasoning block accumulates into. The
    /// id is minted when the block's first chunk streams, reused by every
    /// chunk event, the `agent:thought` snap, and the final
    /// `persist_session_message` — so the live bubble and the DB row share
    /// one identity and the frontend merge needs no content dedup.
    /// Cleared per session at `run_react_loop` entry (one run = one loop
    /// invocation), so entries never leak across runs.
    step_msg_ids: Mutex<HashMap<StreamBlockKey, String>>,
    /// Domain side effects (inbox / compact / infer). Thin loop only calls
    /// `hooks.before_step` / `on_pause` (Phase 3 / G1).
    hooks: LoopHooksHandle,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CumulativeUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cost_usd: f64,
    has_cost: bool,
}

/// Incremental token estimate for a session's canonical message list (see
/// `ReActEngine::estimate_canonical_tokens`). `tokens` is the estimate at the
/// last full tokenization pass, when the canonical had `msgs_len` messages.
#[derive(Debug, Clone, Default)]
pub(super) struct TokenEstimate {
    /// canonical length at the last full tokenization pass
    msgs_len: usize,
    /// estimated tokens at that pass
    tokens: u32,
    /// number of estimation calls so far (drives the periodic full pass)
    passes: u32,
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
    Response(LlmResponse),
    /// Cancelled mid-call (end_session / rollback): exit silently.
    Cancelled,
    /// Persisted/emitted error already; the loop must propagate it.
    Fatal(String),
}

/// Why the ReAct run paused (Phase 2 / C2). Status is already written by
/// `pause_turn*` before the loop returns; this only labels the exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// Text-only / `final_answer` turn end.
    TurnEnd,
    /// `ask` tool (or pending-ask re-surface).
    Ask,
    /// Per-run `max_steps` exhausted.
    Budget,
    /// Observed Paused* at step head or mid-batch (external flip).
    External,
}

/// Explicit exit from `run_react_loop` (Phase 2 / C2). Replaces bare `Ok(())`
/// so cancel / pause / complete / soft-error are distinguishable. Hard
/// failures still return `Err`. Host maps non-`Error` exits to dispatcher
/// success so the permit is released via `unmark_running`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopExit {
    Paused { reason: PauseReason },
    Cancelled,
    Completed,
    Error(String),
}

impl From<haven_memory::repositories::usage::SessionUsage> for CumulativeUsage {
    fn from(u: haven_memory::repositories::usage::SessionUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cost_usd: u.cost_usd,
            has_cost: u.has_cost,
        }
    }
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
        Self {
            router: Arc::new(RwLock::new(router)),
            executor,
            db,
            max_steps: Mutex::new(max_steps),
            context_limits,
            balanced_model_notified: Mutex::new(HashSet::new()),
            run_counter: AtomicU64::new(0),
            cumulative_usage: Mutex::new(HashMap::new()),
            messaging: Mutex::new(MessagingState::new()),
            snapshot_bufs: Mutex::new(HashMap::new()),
            token_estimate_cache: Mutex::new(HashMap::new()),
            context_window_cache: Mutex::new((0, HashMap::new())),
            last_snapshot_step: Mutex::new(HashMap::new()),
            tool_def_cache: Mutex::new(HashMap::new()),
            step_msg_ids: Mutex::new(HashMap::new()),
            hooks: default_hooks(),
        }
    }

    /// Replace loop hooks (tests: `hooks::NoopHooks` to skip inbox/infer).
    #[cfg(test)]
    #[allow(dead_code)] // available for thin-loop tests that construct an engine
    pub(crate) fn with_hooks(mut self, hooks: LoopHooksHandle) -> Self {
        self.hooks = hooks;
        self
    }

    /// Mint (or reuse) the id a streamed thought/reasoning block of
    /// `(session, step, run, kind)` accumulates into. Constant per block so
    /// chunk events, the snap and the final persistence share one id.
    ///
    /// A `thought` block is the content view of a ReAct step: its id is
    /// minted with the `step-` prefix so the message row and the thought
    /// step row (created in `emit_thought_from` under the same id) are one
    /// entity. `reasoning` blocks have no step row and keep `msg-` ids.
    pub(super) fn ensure_msg_id(&self, session_id: &str, step: u32, run: u64, kind: &'static str) -> String {
        let mut map = self.step_msg_ids.lock().unwrap();
        map.entry((session_id.to_string(), step, run, kind))
            .or_insert_with(|| {
                let prefix = if kind == "thought" { "step" } else { "msg" };
                haven_common::types::new_id(prefix)
            })
            .clone()
    }

    /// Read the minted id for a block without consuming it. `None` when the
    /// block never streamed (the caller then falls back to a fresh id).
    pub(super) fn peek_msg_id(
        &self,
        session_id: &str,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> Option<String> {
        self.step_msg_ids
            .lock()
            .unwrap()
            .get(&(session_id.to_string(), step, run, kind))
            .cloned()
    }

    /// The id a streamed block is persisted under: the minted id when the
    /// block streamed (the live bubble and the DB row must match), a fresh
    /// id otherwise (prefix follows `ensure_msg_id`'s per-kind rule). THE
    /// single definition of that fallback — every persist site must go
    /// through here so the "persisted id == streamed bubble id" invariant
    /// cannot drift per site.
    pub(super) fn block_msg_id(&self, session_id: &str, step: u32, run: u64, kind: &'static str) -> String {
        self.peek_msg_id(session_id, step, run, kind)
            .unwrap_or_else(|| {
                let prefix = if kind == "thought" { "step" } else { "msg" };
                haven_common::types::new_id(prefix)
            })
    }

    /// Drop every minted message id belonging to a session. Runs once per
    /// `run_react_loop` invocation so stale ids from a previous run never
    /// leak or collide with a fresh run's minted ids.
    pub(super) fn clear_msg_ids_for_session(&self, session_id: &str) {
        self.step_msg_ids
            .lock()
            .unwrap()
            .retain(|(sid, ..), _| sid != session_id);
    }

    pub fn replace_router(&self, new_router: Arc<LlmRouter>) {
        *self.router.write().unwrap() = new_router;
    }

    pub fn set_max_steps(&self, max_steps: u32) {
        *self.max_steps.lock().unwrap() = max_steps;
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
    /// The result is cached per session against the ToolsManager catalog version:
    /// the definitions only change when a per-session registration
    /// (`load_skill`/`load_mcp`) or a catalog rebuild bumps the version, so
    /// the registry query + JSON mapping is skipped on the vast majority of
    /// steps (the per-session registry query takes the global tools lock and
    /// rebuilds schema JSON on every step otherwise).
    pub(super) async fn build_tool_definitions_for_session(&self, session_id: &str) -> Vec<ToolDefinition> {
        let version = self.executor.get_tools().catalog_version();
        if let Some(cached) = self.tool_def_cache.lock().unwrap().get(session_id).cloned()
            && cached.0 == version
        {
            return cached.1;
        }
        // Structured defs from the manager; the LLM-boundary conversion is a
        // pure `From<ToolDef>` so nothing here re-parses loose schema JSON.
        let defs: Vec<ToolDefinition> = self
            .executor
            .get_tools()
            .list_defs_for_session(session_id)
            .await
            .into_iter()
            .map(Into::into)
            .collect();
        self.tool_def_cache
            .lock()
            .unwrap()
            .insert(session_id.to_string(), (version, defs.clone()));
        defs
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

        let thought = if text.is_empty() {
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
        let usage = &response.usage;
        if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
            // No usage reported by the provider —nothing useful to surface.
            return;
        }

        let router = self.router();
        let step_cost = router
            .compute_cost(role, usage.prompt_tokens, usage.completion_tokens)
            .await;
        // `context_window_for_role` always yields Some; the cached resolver
        // avoids cloning the full LlmConfig on every step.
        let context_window = Some(self.cached_context_window(role).await);

        let (cum_prompt, cum_completion, cum_total, cum_cost_opt, has_cost) = {
            let mut map = self.cumulative_usage.lock().unwrap();
            let entry = map.entry(session_id.to_string()).or_insert_with(|| {
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
            });
            entry.prompt_tokens = entry.prompt_tokens.saturating_add(usage.prompt_tokens);
            entry.completion_tokens = entry
                .completion_tokens
                .saturating_add(usage.completion_tokens);
            entry.total_tokens = entry.total_tokens.saturating_add(usage.total_tokens);
            if let Some(c) = step_cost {
                entry.cost_usd += c;
                entry.has_cost = true;
            }
            let cum_cost = if entry.has_cost {
                Some(entry.cost_usd)
            } else {
                None
            };
            (
                entry.prompt_tokens,
                entry.completion_tokens,
                entry.total_tokens,
                cum_cost,
                entry.has_cost,
            )
        };

        let model = response.model.clone().or_else(|| usage.model_name.clone());

        tracing::debug!(
            "ReAct step {} session {} LLM usage: {}/{}/{} tokens, {} ms, model={:?}",
            step_number,
            session_id,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens,
            duration_ms.unwrap_or(0),
            model
        );

        // Persist the cumulative counters AND one per-call detail row so a
        // resumed session (after session completion or app restart) restores the
        // correct token-stats display instead of restarting from zero, and
        // keeps the granular history behind it. Fire-and-forget on a blocking
        // thread: the autocommit hits the disk/fsync path and awaiting it
        // would stall the agent step loop on every usage event (the previous
        // awaited variant serialized each step behind the disk write). A
        // dropped handle only risks losing the final step's counters on a
        // hard kill — the next usage event rewrites the absolute totals, and
        // the in-memory map stays authoritative for the running session.
        let db = self.db.clone();
        let session_id_for_persist = session_id.to_string();
        let cum_cost = cum_cost_opt.unwrap_or(0.0);
        let call_cost = step_cost.unwrap_or(0.0);
        let call_has_cost = step_cost.is_some();
        let model_for_persist = model.clone();
        let usage_prompt = usage.prompt_tokens;
        let usage_completion = usage.completion_tokens;
        let usage_total = usage.total_tokens;
        let persist = tokio::task::spawn_blocking(move || {
            let _ = db.update_session_usage(
                &session_id_for_persist,
                cum_prompt,
                cum_completion,
                cum_total,
                cum_cost,
                has_cost,
            );
            let _ = db.record_llm_call_usage(
                &session_id_for_persist,
                Some(step_number),
                role.as_str(),
                model_for_persist.as_deref(),
                usage_prompt,
                usage_completion,
                usage_total,
                call_cost,
                call_has_cost,
                duration_ms,
            );
        });
        // Detach: the write completes on the blocking pool without the step
        // loop waiting for it.
        drop(persist);

        EventDispatcher::emit_usage_from(
            emitter,
            UsagePayload {
                session_id: session_id.to_string(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cost_usd: step_cost,
                model,
                cumulative_prompt_tokens: cum_prompt,
                cumulative_completion_tokens: cum_completion,
                cumulative_total_tokens: cum_total,
                cumulative_cost_usd: cum_cost_opt,
                context_window,
            },
        )
        .await;
    }

    /// Drop cumulative counters, the token-estimate cache, the snapshot
    /// buffer, the snapshot-throttle marker and the tool-definition cache for
    /// a finished session so all per-session maps stay bounded across long-running
    /// sessions.
    pub fn reset_cumulative_usage(&self, session_id: &str) {
        let mut map = self.cumulative_usage.lock().unwrap();
        map.remove(session_id);
        drop(map);
        self.reset_token_estimate(session_id);
        self.last_snapshot_step.lock().unwrap().remove(session_id);
        self.tool_def_cache.lock().unwrap().remove(session_id);
        self.snapshot_bufs.lock().unwrap().remove(session_id);
    }

    /// Resolve the model's true context window for the endpoint used by
    /// `role` —explicit `context_window` config, else the builtin catalog
    /// (e.g. 1M for gpt-4.1-nano / Gemini 2.5 Flash), else a 128K default.
    /// This is the real input budget for the token-usage display, not the
    /// per-response output cap (`max_tokens`).
    pub(super) fn context_window_for_role(
        cfg: &haven_common::config::RouterConfig,
        role: EndpointRole,
    ) -> Option<u32> {
        Some(haven_llm::registry::context_window_for(cfg.endpoint(role)))
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
        if let Some(window) = {
            let cache = self.context_window_cache.lock().unwrap();
            if cache.0 == ptr {
                cache.1.get(&role).copied()
            } else {
                None
            }
        } {
            return window;
        }
        // Slow path: resolve from the live router config. A concurrent
        // router swap between the fast-path miss and the insert is harmless:
        // the entry is stored under the pointer that was current at read
        // time and recomputed on the next miss.
        let cfg = router.config().await;
        let window = Self::context_window_for_role(&cfg, role)
            .unwrap_or(self.context_limits.default_context_window);
        let mut cache = self.context_window_cache.lock().unwrap();
        if cache.0 != ptr {
            cache.0 = ptr;
            cache.1.clear();
        }
        cache.1.insert(role, window);
        window
    }

    /// Build a compactor whose context window reflects the *actual* model for
    /// the role that will handle the step (explicit `context_window` config,
    /// else the builtin catalog, else `context_limits.default_context_window`).
    /// The window comes from `cached_context_window`, so a hot-swapped router
    /// config takes effect immediately without cloning the full config on
    /// every step. The compaction threshold (ratio and reserve) and the
    /// fallback window come from `context_limits`.
    pub(super) async fn context_compactor(&self, role: EndpointRole) -> ContextCompactor {
        let window = self.cached_context_window(role).await;
        ContextCompactor::with_ratio(
            window,
            self.context_limits.compaction_reserve_tokens,
            self.context_limits.compaction_ratio,
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
    pub(super) fn estimate_canonical_tokens(&self, session_id: &str, canonical: &[CanonicalMessage]) -> u32 {
        const FULL_ESTIMATE_PASS_INTERVAL: u32 = 8;
        let mut cache = self.token_estimate_cache.lock().unwrap();
        let entry = cache.entry(session_id.to_string()).or_default();
        let full_pass = entry.tokens == 0
            || entry.msgs_len > canonical.len()
            || entry.passes.is_multiple_of(FULL_ESTIMATE_PASS_INTERVAL);
        if full_pass {
            entry.msgs_len = canonical.len();
            entry.tokens = estimate_message_tokens(canonical);
        } else if entry.msgs_len < canonical.len() {
            entry.tokens += estimate_message_tokens(&canonical[entry.msgs_len..]);
            entry.msgs_len = canonical.len();
        }
        entry.passes = entry.passes.saturating_add(1);
        entry.tokens
    }

    /// Drop the per-session token-estimate cache entry (called alongside
    /// `reset_cumulative_usage` on session completion/error).
    pub fn reset_token_estimate(&self, session_id: &str) {
        self.token_estimate_cache.lock().unwrap().remove(session_id);
    }

    /// Check if context compaction is needed before the next LLM call.
    ///
    /// Returns `true` when a compaction actually ran (the caller re-checks
    /// the image flag afterwards, since summarizing away the last image
    /// changes the endpoint routing).
    pub async fn maybe_compact(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        has_image: bool,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) -> bool {
        if canonical.len() < 4 {
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
        if self.estimate_canonical_tokens(session_id, canonical) <= compactor.threshold_tokens() {
            return false;
        }
        if let Some(result) = compactor.compact(canonical, &router).await {
            tracing::info!(
                "compaction for session {}: {} tokens -> {} tokens ({} msgs summarized)",
                session_id,
                result.tokens_before,
                result.tokens_after,
                result.summarized_count
            );
            *canonical = result.compacted;
            // Compaction replaced the list wholesale: the incremental
            // estimate is stale, drop it so the next step does a full pass.
            self.reset_token_estimate(session_id);
            EventDispatcher::emit_compaction_from(
                emitter,
                session_id,
                &result.summary,
                result.tokens_before,
                result.tokens_after,
            )
            .await;
            self.persist_compaction_summary(session_id, &result.summary)
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
        let should_emit = {
            let mut notified = self.balanced_model_notified.lock().unwrap();
            notified.insert(session_id.to_string())
        };
        if should_emit {
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
        {
            let mut notified = self.balanced_model_notified.lock().unwrap();
            notified.remove(session_id);
        }
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
        assert!(matches!(
            LoopExit::Error("x".into()),
            LoopExit::Error(_)
        ));
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

    // ── failure classification & retry nudge ──────────────────────────────

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
                "network",
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

    #[test]
    fn looks_cut_off_detects_mid_sentence_endings() {
        assert!(ReActEngine::looks_cut_off("让我先查一下，")); // trailing comma
        assert!(ReActEngine::looks_cut_off("checking the file,"));
        assert!(ReActEngine::looks_cut_off("waiting for result...")); // ellipsis
        assert!(ReActEngine::looks_cut_off("然后需要："));
        assert!(!ReActEngine::looks_cut_off("好的，已经完成了。"));
        assert!(!ReActEngine::looks_cut_off("The answer is 42."));
        assert!(!ReActEngine::looks_cut_off("完成"));
    }

    #[test]
    fn is_suspect_final_trusts_explicit_tool_calls() {
        let explicit = Action {
            tool_name: "final_answer".into(),
            tool_input: serde_json::Value::Null,
            is_final: true,
            tool_call_id: Some("c1".into()),
        };
        let r = resp("done", vec![], Some(FinishReason::ToolCalls));
        assert!(!ReActEngine::is_suspect_final(
            &Some("done".into()),
            &[explicit],
            &r,
            &[]
        ));
    }

    #[test]
    fn is_suspect_final_flags_truncated_finish() {
        for finish in [
            Some(FinishReason::Length),
            Some(FinishReason::ContentFilter),
            None,
        ] {
            let r = resp("partial text", vec![], finish);
            assert!(
                ReActEngine::is_suspect_final(&Some("partial text".into()), &[], &r, &[]),
                "finish={finish:?} must be suspect"
            );
        }
    }

    #[test]
    fn is_suspect_final_flags_stop_with_cut_off_text_but_accepts_complete() {
        let r = resp("让我先查一下，", vec![], Some(FinishReason::Stop));
        assert!(ReActEngine::is_suspect_final(
            &Some("让我先查一下，".into()),
            &[],
            &r,
            &[]
        ));
        let r2 = resp("好的，已经完成了。", vec![], Some(FinishReason::Stop));
        assert!(!ReActEngine::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r2,
            &[]
        ));
    }

    #[test]
    fn is_suspect_final_ignores_empty_thought() {
        let r = resp("", vec![], Some(FinishReason::Length));
        assert!(!ReActEngine::is_suspect_final(&None, &[], &r, &[]));
    }

    #[test]
    fn is_suspect_final_flags_stop_with_planning_ending() {
        let r = resp("接下来我需要确认一下", vec![], Some(FinishReason::Stop));
        assert!(ReActEngine::is_suspect_final(
            &Some("接下来我需要确认一下".into()),
            &[],
            &r,
            &[]
        ));
    }

    #[test]
    fn is_suspect_final_flags_mid_session_text_only_stop() {
        // Tool result pending (no user message after it): a text-only Stop
        // must not be trusted as final even though the text is complete.
        let canonical = vec![
            CanonicalMessage::user_text("检查工具"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("接下来")],
                Some(vec![CanonicalToolCall {
                    id: "c1".into(),
                    name: "mcp_list".into(),
                    arguments: serde_json::json!({}),
                }]),
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::tool(
                vec![ContentPart::text(r#"{"success":true,"output":"[tools]"}"#)],
                Some("c1".into()),
            ),
        ];
        let r = resp("好的，已经完成了。", vec![], Some(FinishReason::Stop));
        assert!(ReActEngine::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
            &canonical
        ));
    }

    #[test]
    fn is_suspect_final_accepts_text_only_stop_on_fresh_turn() {
        // No tool result in this turn: a clean text-only Stop is final.
        let canonical = vec![CanonicalMessage::user_text("你好")];
        let r = resp("好的，已经完成了。", vec![], Some(FinishReason::Stop));
        assert!(!ReActEngine::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
            &canonical
        ));
    }

    #[test]
    fn canonical_has_pending_tool_context_detects_mid_session() {
        let mid = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        assert!(ReActEngine::canonical_has_pending_tool_context(&mid));
        let fresh = vec![
            CanonicalMessage::user_text("first"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
            CanonicalMessage::user_text("second question"),
        ];
        assert!(!ReActEngine::canonical_has_pending_tool_context(&fresh));
    }

    #[test]
    fn looks_cut_off_flags_planning_endings() {
        assert!(ReActEngine::looks_cut_off("接下来"));
        assert!(ReActEngine::looks_cut_off("确认一下"));
        assert!(!ReActEngine::looks_cut_off("好的，已经完成了。"));
        assert!(!ReActEngine::looks_cut_off("完成"));
    }
}
