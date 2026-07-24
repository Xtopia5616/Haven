use haven_common::config::ToolConfig;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: Value,
    pub output: Option<Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub truncated: bool,
}

impl ToolResult {
    pub fn ok(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            truncated: false,
        }
    }

    pub fn truncated(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
            truncated: true,
        }
    }
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

    fn max_output_chars(&self) -> usize {
        100_000
    }

    fn tool_config(&self) -> Option<ToolConfig> {
        None
    }

    fn validate_input(&self, input: &Value) -> anyhow::Result<()> {
        let schema = self.input_schema();
        if schema.is_null() || schema == serde_json::Value::Null {
            return Ok(());
        }
        let compiled = jsonschema::JSONSchema::compile(&schema)
            .map_err(|e| anyhow::anyhow!("invalid tool schema for '{}': {}", self.name(), e))?;
        if let Err(errors) = compiled.validate(input) {
            let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
            anyhow::bail!("input validation failed for '{}': {}", self.name(), msgs.join("; "));
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

#[derive(Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<Vec<ToolBox>>>,
    name_index: Arc<RwLock<HashMap<String, ToolBox>>>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            name_index: self.name_index.clone(),
        }
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(RwLock::new(Vec::new())),
            name_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(&self, tool: ToolBox) {
        let name = tool.name();
        self.tools.write().await.push(tool.clone());
        self.name_index.write().await.insert(name, tool);
    }

    pub async fn get(&self, name: &str) -> Option<ToolBox> {
        self.name_index.read().await.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<ToolBox> {
        self.tools.read().await.clone()
    }

    pub async fn list_schemas(&self) -> Vec<Value> {
        let tools = self.tools.read().await;
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
    /// Uses a write lock so readers see a consistent snapshot.
    pub async fn rebuild(&self, new_tools: Vec<ToolBox>) {
        let mut index = HashMap::new();
        for t in &new_tools {
            index.insert(t.name(), t.clone());
        }
        *self.tools.write().await = new_tools;
        *self.name_index.write().await = index;
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

pub struct SafetyGateway {
    whitelist: Mutex<HashSet<String>>,
    session_trusted: Arc<Mutex<HashSet<String>>>,
}

impl SafetyGateway {
    pub fn new(whitelist: Vec<String>) -> Self {
        Self {
            whitelist: Mutex::new(whitelist.into_iter().collect()),
            session_trusted: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Hot-replace the user-configured tool whitelist (design §4.6.5).
    /// Clears any session trusts so users see the confirmation prompt
    /// once when re-adding a tool to the whitelist after a policy change.
    pub async fn set_whitelist(&self, whitelist: Vec<String>) {
        let mut guard = self.whitelist.lock().await;
        guard.clear();
        guard.extend(whitelist);
        drop(guard);
        self.session_trusted.lock().await.clear();
    }

    pub async fn check(
        &self,
        tool_name: &str,
        params: &Value,
        risk_level: RiskLevel,
    ) -> ConfirmationResult {
        if risk_level == RiskLevel::Safe {
            return ConfirmationResult::AutoApproved;
        }
        if self.whitelist.lock().await.contains(tool_name) {
            return ConfirmationResult::AutoApproved;
        }
        let trusted = self.session_trusted.lock().await;
        if trusted.contains(tool_name) {
            return ConfirmationResult::AutoApproved;
        }
        ConfirmationResult::RequiresConfirmation {
            tool_name: tool_name.into(),
            params: params.clone(),
            risk_level,
        }
    }

    pub async fn trust_session(&self, tool_name: &str) {
        self.session_trusted.lock().await.insert(tool_name.into());
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

    struct MockTool {
        name: String,
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
            Ok(ToolResult::ok(json!({"ok": true})))
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
    }

    struct SlowMockTool;

    #[async_trait::async_trait]
    impl Tool for SlowMockTool {
        fn name(&self) -> String {
            "slow".into()
        }
        fn description(&self) -> String {
            "slow mock tool".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(ToolResult::ok(json!({"done": true})))
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
    }

    struct SchemaMockTool;

    #[async_trait::async_trait]
    impl Tool for SchemaMockTool {
        fn name(&self) -> String {
            "schema_mock".into()
        }
        fn description(&self) -> String {
            "schema validation mock".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({"ok": true})))
        }
        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "count": { "type": "integer" }
                },
                "required": ["name"]
            })
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
        let tool = Arc::new(MockTool {
            name: "mock1".into(),
        });
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
            .register(Arc::new(MockTool {
                name: "a".into(),
            }))
            .await;
        registry
            .register(Arc::new(MockTool {
                name: "b".into(),
            }))
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
            .register(Arc::new(MockTool {
                name: "mock".into(),
            }))
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
        let old_tool = Arc::new(MockTool {
            name: "old".into(),
        });
        registry.register(old_tool).await;

        let new_tool = Arc::new(MockTool {
            name: "new".into(),
        });
        registry.rebuild(vec![new_tool.clone()]).await;

        assert!(registry.get("old").await.is_none());
        assert!(registry.get("new").await.is_some());
    }

    #[tokio::test]
    async fn test_safety_gateway_new() {
        let gw = SafetyGateway::new(Vec::new());
        let result = gw
            .check("tool1", &json!({}), RiskLevel::Safe)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_safe_auto_approved() {
        let gw = SafetyGateway::new(Vec::new());
        let result = gw
            .check("tool1", &json!({}), RiskLevel::Safe)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_medium_no_whitelist() {
        let gw = SafetyGateway::new(Vec::new());
        let result = gw
            .check("tool1", &json!({}), RiskLevel::Medium)
            .await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_high_in_whitelist() {
        let gw = SafetyGateway::new(vec!["tool1".into()]);
        let result = gw
            .check("tool1", &json!({}), RiskLevel::High)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_high_trust_session() {
        let gw = SafetyGateway::new(Vec::new());
        gw.trust_session("tool1").await;
        let result = gw
            .check("tool1", &json!({}), RiskLevel::High)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_high_no_whitelist() {
        let gw = SafetyGateway::new(Vec::new());
        let result = gw
            .check("tool1", &json!({}), RiskLevel::High)
            .await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_set_whitelist_clears_trust() {
        let gw = SafetyGateway::new(Vec::new());
        gw.trust_session("tool1").await;
        let result = gw
            .check("tool1", &json!({}), RiskLevel::High)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));

        gw.set_whitelist(vec![]).await;
        let result = gw
            .check("tool1", &json!({}), RiskLevel::High)
            .await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn test_validate_input_valid() {
        let tool = SchemaMockTool;
        let result = tool.validate_input(&json!({"name": "test", "count": 5}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_input_missing_required() {
        let tool = SchemaMockTool;
        let result = tool.validate_input(&json!({"count": 5}));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_input_wrong_type() {
        let tool = SchemaMockTool;
        let result = tool.validate_input(&json!({"name": 123}));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_with_timeout_quick() {
        let tool = MockTool {
            name: "quick".into(),
        };
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 30)
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().success);
    }

    #[tokio::test]
    async fn test_execute_with_timeout_slow() {
        let tool = SlowMockTool;
        let result = tool
            .execute_with_timeout(json!({}), CancellationToken::new(), 1)
            .await;
        assert!(result.is_err());
    }
}
