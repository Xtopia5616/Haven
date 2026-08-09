use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub truncated: bool,
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
    /// The `ask` tool's question: when present, the loop pauses the task
    /// and waits for the user's reply.
    pub ask_question: Option<String>,
    /// Quick-reply options for the pending `ask`, surfaced as buttons.
    pub ask_options: Vec<String>,
    /// The `notify` tool's toast title (defaults to "Haven").
    pub notify_title: Option<String>,
    /// The `notify` tool's toast body.
    pub notify_body: Option<String>,
}

/// Per-task side effects a tool declares through its result. The task
/// executor applies them (registering skill/MCP adapters, attaching
/// background jobs) without hard-coding tool names, so a new tool that needs
/// a side effect declares it here instead of adding a name check in the
/// executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRegistration {
    /// Load a skill (raw name) for the current task.
    Skill(String),
    /// Load an MCP server (by name) for the current task.
    McpServer(String),
    /// Attach a background job (an activity of kind `job`) to the current
    /// task (end/rollback cleanup).
    Activity(String),
}

impl ToolResult {
    pub fn ok(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            truncated: false,
            signals: ToolSignals::default(),
        }
    }

    pub fn truncated(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            truncated: true,
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
}

/// Extract the `ask` signal from a tool result's structured output: the
/// question text and optional suggested answers. `(None, vec![])` when the
/// output does not carry a question. The signal must be read BEFORE any
/// truncation: parsing truncated text would yield invalid JSON when the
/// output exceeds the observation budget, silently dropping the question
/// and never pausing the task.
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
/// silent: hiding the question while the task pauses for an answer would
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
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult>;
    fn input_schema(&self) -> Value;

    fn default_timeout_secs(&self) -> u64 {
        30
    }

    /// Whether this tool needs the private `_task_id` input field injected
    /// before execution (e.g. `reminder`/`jobs` scope to the current task).
    /// The id is injected after the LLM-facing input was captured, so it
    /// never reaches the tool schema, the step history, or the LLM.
    fn requires_task_id(&self) -> bool {
        false
    }

    /// Side-channel signals carried by this tool's result (`ask` question /
    /// `notify` toast). Parsed from the structured output by the tool itself,
    /// BEFORE the loop truncates the observation text.
    fn signals(&self, output: &Value) -> ToolSignals {
        let _ = output;
        ToolSignals::default()
    }

    /// Per-task side effects to apply after a successful execution
    /// (skill/MCP adapters, background-job attachment).
    fn registrations(&self, output: &Value) -> Vec<ToolRegistration> {
        let _ = output;
        Vec::new()
    }

    /// Fallback result when this tool times out: instead of failing the step,
    /// the tool may hand the work to a background mechanism and return a
    /// success result carrying the continuation. Only invoked for timeout
    /// errors of the final attempt; returning `None` keeps the error.
    async fn timeout_fallback(&self, input: &Value) -> Option<ToolResult> {
        let _ = input;
        None
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
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.execute(input, cancel),
        )
        .await;
        match result {
            Ok(r) => r,
            Err(_) => anyhow::bail!("tool '{}' timed out after {}s", self.name(), timeout_secs),
        }
    }
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

    pub async fn list_schemas(&self) -> Vec<Value> {
        let tools = self.snapshot.read().await.tools.clone();
        tools
            .iter()
            .map(|t| {
                let risk = t.risk_level(&serde_json::json!({}));
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "risk_level": risk,
                    "input_schema": t.input_schema(),
                })
            })
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
/// `reminder`) validate tool names / risk levels at call time without owning
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
    },
    Blocked,
}

/// Combined safety config under a single RwLock so `check` reads both
/// fields atomically and `set_min_risk_level` updates both atomically.
#[derive(Clone)]
struct SafetyConfig {
    min_risk_level: RiskLevel,
    session_trusted_levels: HashSet<RiskLevel>,
}

pub struct SafetyGateway {
    config: RwLock<SafetyConfig>,
}

impl SafetyGateway {
    pub fn new(min_risk_level: RiskLevel) -> Self {
        Self {
            config: RwLock::new(SafetyConfig {
                min_risk_level,
                session_trusted_levels: HashSet::new(),
            }),
        }
    }

    /// Update the minimum risk level threshold.
    /// Operations below this level auto-approve; at or above require confirmation.
    /// Resets any session trusts on change.
    pub async fn set_min_risk_level(&self, level: RiskLevel) {
        let mut cfg = self.config.write().await;
        cfg.min_risk_level = level;
        cfg.session_trusted_levels.clear();
    }

    pub async fn check(
        &self,
        tool_name: &str,
        params: &Value,
        risk_level: RiskLevel,
    ) -> ConfirmationResult {
        let cfg = self.config.read().await;

        // Below threshold → auto approved
        if risk_level < cfg.min_risk_level {
            return ConfirmationResult::AutoApproved;
        }

        // Session-trusted risk levels → auto approved
        if cfg.session_trusted_levels.contains(&risk_level) {
            return ConfirmationResult::AutoApproved;
        }

        ConfirmationResult::RequiresConfirmation {
            tool_name: tool_name.into(),
            params: params.clone(),
            risk_level,
        }
    }

    /// Trust a risk level for the remainder of the session.
    pub async fn trust_risk_level(&self, level: RiskLevel) {
        self.config
            .write()
            .await
            .session_trusted_levels
            .insert(level);
    }
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
            signals: ToolSignals::default(),
        };
        assert_eq!(result.summary_text(), "unknown failure");
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
            "title": "Reminder",
            "body": "Take a break",
        }));
        assert_eq!(title.as_deref(), Some("Reminder"));
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
        registry
            .register(Arc::new(MockTool::new("a")))
            .await;
        registry
            .register(Arc::new(MockTool::new("b")))
            .await;

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
        registry
            .register(Arc::new(MockTool::new("mock")))
            .await;

        let schemas = registry.list_schemas().await;
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"].as_str().unwrap(), "mock");
        assert_eq!(schemas[0]["description"].as_str().unwrap(), "mock");
        assert!(schemas[0]["input_schema"].is_object());
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
        let result = gw.check("tool1", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_below_threshold_auto_approved() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        // Low is below Medium → auto approved
        let result = gw.check("tool1", &json!({}), RiskLevel::Low).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_at_threshold_requires_confirmation() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        // Medium is at the threshold → requires confirmation
        let result = gw.check("tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_above_threshold_requires_confirmation() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        // High is above Low → requires confirmation
        let result = gw.check("tool1", &json!({}), RiskLevel::High).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_trusted_risk_level_auto_approved() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.trust_risk_level(RiskLevel::Medium).await;
        let result = gw.check("tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_set_threshold_clears_trust() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        gw.trust_risk_level(RiskLevel::Medium).await;
        // Medium is >= Low → check against threshold should pass since trusted
        let result = gw.check("tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));

        // Raising threshold clears session trusts
        gw.set_min_risk_level(RiskLevel::High).await;
        // Medium is below High → auto approved anyway (below threshold)
        let result = gw.check("tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));

        // High is at threshold, trust was cleared → requires confirmation
        let result = gw.check("tool1", &json!({}), RiskLevel::High).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
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
        let tool = crate::builtin::file::FilesTool::default();
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
        assert!(result.is_err());
    }
}
