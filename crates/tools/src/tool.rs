use haven_common::config::{StoredPermission, ToolConfig};
use haven_common::types::{
    ConfirmationMode, PermissionEffect, PermissionScope, RiskLevel, permission_key,
    permission_key_candidates,
};
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use haven_common::tools::ToolDef;

/// Representative operation/risk rows used by the local-tool security
/// regression matrix. Keep this list in the tools crate so the documented
/// matrix has an executable source of truth for every builtin tool family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalToolSecurityCase {
    pub tool_name: &'static str,
    pub operation: &'static str,
    pub risk_level: RiskLevel,
}

pub const LOCAL_TOOL_SECURITY_MATRIX: &[LocalToolSecurityCase] = &[
    LocalToolSecurityCase {
        tool_name: "audio",
        operation: "play",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "audio",
        operation: "record",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "ask",
        operation: "ask",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "files",
        operation: "read",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "files",
        operation: "search:content",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "files",
        operation: "write",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "files",
        operation: "delete",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "process",
        operation: "list",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "process",
        operation: "launch",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "process",
        operation: "kill",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "clipboard",
        operation: "read",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "clipboard",
        operation: "write",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "shell",
        operation: "execute",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "actions",
        operation: "list",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "input",
        operation: "move",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "input",
        operation: "click",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "scheduled_action",
        operation: "set",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "system",
        operation: "info",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "system",
        operation: "env:set",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "system",
        operation: "registry:set",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "system",
        operation: "power:lock",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "system",
        operation: "power:hibernate",
        risk_level: RiskLevel::Critical,
    },
    LocalToolSecurityCase {
        tool_name: "window",
        operation: "list",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "window",
        operation: "focus",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "window",
        operation: "close",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "window",
        operation: "ocr",
        risk_level: RiskLevel::High,
    },
    LocalToolSecurityCase {
        tool_name: "http",
        operation: "request",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "notify",
        operation: "notify",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "agent",
        operation: "list",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "agent",
        operation: "spawn",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "load_skill",
        operation: "load",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "load_mcp",
        operation: "load",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "memory",
        operation: "search",
        risk_level: RiskLevel::Safe,
    },
    LocalToolSecurityCase {
        tool_name: "memory",
        operation: "remember",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "memory",
        operation: "forget",
        risk_level: RiskLevel::Medium,
    },
    LocalToolSecurityCase {
        tool_name: "haven",
        operation: "status",
        risk_level: RiskLevel::Low,
    },
    LocalToolSecurityCase {
        tool_name: "haven",
        operation: "config_set",
        risk_level: RiskLevel::High,
    },
];

/// Check an absolute local path without applying a tool-specific allowlist.
/// This is for native app entry points such as “open skills directory” and
/// “open external path”; the normal tool path goes through `check`, which adds
/// configured `allowed_paths` on top of this reparse-point check.
pub fn is_safe_local_path(path: &Path) -> bool {
    if !path.is_absolute() || is_unc_or_device_path(path) {
        return false;
    }
    resolve_path_without_reparse(path).is_some()
}

/// The durable meaning of a tool invocation's terminal state.
///
/// `TimedOutUnknown` is deliberately distinct from a normal failure: the
/// caller must assume that an external side effect may still be in flight and
/// must not replay the operation automatically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionOutcome {
    Succeeded,
    #[default]
    Failed,
    Cancelled,
    TimedOutAndTerminated,
    TimedOutUnknown,
}

/// Whether replaying the same operation is safe after a transient failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationIdempotency {
    Idempotent,
    NonIdempotent,
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub truncated: bool,
    #[serde(default)]
    pub outcome: ToolExecutionOutcome,
    /// Number of attempts used by the manager. Direct tool calls use 1.
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    /// Side-channel signals the tool attaches to its own result (an `ask`
    /// question to pause for, a `notify` toast to surface). Populated by
    /// `ToolsManager::execute_tool` from the tool's `signals()` hook BEFORE
    /// any observation truncation, so the ReAct loop reads structured data
    /// instead of name-matching and re-parsing output JSON.
    #[serde(default)]
    pub signals: ToolSignals,
}

/// Side-channel signals a tool declares through its result. Declared by the
/// tool itself (via `Tool::signals`) so the ReAct loop does not need to know
/// which tool names carry which signals.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolSignals {
    /// The `ask` tool's question: when present, the loop pauses the session
    /// and waits for the user's reply.
    pub ask_question: Option<String>,
    /// Quick-reply options for the pending `ask`, surfaced as buttons.
    pub ask_options: Vec<String>,
    /// The `notify` tool's toast title (defaults to "Haven").
    pub notify_title: Option<String>,
    /// The `notify` tool's toast body.
    pub notify_body: Option<String>,
}

/// Scheduling contract for a tool call inside one assistant batch.
///
/// `ReadOnly` calls may overlap without a resource key. `SharedResource`
/// calls may overlap with other readers of the same key but serialize with a
/// writer for that key. `Resource` calls serialize with all calls using the
/// same key. `Exclusive` calls serialize with the whole batch. The
/// conservative default is exclusive unless a tool explicitly opts into a
/// less restrictive mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolConcurrency {
    ReadOnly,
    SharedResource(String),
    Resource(String),
    Exclusive,
}

/// Per-session side effects a tool declares through its result. The session
/// executor applies them (registering skill/MCP adapters, attaching
/// background actions) without hard-coding tool names, so a new tool that needs
/// a side effect declares it here instead of adding a name check in the
/// executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRegistration {
    /// Load a skill (raw name) for the current session.
    Skill(String),
    /// Load an MCP server (by name) for the current session.
    McpServer(String),
    /// Attach a background action (an action of kind `action`) to the current
    /// session (end/rollback cleanup).
    Action(String),
}

