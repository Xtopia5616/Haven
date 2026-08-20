use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::session::{SessionExecutor, SessionStatus};
use haven_common::config::ContextLimitsConfig;
use haven_common::types::MessageAttachment;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::{EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition};
use haven_memory::Database;
use haven_tools::inbox::{InboxBus, MessageType};
use haven_tools::is_silent_action;

use crate::compactor::{ContextCompactor, estimate_message_tokens};
use crate::event::{AgentEvent, AgentEventEmitter, EventDispatcher, UsagePayload};
use crate::types::{Action, BranchPoint, ReActStep};
use chrono::Utc;
use tokio::sync::watch;

/// Stable identity for a tool call across the action/observation UI pairing,
/// matching the frontend's `tool_call_id || tool_name` id so an interrupted
/// observation lands on the same card the action event opened.
fn tool_key(a: &Action) -> String {
    a.tool_call_id
        .clone()
        .unwrap_or_else(|| a.tool_name.clone())
}

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
async fn choose_agent_role(router: &LlmRouter, has_image: bool) -> EndpointRole {
    if has_image {
        router.vision_role().await
    } else {
        EndpointRole::DefaultModel
    }
}

/// Interval (in ReAct steps) at which long-running sessions re-run fact
/// inference mid-session, so memory is refreshed before the session
/// ever pauses or completes.
/// Message persisted when a run exhausts its step budget (`max_steps`). The
/// session is intentionally paused as a checkpoint —the session is NOT finished,
/// and the next user message resumes it with a fresh budget. System notices
/// like this must NOT land in the chat as an assistant bubble; they are
/// surfaced as a notification (in-app toast + Windows) instead.
const BUDGET_EXHAUSTED_TITLE: &str = "任务步骤上限已用尽";
const BUDGET_EXHAUSTED_BODY: &str = "本轮运行的步骤上限已用完，任务已暂停。发一条消息即可继续。";

/// Fallback interval (in ReAct steps) for the automatic cross-session inbox
/// check. Delivery notifications drive the check in-process (immediate), and
/// this cadence only catches missed notifications (e.g. another process
/// wrote to the mailbox).
const MESSAGING_POLL_EVERY_STEPS: u32 = 3;

/// Per-message text cap when injecting cross-session messages into the
/// model context (defensive: a full message is at most 16 KiB, but a burst
/// must not flood the observation budget).
const MESSAGING_INJECT_CHARS: usize = 400;

/// Nudge appended to the retry call when a text-only response looks cut off
/// (truncated generation or text ending mid-sentence). The retry is private
/// to the loop —the nudge is never persisted into the canonical, so the
/// conversation stream stays clean if the retry succeeds or falls back.
const CUT_OFF_RETRY_NUDGE: &str =
    "Your previous response was cut off before you finished. Please continue and complete it.";

/// A stronger nudge for the mid-session retry. The model stopped with a text-only
/// reply while a tool result is still pending (it described the next step but
/// did not run it). The generic cut-off nudge ("continue and complete") does not
/// push it to actually issue the tool call it was narrating, so this variant
/// spells out that the session still needs a tool call.
const MID_ACTION_RETRY_NUDGE: &str = "The session is not finished: the last step ran a tool and its result is in context, but your reply only described the next step instead of doing it. If the session still needs a tool call or a follow-up action, make that tool call NOW instead of describing it. Do not repeat work already done. Continue and finish the actual session.";

/// How many times a text-only response that looks cut off / mid-session is retried
/// with a continuation nudge before it is accepted as a final answer. Bounded so
/// a model that keeps refusing to call a tool cannot spin the loop forever.
/// Configurable via `context_limits.cut_off_retries` (was a `MAX_CUT_OFF_RETRIES`
/// constant before it was unified into settings).
/// Poll interval of the per-call stall watchdog (see `StreamForwarder`).
const STALL_WATCHDOG_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// A provider stream that delivers no chunk for this long is announced to the
/// UI as `StreamStalled` — long before the router's idle timeout aborts the
/// stream, so the status chip can show a factual waiting state instead of a
/// frozen conversation. Covers the first-chunk wait too (the anchor starts at
/// the call's creation). Configurable via `context_limits.stream_stall_warn_delay_ms`
/// (was a `STALL_WARN_DELAY_MS` constant before it was unified into settings).
///
/// Current wall-clock time in milliseconds since the Unix epoch. Used by the
/// stall watchdog anchors (the chunk timestamps it compares against).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mid-run React-state snapshot writes are throttled to once per this many
/// steps (`save_branch_point`). The in-memory canonical/history/branch-point
/// map is always current (branch points are inserted every step regardless),
/// and every pause/error/final path plus every cancellation exit writes the
/// snapshot unconditionally, so the DB row only lags behind by this many
/// steps in a hard-crash window — the resume then re-runs at most this many
/// tool batches, which is already the behavior for a crash mid-batch today.
const SNAPSHOT_WRITE_INTERVAL: u32 = 3;

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
type StreamBlockKey = (String, u32, u64, &'static str);

/// RAII guard clearing a session's minted streaming-message ids when the
/// ReAct run exits (every path — early returns, `?` propagation, cancels),
/// so finished sessions never leave stale entries in `step_msg_ids`.
struct RunMsgIdGuard<'a> {
    engine: &'a ReActEngine,
    session_id: String,
}

impl Drop for RunMsgIdGuard<'_> {
    fn drop(&mut self) {
        self.engine.clear_msg_ids_for_session(&self.session_id);
    }
}

/// State for the automatic cross-session inbox check, one per engine (shared
/// across sessions — each session's mailbox is keyed by its own id).
struct MessagingState {
    /// Shared file bus (default root, process-wide notifier).
    bus: InboxBus,
    /// Delivery notifications: `changed()` fires when any mailbox got a
    /// message, so sessions react immediately instead of only polling.
    rx: watch::Receiver<u64>,
    /// Steps since the last actual inbox drain (fallback cadence for
    /// missed notifications, e.g. a different process wrote the mailbox).
    steps_since_poll: u32,
    /// Session title cache for the registry heartbeat (read once from the
    /// DB; titles change rarely).
    title_cache: HashMap<String, Option<String>>,
}

