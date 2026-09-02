use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub use haven_common::tools::ToolDef;

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

/// The scope in which a typed operation is allowed to observe or mutate
/// state. This is deliberately separate from the LLM-facing tool name: one
/// grouped tool can contain both global and session-scoped operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOperationScope {
    Global,
    Session,
}

/// How an operation responds to cancellation and the outer timeout. The
/// execution wrapper still owns the actual token; this metadata tells the
/// safety, retry, and audit layers what can be concluded after cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCancellationPolicy {
    Cooperative,
    Terminating,
    Unknown,
}

/// Complete runtime policy for one typed operation.
///
/// `Tool` remains the provider-facing object-safe boundary and therefore
/// accepts JSON at its edge. Implementations behind that boundary use
/// [`TypedToolOperation`] and expose this metadata from the same typed
/// operation that parses and executes the arguments. This prevents risk,
/// retry, concurrency, and timeout decisions from drifting into unrelated
/// string matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOperationMetadata {
    pub capability: &'static str,
    pub operation: &'static str,
    pub scope: ToolOperationScope,
    pub risk_level: RiskLevel,
    pub idempotency: OperationIdempotency,
    pub cancellation: ToolCancellationPolicy,
    pub timeout_secs: u64,
    pub concurrency: ToolConcurrency,
}

/// A typed runtime operation. The only JSON conversion is performed by
/// [`TypedToolAdapter`] at the provider boundary; the operation itself sees
/// typed arguments, returns a typed output, and reports a typed error.
#[async_trait::async_trait]
pub trait TypedToolOperation: Send + Sync {
    type Args: serde::de::DeserializeOwned + Send;
    type Output: serde::Serialize + Send;
    type Error: std::fmt::Display + Send + Sync + 'static;

    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata;
    fn default_metadata(&self) -> ToolOperationMetadata;
    fn input_schema(&self) -> Value;

    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error>;
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
        if max_chars == 0 {
            return String::new();
        }

        // The marker is part of the observation budget. Appending it after a
        // `max_chars` prefix made every result exceed the configured cap and
        // compounded context pressure across a tool batch.
        let mut prefix_chars = max_chars.saturating_sub(32);
        let mut marker = String::new();
        for _ in 0..4 {
            let omitted = char_count.saturating_sub(prefix_chars);
            marker = format!("[... truncated {omitted} chars omitted]");
            let next_prefix = max_chars.saturating_sub(marker.chars().count());
            if next_prefix == prefix_chars {
                break;
            }
            prefix_chars = next_prefix;
        }
        if marker.chars().count() > max_chars {
            return text.chars().take(max_chars).collect();
        }
        let prefix: String = text.chars().take(prefix_chars).collect();
        format!("{prefix}{marker}")
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

/// Object-safe provider adapter for one typed operation group. It performs
/// JSON parsing only at the LLM/provider edge and delegates all policy and
/// execution decisions to the typed operation.
pub struct TypedToolAdapter<O> {
    name: String,
    description: String,
    operation: O,
}

impl<O> TypedToolAdapter<O> {
    pub fn new(name: impl Into<String>, description: impl Into<String>, operation: O) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            operation,
        }
    }

    pub fn operation(&self) -> &O {
        &self.operation
    }
}

#[async_trait::async_trait]
impl<O> Tool for TypedToolAdapter<O>
where
    O: TypedToolOperation,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).risk_level)
            .unwrap_or(RiskLevel::Critical)
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).idempotency)
            .unwrap_or(OperationIdempotency::Unknown)
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        match self.operation.default_metadata().cancellation {
            ToolCancellationPolicy::Terminating => ToolExecutionOutcome::TimedOutAndTerminated,
            ToolCancellationPolicy::Cooperative | ToolCancellationPolicy::Unknown => {
                ToolExecutionOutcome::TimedOutUnknown
            }
        }
    }

    fn input_schema(&self) -> Value {
        self.operation.input_schema()
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).concurrency)
            .unwrap_or(ToolConcurrency::Exclusive)
    }

    fn default_timeout_secs(&self) -> u64 {
        self.operation.default_metadata().timeout_secs
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        serde_json::from_value::<O::Args>(input.clone())
            .ok()
            .map(|args| self.operation.metadata(&args).timeout_secs)
            .unwrap_or_else(|| self.default_timeout_secs())
    }

    fn tool_def(&self) -> ToolDef {
        let metadata = self.operation.default_metadata();
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            metadata.risk_level,
        )
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let args = parse_tool_input::<O::Args>(&self.name(), input)?;
        let output = self
            .operation
            .execute_typed(args, cancel)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let output = serde_json::to_value(output)
            .map_err(|error| anyhow::anyhow!("serialize typed tool output: {error}"))?;
        Ok(ToolResult::ok(output))
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
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
        assert_eq!(observation.chars().count(), 3);
        assert!(observation.is_char_boundary(observation.len()));
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

    pub(crate) struct MockTool {
        name: String,
        schema: Value,
        execute_delay: Option<Duration>,
    }

    impl MockTool {
        pub(crate) fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                schema: json!({"type": "object"}),
                execute_delay: None,
            }
        }

        pub(crate) fn with_schema(name: &str, schema: Value) -> Self {
            Self {
                name: name.into(),
                schema,
                execute_delay: None,
            }
        }

        pub(crate) fn with_delay(name: &str, delay: Duration) -> Self {
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