impl ToolResult {
    pub fn ok(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            truncated: false,
            outcome: ToolExecutionOutcome::Succeeded,
            attempts: 1,
            signals: ToolSignals::default(),
        }
    }

    pub fn truncated(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            truncated: true,
            outcome: ToolExecutionOutcome::Succeeded,
            attempts: 1,
            signals: ToolSignals::default(),
        }
    }

    pub fn failed(output: Value, error: impl Into<String>) -> Self {
        Self {
            success: false,
            output,
            error: Some(error.into()),
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
        }
    }

    pub fn cancelled(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: Value::Null,
            error: Some(error.into()),
            truncated: false,
            outcome: ToolExecutionOutcome::Cancelled,
            attempts: 1,
            signals: ToolSignals::default(),
        }
    }

    pub fn timed_out(outcome: ToolExecutionOutcome, error: impl Into<String>) -> Self {
        debug_assert!(matches!(
            outcome,
            ToolExecutionOutcome::TimedOutAndTerminated | ToolExecutionOutcome::TimedOutUnknown
        ));
        Self {
            success: false,
            output: Value::Null,
            error: Some(error.into()),
            truncated: false,
            outcome,
            attempts: 1,
            signals: ToolSignals::default(),
        }
    }

    /// Plain-text summary of the result: the serialized output on success,
    /// the error message on failure. Plain-string outputs are returned as-is
    /// (unquoted) so a tool returning "text" reads like text, not JSON; only
    /// structured outputs are serialized. On failure with an empty/absent
    /// error (e.g. a shell command that exited non-zero without stderr), fall
    /// back to the serialized output so the result is never an empty string —
    /// an empty observation looks to the model and the chat like the tool
    /// never returned anything. Callers that need truncation apply it on top.
    pub fn summary_text(&self) -> String {
        if self.success {
            match &self.output {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string(&self.output).unwrap_or_else(|_| "success".into()),
            }
        } else {
            match self.error.as_deref() {
                Some(e) if !e.trim().is_empty() => e.to_string(),
                _ => {
                    let out = serde_json::to_string(&self.output).unwrap_or_default();
                    if out.is_empty() || out == "null" {
                        "unknown failure".into()
                    } else {
                        out
                    }
                }
            }
        }
    }

    /// Build the bounded observation shared by canonical, history and step
    /// projections. The signal fields are intentionally not derived from this
    /// string; callers must read `signals` before applying the cap.
    pub fn observation_text(&self, max_chars: usize) -> String {
        let text = self.summary_text();
        let char_count = text.chars().count();
        if char_count <= max_chars {
            return text;
        }
        let cutoff = text
            .char_indices()
            .nth(max_chars)
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        format!(
            "{}[... truncated {} chars omitted]",
            &text[..cutoff],
            char_count - text[..cutoff].chars().count()
        )
    }
}

fn default_attempts() -> u32 {
    1
}

/// Extract the `ask` signal from a tool result's structured output: the
/// question text and optional suggested answers. `(None, vec![])` when the
/// output does not carry a question. The signal must be read BEFORE any
/// truncation: parsing truncated text would yield invalid JSON when the
/// output exceeds the observation budget, silently dropping the question
/// and never pausing the session.
pub fn extract_ask_signal(output: &Value) -> (Option<String>, Vec<String>) {
    let question = output
        .get("question")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let options = output
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    (question, options)
}

/// Extract the `notify` signal from a tool result's structured output: the
/// notification title (default "Haven") and body. `(None, None)` when the
/// output does not request a notification.
pub fn extract_notify_signal(output: &Value) -> (Option<String>, Option<String>) {
    if output.get("notify").and_then(|v| v.as_bool()) != Some(true) {
        return (None, None);
    }
    let title = output
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Haven")
        .to_string();
    let body = output
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    (Some(title), Some(body))
}