impl MessagingState {
    fn new() -> Self {
        let bus = InboxBus::default_root();
        let rx = bus.subscribe();
        Self {
            bus,
            rx,
            steps_since_poll: 0,
            title_cache: HashMap::new(),
        }
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
    current_run_id: AtomicU64,
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
}

/// Borrowed serialization view of a `ReActSnapshot`. Serializing this instead
/// of building an owned `ReActSnapshot` skips the per-step deep copies of
/// canonical/history/branch_points (which accumulate to O(n²) over a long
/// session). Field names/shape match `ReActSnapshot` exactly so the persisted
/// JSON stays wire-compatible.
#[derive(serde::Serialize)]
struct SnapshotView<'a> {
    canonical: &'a [CanonicalMessage],
    history: &'a [ReActStep],
    step_number: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    branch_points: &'a HashMap<u32, BranchPoint>,
    /// `saved_at` is written at serialization time: resume uses it to recover
    /// messages persisted after this snapshot by timestamp (see
    /// `ReActSnapshot::saved_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CumulativeUsage {
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
struct TokenEstimate {
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
struct StepCtx {
    session_id: String,
    step_num: u32,
    run_id: u64,
    emitter: Arc<dyn AgentEventEmitter>,
}

/// Result of one step's LLM call (including the compaction retry). The loop
/// dispatches on this instead of inlining ~130 lines of error handling.
enum StepCallOutcome {
    /// A usable response (possibly from the post-compaction retry).
    Response(LlmResponse),
    /// Cancelled mid-call (end_session / rollback): exit silently.
    Cancelled,
    /// Persisted/emitted error already; the loop must propagate it.
    Fatal(String),
}

/// Update a session's status and emit the `SessionUpdated` event, in that order.
/// Shared by every status-transition path (pause, budget pause, agent layer)
/// so the pair cannot drift.
pub(crate) async fn set_status_and_emit(
    executor: &SessionExecutor,
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: &str,
    status: SessionStatus,
) -> anyhow::Result<()> {
    let status_str = status.as_str().to_string();
    tracing::debug!("session {} status -> {}", session_id, status_str);
    executor.update_session_status(session_id, status).await?;
    emitter
        .emit(crate::event::AgentEvent::SessionUpdated {
            session_id: session_id.into(),
            status: status_str,
        })
        .await;
    Ok(())
}

/// Failure classification used to shape the post-failure retry nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// The environment cannot run the approach: missing command, wrong shell,
    /// network/proxy trouble, bad paths. The approach itself may be sound.
    Environmental,
    /// The approach/usage itself is flawed (bad params, parse failures).
    Logic,
    /// Cannot tell from the error text.
    Unknown,
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

/// `message_inbox` result is an empty poll (`count: 0`): nothing for the
/// user to see, so the observation card is suppressed.
fn empty_inbox_output(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|v| v.get("count").and_then(|c| c.as_u64()))
        == Some(0)
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
            current_run_id: AtomicU64::new(0),
            cumulative_usage: Mutex::new(HashMap::new()),
            messaging: Mutex::new(MessagingState::new()),
            snapshot_bufs: Mutex::new(HashMap::new()),
            token_estimate_cache: Mutex::new(HashMap::new()),
            context_window_cache: Mutex::new((0, HashMap::new())),
            last_snapshot_step: Mutex::new(HashMap::new()),
            tool_def_cache: Mutex::new(HashMap::new()),
            step_msg_ids: Mutex::new(HashMap::new()),
        }
    }

    /// Mint (or reuse) the id a streamed thought/reasoning block of
    /// `(session, step, run, kind)` accumulates into. Constant per block so
    /// chunk events, the snap and the final persistence share one id.
    ///
    /// A `thought` block is the content view of a ReAct step: its id is
    /// minted with the `step-` prefix so the message row and the thought
    /// step row (created in `emit_thought_from` under the same id) are one
    /// entity. `reasoning` blocks have no step row and keep `msg-` ids.
    fn ensure_msg_id(&self, session_id: &str, step: u32, run: u64, kind: &'static str) -> String {
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
    fn peek_msg_id(
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
    fn block_msg_id(&self, session_id: &str, step: u32, run: u64, kind: &'static str) -> String {
        self.peek_msg_id(session_id, step, run, kind)
            .unwrap_or_else(|| {
                let prefix = if kind == "thought" { "step" } else { "msg" };
                haven_common::types::new_id(prefix)
            })
    }

    /// Drop every minted message id belonging to a session. Runs once per
    /// `run_react_loop` invocation so stale ids from a previous run never
    /// leak or collide with a fresh run's minted ids.
    fn clear_msg_ids_for_session(&self, session_id: &str) {
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

    pub fn current_run_id(&self) -> u64 {
        self.current_run_id.load(Ordering::SeqCst)
    }

    pub fn set_current_run_id(&self, id: u64) {
        self.current_run_id.store(id, Ordering::SeqCst);
    }

    /// Live three-way connectivity probe to the default-model endpoint. Used
    /// by the top-right status indicator to show 就绪 / 已断开 / 未配置.
    pub async fn check_connection(&self) -> haven_llm::LlmConnectionStatus {
        let router = self.router();
        router
            .connection_status(haven_llm::EndpointRole::DefaultModel)
            .await
    }

    fn router(&self) -> Arc<LlmRouter> {
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
    async fn build_tool_definitions_for_session(&self, session_id: &str) -> Vec<ToolDefinition> {
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

    /// Drain user-facing context into the canonical message list: supplements
    /// (paused-session replies), steering (mid-run user interjections) and
    /// completed background-action results. Each becomes a `User` message so the
    /// agent sees it on the next LLM call.
    ///
    /// Returns `true` when at least one message was injected. Called at the
    /// top of every step, and again right before a step completes with final
    /// content —a message that arrived while the LLM call was in flight is
    /// delivered there instead of being deferred until the turn ends.
    async fn inject_pending_context(
        &self,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
    ) -> bool {
        let mut injected = false;

        // One combined drain pass instead of three separate queue reads:
        // the ses-map lock is taken once per step instead of three times.
        let (supplements, steering, action_results) =
            self.executor.drain_pending_context(&ctx.session_id).await;
        for supplement in &supplements {
            // A reply to a pending `ask` is injected as a paired answer so
            // the model sees the old question as resolved instead of treating
            // it as a second open question to answer again.
            let prefix = if supplement.is_answer {
                "Answer to your previous question"
            } else {
                "Additional context from user"
            };
            self.push_user_context(
                ctx,
                canonical,
                prefix,
                &supplement.text,
                &supplement.attachments,
                supplement.message_id.as_deref(),
            )
            .await;
            injected = true;
        }

        for s in &steering {
            self.push_user_context(
                ctx,
                canonical,
                "Steering",
                &s.text,
                &s.attachments,
                s.message_id.as_deref(),
            )
            .await;
            injected = true;
        }

        // Deliver completed background-action results as context. These are
        // kept separate from the steering queue so action output is never
        // mistaken for a user reply (which would let the `ask` pause path
        // resume the session without the user's answer). The payload text is
        // self-labelling (`[Background action result] ... action_id ...`) and is
        // pushed as a User-role message because a mid-conversation System
        // message is rejected by some providers and a Tool message would need
        // a preceding assistant tool_call (see `is_dangling_boundary`).
        for s in &action_results {
            canonical.push(CanonicalMessage::user_text(s));
            injected = true;
        }

        injected
    }

    /// Cross-session messaging integration, run at the top of every ReAct
    /// step (after `inject_pending_context`, before the LLM call):
    ///
    /// 1. **Heartbeat** — re-register this session (`last_seen = now`) with
    ///    its DB title, every step, so long-thinking sessions stay `online`
    ///    and `agents_list`/the UI can show what a session is about.
    /// 2. **Automatic inbox check** — drain the mailbox when an in-process
    ///    delivery notification arrived (push, immediate) or every
    ///    [`MESSAGING_POLL_EVERY_STEPS`] steps (fallback for cross-process
    ///    writers). Each message is injected as low-trust user context for
    ///    the next LLM call — no reliance on the agent remembering to poll.
    /// 3. **Receipts** — freshly read messages are auto-acked so senders
    ///    learn their message was consumed.
    async fn maybe_poll_inbox(
        &self,
        session_id: &str,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
    ) {
        // Session title for the registry (read once from the DB, then cached
        // per session; never hold the engine mutex across an await).
        let cached_title = {
            let st = self.messaging.lock().unwrap();
            st.title_cache.get(session_id).cloned()
        };
        let title = match cached_title {
            Some(t) => t,
            None => {
                let t = self
                    .db
                    .run_blocking({
                        let sid = session_id.to_string();
                        move |db| {
                            let title = db.get_session(&sid).ok().flatten().and_then(|s| s.title);
                            Ok::<Option<String>, anyhow::Error>(title)
                        }
                    })
                    .await
                    .unwrap_or(None);
                self.messaging
                    .lock()
                    .unwrap()
                    .title_cache
                    .insert(session_id.to_string(), t.clone());
                t
            }
        };

        let (bus, due) = {
            let mut st = self.messaging.lock().unwrap();
            let bus = st.bus.clone();
            st.steps_since_poll += 1;
            let notified = st.rx.has_changed().unwrap_or(false);
            if notified {
                let _ = st.rx.borrow_and_update();
            }
            let due = notified || st.steps_since_poll >= MESSAGING_POLL_EVERY_STEPS;
            if due {
                st.steps_since_poll = 0;
            }
            (bus, due)
        };

        // Heartbeat on the blocking pool, every step regardless of polling.
        let sid = session_id.to_string();
        let hb_sid = sid.clone();
        let hb_title = title.clone();
        let hb_bus = bus.clone();
        tokio::task::spawn_blocking(move || {
            let _ = hb_bus.register_with_title(&hb_sid, &[], hb_title.as_deref());
        })
        .await
        .ok();

        if !due {
            return;
        }

        let poll_sid = sid.clone();
        let messages = match tokio::task::spawn_blocking(move || {
            let read = bus.read_and_archive(&poll_sid)?;
            let _receipts = bus.send_receipts(&poll_sid, &read);
            Ok::<_, anyhow::Error>(read)
        })
        .await
        {
            Ok(Ok(msgs)) => msgs,
            Ok(Err(e)) => {
                tracing::debug!("messaging inbox poll failed for {session_id}: {e}");
                return;
            }
            Err(e) => {
                tracing::debug!("messaging inbox poll join failed: {e}");
                return;
            }
        };
        if messages.is_empty() {
            return;
        }

        let mut text = String::new();
        for env in &messages {
            let body: String = env.text.chars().take(MESSAGING_INJECT_CHARS).collect();
            match env.r#type {
                MessageType::Receipt => {
                    let of = env.in_reply_to.as_deref().unwrap_or("<unknown>");
                    text.push_str(&format!(
                        "[Read receipt] {} read your message {of}\n",
                        env.from
                    ));
                }
                _ => {
                    text.push_str(&format!(
                        "[Cross-session message from {} ({})]: {body}\n",
                        env.from, env.r#type
                    ));
                }
            }
        }
        self.push_user_context(
            ctx,
            canonical,
            "Cross-session message",
            text.trim_end(),
            &[],
            None,
        )
        .await;
    }

    /// Emit a Supplement event, persist a matching thought-step row and push
    /// a user message into the canonical array. Shared by the supplement and
    /// steering queues (identical mechanics, different text prefixes) so the
    /// two paths cannot drift. The thought-step row anchors the user message
    /// to a step after a reload: the row is created under the message's own
    /// id (`message_id`, persisted at submit time) so review/rollback can
    /// resolve the step by id; without it an interrupted input would have no
    /// determinable step. The step row stores no text — the user message row
    /// is the single content authority.
    async fn push_user_context(
        &self,
        ctx: &StepCtx,
        canonical: &mut Vec<CanonicalMessage>,
        prefix: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<&str>,
    ) {
        ctx.emitter
            .emit(crate::event::AgentEvent::Supplement {
                session_id: ctx.session_id.clone(),
                additional_context: text.to_string(),
                step_number: ctx.step_num,
                run_id: ctx.run_id,
            })
            .await;
        let step_id = message_id
            .map(String::from)
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        let _ = self
            .db
            .run_blocking({
                let session_id = ctx.session_id.clone();
                let step_id = step_id.clone();
                let step_num = ctx.step_num;
                move |db| {
                    if let Err(e) = db.create_thought_step(&session_id, step_num as i32, &step_id) {
                        tracing::warn!(
                            "create_thought_step failed (session={} step={}): {}",
                            session_id,
                            step_num,
                            e
                        );
                    }
                    Ok::<(), anyhow::Error>(())
                }
            })
            .await;
        let mut content = vec![ContentPart::text(format!("{prefix}: {text}"))];
        content.extend(attachments.iter().map(attachment_to_content_part));
        // No content-based dedup here: duplicate submissions are prevented at
        // the UI layer (the submit path is in-flight locked), and the DB rows
        // carry unique ids that anchor each input to its own step row. The
        // canonical is an append-only transcript of what the user actually
        // sent — collapsing identical inputs would silently drop legitimate
        // repeated turns (e.g. the user saying "继续" twice on purpose).
        canonical.push(CanonicalMessage::user(content));
    }

    /// Shared ReAct loop body. Runs from `start_step` through `max_steps`.
    /// Called by both `run_session` (fresh) and `run_session_resumed` (resumed from
    /// snapshot).
    ///
    /// Tool definitions are rebuilt at the top of each step so that tools
    /// loaded via `load_skill` / `load_mcp` (registered per-session) become
    /// visible to the LLM on the very next step.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_react_loop(
        &self,
        session_id: &str,
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
        start_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn() + Send + Sync),
        run_id: u64,
    ) -> anyhow::Result<()> {
        let max_steps = *self.max_steps.lock().unwrap();
        // When resuming past the configured cap (e.g. a session that used all
        // `max_steps` then paused for the user's next turn), give the loop
        // another full budget so the resume doesn't degenerate into an
        // immediate budget-exhaustion pause below. This intentionally
        // re-budgets on every resume —a session can run `max_steps` per run,
        // not once per session lifetime (documented in refactor-dedup.md A9).
        let effective_max = max_steps.max(start_step.saturating_sub(1).saturating_add(max_steps));
        let mut last_step = start_step.saturating_sub(1);
        // One run = one loop invocation: minted streaming-message ids from a
        // previous run of this session are dropped so a fresh run's blocks
        // always get fresh ids (and stale entries never accumulate). The
        // guard clears them again on EVERY exit path (early returns, `?`
        // propagation, cancels), so a session whose last run ends keeps no
        // entries in the engine-wide map.
        self.clear_msg_ids_for_session(session_id);
        let _msg_id_guard = RunMsgIdGuard {
            engine: self,
            session_id: session_id.to_string(),
        };
        tracing::info!(
            "ReAct loop start: session={} run_id={} start_step={} max_steps={} effective_max={}",
            session_id,
            run_id,
            start_step,
            max_steps,
            effective_max
        );
        // Cut-off retry counter: a text-only response that looks truncated (or
        // is a mid-session narration that stopped without a tool call) is retried
        // up to `context_limits.cut_off_retries` times per run with a continuation nudge (the
        // nudge is not persisted into the canonical). Kept separate from the
        // empty response budget — the two heuristics address different failure
        // modes.
        let mut cut_off_retries: u32 = 0;
        // Level-triggered status subscription, taken ONCE per run and reused
        // across steps: unlike the edge-triggered Notify it replaces, a
        // transition that lands between a state read and the `changed()` wait
        // is never lost — the receiver's stored value moves and `changed()`
        // resolves immediately. No polling timeout is needed, and one
        // long-lived receiver avoids allocating a watch subscription (two
        // ses-map lock acquisitions) on every step.
        let mut status_rx = self.executor.subscribe_status(session_id).await;

        for step_num in start_step..=effective_max {
            last_step = step_num;
            let cancel = self.executor.cancellation_token(session_id).await;
            // The paused-state snapshot is saved once per pause episode: the
            // canonical/history/branch points cannot change while paused, so
            // re-observing a still-paused state after a transition (e.g.
            // Paused -> PausedAwaitingAnswer) must not rewrite the identical
            // snapshot to disk.
            let mut paused_snapshot_saved = false;
            loop {
                // Check cancellation first: end_session / rollback cancel the
                // token, so the loop must exit silently without touching
                // status or emitting events. The state check below would
                // otherwise observe the Error sentinel of a session that
                // end_session already removed from memory and announce a
                // spurious "session interrupted" error. A final snapshot is
                // written so the DB row is never left stale for the rollback
                // that just cancelled us.
                if cancel.is_cancelled() {
                    self.save_exit_snapshot(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(());
                }
                let state = self.executor.get_session_state(session_id).await;
                match state {
                    // Session vanished from the working set (end_session / terminal
                    // cleanup): exit silently.
                    None => return Ok(()),
                    Some(SessionStatus::Completed) => return Ok(()),
                    Some(SessionStatus::Error) => {
                        // An external path marked the session Error while the
                        // loop was alive: announce the interruption so the
                        // user sees why it stopped.
                        self.emit_error(&emitter, session_id, "session interrupted")
                            .await;
                        return Ok(());
                    }
                    Some(s) if s.is_paused() => {
                        if !paused_snapshot_saved {
                            self.save_snapshot_with_branches(
                                session_id,
                                canonical,
                                history,
                                step_num,
                                branch_points,
                            )
                            .await;
                            paused_snapshot_saved = true;
                        }
                    }
                    _ => break,
                }
                // Wait for the next status change or cancellation, then
                // re-evaluate at the loop head. A resume that landed during
                // the snapshot save above resolves `changed()` immediately.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Same final-write as the loop-head cancel above:
                        // rollback/end_session must find the DB row current.
                        if !paused_snapshot_saved {
                            self.save_exit_snapshot(
                                session_id,
                                canonical,
                                history,
                                step_num,
                                branch_points,
                            )
                            .await;
                        }
                        return Ok(());
                    }
                    r = status_rx.changed() => {
                        // `Err` means the sender was dropped (session cleaned
                        // up): the state re-check at the loop head handles it.
                        let _ = r;
                    }
                }
            }

            // Per-step context shared by all helpers below (context injection,
            // streaming, error handling, final-answer delivery).
            let ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num,
                run_id,
                emitter: emitter.clone(),
            };

            // Deliver user interjections (supplements, steering) and
            // background-action results as context at the top of each step so
            // they land in the gap between tool calls and the next LLM call.
            self.inject_pending_context(&ctx, canonical).await;

            // Cross-session messaging: heartbeat + automatic inbox check
            // (in-process delivery notifications drive it; the step cadence
            // is the fallback).
            self.maybe_poll_inbox(session_id, &ctx, canonical).await;

            // Scan for image content once per step; the flag is shared by the
            // compactor window selection and the endpoint role below, so the
            // image check is not repeated over every content part. Compaction
            // may summarize away the last image, so re-scan only when one ran.
            let mut has_image = canonical_has_image(canonical);
            if self
                .maybe_compact(session_id, canonical, has_image, &emitter)
                .await
            {
                has_image = canonical_has_image(canonical);
            }

            // Incremental fact inference on long-running sessions:
            // turns that never pause would otherwise only trigger extraction
            // at the very end. Every `context_limits.fact_infer_interval_steps`
            // steps we re-run inference; the upsert/known-facts machinery makes
            // this idempotent (re-confirmed facts are reinforced, not duplicated).
            // Step 0 never infers: there is no prior message window to scan.
            let infer_interval = self.context_limits.fact_infer_interval_steps;
            if step_num > 0 && infer_interval > 0 && step_num % infer_interval == 0 {
                infer();
            }

            // No canonical may be sent to the LLM containing a tool message
            // without a preceding assistant tool_calls (providers reject it
            // with a 400). Sanitize as a final gate so compaction or a
            // mid-batch interruption can never poison a request.
            crate::sanitize_canonical(canonical);

            // Rebuild tool definitions each step so that per-session tools
            // registered by `load_skill` / `load_mcp` are visible to the LLM.
            let tools: Vec<ToolDefinition> =
                self.build_tool_definitions_for_session(session_id).await;

            let router = self.router();
            // Same cancellation token as the loop-head wait above; one
            // executor lookup per step instead of two.
            let cancel_res = cancel.clone();
            // Convert once per step; retries below reuse the converted
            // messages (the canonical is only replaced by the compaction
            // path, which re-converts) instead of cloning the whole
            // canonical and re-serializing every tool-call argument again.
            let mut llm_messages = canonical.clone();
            let role = choose_agent_role(&router, has_image).await;
            // Accumulate streamed text locally so that if the LLM call fails
            // mid-stream, we can persist whatever was already received instead
            // of losing it entirely.
            let partial_thought: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(String::new()));
            let partial_reasoning: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(String::new()));
            tracing::debug!(
                "ReAct step {} session {} calling LLM, {} messages, {} tools",
                step_num,
                session_id,
                llm_messages.len(),
                tools.len()
            );
            tracing::trace!(
                "ReAct step {} canonical messages: {:?}",
                step_num,
                llm_messages
                    .iter()
                    .map(|m| (m.role, m.content.len()))
                    .collect::<Vec<_>>()
            );
            let mut response = match self
                .call_step_llm(
                    &ctx,
                    router.clone(),
                    role,
                    &mut llm_messages,
                    &tools,
                    cancel_res.clone(),
                    canonical,
                    history,
                    branch_points,
                    &partial_thought,
                    &partial_reasoning,
                )
                .await
            {
                StepCallOutcome::Response(resp) => resp,
                StepCallOutcome::Cancelled => {
                    // A final snapshot keeps the DB row current for the
                    // rollback/continue that cancelled the LLM call (the
                    // response was never parsed, so the saved state is the
                    // clean pre-step state).
                    self.save_exit_snapshot(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(());
                }
                StepCallOutcome::Fatal(msg) => return Err(anyhow::anyhow!("{}", msg)),
            };

            // L2/C2: a rollback or end_session may have cancelled the session while
            // the LLM call was in flight (the HTTP call itself may not observe
            // the token promptly and can return well after the 5s rollback
            // wait). Re-check before persisting anything so a stale response
            // cannot overwrite the restored snapshot or push ghost steps.
            if cancel_res.is_cancelled() {
                tracing::info!(
                    "ReAct step {} session {} cancelled during LLM call; discarding response",
                    step_num,
                    session_id
                );
                self.save_exit_snapshot(session_id, canonical, history, step_num, branch_points)
                    .await;
                return Ok(());
            }

            tracing::debug!(
                "ReAct step {} LLM response: {} text chars, {} tool_calls, reasoning={}",
                step_num,
                response.text.len(),
                response.tool_calls.len(),
                response.reasoning.is_some()
            );

            if let Some(ref reasoning) = response.reasoning {
                let reasoning_id = self.block_msg_id(session_id, step_num, run_id, "reasoning");
                self.persist_session_message(
                    session_id,
                    "assistant",
                    reasoning,
                    Some("reasoning"),
                    None,
                    Some(&reasoning_id),
                )
                .await;
                // Reconcile the frontend's streamed reasoning with the
                // authoritative complete text. The frontend builds reasoning
                // only from batched deltas, so a dropped/delayed final chunk
                // would permanently lose trailing characters. Emitting the
                // complete reasoning as a final delta lets the frontend's
                // cumulative-detection (delta.startsWith(curr) —replace)
                // snap the content to the exact full text. This runs after the
                // chunk batcher has flushed, so it is guaranteed to be the
                // last reasoning event for this step. The delta carries the
                // same minted message id the streamed chunks used.
                emitter
                    .emit(crate::event::AgentEvent::ReasoningChunk {
                        session_id: session_id.into(),
                        delta: reasoning.clone(),
                        step_number: step_num,
                        run_id,
                        message_id: reasoning_id,
                    })
                    .await;
            }

            let (mut thought, mut actions) =
                Self::parse_default_model_response(&response, step_num);

            // Empty-response retry budget for THIS step. A completely empty
            // model response (no text, no reasoning, no tool calls) is almost
            // always a transient upstream glitch. Retry the same context up
            // to `context_limits.empty_response_max_retries` times before concluding the model
            // decided nothing — otherwise the session would instantly "complete"
            // with a "No action decided." message and pause without answering.
            // Declared per step so an earlier empty response in this run
            // cannot starve a later incident of its retries; the exhausted
            // state (reached 0) also drives the explicit error path below.
            let mut empty_retries_remaining = self.context_limits.empty_response_max_retries;
            // A response carrying `web_search_call` items is NOT empty: it is
            // a server-side search round that must round-trip instead.
            if thought.is_none() && actions.is_empty() && response.web_search_calls.is_empty() {
                while empty_retries_remaining > 0 {
                    empty_retries_remaining -= 1;
                    if cancel_res.is_cancelled() {
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(());
                    }
                    // Settling delay between attempts: an upstream glitch that
                    // just produced an empty stream often clears within a
                    // second or two. Cancellable: a user rollback / end_session
                    // during the delay must not wait out the whole sleep — the
                    // retry loop is exited as soon as the token fires, so the
                    // handler releases the running slot promptly instead of
                    // blocking the rollback's 5s wait.
                    tokio::select! {
                        _ = cancel_res.cancelled() => {
                            self.save_exit_snapshot(session_id, canonical, history, step_num, branch_points)
                                .await;
                            return Ok(());
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(
                            self.context_limits.empty_response_retry_delay_ms,
                        )) => {}
                    }
                    if cancel_res.is_cancelled() {
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(());
                    }
                    tracing::warn!(
                        "ReAct step {} session {} model returned an empty response; retrying ({} left)",
                        step_num,
                        session_id,
                        empty_retries_remaining
                    );
                    // Cancellable: end_session / rollback must be able to abort
                    // the retries mid-flight (the non-cancellable variant
                    // would also grant each attempt a fresh total-duration
                    // budget instead of sharing the step's cancellation).
                    // Chunks are forwarded live so a recovering provider is
                    // visible instead of freezing the UI for the whole budget.
                    match self
                        .stream_retry_step(
                            &ctx,
                            router.clone(),
                            role,
                            &llm_messages,
                            &tools,
                            cancel_res.clone(),
                            &partial_thought,
                            &partial_reasoning,
                        )
                        .await
                    {
                        Ok(retry_resp) => {
                            let (t2, a2) =
                                Self::parse_default_model_response(&retry_resp, step_num);
                            if t2.is_some() || !a2.is_empty() {
                                thought = t2;
                                actions = a2;
                                // The retry produced the content: the whole
                                // response must follow it, or the canonical
                                // assistant message would carry the retry's
                                // tool calls WITHOUT its reasoning (DeepSeek
                                // thinking mode 400s on the next request).
                                response = retry_resp;
                                break;
                            }
                            tracing::warn!(
                                "ReAct step {} retry also returned an empty response",
                                step_num
                            );
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            self.save_exit_snapshot(
                                session_id,
                                canonical,
                                history,
                                step_num,
                                branch_points,
                            )
                            .await;
                            return Ok(());
                        }
                        Err(e) => {
                            tracing::warn!(
                                "ReAct step {} empty-response retry failed: {}",
                                step_num,
                                e
                            );
                        }
                    }
                }
            }

            // A text-only response that ends without an explicit tool call is
            // only trusted as a deliberate final answer when it looks
            // complete: the provider reported Stop AND the text does not end
            // mid-sentence AND the agent is not still mid-session. Anything else
            // —a truncated generation (Length / ContentFilter / unknown
            // finish), text cut off mid-thought, or a mid-session narration that
            // stopped without issuing the tool call it described —must not
            // end the turn presenting a partial answer as final.
            // Retry with a continuation nudge (never persisted into the
            // canonical), up to `context_limits.cut_off_retries` times; if every retry is
            // also unusable, fall back to the original text below.
            let pending_ask = Self::canonical_has_pending_ask(canonical);
            // Responses that carried a web search call must never be re-asked:
            // the search round itself is a legitimate (non-cut-off) outcome,
            // and retrying would trigger a duplicate server-side search.
            while cut_off_retries < self.context_limits.cut_off_retries
                && !pending_ask
                && response.web_search_calls.is_empty()
                && Self::is_suspect_final(&thought, &actions, &response, canonical)
            {
                cut_off_retries += 1;
                if cancel_res.is_cancelled() {
                    self.save_exit_snapshot(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(());
                }
                // Mid-session narrations get a stronger nudge that explicitly asks
                // for the pending tool call; plain truncation gets the generic
                // continuation nudge.
                let mid_session = Self::canonical_has_pending_tool_context(canonical);
                let nudge = if mid_session {
                    MID_ACTION_RETRY_NUDGE
                } else {
                    CUT_OFF_RETRY_NUDGE
                };
                tracing::warn!(
                    "ReAct step {} session {} response looks cut off (finish={:?}, mid_session={}); retrying (attempt {}/{})",
                    step_num,
                    session_id,
                    response.finish_reason,
                    mid_session,
                    cut_off_retries,
                    self.context_limits.cut_off_retries
                );
                let mut retry_messages = llm_messages.clone();
                retry_messages.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(nudge)],
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                });
                // Cancellable so an interruption (rollback / end_session) aborts
                // the retry promptly instead of letting its stream run to
                // completion after the session was cancelled (the empty-response
                // retry below uses the same cancellable variant). Chunks are
                // forwarded live like the primary call, so a resumed provider
                // streams visibly instead of freezing the UI mid-step.
                match self
                    .stream_retry_step(
                        &ctx,
                        router.clone(),
                        role,
                        &retry_messages,
                        &tools,
                        cancel_res.clone(),
                        &partial_thought,
                        &partial_reasoning,
                    )
                    .await
                {
                    Ok(retry_resp) => {
                        let (t2, a2) = Self::parse_default_model_response(&retry_resp, step_num);
                        if t2.is_some() || !a2.is_empty() {
                            thought = t2;
                            actions = a2;
                            // Same reasoning-attachment rule as the
                            // empty-response retry: the canonical push must
                            // carry the retry response's own reasoning, not
                            // the cut-off original's.
                            response = retry_resp;
                        } else {
                            tracing::warn!(
                                "ReAct step {} cut-off retry also returned an empty response",
                                step_num
                            );
                            break;
                        }
                    }
                    Err(haven_llm::LlmError::Cancelled) => {
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!("ReAct step {} cut-off retry failed: {}", step_num, e);
                        break;
                    }
                }
            }

            // An unanswered `ask` still pending in the canonical (a reply was
            // lost to compaction/sanitization, or the model never resolved
            // the question): the text-only-Stop heuristic must not end the
            // turn. Drop the synthesized final so the empty-actions path
            // below re-surfaces the question and pauses for the user's
            // answer instead of "completing" with the question unanswered.
            // Applied after the retries so a retry that produced a
            // synthesized final is covered too; explicit final tool calls
            // (the model decided to answer despite the pending question) are
            // respected.
            if pending_ask
                && !actions.is_empty()
                && actions
                    .iter()
                    .all(|a| a.is_final && a.tool_call_id.is_none())
            {
                tracing::warn!(
                    "ReAct step {} session {} stopped while an ask is pending; keeping the turn open",
                    step_num,
                    session_id
                );
                actions.clear();
            }

            // Repair tool-call arguments that are missing required fields
            // before they reach the provider / tool (common after an
            // interrupted/continued generation). A missing required field is
            // filled from the schema default, else a type placeholder, so the
            // call deserializes instead of triggering a 400. Runs on the
            // finalized actions so retry-replaced responses are covered too.
            if !actions.is_empty() {
                let repaired = self
                    .supplement_missing_required_fields(session_id, &mut actions)
                    .await;
                if repaired > 0 {
                    tracing::warn!(
                        "ReAct step {} session {} repaired {} tool call(s) with missing required fields",
                        step_num,
                        session_id,
                        repaired
                    );
                }
            }

            tracing::trace!(
                "ReAct step {} parsed: thought={}, actions={}",
                step_num,
                thought
                    .as_ref()
                    .map(|t| format!("{} chars", t.len()))
                    .unwrap_or_else(|| "none".into()),
                actions
                    .iter()
                    .map(|a| a.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            if let Some(ref t) = thought {
                let message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                EventDispatcher::emit_thought_from(
                    &emitter,
                    session_id,
                    t,
                    step_num,
                    run_id,
                    &message_id,
                    &self.db,
                )
                .await;
                history.push(ReActStep {
                    step_number: step_num,
                    thought: Some(t.clone()),
                    action: None,
                    observation: None,
                });
            }

            // ── Web search round-trip ─────────────────────────────────────
            // `web_search_call` output items come from the provider's
            // server-side search tool (DeepSeek built-in). The search itself
            // runs on the provider; the items must be passed back verbatim in
            // the next request's input so the server restores the search
            // context. Push an assistant message carrying them into the
            // canonical so every subsequent path round-trips them.
            let has_web_search = !response.web_search_calls.is_empty();
            let synthesized_final = !actions.is_empty()
                && actions
                    .iter()
                    .all(|a| a.is_final && a.tool_call_id.is_none());
            if has_web_search && (actions.is_empty() || synthesized_final) {
                // The text must match what `persist_session_message` stores
                // (trimmed thought) or the resume dedup fails on the leading
                // whitespace and re-seeds the message as a [conversation] line.
                let push_text = thought.as_deref().unwrap_or(&response.text);
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(push_text.to_string())],
                    None,
                    if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    response.web_search_calls.clone(),
                    response.thinking_blocks.clone(),
                ));
                if actions.is_empty() {
                    // Search round: no answer yet, the follow-up request
                    // carries the search context and produces the answer.
                    // Keep the turn open and let the loop re-request.
                    if let Some(ref t) = thought {
                        let message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                        self.persist_session_message(
                            session_id,
                            "assistant",
                            t,
                            Some("text"),
                            None,
                            Some(&message_id),
                        )
                        .await;
                    }
                    self.save_branch_point(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                        false,
                    )
                    .await;
                    tracing::debug!(
                        "ReAct step {} session {} server-side web search round ({} item(s)); continuing",
                        step_num,
                        session_id,
                        response.web_search_calls.len()
                    );
                    continue;
                }
                // synthesized_final: the answer arrived in the same response
                // as the search call —fall through to the final-answer path,
                // which ends the turn. The canonical push above keeps the
                // search context alive for follow-up turns.
            }

            if actions.is_empty() {
                // An `ask` is still pending unanswered (the model stopped
                // without resolving it, and no user reply arrived to pair
                // with it): the turn must not end on a heuristic final.
                // Re-surface the question and pause so the user's next
                // message is treated as the answer.
                if pending_ask {
                    let question = Self::extract_pending_ask_question(canonical);
                    self.pause_turn(
                        session_id,
                        canonical,
                        history,
                        step_num + 1,
                        branch_points,
                        &emitter,
                        SessionStatus::PausedAwaitingAnswer,
                        &question,
                        None,
                        infer,
                        // The question is re-persisted as a plain assistant
                        // message (fresh id, `is_ask` false so pause_turn
                        // persists it): the row re-seeds the resume canonical.
                        // The review renders the ask CARD from the original
                        // question message (persisted under the ask step's id
                        // at pause time) and drops this fresh bubble by
                        // content match (legacy path).
                        None,
                        false,
                    )
                    .await?;
                    return Ok(());
                }
                // The empty-response retries all failed: the model produced
                // nothing (no text, no tool calls) on every attempt. Ending
                // the turn with a fake "No action decided." answer would look
                // like the assistant ignored the user — surface an explicit
                // error instead so the user can retry the session, and the real
                // cause (upstream silent failure) is visible.
                if thought.is_none()
                    && empty_retries_remaining < self.context_limits.empty_response_max_retries
                {
                    let err_msg = "模型连续多次返回空响应（服务端异常）。请稍后点击「继续任务」重试，或检查模型服务状态。"
                        .to_string();
                    self.emit_error(&emitter, session_id, &err_msg).await;
                    self.executor
                        .update_session_status(session_id, SessionStatus::Error)
                        .await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                // Guard against clobbering: when the response carried no text
                // (thought is None after failed retries), `history.last()`
                // points at a PREVIOUS step; only attach the synthesized final
                // to this step's own entry, otherwise the previous step's
                // action/observation is silently overwritten.
                if let Some(last) = history.last_mut().filter(|s| s.step_number == step_num) {
                    last.action = Some(Action {
                        tool_name: "final_answer".into(),
                        tool_input: serde_json::Value::Null,
                        is_final: true,
                        tool_call_id: None,
                    });
                    if last.observation.is_none() {
                        last.observation = Some(msg.clone());
                    }
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: Some(msg.clone()),
                        action: Some(Action {
                            tool_name: "final_answer".into(),
                            tool_input: serde_json::Value::Null,
                            is_final: true,
                            tool_call_id: None,
                        }),
                        observation: Some(msg.clone()),
                    });
                }
                // A user message (or background-action result) arrived while the
                // model was generating this answer: deliver it in the gap
                // between the tool calls and the final content instead of
                // deferring it until after the turn completes. The finished
                // answer is persisted so the conversation stays consistent,
                // then the loop re-runs with the new context.
                let before_inject_len = canonical.len();
                if self.inject_pending_context(&ctx, canonical).await {
                    self.deliver_final_with_pending_context(
                        &ctx,
                        &msg,
                        response.reasoning.clone(),
                        canonical,
                        history,
                        branch_points,
                        before_inject_len,
                        // This branch carries no web search and no tool calls,
                        // so no assistant message was pushed yet.
                        false,
                    )
                    .await;
                    continue;
                }
                // Mirror the finished answer into the canonical before the
                // pause: the snapshot then carries the complete conversation
                // in the right order. Without this, the pause snapshot ends
                // right after the tool results and the resume re-seed has to
                // re-insert the answer at the transcript head (sys_end),
                // placing it BEFORE the tool results that preceded it.
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(msg.clone())],
                    None,
                    if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    Vec::new(),
                    response.thinking_blocks.clone(),
                ));
                let persist_message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                self.pause_turn(
                    session_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    SessionStatus::Paused,
                    &msg,
                    Some(step_num),
                    infer,
                    Some(&persist_message_id),
                    false,
                )
                .await?;
                return Ok(());
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Session completed.".into());
                // Same clobber guard as the empty-actions branch above.
                if let Some(s) = history.last_mut().filter(|s| s.step_number == step_num) {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                // The response may already have pushed its own assistant
                // message: a web-search round (pushed above with the search
                // context) or a response mixing real tool calls with the final
                // action (pushed with tool_calls below). In those cases the
                // final text is already in the canonical and must not be
                // duplicated.
                let already_pushed = has_web_search || actions.iter().any(|a| !a.is_final);
                // Same mid-turn delivery as the empty-actions branch: a
                // message that arrived during this final LLM call is injected
                // before the turn ends so it influences the answer.
                let before_inject_len = canonical.len();
                if self.inject_pending_context(&ctx, canonical).await {
                    self.deliver_final_with_pending_context(
                        &ctx,
                        &final_text,
                        response.reasoning.clone(),
                        canonical,
                        history,
                        branch_points,
                        before_inject_len,
                        already_pushed,
                    )
                    .await;
                    continue;
                }
                // Mirror the finished answer into the canonical before the
                // pause (same ordering rationale as the empty-actions branch).
                if !already_pushed {
                    canonical.push(CanonicalMessage::assistant(
                        vec![ContentPart::text(final_text.clone())],
                        None,
                        if response.thinking_blocks.is_empty() {
                            response.reasoning.clone()
                        } else {
                            None
                        },
                        Vec::new(),
                        response.thinking_blocks.clone(),
                    ));
                }
                let persist_message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                self.pause_turn(
                    session_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    SessionStatus::Paused,
                    &final_text,
                    Some(step_num),
                    infer,
                    Some(&persist_message_id),
                    false,
                )
                .await?;
                return Ok(());
            }

            if let Some(ref t) = thought {
                let text = t.trim();
                if !text.is_empty() {
                    let message_id = self.block_msg_id(session_id, step_num, run_id, "thought");
                    self.persist_session_message(
                        session_id,
                        "assistant",
                        text,
                        Some("text"),
                        None,
                        Some(&message_id),
                    )
                    .await;
                }
            }

            let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
            // Mint one `step-*` id per action, shared by the Action event,
            // the tool's step row (created inside execute_step) and the
            // Observation event, so the live card and the review badge (both
            // keyed `step-<id>`) are one entity. The ids are indexed by the
            // action's position in `non_final` (NOT by `tool_call_id`, which
            // two actions of a malformed provider response could share —
            // keying by it would collapse both onto one step id and the
            // second step-row insert would fail the PRIMARY KEY).
            let action_step_ids: Vec<String> = non_final
                .iter()
                .map(|_| haven_common::types::new_id("step"))
                .collect();
            for (idx, action) in non_final.iter().enumerate() {
                let step_id = &action_step_ids[idx];
                // Persist the pending step row BEFORE the live card so an
                // interrupt / Continue resync / app restart can rebuild it
                // from session_steps (the card used to be live-only and
                // vanished on every DB rebuild).
                self.executor
                    .begin_action_step(
                        session_id,
                        &action.tool_name,
                        &action.tool_input,
                        step_num,
                        step_id,
                    )
                    .await;
                emitter
                    .emit(crate::event::AgentEvent::Action {
                        session_id: session_id.into(),
                        tool_name: action.tool_name.clone(),
                        input: action.tool_input.clone(),
                        step_number: step_num,
                        run_id,
                        tool_call_id: action.tool_call_id.clone(),
                        step_id: step_id.clone(),
                    })
                    .await;
            }

            if !non_final.is_empty() {
                // The tool_calls echoed into the canonical assistant message
                // must exactly match the tool results pushed below, or
                // providers reject the request with a 400. They are built
                // from the ACTIONS (not `response.tool_calls`) so that a
                // retry-replaced response stays consistent: when the empty /
                // cut-off retry produced the tool calls, the original
                // `response.tool_calls` is empty and zipping it with the
                // retried actions would emit an assistant message WITHOUT
                // tool_calls followed by orphaned tool results (silently
                // dropped by sanitize_canonical, losing the observations).
                // The Action side already carries the synthesized UUID for
                // empty provider ids, matching the tool-result side below.
                let tool_calls: Option<Vec<CanonicalToolCall>> = Some(
                    non_final
                        .iter()
                        .map(|a| CanonicalToolCall {
                            id: a.tool_call_id.clone().unwrap_or_default(),
                            name: a.tool_name.clone(),
                            arguments: a.tool_input.clone(),
                        })
                        .collect(),
                );
                // The text must match what `persist_session_message` stores
                // (trimmed thought) so resume dedup cannot fail; a
                // retry-replaced response also must not echo the cut-off
                // original text.
                let push_text = thought.as_deref().unwrap_or(&response.text);
                // A response mixing real tool calls with a web search round
                // carries both: the `web_search_call` items round-trip in the
                // same assistant message so the next request restores the
                // search context alongside the function tool results.
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(push_text.to_string())],
                    tool_calls,
                    if response.thinking_blocks.is_empty() {
                        response.reasoning.clone()
                    } else {
                        None
                    },
                    response.web_search_calls.clone(),
                    response.thinking_blocks.clone(),
                ));
            }

            self.save_branch_point(
                session_id,
                canonical,
                history,
                step_num,
                branch_points,
                false,
            )
            .await;

            use futures_util::StreamExt;

            let mut tool_futures = futures_util::stream::FuturesUnordered::new();
            for (idx, action) in non_final.iter().enumerate() {
                let session_id = session_id.to_string();
                let tool_name = action.tool_name.clone();
                let tool_input = action.tool_input.clone();
                let action = (*action).clone();
                let max_obs = self.context_limits.max_observation_chars;
                let executor = self.executor.clone();
                // The same step id minted at Action-emit time keys the step
                // row execute_step creates, so the live card id, the DB badge
                // id and this step id are identical everywhere.
                let step_id = action_step_ids[idx].clone();
                tool_futures.push(async move {
                    tracing::debug!(
                        "executing tool '{}' at step {} (input keys: {:?})",
                        tool_name,
                        step_num,
                        tool_input
                            .as_object()
                            .map(|o| o.keys().collect::<Vec<_>>())
                            .unwrap_or_default()
                    );
                    tracing::trace!(
                        "tool '{}' at step {} full input: {} chars",
                        tool_name,
                        step_num,
                        tool_input
                            .as_object()
                            .map(|o| serde_json::to_string(o).map(|s| s.len()).unwrap_or(0))
                            .unwrap_or(0)
                    );
                    let result = executor
                        .execute_step(
                            &session_id,
                            &tool_name,
                            tool_input.clone(),
                            step_num,
                            &step_id,
                        )
                        .await;
                    let (text, is_error, ask_question, ask_options, notify_title, notify_body) =
                        match result {
                            Ok(r) => {
                                tracing::debug!(
                                    "tool '{}' at step {} completed: success={}, {} chars",
                                    tool_name,
                                    step_num,
                                    r.success,
                                    serde_json::to_string(&r.output)
                                        .map(|s| s.len())
                                        .unwrap_or(0)
                                );
                                tracing::trace!(
                                    "tool '{}' at step {} full output: {} chars",
                                    tool_name,
                                    step_num,
                                    serde_json::to_string(&r.output)
                                        .map(|s| s.len())
                                        .unwrap_or(0)
                                );
                                let text = r.summary_text();
                                let text = if text.len() > max_obs {
                                    let cutoff = text.floor_char_boundary(max_obs);
                                    format!(
                                        "{}[... truncated {} chars omitted]",
                                        &text[..cutoff],
                                        text.len() - cutoff
                                    )
                                } else {
                                    text
                                };
                                // The ask/notify signals are attached to the
                                // result by the tool itself (declared via
                                // `Tool::signals`) BEFORE the loop truncates
                                // the observation text, so a question or toast
                                // is never lost to the budget.
                                let ask_question = r.signals.ask_question.clone();
                                let ask_options = r.signals.ask_options.clone();
                                let notify_title = r.signals.notify_title.clone();
                                let notify_body = r.signals.notify_body.clone();
                                (
                                    text,
                                    !r.success,
                                    ask_question,
                                    ask_options,
                                    notify_title,
                                    notify_body,
                                )
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "tool '{}' at step {} failed: {}",
                                    tool_name,
                                    step_num,
                                    e
                                );
                                (e.to_string(), true, None, Vec::new(), None, None)
                            }
                        };
                    (
                        action,
                        tool_name,
                        text,
                        is_error,
                        ask_question,
                        ask_options,
                        notify_title,
                        notify_body,
                        step_id,
                    )
                });
            }

            let mut any_tool_failure = false;
            // Bounded per-step failure evidence (tool name + error tail) used
            // to classify failures as environmental vs logic when composing
            // the retry nudge — a broken proxy or a missing command must not
            // push the model to abandon a sound approach.
            let mut failure_signals: Vec<(String, String)> = Vec::new();
            // Tool calls in this batch that already produced a result, keyed
            // by the same identity the action/observation pairing uses. When
            // the batch is cancelled mid-flight, every `non_final` action NOT
            // in this set was cut off — it must still be repaired with an
            // "Interrupted" result and surfaced, not silently dropped.
            let mut completed_tool_keys: HashSet<String> = HashSet::new();
            // If the agent invoked the `ask` tool, the session must pause and
            // wait for the user's reply (delivered as a supplement). Collect
            // every question in the batch so all are surfaced, plus the step
            // row id of each ask action: the question message is persisted
            // under that id so the ask card and its content share one entity.
            let mut asked_questions: Vec<String> = Vec::new();
            let mut ask_step_ids: Vec<String> = Vec::new();
            // Drain tool results while remaining responsive to cancellation.
            // Without select!, a cancel arriving mid-batch would only be
            // detected at the next step boundary —after all tools finish.
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_res.cancelled() => {
                        tracing::info!("ReAct loop cancelled during tool batch at step {}", step_num);
                        // Tool calls still in flight were cut off, not skipped:
                        // repair EACH one with an "Interrupted" result so the
                        // model sees the tool was attempted (and may retry it),
                        // and surface it in the UI as an interrupted
                        // observation card rather than leaving a silent gap.
                        for (idx, action) in non_final.iter().enumerate() {
                            if completed_tool_keys.contains(&tool_key(action)) {
                                continue;
                            }
                            let silent_action =
                                is_silent_action(&action.tool_name, &action.tool_input);
                            let interrupted_text = crate::interrupted_result_text(
                                &action.tool_name,
                                &action.tool_input,
                            );
                            // Complete the pending step row minted at Action
                            // time so review/resume rebuilds the Interrupted
                            // card from session_steps (not live-only).
                            let step_id = action_step_ids[idx].clone();
                            self.executor
                                .finish_interrupted_step(
                                    session_id,
                                    &action.tool_name,
                                    &action.tool_input,
                                    step_num,
                                    &step_id,
                                    &interrupted_text,
                                )
                                .await;
                            emitter
                                .emit(crate::event::AgentEvent::Observation {
                                    session_id: session_id.into(),
                                    observation: interrupted_text.clone(),
                                    tool_name: action.tool_name.clone(),
                                    step_number: step_num,
                                    run_id,
                                    silent: silent_action,
                                    tool_call_id: action.tool_call_id.clone(),
                                    ask_options: Vec::new(),
                                    step_id,
                                })
                                .await;
                            canonical.push(CanonicalMessage::tool(
                                vec![ContentPart::text(interrupted_text.clone())],
                                action.tool_call_id.clone(),
                            ));
                            if let Some(step) = history
                                .iter_mut()
                                .find(|s| s.step_number == step_num && s.action.is_none())
                            {
                                step.action = Some((*action).clone());
                                step.observation = Some(interrupted_text);
                            } else {
                                history.push(ReActStep {
                                    step_number: step_num,
                                    thought: None,
                                    action: Some((*action).clone()),
                                    observation: Some(interrupted_text),
                                });
                            }
                        }
                        // A rollback that lands mid-batch must find the DB row
                        // at the pre-batch branch point (the response and
                        // partial tool results are discarded by the exit).
                        self.save_exit_snapshot(
                            session_id,
                            canonical,
                            history,
                            step_num,
                            branch_points,
                        )
                        .await;
                        return Ok(());
                    }
                    item = tool_futures.next() => {
                        let Some((
                            action,
                            tool_name,
                            step_result,
                            is_error,
                            ask_question,
                            ask_options,
                            notify_title,
                            notify_body,
                            step_id,
                        )) = item
                        else {
                            break;
                        };
                        if is_error {
                            any_tool_failure = true;
                            if failure_signals.len() < 3 {
                                let cap: String = step_result.chars().take(600).collect();
                                failure_signals.push((tool_name.clone(), cap));
                            }
                        }
                        // The `notify` tool requests a user-facing notification:
                        // emit it (in-app toast + Windows) without pausing the
                        // ReAct loop.
                        if let (Some(title), Some(body)) = (&notify_title, &notify_body) {
                            emitter
                                .emit(crate::event::AgentEvent::Notification {
                                    session_id: session_id.into(),
                                    title: title.clone(),
                                    body: body.clone(),
                                })
                                .await;
                        }
                        // Surface an `ask` result as a readable question rather
                        // than raw JSON. The user's reply arrives via
                        // process_input —supplement —Paused → Pending resume.
                        if let Some(q) = &ask_question {
                            asked_questions.push(q.clone());
                            ask_step_ids.push(step_id.clone());
                        }
                        // `ask` must never be silent: hiding the question
                        // while the session pauses for an answer would leave the
                        // user waiting on a question they can't see.
                        let silent = is_silent_action(&tool_name, &action.tool_input)
                            // An empty message_inbox poll carries no user
                            // information — hide the card instead of spamming
                            // the chat on every routine check.
                            || (tool_name == "message_inbox"
                                && empty_inbox_output(&step_result));
                        // For `ask`, the chat/review bubble shows the readable
                        // question text; the canonical (model) context keeps
                        // the raw JSON so the model can still parse the flag.
                        // Same for `notify`: show a readable confirmation
                        // instead of the raw signal JSON.
                        let display_observation = if let Some(q) = &ask_question {
                            q.clone()
                        } else if let Some(title) = &notify_title {
                            let body = notify_body.clone().unwrap_or_default();
                            if body.is_empty() {
                                step_result.clone()
                            } else {
                                format!("Notification sent: {title}: {body}")
                            }
                        } else {
                            step_result.clone()
                        };
                        emitter
                            .emit(crate::event::AgentEvent::Observation {
                                session_id: session_id.into(),
                                observation: display_observation.clone(),
                                tool_name: tool_name.clone(),
                                step_number: step_num,
                                run_id,
                                silent,
                                tool_call_id: action.tool_call_id.clone(),
                                ask_options: ask_options.clone(),
                                step_id,
                            })
                            .await;

                        if let Some(last) = history
                            .last_mut()
                            .filter(|s| s.step_number == step_num && s.action.is_none())
                        {
                            // First tool result of this step: fill the thought
                            // entry pushed at step start.
                            last.action = Some(action.clone());
                            last.observation = Some(display_observation.clone());
                        } else {
                            // A later tool of a multi-tool step, or a tool-only
                            // step (thought was None, so no entry was pushed at
                            // step start): append a fresh entry instead of
                            // overwriting the previous entry. The old behavior
                            // kept only the LAST completed tool per step (and
                            // could clobber the PREVIOUS step's entry when the
                            // response carried no thought), silently dropping
                            // every other tool from the step history — which
                            // also made restore_per_session_tools miss parallel
                            // load_skill/load_mcp registrations on restart.
                            history.push(ReActStep {
                                step_number: step_num,
                                thought: None,
                                action: Some(action.clone()),
                                observation: Some(display_observation),
                            });
                        }

                        canonical.push(CanonicalMessage::tool(
                            vec![ContentPart::text(step_result)],
                            action.tool_call_id.clone(),
                        ));
                        completed_tool_keys.insert(tool_key(&action));
                    }
                }
            }

            // Skip the retry nudge when the batch asked the user: it would be
            // baked into the paused snapshot ahead of the user's real answer,
            // contradicting the pending question.
            if any_tool_failure && asked_questions.is_empty() && step_num < max_steps - 1 {
                canonical.push(CanonicalMessage::user_text(Self::build_failure_nudge(
                    &failure_signals,
                )));
            }

            // The agent asked the human a question: pause so the user can
            // answer. Their reply arrives as a supplement and resumes the session
            // (Paused —Pending —dispatcher re-enters the loop, injecting the
            // answer as context at the top of the next step).
            if !asked_questions.is_empty() {
                let question = asked_questions.join("\n\n");
                // Persist one question message per ask step, each under the
                // step row's id: the message row is the ask card's content
                // authority (the step row only carries execution state), and
                // the shared id lets the review builder link them without
                // content matching or a sentinel. The message row also
                // re-seeds the question into the canonical on resume. A
                // defensive fresh id keeps the question visible even if a
                // step row is missing.
                for (i, q) in asked_questions.iter().enumerate() {
                    let msg_id = ask_step_ids
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| haven_common::types::new_id("step"));
                    self.persist_session_message(
                        session_id,
                        "assistant",
                        q,
                        Some("text"),
                        None,
                        Some(&msg_id),
                    )
                    .await;
                }
                // TOCTOU: a reply that arrived during the drain window went to
                // the steering queue (session was still Running). Convert it to a
                // supplement and, if present, resume immediately as Pending so
                // the answer isn't stranded while the session sits Paused. The
                // steering queue holds only user interjections now —background
                // action results are buffered separately —so `has_answer` truly
                // reflects a human reply.
                let steering = self.executor.get_steering(session_id).await;
                let has_answer = !steering.is_empty();
                for s in &steering {
                    // The interjection is the user's reply to the pending
                    // question: queue it as a paired answer so the model does
                    // not re-answer the old question on resume.
                    if let Err(e) = self
                        .executor
                        .add_answer_with_attachments(
                            session_id,
                            &s.text,
                            &s.attachments,
                            s.message_id.clone(),
                        )
                        .await
                    {
                        tracing::warn!(
                            "failed to queue user answer for asked question (session={}): {}",
                            session_id,
                            e
                        );
                    }
                }
                let status = if has_answer {
                    SessionStatus::Pending
                } else {
                    // No reply arrived: the session pauses awaiting the user's
                    // answer (PausedAwaitingAnswer blocks auto-wake by
                    // background-action completions).
                    SessionStatus::PausedAwaitingAnswer
                };
                self.pause_turn(
                    session_id,
                    canonical,
                    history,
                    step_num + 1,
                    branch_points,
                    &emitter,
                    status,
                    &question,
                    None,
                    infer,
                    // The question messages were persisted above (one per ask
                    // step, under the step ids); `is_ask` tells pause_turn to
                    // skip its own persist.
                    None,
                    true,
                )
                .await?;
                return Ok(());
            }

            let state = self.executor.get_session_state(session_id).await;
            match state {
                Some(s) if s.is_paused() => {
                    self.save_snapshot_with_branches(
                        session_id,
                        canonical,
                        history,
                        step_num,
                        branch_points,
                    )
                    .await;
                    return Ok(());
                }
                // Session gone (end_session/terminal cleanup) or terminal: exit.
                None | Some(SessionStatus::Error) | Some(SessionStatus::Completed) => return Ok(()),
                _ => {}
            }
        }

        self.pause_turn_budget(
            session_id,
            canonical,
            history,
            last_step + 1,
            branch_points,
            &emitter,
            infer,
        )
        .await?;
        Ok(())
    }

    /// Persist an assistant message into the session's message stream.
    /// Delegates to the shared `crate::persist_session_message` so this path
    /// cannot drift from the user-turn persistence path (same trim, same
    /// error policy). Persistence failures are logged here instead of being
    /// silently swallowed: a dropped write would make the streamed content
    /// disappear after a reload while the UI keeps showing it.
    async fn persist_session_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        message_id: Option<&str>,
    ) {
        if let Err(e) = crate::persist_session_message(
            &self.executor,
            session_id,
            role,
            content,
            message_type,
            &[],
            false,
            message_id,
            tool_call_id,
        )
        .await
        {
            tracing::warn!(
                "ReAct: failed to persist {} message for session {} (type={:?}): {}",
                role,
                session_id,
                message_type,
                e
            );
        }
    }

    /// Persist a compaction summary into episodic long-term memory
    /// (`memory_episodes`) so context that compaction summarized away stays
    /// retrievable across sessions (embedding + keyword recall). Fire-and-forget:
    /// a dropped write only loses the summary episode, never the session itself.
    async fn persist_compaction_summary(&self, session_id: &str, summary: &str) {
        let summary = summary.trim();
        if summary.is_empty() {
            return;
        }
        let db = self.db.clone();
        let session_id = session_id.to_string();
        let summary = summary.to_string();
        let session_id_owned = session_id.clone();
        if let Err(e) = db
            .run_blocking(move |db| {
                db.add_episode(&session_id_owned, &summary)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
        {
            tracing::warn!(
                "ReAct: failed to persist compaction summary for session {}: {}",
                session_id,
                e
            );
        }
    }

    /// Finalize a turn: persist the assistant text, save the branch point
    /// (when requested), snapshot the ReAct state, then mark the session with
    /// the given status and notify the frontend + inference. Shared by all
    /// pause/complete paths so the persist → branch-point → snapshot →
    /// status → event ordering cannot drift between them. The snapshot is
    /// taken after the branch point so it includes the newly added entry.
    /// Callers pause with `SessionStatus::Paused` (scheduling) or
    /// `SessionStatus::PausedAwaitingAnswer` (the `ask` tool is blocked on a
    /// human reply — that flavor also blocks background-action auto-wake).
    /// The step-budget checkpoint uses `pause_turn_budget` instead, which
    /// skips the assistant-message persist (the notice is a notification).
    #[allow(clippy::too_many_arguments)]
    async fn pause_turn(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        snapshot_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        status: SessionStatus,
        final_text: &str,
        branch_point_step: Option<u32>,
        infer: &(dyn Fn() + Send + Sync),
        // Pre-minted id of the streamed thought bubble this final text is the
        // authoritative copy of (`None` mints a fresh id).
        persist_message_id: Option<&str>,
        // True when this pause follows an `ask` batch: the question message
        // rows were already persisted by the caller (one per ask step, under
        // the step ids), so the persist below is skipped.
        is_ask: bool,
    ) -> anyhow::Result<()> {
        tracing::info!(
            "ReAct turn finished: session={} step={} status={} final={} chars",
            session_id,
            snapshot_step,
            status.as_str(),
            final_text.chars().count()
        );
        if std::env::var("HAVEN_DEBUG_PAUSE").is_ok() {
            eprintln!(
                "DEBUG pause_turn persist ask={} id={:?} final={}",
                is_ask, persist_message_id, final_text
            );
        }
        if !is_ask {
            self.persist_session_message(
                session_id,
                "assistant",
                final_text,
                Some("text"),
                None,
                persist_message_id,
            )
            .await;
        }
        if let Some(step) = branch_point_step {
            self.save_branch_point(session_id, canonical, history, step, branch_points, false)
                .await;
        }
        self.save_snapshot_with_branches(
            session_id,
            canonical,
            history,
            snapshot_step,
            branch_points,
        )
        .await;
        // The status itself carries the awaiting-answer flavor
        // (`PausedAwaitingAnswer`), so the transition is atomic: a
        // background-action completion landing concurrently reads the final
        // state and cannot auto-wake an answer-blocked session.
        set_status_and_emit(&self.executor, emitter, session_id, status).await?;
        infer();
        Ok(())
    }

    /// Pause the session because the run exhausted its step budget. Mirrors
    /// `pause_turn`'s checkpoint side effects (snapshot, Paused status,
    /// infer) but does NOT persist an assistant chat message: system notices
    /// of this kind must not pollute the conversation stream as fake agent
    /// replies —they are surfaced as a notification (in-app toast +
    /// Windows) instead, so the user sees them without the chat pretending
    /// the turn produced an answer.
    #[allow(clippy::too_many_arguments)]
    async fn pause_turn_budget(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        snapshot_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn() + Send + Sync),
    ) -> anyhow::Result<()> {
        tracing::info!(
            "ReAct step budget exhausted: session={} next_step={}",
            session_id,
            snapshot_step
        );
        self.save_snapshot_with_branches(
            session_id,
            canonical,
            history,
            snapshot_step,
            branch_points,
        )
        .await;
        set_status_and_emit(&self.executor, emitter, session_id, SessionStatus::Paused).await?;
        emitter
            .emit(crate::event::AgentEvent::Notification {
                session_id: session_id.into(),
                title: BUDGET_EXHAUSTED_TITLE.into(),
                body: BUDGET_EXHAUSTED_BODY.into(),
            })
            .await;
        infer();
        Ok(())
    }

    /// True when the canonical ends with an unanswered `ask`: an `ask` tool
    /// result is present and no user message follows it. The ask pause path
    /// normally prevents this state from reaching an LLM call, but a reply
    /// lost to compaction/sanitization or a dropped answer can leave the
    /// question dangling — and a model Stop response must then not be judged
    /// final (it would end the turn with the question still unanswered).
    ///
    /// Scans backward from the tail: the first user message OR ask result
    /// encountered decides. This is equivalent to the old forward scan (the
    /// last ask must be followed only by non-User messages) but resolves in
    /// O(recent window) instead of O(whole canonical) per step.
    fn canonical_has_pending_ask(canonical: &[CanonicalMessage]) -> bool {
        for m in canonical.iter().rev() {
            match m.role {
                CanonicalRole::User => return false,
                CanonicalRole::Tool
                    if m.content.iter().any(|p| {
                        matches!(p, ContentPart::Text(t) if t.contains("\"ask\":true") || t.contains("\"ask\": true"))
                    }) =>
                {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Extract the question text of the last unanswered `ask` tool result in
    /// the canonical. Falls back to a generic prompt when the tool output is
    /// truncated or unparseable.
    fn extract_pending_ask_question(canonical: &[CanonicalMessage]) -> String {
        for m in canonical.iter().rev() {
            if m.role != CanonicalRole::Tool {
                continue;
            }
            for p in &m.content {
                let ContentPart::Text(t) = p else { continue };
                if !(t.contains("\"ask\":true") || t.contains("\"ask\": true")) {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t)
                    && let Some(q) = v.get("question").and_then(|q| q.as_str())
                {
                    return q.to_string();
                }
            }
        }
        "I have a pending question for you.".into()
    }

    /// True when a text-only response should not be trusted as a deliberate
    /// final answer: either the provider did not report Stop (truncated /
    /// filtered / unknown finish), the text itself ends mid-sentence (trailing
    /// comma/connector/ellipsis —the generation was interrupted rather than
    /// concluded), or it ends on a planning/transition phrase (「接下来」「确认
    /// 一下」…) that signals the model was about to take a further action but
    /// stopped short of describing/issuing it.
    fn looks_cut_off(text: &str) -> bool {
        const PLAN_ENDINGS: &[&str] = &[
            // Chinese: plan/transition phrases that expect a following action
            "接下来",
            "下一步",
            "然后",
            "接着",
            "再确认",
            "确认一下",
            "检查一下",
            "核对一下",
            "查看一下",
            "再看",
            "以便",
            "才能",
            // English: transition/plan phrases
            "next",
            "next step",
            "then",
            "let me",
            "I will",
            "I'll",
        ];
        let t = text.trim_end();
        text.ends_with("...")
            || text.ends_with("路路路")
            || PLAN_ENDINGS.iter().any(|w| t.ends_with(w))
            || matches!(
                t.chars().last(),
                Some('，')
                    | Some('：')
                    | Some('！')
                    | Some(',')
                    | Some(';')
                    | Some(':')
                    | Some('…')
            )
    }

    /// True when the parsed response is a text-only "final" that must be
    /// retried before ending the turn. Trusts explicit tool calls (final or
    /// not) and empty responses (handled by the empty-response retry); only
    /// a thought without actions is examined, and it must pass the
    /// finish-reason, mid-sentence, and mid-session checks.
    ///
    /// `canonical` supplies the mid-session signal: when the agent has pending
    /// tool context (the canonical ends in tool results with no user reply), a
    /// text-only Stop is far more likely to be "I'll do X next" narration than
    /// a deliberate final answer, so it is treated as suspect too.
    fn is_suspect_final(
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
        canonical: &[CanonicalMessage],
    ) -> bool {
        if !actions.is_empty()
            && !actions
                .iter()
                .all(|a| a.is_final && a.tool_call_id.is_none())
        {
            return false;
        }
        match thought {
            Some(t) => {
                response.finish_reason != Some(FinishReason::Stop)
                    || Self::looks_cut_off(t)
                    || Self::canonical_has_pending_tool_context(canonical)
            }
            None => false,
        }
    }

    /// True when the agent is mid-session: scanning back from the tail, the first
    /// User message or Tool result decides. A Tool result before any User
    /// message means tool(s) ran this turn and the reply has not come yet, so
    /// a text-only Stop should not be trusted as final. A User message first
    /// means a fresh turn (the agent is answering, not continuing tool work).
    fn canonical_has_pending_tool_context(canonical: &[CanonicalMessage]) -> bool {
        for m in canonical.iter().rev() {
            match m.role {
                CanonicalRole::User => return false,
                CanonicalRole::Tool => return true,
                _ => {}
            }
        }
        false
    }

    /// Compose the retry nudge after a step where tool calls failed. The
    /// failure evidence is classified first: environment-type failures
    /// (missing command, wrong shell syntax, network/proxy, paths) must NOT
    /// push the model to abandon its approach — the correct move is to
    /// diagnose and fix the environment (different shell, different tool,
    /// corrected path) and retry. Logic failures get a fix-and-retry nudge
    /// with an explicit threshold before switching approach. This replaces
    /// the old unconditional "try a completely different approach" nudge,
    /// which repeatedly sent users down wrong paths when the real cause was
    /// environmental (Get-FileHash missing in the chosen shell, a broken
    /// proxy, a different 7z path). The generic branch reuses the canonical
    /// guidance from the system prompt (guideline 12) so the two cannot
    /// drift.
    fn build_failure_nudge(failures: &[(String, String)]) -> String {
        let has_env = failures
            .iter()
            .any(|(t, e)| Self::classify_tool_failure(t, e) == FailureKind::Environmental);
        let has_logic = failures
            .iter()
            .any(|(t, e)| Self::classify_tool_failure(t, e) == FailureKind::Logic);
        if has_env {
            "The tool failures look ENVIRONMENTAL (missing command / wrong shell syntax / network / path), not logic errors. Do NOT abandon your approach. Diagnose the environment first: verify the command exists in the shell you chose (cmd vs PowerShell syntax differs; `&&` only works in cmd), check network/proxy/endpoints, fix paths and prerequisites. Switching tools (e.g. curl -> aria2) or shells is an environment fix, not a change of approach — keep the same approach and retry."
                .into()
        } else if has_logic {
            "The previous approach failed with logic errors. Analyze the exact error, fix the specific mistake, and retry. Only consider a completely different approach if the same method fails again after you fixed it."
                .into()
        } else {
            format!(
                "The previous approach encountered errors. {}",
                haven_common::prompts::TOOL_FAILURE_DIAGNOSIS
            )
        }
    }

    /// Heuristic classification of a tool failure: environment problems (the
    /// user's tools/environment cannot run the approach) vs logic problems
    /// (the approach itself is flawed). Used to shape the retry nudge so
    /// environmental failures do not trigger an unnecessary method switch.
    fn classify_tool_failure(tool_name: &str, err: &str) -> FailureKind {
        // Tool-usage mistakes by the model itself (missing params, invalid
        // input) are logic errors: the schema/validation error names the fix.
        if tool_name == "files"
            && (err.contains("MISSING REQUIRED FIELD")
                || err.contains("old_string")
                || err.contains("not found in file"))
        {
            return FailureKind::Logic;
        }
        let e = err.to_lowercase();
        const ENV_MARKERS: &[&str] = &[
            // command / executable missing
            "not recognized",
            "not recognized as an internal or external command",
            "不是内部或外部命令",
            "command not found",
            "无法识别",
            "not found",
            "cannot be found",
            "cannot find",
            "找不到",
            "no such file",
            "no such directory",
            "spawn",
            "program not found",
            // network / proxy / transport
            "connection",
            "timed out",
            "timeout",
            "refused",
            "reset",
            "proxy",
            "unreachable",
            "resolve",
            "dns",
            "ssl",
            "tls",
            "certificate",
            "failed to connect",
            "tunnel",
            "network",
            // paths / permissions
            "path does not exist",
            "路径不存在",
            "access denied",
            "拒绝访问",
            // PowerShell/7z style environment mismatches
            "无法将",
            "不是有效的",
        ];
        if ENV_MARKERS.iter().any(|m| e.contains(m)) {
            return FailureKind::Environmental;
        }
        const LOGIC_MARKERS: &[&str] = &[
            "validation failed",
            "missing required",
            "parse error",
            "syntax error",
            "unterminated",
            "invalid json",
            "is required for",
        ];
        if LOGIC_MARKERS.iter().any(|m| e.contains(m)) {
            return FailureKind::Logic;
        }
        FailureKind::Unknown
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

    /// Persist one final snapshot before leaving the loop on a cancellation,
    /// so the DB row is never stale when `rollback_session` / `continue_session`
    /// read it after the handler exits. The mid-run throttle in
    /// `save_branch_point` may have skipped the last write, and the state at
    /// this point is always a clean step boundary (the cancelled response or
    /// partial tool results are discarded by the exit).
    async fn save_exit_snapshot(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        self.save_snapshot_with_branches(
            session_id,
            canonical,
            history,
            step_number,
            branch_points,
        )
        .await;
    }

    /// Save snapshot including branch points for tree-structured rollback (鎼?).
    ///
    /// Serializes a borrowed view of the ReAct state (no per-step deep copies
    /// of canonical/history/branch_points —those clones were O(n²) over a
    /// long session) into a reusable buffer, then writes to SQLite on the
    /// blocking thread pool so the WAL fsync never stalls the async runtime.
    async fn save_snapshot_with_branches(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        let view = SnapshotView {
            canonical,
            history,
            step_number,
            branch_points,
            saved_at: Some(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        };
        // Serialize into the session's own buffer inside a scoped block so the
        // mutex guard is dropped before the await below (the guard is not
        // Send, so it must not be live across the spawn_blocking boundary).
        let bytes = {
            let mut bufs = self.snapshot_bufs.lock().unwrap();
            let buf = bufs.entry(session_id.to_string()).or_default();
            buf.clear();
            if serde_json::to_writer(&mut *buf, &view).is_err() {
                return;
            }
            std::mem::take(buf)
        };
        let json = String::from_utf8(bytes).unwrap_or_default();
        let db = self.db.clone();
        let tid_owned = session_id.to_string();
        // Return ownership of the serialized bytes so the allocation is
        // handed back to the session's buffer for reuse on the next snapshot.
        let back: String = db
            .run_blocking(move |db| {
                if let Err(e) = db.save_react_state(&tid_owned, &json) {
                    tracing::warn!("save_react_state failed for session {}: {}", tid_owned, e);
                }
                Ok(json)
            })
            .await
            .unwrap_or_default();
        if let Ok(mut bufs) = self.snapshot_bufs.lock() {
            *bufs.entry(session_id.to_string()).or_default() = back.into_bytes();
        }
    }
    /// and save a snapshot so the session can be resumed via "continue" or
    /// rolled back. Without this, any text streamed before the error is lost
    /// on page refresh because it was only in the frontend's memory.
    async fn persist_partial_on_error(
        &self,
        ctx: &StepCtx,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) {
        // Save a branch point BEFORE persisting the partial output, so
        // last_msg_at captures the timestamp of the last message BEFORE the
        // partial. This lets continue_session / rollback_session precisely delete
        // only the partial output via delete_messages_after(last_msg_at).
        // The canonical/history here represent the state BEFORE the failed
        // LLM call (the response was never pushed to canonical), so resuming
        // will retry the step cleanly.
        // FORCED write: continue_session / rollback_session locate this branch
        // point in the DB snapshot; a throttled (stale) row would silently
        // skip their message truncation.
        self.save_branch_point(
            &ctx.session_id,
            canonical,
            history,
            ctx.step_num,
            branch_points,
            true,
        )
        .await;

        let thought_text = partial_thought.lock().unwrap().clone();
        let reasoning_text = partial_reasoning.lock().unwrap().clone();
        if !reasoning_text.trim().is_empty() {
            let message_id =
                self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
            self.persist_session_message(
                &ctx.session_id,
                "assistant",
                reasoning_text.trim(),
                Some("reasoning"),
                None,
                Some(&message_id),
            )
            .await;
        }
        if !thought_text.trim().is_empty() {
            let text = thought_text.trim();
            let message_id =
                self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
            self.persist_session_message(
                &ctx.session_id,
                "assistant",
                text,
                Some("text"),
                None,
                Some(&message_id),
            )
            .await;
            EventDispatcher::emit_thought_from(
                &ctx.emitter,
                &ctx.session_id,
                text,
                ctx.step_num,
                ctx.run_id,
                &message_id,
                &self.db,
            )
            .await;
        }
        // The stream text now lives in the message stream (persisted above),
        // so any checkpointed partial row for this session is obsolete — and an
        // in-flight checkpoint write must not re-create it. Discard goes
        // through the PartialStore, whose generation bump invalidates stale
        // writes.
        self.executor.partials.discard(&ctx.session_id).await;
    }

    /// Save a branch point at the current step before tool execution (—).
    ///
    /// The DB snapshot write is throttled to every `SNAPSHOT_WRITE_INTERVAL`
    /// steps on the happy path (`force = false`): the in-memory branch-point
    /// map is always current, and every pause/error/final path plus every
    /// cancellation exit writes unconditionally. Error paths MUST pass
    /// `force = true` (e.g. `persist_partial_on_error`): `continue_session` /
    /// `rollback_session` locate the failed step's branch point in the DB
    /// snapshot, and a stale row would silently skip their message truncation.
    async fn save_branch_point(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        force: bool,
    ) {
        // `get_last_message_created_at` is a blocking SQLite read; run it on
        // the blocking thread pool instead of the async runtime.
        let db = self.db.clone();
        let session_id_owned = session_id.to_string();
        let last_msg_at = db
            .run_blocking(move |db| Ok(db.get_last_message_created_at(&session_id_owned)))
            .await
            .ok()
            .flatten();
        branch_points.insert(
            step_number,
            BranchPoint {
                canonical: canonical.to_vec(),
                history: history.to_vec(),
                step_number,
                last_msg_at,
            },
        );
        // The throttle marker guard is confined to this block so it is always
        // dropped before the write's await.
        let due = {
            let mut last_written = self.last_snapshot_step.lock().unwrap();
            let due = force
                || last_written.get(session_id).is_none_or(|last| {
                    step_number.saturating_sub(*last) >= SNAPSHOT_WRITE_INTERVAL
                });
            if due {
                last_written.insert(session_id.to_string(), step_number);
            }
            due
        };
        if due {
            self.save_snapshot_with_branches(
                session_id,
                canonical,
                history,
                step_number,
                branch_points,
            )
            .await;
        }
    }

    /// Run one streamed LLM call for an agent step: spawn the chunk consumer,
    /// forward text/reasoning chunks to the frontend while accumulating them
    /// into the partial buffers (persisted if the step fails mid-stream), then
    /// drain the consumer and return the aggregated response. Shared by the
    /// primary step call and the post-compaction retry so the two cannot
    /// drift. Error handling stays at the call site.
    /// One step's primary LLM call: streamed with live chunk forwarding,
    /// cancellation, the stall watchdog and partial-text recovery. Returns
    /// the aggregated response plus the wall-clock duration of the API call
    /// in milliseconds (persisted with the per-call usage detail).
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    async fn stream_llm_step(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        llm_messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<(LlmResponse, u64), haven_llm::LlmError> {
        // Mint the block ids this call's chunks accumulate into. Reused by
        // the chunk events, the snap and the final persistence of this step.
        let thought_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        let reasoning_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
        let (forwarder, on_chunk) = StreamForwarder::new(
            ctx,
            self.context_limits.event_chunk_batch_max_bytes,
            self.context_limits.stream_stall_warn_delay_ms,
            partial_thought,
            partial_reasoning,
            self.executor.partials.clone(),
            self.context_limits.partial_checkpoint_min_chars,
            std::time::Duration::from_secs(self.context_limits.partial_checkpoint_interval_secs),
            cancel.clone(),
            true,
            thought_msg_id,
            reasoning_msg_id,
        );
        let started = std::time::Instant::now();
        let result = router
            .chat_stream_with_tools_aggregated_cancellable(
                role,
                llm_messages,
                tools,
                on_chunk,
                cancel,
            )
            .await;
        let duration_ms = started.elapsed().as_millis() as u64;
        forwarder.flush().await;
        match result {
            Ok(resp) => {
                tracing::debug!(
                    "ReAct step {} session {} LLM stream took {} ms ({} text chars, {} tool_calls)",
                    ctx.step_num,
                    ctx.session_id,
                    duration_ms,
                    resp.text.len(),
                    resp.tool_calls.len()
                );
                Ok((resp, duration_ms))
            }
            Err(e) => Err(e),
        }
    }

    /// One empty-response / cut-off retry with live chunk forwarding: streamed
    /// text is accumulated into the partial buffers (crash recovery) and
    /// forwarded to the frontend, so a recovering provider never leaves the
    /// UI frozen for the retry's whole budget. Reasoning is accumulated but
    /// NOT forwarded (the UI block may already hold the primary generation's
    /// reconciled text; a fresh reasoning pass would concatenate onto it).
    /// The stall watchdog runs exactly like the primary call.
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    async fn stream_retry_step(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<LlmResponse, haven_llm::LlmError> {
        // Retry chunks reuse the primary call's minted ids (same step/run),
        // so the frontend continues the same bubble instead of splitting it.
        let thought_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        let reasoning_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
        let (forwarder, on_chunk) = StreamForwarder::new(
            ctx,
            self.context_limits.event_chunk_batch_max_bytes,
            self.context_limits.stream_stall_warn_delay_ms,
            partial_thought,
            partial_reasoning,
            self.executor.partials.clone(),
            self.context_limits.partial_checkpoint_min_chars,
            std::time::Duration::from_secs(self.context_limits.partial_checkpoint_interval_secs),
            cancel.clone(),
            false,
            thought_msg_id,
            reasoning_msg_id,
        );
        let result = router
            .chat_stream_with_tools_aggregated_cancellable(role, messages, tools, on_chunk, cancel)
            .await;
        forwarder.flush().await;
        result
    }

    /// One step's full LLM call, including the context-length compaction
    /// retry and all failure paths. The loop dispatches on the returned
    /// [`StepCallOutcome`] instead of inlining the error handling: a
    /// `Fatal` outcome has already persisted partial text, emitted the error
    /// event and marked the session Error.
    #[allow(clippy::too_many_arguments)] // consolidates ~130 lines of inline error handling
    async fn call_step_llm(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        llm_messages: &mut Vec<CanonicalMessage>,
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        canonical: &mut Vec<CanonicalMessage>,
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> StepCallOutcome {
        match self
            .stream_llm_step(
                ctx,
                router.clone(),
                role,
                llm_messages,
                tools,
                cancel.clone(),
                partial_thought,
                partial_reasoning,
            )
            .await
        {
            Ok((resp, duration_ms)) => {
                if router.balanced_model_active() {
                    self.emit_balanced_model(
                        &ctx.emitter,
                        &ctx.session_id,
                        "switching to balanced model",
                    )
                    .await;
                }
                self.record_usage_and_emit(
                    &ctx.session_id,
                    role,
                    &resp,
                    ctx.step_num as i32,
                    Some(duration_ms),
                    &ctx.emitter,
                )
                .await;
                StepCallOutcome::Response(resp)
            }
            Err(haven_llm::LlmError::ContextLengthExceeded) => {
                tracing::warn!(
                    "context length exceeded for session {}, forcing compaction",
                    ctx.session_id
                );
                if let Some(result) = {
                    let compactor = self.context_compactor(role).await;
                    compactor.compact(canonical, &self.router()).await
                } {
                    tracing::debug!(
                        "compacted {} -> {} tokens",
                        result.tokens_before,
                        result.tokens_after
                    );
                    *canonical = result.compacted;
                    // The retry must convert the *compacted* canonical
                    // (the old messages are stale), and the role must be
                    // re-resolved: summarizing away the last image-bearing
                    // turn changes the routing for the retry.
                    *llm_messages = canonical.clone();
                    let retry_role = if canonical_has_image(canonical) {
                        router.vision_role().await
                    } else {
                        EndpointRole::DefaultModel
                    };
                    EventDispatcher::emit_compaction_from(
                        &ctx.emitter,
                        &ctx.session_id,
                        &result.summary,
                        result.tokens_before,
                        result.tokens_after,
                    )
                    .await;
                    self.persist_compaction_summary(&ctx.session_id, &result.summary)
                        .await;
                    // Reset the accumulators: the first attempt's partial
                    // text was based on pre-compaction context and should
                    // not be mixed with the retry's output.
                    partial_thought.lock().unwrap().clear();
                    partial_reasoning.lock().unwrap().clear();
                    match self
                        .stream_llm_step(
                            ctx,
                            router.clone(),
                            retry_role,
                            llm_messages,
                            tools,
                            cancel,
                            partial_thought,
                            partial_reasoning,
                        )
                        .await
                    {
                        Ok((retry_resp, retry_duration_ms)) => {
                            self.record_usage_and_emit(
                                &ctx.session_id,
                                retry_role,
                                &retry_resp,
                                ctx.step_num as i32,
                                Some(retry_duration_ms),
                                &ctx.emitter,
                            )
                            .await;
                            StepCallOutcome::Response(retry_resp)
                        }
                        Err(haven_llm::LlmError::Cancelled) => StepCallOutcome::Cancelled,
                        Err(e2) => {
                            let err_msg = format!("Compaction retry also failed: {}", e2);
                            tracing::error!(
                                "ReAct step {} session {} fatal: {}",
                                ctx.step_num,
                                ctx.session_id,
                                err_msg
                            );
                            self.persist_partial_on_error(
                                ctx,
                                canonical,
                                history,
                                branch_points,
                                partial_thought,
                                partial_reasoning,
                            )
                            .await;
                            self.emit_error(&ctx.emitter, &ctx.session_id, &err_msg)
                                .await;
                            self.mark_session_error(&ctx.session_id).await;
                            StepCallOutcome::Fatal(err_msg)
                        }
                    }
                } else {
                    let err_msg = "context length exceeded but compaction failed".to_string();
                    tracing::error!(
                        "ReAct step {} session {} fatal: {}",
                        ctx.step_num,
                        ctx.session_id,
                        err_msg
                    );
                    self.persist_partial_on_error(
                        ctx,
                        canonical,
                        history,
                        branch_points,
                        partial_thought,
                        partial_reasoning,
                    )
                    .await;
                    EventDispatcher::emit_session_error_from(
                        &ctx.emitter,
                        &ctx.session_id,
                        &err_msg,
                    )
                    .await;
                    self.mark_session_error(&ctx.session_id).await;
                    StepCallOutcome::Fatal(err_msg)
                }
            }
            Err(haven_llm::LlmError::Cancelled) => StepCallOutcome::Cancelled,
            Err(e) => {
                let err_msg = format!("Both default model and balanced model failed: {}", e);
                tracing::error!(
                    "ReAct step {} session {} fatal: {}",
                    ctx.step_num,
                    ctx.session_id,
                    err_msg
                );
                self.persist_partial_on_error(
                    ctx,
                    canonical,
                    history,
                    branch_points,
                    partial_thought,
                    partial_reasoning,
                )
                .await;
                EventDispatcher::emit_session_error_from(&ctx.emitter, &ctx.session_id, &err_msg)
                    .await;
                self.mark_session_error(&ctx.session_id).await;
                StepCallOutcome::Fatal(err_msg)
            }
        }
    }

    /// Mark the session Error without propagating DB failures (the loop is
    /// already unwinding through the Fatal outcome).
    async fn mark_session_error(&self, session_id: &str) {
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

    /// Shared tail of the two "final answer" branches when a user message or
    /// background-action result arrived while the LLM was generating: persist
    /// the finished answer, insert it BEFORE the injected messages (so the
    /// re-run's LLM call sees the completed answer followed by the
    /// interjection, instead of answering blind and duplicating the bubble),
    /// and keep a rollback target for the interrupted final step.
    #[allow(clippy::too_many_arguments)] // consolidates two near-identical final branches
    async fn deliver_final_with_pending_context(
        &self,
        ctx: &StepCtx,
        final_text: &str,
        reasoning: Option<String>,
        canonical: &mut Vec<CanonicalMessage>,
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        before_inject_len: usize,
        already_pushed: bool,
    ) {
        let message_id = self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        self.persist_session_message(
            &ctx.session_id,
            "assistant",
            final_text,
            Some("text"),
            None,
            Some(&message_id),
        )
        .await;
        if !already_pushed {
            canonical.insert(
                before_inject_len,
                CanonicalMessage::assistant(
                    vec![ContentPart::text(final_text.to_string())],
                    None,
                    reasoning,
                    Vec::new(),
                    Vec::new(),
                ),
            );
        }
        self.save_branch_point(
            &ctx.session_id,
            canonical,
            history,
            ctx.step_num,
            branch_points,
            false,
        )
        .await;
    }

    /// Update the per-session cumulative token counters, persist one per-call
    /// usage-detail row, and emit an `AgentEvent::Usage` event so the UI can
    /// refresh its display.
    /// `role` is the endpoint that produced the response (used for cost
    /// lookup); `response` carries the token counts and model name;
    /// `step_number` is the ReAct step the call served and `duration_ms` its
    /// wall-clock duration, both recorded with the detail row.
    async fn record_usage_and_emit(
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
    fn context_window_for_role(
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
    async fn cached_context_window(&self, role: EndpointRole) -> u32 {
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
    async fn context_compactor(&self, role: EndpointRole) -> ContextCompactor {
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
    fn estimate_canonical_tokens(&self, session_id: &str, canonical: &[CanonicalMessage]) -> u32 {
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
    async fn emit_balanced_model(
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
    async fn emit_error(
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

/// One LLM call's live-chunk forwarding bundle: micro-batched
/// thought/reasoning channels (see `spawn_chunk_consumer_raw`), the
/// web-search event session, and a stall watchdog that emits `StreamStalled`
/// when the provider goes silent mid-call — the router only aborts at its
/// idle timeout, so without the watchdog the UI would sit frozen with
/// zero feedback during the whole stall window. `flush` drains the
/// batchers and stops the watchdog.
///
/// The `on_chunk` callback accumulates into the partial buffers
/// (checkpointed into `partial_messages` for crash recovery) and forwards
/// text chunks to the frontend. Reasoning chunks are always accumulated
/// but only forwarded when `forward_reasoning` is set: the UI reasoning
/// block may already hold the primary generation's reconciled text, and a
/// fresh reasoning pass from a retry would concatenate onto it.
struct StreamForwarder {
    chunk_tx: crate::event::ChunkSender,
    reasoning_tx: crate::event::ChunkSender,
    ws_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    consumer: crate::event::ConsumerHandle,
    ws_session: tokio::task::JoinHandle<()>,
    watchdog: tokio::task::JoinHandle<()>,
}

impl StreamForwarder {
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    fn new(
        ctx: &StepCtx,
        max_batch_bytes: usize,
        stall_warn_delay_ms: u64,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
        partial_store: Arc<crate::partial::PartialStore>,
        checkpoint_min_chars: usize,
        checkpoint_interval: std::time::Duration,
        cancel: tokio_util::sync::CancellationToken,
        forward_reasoning: bool,
        // Minted ids shared with the chunk events, the snap and the final
        // persistence, so the live bubble and the DB row match.
        thought_msg_id: String,
        reasoning_msg_id: String,
    ) -> (Self, impl FnMut(&haven_llm::StreamChunk) + Send + 'static) {
        let (chunk_tx, reasoning_tx, consumer_handle) =
            EventDispatcher::spawn_chunk_consumer_raw(&ctx.emitter, max_batch_bytes);
        let chunk_tx_c = chunk_tx.clone();
        let reasoning_tx_c = reasoning_tx.clone();
        let session_id_c = Arc::<str>::from(ctx.session_id.as_str());
        let pt = partial_thought.clone();
        let pr = partial_reasoning.clone();
        let checkpoint_session = ctx.session_id.clone();
        let checkpoint_inflight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Crash/stop recovery: the accumulated thought text is checkpointed
        // into the `partial_messages` scratch table while streaming so a
        // crash, user stop, or app exit does not lose the whole reply. The
        // first chunk checkpoints immediately; afterwards at most every
        // `checkpoint_interval` or every `checkpoint_min_chars` new chars,
        // and never while a write is in flight. All writes go through the
        // executor's `PartialStore`, which serializes them against
        // promote/discard and drops writes that land after the session was
        // ended/rolled back.
        let mut checkpoint_at = std::time::Instant::now() - checkpoint_interval;
        let mut checkpoint_len = 0usize;
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let ws_tx_c = ws_tx.clone();
        let em_ws = ctx.emitter.clone();
        let ws_session = tokio::spawn(async move {
            while let Some(event) = ws_rx.recv().await {
                em_ws.emit(event).await;
            }
        });
        let step_num = ctx.step_num;
        let run_id = ctx.run_id;
        let last_chunk_ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_chunk_c = last_chunk_ms.clone();
        let thought_mid = Arc::<str>::from(thought_msg_id.as_str());
        let reasoning_mid = Arc::<str>::from(reasoning_msg_id.as_str());
        let on_chunk = move |c: &haven_llm::StreamChunk| {
            if let Some(t) = c.text.as_deref() {
                // Single lock scope per chunk: push, read the new length
                // and clone the checkpoint snapshot (when due) under one
                // guard instead of locking up to three times per token.
                let checkpoint_snapshot = {
                    let mut guard = pt.lock().unwrap();
                    guard.push_str(t);
                    let len = guard.len();
                    let now = std::time::Instant::now();
                    if !checkpoint_inflight.load(std::sync::atomic::Ordering::Relaxed)
                        && (now.duration_since(checkpoint_at) >= checkpoint_interval
                            || len.saturating_sub(checkpoint_len) >= checkpoint_min_chars)
                    {
                        checkpoint_at = now;
                        checkpoint_len = len;
                        Some(guard.clone())
                    } else {
                        None
                    }
                };
                if let Err(e) = chunk_tx_c.try_send((
                    session_id_c.clone(),
                    thought_mid.clone(),
                    t.to_string(),
                    step_num,
                    run_id,
                )) {
                    tracing::warn!("thought chunk channel full, dropping: {}", e);
                }
                if let Some(snapshot) = checkpoint_snapshot {
                    // Generation captured BEFORE the write is spawned: if a
                    // promote/discard bumps it while the write is queued, the
                    // PartialStore drops the stale snapshot.
                    let gen_id = partial_store.generation(&checkpoint_session);
                    let store = partial_store.clone();
                    let tid = checkpoint_session.clone();
                    let flag = checkpoint_inflight.clone();
                    tokio::spawn(async move {
                        store.checkpoint(&tid, gen_id, &snapshot).await;
                        flag.store(false, std::sync::atomic::Ordering::Relaxed);
                    });
                }
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(r) = &c.reasoning {
                pr.lock().unwrap().push_str(r);
                if forward_reasoning
                    && let Err(e) = reasoning_tx_c.try_send((
                        session_id_c.clone(),
                        reasoning_mid.clone(),
                        r.clone(),
                        step_num,
                        run_id,
                    ))
                {
                    tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                }
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(phase) = c.web_search {
                let _ = ws_tx_c.send(AgentEvent::WebSearch {
                    session_id: session_id_c.to_string(),
                    phase: phase.as_str().to_string(),
                    step_number: step_num,
                    run_id,
                });
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
        };
        // Stall watchdog: announce `StreamStalled` once per silent episode
        // (a chunk anchor that produced no traffic for `stall_warn_delay_ms`).
        // The anchor starts at creation so a slow first chunk is covered
        // too; the emitted-anchor sentinel starts at MAX so the no-chunk
        // case (anchor 0) announces exactly once. Aborted by `flush` and
        // by session cancellation.
        let watchdog = {
            let em = ctx.emitter.clone();
            let tid = ctx.session_id.clone();
            let last = last_chunk_ms.clone();
            let created_ms = now_millis();
            let mut emitted_anchor: u64 = u64::MAX;
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(STALL_WATCHDOG_POLL) => {
                            let last_ms = last.load(std::sync::atomic::Ordering::Relaxed);
                            let base = if last_ms == 0 { created_ms } else { last_ms };
                            if now_millis().saturating_sub(base) >= stall_warn_delay_ms
                                && last_ms != emitted_anchor
                            {
                                emitted_anchor = last_ms;
                                em.emit(AgentEvent::StreamStalled {
                                    session_id: tid.clone(),
                                })
                                .await;
                            }
                        }
                    }
                }
            })
        };
        (
            Self {
                chunk_tx,
                reasoning_tx,
                ws_tx,
                consumer: consumer_handle,
                ws_session,
                watchdog,
            },
            on_chunk,
        )
    }

    /// Drain every buffered chunk to the frontend (batchers flush on
    /// channel close) and stop the watchdog. Must run once the router
    /// call has returned so no straggler events survive the step.
    async fn flush(self) {
        self.watchdog.abort();
        drop(self.chunk_tx);
        drop(self.reasoning_tx);
        drop(self.ws_tx);
        if let Some(handle) = self.consumer {
            let _ = handle.await;
        }
        let _ = self.ws_session.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use haven_common::types::{CanonicalRole, CanonicalToolCall};
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmResponse, StreamChunk};
    use std::pin::Pin;

    #[test]
    fn empty_inbox_output_detects_only_empty_polls() {
        assert!(empty_inbox_output(r#"{"count": 0, "messages": []}"#));
        assert!(empty_inbox_output(r#"{"count":0}"#));
        assert!(!empty_inbox_output(
            r#"{"count": 1, "messages": [{"id": "msg-x"}]}"#
        ));
        assert!(!empty_inbox_output("not json"));
        assert!(!empty_inbox_output(r#"{"count": null}"#));
    }

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