/// Whether an action should be hidden from the chat UI. `ask` must never be
/// silent: hiding the question while the session pauses for an answer would
/// leave the user waiting on a question they can't see.
pub fn is_silent_action(tool_name: &str, input: &Value) -> bool {
    tool_name != "ask"
        && input
            .get("silent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn risk_level(&self, input: &Value) -> RiskLevel;
    /// Retry policy is an operation property, not a safety-risk property.
    /// The default is conservative because an unknown operation may have
    /// performed an external side effect before returning an error.
    fn idempotency(&self, _input: &Value) -> OperationIdempotency {
        OperationIdempotency::Unknown
    }

    /// Whether an outer timeout can establish that this invocation stopped.
    /// Tools backed by child processes or remote servers should return
    /// `TimedOutUnknown` unless they can prove termination.
    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        ToolExecutionOutcome::TimedOutUnknown
    }

    /// Intrinsic retry budget used when no per-tool configuration exists.
    fn default_max_retries(&self) -> u32 {
        0
    }

    fn default_retry_backoff_secs(&self) -> u64 {
        0
    }
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult>;
    fn input_schema(&self) -> Value;

    /// Declare how calls from one assistant batch may overlap. Tools are
    /// exclusive by default; read-only/resource contracts must be explicit so
    /// a non-idempotent implementation cannot accidentally run in parallel.
    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        let _ = input;
        ToolConcurrency::Exclusive
    }

    /// Canonical structured definition of this tool (name / description /
    /// schema / default risk). `ToolDef` is the unified abstraction the
    /// registry and manager surface; per-call risk is still refined via
    /// `risk_level(input)`. Tools may override to e.g. memoize the schema.
    fn tool_def(&self) -> ToolDef {
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.risk_level(&Value::Object(Default::default())),
        )
    }

    fn default_timeout_secs(&self) -> u64 {
        30
    }

    /// Per-call timeout. Defaults to [`Self::default_timeout_secs`]; tools with
    /// op-dependent waits (e.g. `agent` request) override this.
    fn timeout_secs_for(&self, _input: &Value) -> u64 {
        self.default_timeout_secs()
    }

    /// Whether this tool needs the private `_session_id` input field injected
    /// before execution (e.g. `schedule`/`actions` scope to the current session).
    /// The id is injected after the LLM-facing input was captured, so it
    /// never reaches the tool schema, the step history, or the LLM.
    fn requires_session_id(&self) -> bool {
        false
    }

    /// Whether this tool streams live output to the chat card while running
    /// (`agent:tool_output`). When true, `_session_id` and `_step_id` are
    /// injected privately so the tool can key preview events to the card.
    fn supports_live_output(&self) -> bool {
        false
    }

    /// Side-channel signals carried by this tool's result (`ask` question /
    /// `notify` toast). Parsed from the structured output by the tool itself,
    /// BEFORE the loop truncates the observation text.
    fn signals(&self, output: &Value) -> ToolSignals {
        let _ = output;
        ToolSignals::default()
    }

    /// Per-session side effects to apply after a successful execution
    /// (skill/MCP adapters, background-action attachment).
    fn registrations(&self, output: &Value) -> Vec<ToolRegistration> {
        let _ = output;
        Vec::new()
    }

    fn validate_input(&self, input: &Value) -> anyhow::Result<()> {
        let schema = self.input_schema();
        if schema.is_null() || schema == serde_json::Value::Null {
            return Ok(());
        }
        let validator = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("invalid tool schema for '{}': {}", self.name(), e))?;
        let errors: Vec<_> = validator.iter_errors(input).collect();
        if !errors.is_empty() {
            // Make the most common mistakes loud: missing required fields are
            // listed up front (with allowed enum values when the schema
            // declares them) instead of being buried in generic messages like
            // "required property 'operation' was not present".
            let mut missing: Vec<String> = Vec::new();
            let mut rest: Vec<String> = Vec::new();
            for e in errors {
                if let jsonschema::error::ValidationErrorKind::Required { property } = e.kind()
                    && let Some(s) = property.as_str()
                {
                    missing.push(s.to_string());
                    continue;
                }
                rest.push(e.to_string());
            }
            let mut msg = format!("input validation failed for '{}'", self.name());
            if !missing.is_empty() {
                msg.push_str(&format!(
                    ": MISSING REQUIRED FIELD(S): {}",
                    missing.join(", ")
                ));
                if let Some(props) = schema.get("properties") {
                    let hints: Vec<String> = missing
                        .iter()
                        .filter_map(|m| {
                            let prop = props.get(m)?;
                            let vals = prop.get("enum")?.as_array()?;
                            let vals: Vec<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
                            if vals.is_empty() {
                                None
                            } else {
                                Some(format!("{m} must be one of: {}", vals.join(", ")))
                            }
                        })
                        .collect();
                    if !hints.is_empty() {
                        msg.push_str(&format!("\nAllowed values: {}", hints.join("; ")));
                    }
                }
            }
            if !rest.is_empty() {
                msg.push_str(&format!("\nOther: {}", rest.join("; ")));
            }
            anyhow::bail!(msg);
        }
        Ok(())
    }

    async fn execute_with_timeout(
        &self,
        input: Value,
        cancel: CancellationToken,
        timeout_secs: u64,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(format!(
                "tool '{}' cancelled before execution",
                self.name()
            )));
        }
        let execution_cancel = cancel.child_token();
        tokio::select! {
            result = self.execute(input, execution_cancel.clone()) => result,
            _ = cancel.cancelled() => {
                execution_cancel.cancel();
                Ok(ToolResult::cancelled(format!("tool '{}' cancelled", self.name())))
            }
            _ = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                execution_cancel.cancel();
                Ok(ToolResult::timed_out(
                    self.timeout_outcome(),
                    format!("tool '{}' timed out after {}s", self.name(), timeout_secs),
                ))
            }
        }
    }
}

/// Convert LLM JSON input into a builtin tool's typed params (entry ② of the
/// builtin two-entry contract: ① `XxxTool::run(&XxxParams)` native call with
/// zero serialization, ② `Tool::execute(Value)` JSON entry that converts and
/// validates here, then lands in the same `run`). Serde reports missing
/// fields, wrong types, and unknown enum variants with their allowed values.
pub fn parse_tool_input<T: serde::de::DeserializeOwned>(
    tool_name: &str,
    input: Value,
) -> anyhow::Result<T> {
    serde_json::from_value(input)
        .map_err(|e| anyhow::anyhow!("invalid '{}' input: {}", tool_name, e))
}

pub type ToolBox = Arc<dyn Tool>;

/// Combined tools + name index under a single RwLock so rebuilds update
/// both atomically — readers never see new `tools` with stale `name_index`.
#[derive(Default, Clone)]
struct RegistrySnapshot {
    tools: Vec<ToolBox>,
    name_index: HashMap<String, ToolBox>,
}

#[derive(Default)]
pub struct ToolRegistry {
    snapshot: Arc<RwLock<RegistrySnapshot>>,
    /// Monotonically incremented on every mutation (register/rebuild).
    /// Consumers (e.g. SystemPromptBuilder) compare this against a cached
    /// value to decide whether the schema snapshot is stale, which is more
    /// robust than comparing tool counts: a rebuild that swaps tools while
    /// keeping the same count still bumps the version.
    version: Arc<AtomicU64>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            snapshot: self.snapshot.clone(),
            version: self.version.clone(),
        }
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(RegistrySnapshot::default())),
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Current registry version. Bumps on every `register`/`rebuild`.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    pub async fn register(&self, tool: ToolBox) {
        let name = tool.name();
        let mut snap = self.snapshot.write().await;
        snap.tools.push(tool.clone());
        snap.name_index.insert(name, tool);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub async fn get(&self, name: &str) -> Option<ToolBox> {
        self.snapshot.read().await.name_index.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<ToolBox> {
        self.snapshot.read().await.tools.clone()
    }

    /// Structured tool definitions of every registered tool. The canonical
    /// surface for consumers (agent schema builder, prompt builder, UI).
    pub async fn list_defs(&self) -> Vec<ToolDef> {
        let tools = self.snapshot.read().await.tools.clone();
        tools.iter().map(|t| t.tool_def()).collect()
    }

    pub async fn list_schemas(&self) -> Vec<Value> {
        self.list_defs()
            .await
            .into_iter()
            .map(|d| d.json())
            .collect()
    }

    /// Atomically rebuild the entire registry from a list of tools.
    /// Uses a single write lock so readers see a consistent snapshot.
    pub async fn rebuild(&self, new_tools: Vec<ToolBox>) {
        let mut index = HashMap::new();
        for t in &new_tools {
            index.insert(t.name(), t.clone());
        }
        let mut snap = self.snapshot.write().await;
        snap.tools = new_tools;
        snap.name_index = index;
        drop(snap);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// Non-owning probe into the current snapshot (see [`RegistryProbe`]).
    /// The probe holds a weak handle, so it never keeps the snapshot alive
    /// and cannot create a reference cycle (the snapshot owns the tools, and
    /// a tool holding a strong registry reference would loop back to itself).
    pub fn probe(&self) -> RegistryProbe {
        RegistryProbe {
            snapshot: Arc::downgrade(&self.snapshot),
        }
    }
}

/// Weak lookup handle into a [`ToolRegistry`] snapshot. Lets a tool (e.g.
/// `schedule`) validate tool names / risk levels at call time without owning
/// the registry — the snapshot is mutated in place by `rebuild`, so the weak
/// handle always observes the current state. `find` returns `None` for
/// unknown names or when the registry was dropped.
pub struct RegistryProbe {
    snapshot: std::sync::Weak<RwLock<RegistrySnapshot>>,
}

impl RegistryProbe {
    /// Look up a tool by name; `None` when unknown or the registry is gone.
    pub async fn find(&self, name: &str) -> Option<ToolBox> {
        self.snapshot
            .upgrade()?
            .read()
            .await
            .name_index
            .get(name)
            .cloned()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ConfirmationResult {
    AutoApproved,
    RequiresConfirmation {
        tool_name: String,
        params: Value,
        risk_level: RiskLevel,
        /// Stable key used for grant matching (`tool` / `tool:op`).
        permission_key: String,
    },
    /// Hard deny — permanent/session denylist, disabled operation, or path sandbox.
    Blocked {
        reason: String,
    },
}

/// Per-session allow/deny sets keyed by permission key.
#[derive(Clone, Default)]
struct SessionGrants {
    allow: HashSet<String>,
    deny: HashSet<String>,
}

/// Combined safety config under a single RwLock so `check` reads atomically.
#[derive(Clone)]
struct SafetyConfig {
    confirmation_mode: ConfirmationMode,
    min_risk_level: RiskLevel,
    /// Permanent (Always) grants from `SecurityConfig.permissions`.
    permanent: HashMap<String, PermissionEffect>,
    /// Per-conversation grants keyed by session id.
    session_grants: HashMap<String, SessionGrants>,
    /// Live copy of `tool_settings` for disabled_operations / risk_override /
    /// allowed_paths enforcement.
    tool_settings: HashMap<String, ToolConfig>,
}

pub struct SafetyGateway {
    config: RwLock<SafetyConfig>,
}

impl SafetyGateway {
    pub fn new(min_risk_level: RiskLevel) -> Self {
        Self {
            config: RwLock::new(SafetyConfig {
                confirmation_mode: ConfirmationMode::Ask,
                min_risk_level,
                permanent: HashMap::new(),
                session_grants: HashMap::new(),
                tool_settings: HashMap::new(),
            }),
        }
    }

    /// Replace threshold + mode + permanent grants from settings. Clears
    /// session grants so a policy change cannot leave stale trusts.
    pub async fn apply_security(
        &self,
        mode: ConfirmationMode,
        min_risk_level: RiskLevel,
        permissions: &[StoredPermission],
    ) {
        let mut cfg = self.config.write().await;
        cfg.confirmation_mode = mode;
        cfg.min_risk_level = min_risk_level;
        cfg.permanent.clear();
        for p in permissions {
            cfg.permanent.insert(p.key.clone(), p.effect);
        }
        cfg.session_grants.clear();
    }

    /// Update the minimum risk level threshold. Clears session grants.
    pub async fn set_min_risk_level(&self, level: RiskLevel) {
        let mut cfg = self.config.write().await;
        cfg.min_risk_level = level;
        cfg.session_grants.clear();
    }

    /// Refresh the live tool_settings mirror used by path/op/risk overrides.
    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        self.config.write().await.tool_settings = settings;
    }

    /// Effective risk after optional `tool_settings.risk_override`.
    pub async fn effective_risk(&self, tool_name: &str, reported: RiskLevel) -> RiskLevel {
        let cfg = self.config.read().await;
        effective_risk_from(&cfg, tool_name, reported)
    }

    pub async fn check(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        params: &Value,
        risk_level: RiskLevel,
    ) -> ConfirmationResult {
        let key = permission_key(tool_name, params);
        let cfg = self.config.read().await;
        let risk = effective_risk_from(&cfg, tool_name, risk_level);

        if let Some(reason) = disabled_operation_block(&cfg.tool_settings, tool_name, params) {
            return ConfirmationResult::Blocked { reason };
        }
        if let Some(reason) = path_sandbox_block(&cfg.tool_settings, tool_name, params) {
            return ConfirmationResult::Blocked { reason };
        }

        // Deny always wins over Allow (permanent deny → session deny →
        // permanent allow → session allow). Session deny can override a
        // permanent allow for the rest of that conversation.
        if match_grant(&cfg.permanent, &key) == Some(PermissionEffect::Deny) {
            return ConfirmationResult::Blocked {
                reason: format!("permanently denied: {key}"),
            };
        }
        if let Some(sid) = session_id
            && let Some(grants) = cfg.session_grants.get(sid)
            && match_key_set(&grants.deny, &key)
        {
            return ConfirmationResult::Blocked {
                reason: format!("denied for this session: {key}"),
            };
        }
        if match_grant(&cfg.permanent, &key) == Some(PermissionEffect::Allow) {
            return ConfirmationResult::AutoApproved;
        }
        if let Some(sid) = session_id
            && let Some(grants) = cfg.session_grants.get(sid)
            && match_key_set(&grants.allow, &key)
        {
            return ConfirmationResult::AutoApproved;
        }

        // Autopilot skips prompts except Critical — keep a hard floor for
        // irreversible ops (e.g. power hibernate).
        let needs_prompt = match cfg.confirmation_mode {
            ConfirmationMode::Autopilot => risk >= RiskLevel::Critical,
            ConfirmationMode::Paranoid => risk > RiskLevel::Safe,
            ConfirmationMode::Ask => risk >= cfg.min_risk_level,
        };

        if !needs_prompt {
            return ConfirmationResult::AutoApproved;
        }

        ConfirmationResult::RequiresConfirmation {
            tool_name: tool_name.into(),
            params: params.clone(),
            risk_level: risk,
            permission_key: key,
        }
    }

    /// Record a grant. `Once` is a no-op (caller already approved this call).
    /// `Always` updates the in-memory permanent map; the app layer must also
    /// persist to `SecurityConfig.permissions`.
    pub async fn grant(
        &self,
        session_id: Option<&str>,
        key: &str,
        effect: PermissionEffect,
        scope: PermissionScope,
    ) {
        if matches!(scope, PermissionScope::Once) || key.is_empty() {
            return;
        }
        let mut cfg = self.config.write().await;
        match scope {
            PermissionScope::Always => {
                cfg.permanent.insert(key.to_string(), effect);
            }
            PermissionScope::Session => {
                let Some(sid) = session_id else {
                    return;
                };
                let entry = cfg.session_grants.entry(sid.to_string()).or_default();
                match effect {
                    PermissionEffect::Allow => {
                        entry.deny.remove(key);
                        entry.allow.insert(key.to_string());
                    }
                    PermissionEffect::Deny => {
                        entry.allow.remove(key);
                        entry.deny.insert(key.to_string());
                    }
                }
            }
            PermissionScope::Once => {}
        }
    }

    /// Snapshot of permanent grants for the settings UI.
    pub async fn list_permanent(&self) -> Vec<StoredPermission> {
        let cfg = self.config.read().await;
        let mut out: Vec<_> = cfg
            .permanent
            .iter()
            .map(|(key, effect)| StoredPermission {
                key: key.clone(),
                effect: *effect,
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Remove one permanent grant from memory. App layer persists the change.
    pub async fn revoke_permanent(&self, key: &str) -> bool {
        self.config.write().await.permanent.remove(key).is_some()
    }

    /// Drop one session's grants (conversation ended / deleted).
    pub async fn clear_session_trust(&self, session_id: &str) {
        self.config.write().await.session_grants.remove(session_id);
    }

    /// Drop every session grant (history cleared / app reset).
    pub async fn clear_all_trust(&self) {
        self.config.write().await.session_grants.clear();
    }
}

fn effective_risk_from(cfg: &SafetyConfig, tool_name: &str, reported: RiskLevel) -> RiskLevel {
    cfg.tool_settings
        .get(tool_name)
        .and_then(|t| t.risk_override.as_deref())
        .and_then(parse_risk_override)
        .unwrap_or(reported)
}

fn parse_risk_override(raw: &str) -> Option<RiskLevel> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "safe" => Some(RiskLevel::Safe),
        "low" => Some(RiskLevel::Low),
        "medium" => Some(RiskLevel::Medium),
        "high" => Some(RiskLevel::High),
        "critical" => Some(RiskLevel::Critical),
        _ => None,
    }
}

fn match_grant(map: &HashMap<String, PermissionEffect>, key: &str) -> Option<PermissionEffect> {
    let candidates = permission_key_candidates(key);
    // A child Allow must never outrank a parent Deny. Check the entire
    // inheritance chain for denies before considering any allow, otherwise a
    // broad deny such as `files` could be bypassed by `files:read`.
    if candidates
        .iter()
        .any(|candidate| map.get(*candidate) == Some(&PermissionEffect::Deny))
    {
        return Some(PermissionEffect::Deny);
    }
    candidates
        .iter()
        .any(|candidate| map.get(*candidate) == Some(&PermissionEffect::Allow))
        .then_some(PermissionEffect::Allow)
}

fn match_key_set(set: &HashSet<String>, key: &str) -> bool {
    permission_key_candidates(key)
        .into_iter()
        .any(|c| set.contains(c))
}

fn disabled_operation_block(
    settings: &HashMap<String, ToolConfig>,
    tool_name: &str,
    params: &Value,
) -> Option<String> {
    let cfg = settings.get(tool_name)?;
    if cfg.disabled_operations.is_empty() {
        return None;
    }
    let op = params
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let scope = params.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    for disabled in &cfg.disabled_operations {
        let d = disabled.trim();
        if d.is_empty() {
            continue;
        }
        if d == op || d == scope || (!scope.is_empty() && d == format!("{scope}:{op}")) {
            return Some(format!(
                "operation '{disabled}' is disabled for tool '{tool_name}'"
            ));
        }
    }
    None
}

fn path_sandbox_block(
    settings: &HashMap<String, ToolConfig>,
    tool_name: &str,
    params: &Value,
) -> Option<String> {
    let cfg = settings.get(tool_name)?;
    if cfg.allowed_paths.is_empty() {
        return None;
    }
    let allowed: Vec<PathBuf> = cfg.allowed_paths.iter().map(PathBuf::from).collect();
    let paths = collect_path_params(params);
    if paths.is_empty() {
        return None;
    }
    for path in paths {
        if !path_is_allowed(&path, &allowed) {
            return Some(format!(
                "path '{}' is outside allowed_paths for tool '{tool_name}'",
                path.display()
            ));
        }
    }
    None
}

fn collect_path_params(params: &Value) -> Vec<PathBuf> {
    const KEYS: &[&str] = &[
        "path",
        "paths",
        "source",
        "destination",
        "target",
        "cwd",
        "file",
        "dir",
        "directory",
    ];
    let mut out = Vec::new();
    let Some(obj) = params.as_object() else {
        return out;
    };
    for key in KEYS {
        match obj.get(*key) {
            Some(Value::String(s)) if !s.is_empty() => out.push(PathBuf::from(s)),
            Some(Value::Array(arr)) => {
                for v in arr {
                    if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                        out.push(PathBuf::from(s));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn path_is_allowed(path: &Path, allowed: &[PathBuf]) -> bool {
    // Relative paths and UNC/device paths are rejected even when the current
    // working directory happens to be inside an allowed root. Otherwise the
    // meaning of the same tool input changes with process launch context.
    if !path.is_absolute() || is_unc_or_device_path(path) {
        return false;
    }
    let Some(canon) = resolve_path_without_reparse(path) else {
        return false;
    };
    for base in allowed {
        if !base.is_absolute() || is_unc_or_device_path(base) {
            continue;
        }
        let Some(base_abs) = resolve_path_without_reparse(base) else {
            continue;
        };
        if path_is_within(&canon, &base_abs) {
            return true;
        }
    }
    false
}

/// Resolve the existing prefix of a path while rejecting every symlink or
/// Windows reparse point encountered on that prefix. The non-existing suffix
/// is appended only after the trusted prefix has been canonicalized. This is
/// intentionally fail-closed: a metadata/canonicalization error denies the
/// operation instead of falling back to lexical prefix matching.
fn resolve_path_without_reparse(path: &Path) -> Option<PathBuf> {
    let abs = normalize_path(path)?;
    let components: Vec<_> = abs.components().collect();
    let mut existing = PathBuf::new();
    let mut suffix: Vec<OsString> = Vec::new();
    let mut missing_started = false;

    for (index, component) in components.iter().enumerate() {
        if missing_started {
            suffix.push(component.as_os_str().to_owned());
            continue;
        }

        existing.push(component.as_os_str());
        match std::fs::symlink_metadata(&existing) {
            Ok(metadata) => {
                if is_reparse_point(&metadata) {
                    return None;
                }
                if index + 1 < components.len() && !metadata.file_type().is_dir() {
                    return None;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing.pop();
                missing_started = true;
                suffix.push(component.as_os_str().to_owned());
            }
            Err(_) => return None,
        }
    }

    let mut resolved = std::fs::canonicalize(&existing).ok()?;
    for component in suffix {
        resolved.push(component);
    }
    Some(resolved)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn is_unc_or_device_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("\\\\") || value.starts_with("//")
}

fn path_is_within(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().to_ascii_lowercase();
        let base = base.to_string_lossy().to_ascii_lowercase();
        Path::new(&path).starts_with(Path::new(&base))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(base)
    }
}

/// Lexically normalize `.` / `..` after making the path absolute so
/// `allowed\..\Windows` cannot prefix-match `allowed`.
fn normalize_path(path: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(path).ok()?;
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn test_tool_result_ok() {
        let result = ToolResult::ok(json!({"status": "done"}));
        assert!(result.success);
        assert!(!result.truncated);
        assert_eq!(result.output, json!({"status": "done"}));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_tool_result_truncated() {
        let result = ToolResult::truncated(json!({"content": "partial"}));
        assert!(result.success);
        assert!(result.truncated);
        assert_eq!(result.output, json!({"content": "partial"}));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_tool_result_summary_text_success() {
        let result = ToolResult::ok(json!({"status": "done"}));
        assert_eq!(result.summary_text(), r#"{"status":"done"}"#);
    }

    #[test]
    fn test_tool_result_summary_text_plain_string_unquoted() {
        // A tool returning a plain string must read as text, not JSON-quoted.
        let result = ToolResult::ok(json!("some plain text"));
        assert_eq!(result.summary_text(), "some plain text");
    }

    #[test]
    fn test_tool_result_summary_text_error() {
        let result = ToolResult {
            success: false,
            output: json!(null),
            error: Some("boom".into()),
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
        };
        assert_eq!(result.summary_text(), "boom");
    }

    #[test]
    fn test_tool_result_summary_text_error_fallback() {
        let result = ToolResult {
            success: false,
            output: json!(null),
            error: None,
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
        };
        assert_eq!(result.summary_text(), "unknown failure");
    }

    #[test]
    fn test_tool_result_summary_text_empty_error_falls_back_to_output() {
        // A failure with an empty error string (e.g. a shell command that
        // exited non-zero without stderr) must still yield a non-empty
        // summary — otherwise the tool appears to return no result at all.
        let result = ToolResult {
            success: false,
            output: json!({"output": "some stdout"}),
            error: Some(String::new()),
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
        };
        assert_eq!(result.summary_text(), r#"{"output":"some stdout"}"#);
    }

    #[test]
    fn test_tool_result_summary_text_whitespace_error_falls_back() {
        let result = ToolResult {
            success: false,
            output: json!(null),
            error: Some("   ".into()),
            truncated: false,
            outcome: ToolExecutionOutcome::Failed,
            attempts: 1,
            signals: ToolSignals::default(),
        };
        assert_eq!(result.summary_text(), "unknown failure");
    }

    #[test]
    fn observation_text_has_one_unicode_safe_budgeted_shape() {
        let result = ToolResult::ok(json!("你好世界"));
        let observation = result.observation_text(3);
        assert_eq!(observation, "你好世[... truncated 1 chars omitted]");
    }

    #[test]
    fn test_extract_ask_signal() {
        let (q, opts) = extract_ask_signal(&json!({
            "ask": true,
            "question": "which?",
            "options": ["A", "B"],
        }));
        assert_eq!(q.as_deref(), Some("which?"));
        assert_eq!(opts, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn test_extract_ask_signal_missing() {
        let (q, opts) = extract_ask_signal(&json!({"result": 42}));
        assert!(q.is_none());
        assert!(opts.is_empty());
    }

    #[test]
    fn test_extract_notify_signal() {
        let (title, body) = extract_notify_signal(&json!({
            "notify": true,
            "title": "ScheduledAction",
            "body": "Take a break",
        }));
        assert_eq!(title.as_deref(), Some("ScheduledAction"));
        assert_eq!(body.as_deref(), Some("Take a break"));
    }

    #[test]
    fn test_extract_notify_signal_defaults() {
        let (title, body) = extract_notify_signal(&json!({"notify": true}));
        assert_eq!(title.as_deref(), Some("Haven"));
        assert_eq!(body.as_deref(), Some(""));
    }

    #[test]
    fn test_extract_notify_signal_not_requested() {
        let (title, body) = extract_notify_signal(&json!({"notify": false}));
        assert!(title.is_none());
        assert!(body.is_none());
    }

    #[test]
    fn test_is_silent_action() {
        assert!(is_silent_action("shell", &json!({"silent": true})));
        assert!(!is_silent_action("shell", &json!({"silent": false})));
        assert!(!is_silent_action("shell", &json!({})));
        // `ask` must never be silent, even when the input asks for it.
        assert!(!is_silent_action("ask", &json!({"silent": true})));
    }

    struct MockTool {
        name: String,
        schema: Value,
        execute_delay: Option<Duration>,
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                schema: json!({"type": "object"}),
                execute_delay: None,
            }
        }

        fn with_schema(name: &str, schema: Value) -> Self {
            Self {
                name: name.into(),
                schema,
                execute_delay: None,
            }
        }

        fn with_delay(name: &str, delay: Duration) -> Self {
            Self {
                name: name.into(),
                schema: json!({"type": "object"}),
                execute_delay: Some(delay),
            }
        }
    }

    /// The schema-validation mock used by the `validate_input` tests.
    fn schema_mock() -> MockTool {
        MockTool::with_schema(
            "schema_mock",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "count": { "type": "integer" }
                },
                "required": ["name"]
            }),
        )
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> String {
            self.name.clone()
        }
        fn description(&self) -> String {
            "mock".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            if let Some(delay) = self.execute_delay {
                tokio::time::sleep(delay).await;
            }
            Ok(ToolResult::ok(json!({"ok": true})))
        }
        fn input_schema(&self) -> Value {
            self.schema.clone()
        }
    }

    #[tokio::test]
    async fn test_registry_new() {
        let registry = ToolRegistry::new();
        let tools = registry.list().await;
        assert!(tools.is_empty());
    }

    #[test]
    fn undeclared_tools_are_serialized_by_default() {
        assert_eq!(
            MockTool::new("undeclared").concurrency(&json!({})),
            ToolConcurrency::Exclusive
        );
    }

    #[tokio::test]
    async fn test_registry_register_and_get() {
        let registry = ToolRegistry::new();
        let tool = Arc::new(MockTool::new("mock1"));
        registry.register(tool).await;

        let fetched = registry.get("mock1").await;
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name(), "mock1");
    }

    #[tokio::test]
    async fn test_registry_get_not_found() {
        let registry = ToolRegistry::new();
        let fetched = registry.get("nonexistent").await;
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn test_registry_list_multiple() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockTool::new("a"))).await;
        registry.register(Arc::new(MockTool::new("b"))).await;

        let tools = registry.list().await;
        assert_eq!(tools.len(), 2);
    }

    #[tokio::test]
    async fn test_registry_list_empty() {
        let registry = ToolRegistry::new();
        let tools = registry.list().await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_registry_list_schemas() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockTool::new("mock"))).await;

        let schemas = registry.list_schemas().await;
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"].as_str().unwrap(), "mock");
        assert_eq!(schemas[0]["description"].as_str().unwrap(), "mock");
        assert!(schemas[0]["input_schema"].is_object());
    }

    #[tokio::test]
    async fn test_registry_list_defs_structured() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(MockTool::new("mock"))).await;

        let defs = registry.list_defs().await;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "mock");
        assert_eq!(defs[0].description, "mock");
        assert_eq!(defs[0].risk_level, RiskLevel::Safe);
        assert!(defs[0].input_schema.is_object());
    }

    #[test]
    fn test_tool_def_default_matches_tool_fields() {
        let tool = MockTool::new("mock");
        let def = tool.tool_def();
        assert_eq!(def.name, tool.name());
        assert_eq!(def.description, tool.description());
        assert_eq!(def.input_schema, tool.input_schema());
        assert_eq!(def.risk_level, tool.risk_level(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_registry_rebuild() {
        let registry = ToolRegistry::new();
        let old_tool = Arc::new(MockTool::new("old"));
        registry.register(old_tool).await;

        let new_tool = Arc::new(MockTool::new("new"));
        registry.rebuild(vec![new_tool.clone()]).await;

        assert!(registry.get("old").await.is_none());
        assert!(registry.get("new").await.is_some());
    }

    #[tokio::test]
    async fn test_safety_gateway_new_default_threshold() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        // Safe is below Low → auto approved
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_below_threshold_auto_approved() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        // Low is below Medium → auto approved
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Low).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_at_threshold_requires_confirmation() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        // Medium is at the threshold → requires confirmation
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_above_threshold_requires_confirmation() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        // High is above Low → requires confirmation
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::High).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_allow_tool_key() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        let result = gw
            .check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_allow_is_per_session_and_per_tool() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(Some("ses-b"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(Some("ses-a"), "tool2", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(None, "tool1", &json!({}), RiskLevel::Medium).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_permanent_deny_blocks() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "shell",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;
        let result = gw.check(None, "shell", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_safety_gateway_parent_key_matches_operation() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "files",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        let result = gw
            .check(
                Some("ses-a"),
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_clear_session_trust() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));

        gw.clear_session_trust("ses-a").await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_set_threshold_clears_session_grants() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));

        gw.set_min_risk_level(RiskLevel::High).await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_disabled_operation_blocks() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                disabled_operations: vec!["delete".into()],
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_safety_gateway_autopilot_skips_prompt_except_critical() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.apply_security(ConfirmationMode::Autopilot, RiskLevel::Medium, &[])
            .await;
        assert!(matches!(
            gw.check(None, "shell", &json!({}), RiskLevel::High).await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(
                None,
                "system",
                &json!({"scope":"power","operation":"hibernate"}),
                RiskLevel::Critical
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_deny_overrides_permanent_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "files",
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            "files",
            PermissionEffect::Deny,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-a"),
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_child_allow_cannot_bypass_parent_deny() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "files",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            None,
            "files:read",
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;

        assert!(matches!(
            gw.check(None, "files", &json!({"operation": "read"}), RiskLevel::Low)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_child_deny_cannot_be_bypassed_by_parent_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "files",
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            None,
            "files:delete",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;

        assert!(matches!(
            gw.check(
                None,
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_parent_deny_beats_session_child_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "system:power",
            PermissionEffect::Deny,
            PermissionScope::Session,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            "system:power:lock",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;

        assert!(matches!(
            gw.check(
                Some("ses-a"),
                "system",
                &json!({"scope": "power", "operation": "lock"}),
                RiskLevel::High,
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_permanent_deny_beats_session_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "shell",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            "shell",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;

        assert!(matches!(
            gw.check(Some("ses-a"), "shell", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_local_tool_security_matrix_gates_every_risk_bearing_case() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        for case in LOCAL_TOOL_SECURITY_MATRIX {
            let result = gw
                .check(None, case.tool_name, &json!({}), case.risk_level)
                .await;
            if case.risk_level >= RiskLevel::Medium {
                assert!(
                    matches!(result, ConfirmationResult::RequiresConfirmation { .. }),
                    "{}:{} should be gated, got {result:?}",
                    case.tool_name,
                    case.operation
                );
            } else {
                assert!(
                    matches!(result, ConfirmationResult::AutoApproved),
                    "{}:{} should be automatic, got {result:?}",
                    case.tool_name,
                    case.operation
                );
            }
        }
    }

    #[tokio::test]
    async fn test_adapter_authorization_is_shared_but_session_scoped() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        let mcp_name = crate::McpToolAdapter::qualified_name_of("calendar", "create_event");
        let skill_name = crate::SkillToolAdapter::qualified_name_of("calendar");

        gw.grant(
            None,
            &mcp_name,
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            &skill_name,
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;

        assert!(matches!(
            gw.check(None, &mcp_name, &json!({}), RiskLevel::High).await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(Some("ses-a"), &skill_name, &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(None, &skill_name, &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn test_local_tool_security_matrix_covers_every_builtin_family() {
        let names: HashSet<_> = LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .map(|case| case.tool_name)
            .collect();
        for expected in [
            "audio",
            "ask",
            "files",
            "process",
            "clipboard",
            "shell",
            "actions",
            "input",
            "scheduled_action",
            "system",
            "window",
            "http",
            "notify",
            "agent",
            "load_skill",
            "load_mcp",
            "memory",
            "haven",
        ] {
            assert!(
                names.contains(expected),
                "missing matrix family: {expected}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_path_sandbox_rejects_symlink_reparse_escape() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = allowed.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        assert!(!path_is_allowed(
            &link.join("secret.txt"),
            &[allowed.path().to_path_buf()]
        ));
    }

    #[test]
    fn test_path_sandbox_allows_only_absolute_paths_inside_canonical_root() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = allowed.path().join("new").join("file.txt");

        assert!(path_is_allowed(&inside, &[allowed.path().to_path_buf()]));
        assert!(!path_is_allowed(
            &outside.path().join("file.txt"),
            &[allowed.path().to_path_buf()]
        ));
        assert!(!path_is_allowed(
            Path::new("relative.txt"),
            &[allowed.path().to_path_buf()]
        ));
        assert!(!path_is_allowed(
            Path::new("//server/share/file.txt"),
            &[allowed.path().to_path_buf()]
        ));
    }

    #[tokio::test]
    async fn test_path_sandbox_checks_source_and_destination_together() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                allowed_paths: vec![allowed.path().to_string_lossy().into_owned()],
                ..ToolConfig::default()
            },
        );
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        let result = gw
            .check(
                None,
                "files",
                &json!({
                    "operation": "copy",
                    "source": allowed.path().join("source.txt"),
                    "destination": outside.path().join("destination.txt"),
                }),
                RiskLevel::Medium,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_path_sandbox_rejects_parent_escape() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                allowed_paths: vec!["C:\\allowed".into()],
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "read", "path": "C:\\allowed\\..\\Windows\\System32"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[test]
    fn test_validate_input_valid() {
        let tool = schema_mock();
        let result = tool.validate_input(&json!({"name": "test", "count": 5}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_input_missing_required() {
        let tool = schema_mock();
        let result = tool.validate_input(&json!({"count": 5}));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_input_missing_required_message_lists_fields() {
        let tool = schema_mock();
        let err = tool.validate_input(&json!({})).unwrap_err().to_string();
        assert!(
            err.contains("MISSING REQUIRED FIELD(S): name"),
            "missing fields must be called out explicitly, got: {err}"
        );
    }

    #[test]
    fn test_validate_input_enum_hint_included() {
        // The file tool schema declares an enum on `operation`; a missing
        // operation must surface the allowed values so the model can self-correct.
        let tool = crate::builtin::files::FilesTool::default();
        let err = tool
            .validate_input(&json!({"path": "x.txt"}))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("operation"),
            "operation must be named, got: {err}"
        );
        assert!(
            err.contains("read, write, edit"),
            "allowed operations should be listed, got: {err}"
        );
    }

    #[test]
    fn test_validate_input_wrong_type() {
        let tool = schema_mock();
        let result = tool.validate_input(&json!({"name": 123}));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_with_timeout_quick() {
        let tool = MockTool::new("quick");
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 30)
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);
    }

    #[tokio::test]
    async fn test_execute_with_timeout_slow() {
        let tool = MockTool::with_delay("slow", Duration::from_secs(10));
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 1)
            .await;
        let result = result.unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::TimedOutUnknown);
        assert!(!result.success);
    }

    #[tokio::test]
    async fn test_execute_with_timeout_cancelled_is_structured() {
        let tool = MockTool::with_delay("cancelled", Duration::from_secs(10));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool
            .execute_with_timeout(json!({}), cancel, 30)
            .await
            .unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::Cancelled);
        assert!(!result.success);
    }
}
