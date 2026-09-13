use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{
    OperationIdempotency, Tool, ToolBox, ToolConcurrency, ToolDef, ToolExecutionOutcome,
    ToolOperationScope, ToolRegistration, ToolResult, ToolSignals,
};
use haven_common::tools::{ToolCatalogGroup, ToolPrompt};

/// Declarative contract for a model-facing operation view. The aggregate tool
/// remains the execution implementation, while this record is the one source
/// for the view's model schema and runtime policy metadata.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct OperationViewContract {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) fixed: Vec<(String, Value)>,
    pub(crate) schema: Value,
    pub(crate) risk_level: RiskLevel,
    pub(crate) risk_rule: Option<OperationViewRiskRule>,
    pub(crate) idempotency: OperationIdempotency,
    pub(crate) scope: ToolOperationScope,
    pub(crate) concurrency: ToolConcurrency,
    pub(crate) permission_key: String,
    pub(crate) catalog_group: ToolCatalogGroup,
    pub(crate) renderer: String,
    pub(crate) icon: String,
    pub(crate) prompt: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum OperationViewRiskRule {
    ContentSearchMedium,
}

/// A narrow provider-facing view over an aggregate tool.
///
/// The aggregate implementation remains the single execution and policy
/// source. This adapter only fixes the operation discriminator and publishes
/// the smaller schema that the model needs for that operation. Native and
/// model-facing callers therefore continue to share the same implementation.
pub(crate) struct OperationViewTool {
    inner: ToolBox,
    contract: OperationViewContract,
    fixed: Map<String, Value>,
}

impl OperationViewTool {
    pub(crate) fn new(inner: ToolBox, mut contract: OperationViewContract) -> Arc<Self> {
        annotate_schema(&mut contract);
        let fixed = Map::from_iter(contract.fixed.iter().cloned());
        Arc::new(Self {
            inner,
            contract,
            fixed,
        })
    }

    fn routed_input(&self, input: &Value) -> Value {
        let mut object = match input {
            Value::Object(object) => object.clone(),
            _ => Map::new(),
        };
        for (key, value) in &self.fixed {
            object.insert(key.clone(), value.clone());
        }
        Value::Object(object)
    }
}

/// Add view-level schema metadata after the selected operation branch has been
/// extracted. The branch remains the validation authority; these fields only
/// make the narrow provider schema self-describing in tool inspectors and
/// provider traces.
fn annotate_schema(contract: &mut OperationViewContract) {
    let Some(schema) = contract.schema.as_object_mut() else {
        return;
    };
    schema.insert("title".into(), Value::String(contract.name.into()));
    schema.insert(
        "description".into(),
        Value::String(contract.description.into()),
    );
}

/// Extract one operation branch from an aggregate tool schema and remove the
/// fixed discriminator from the provider-facing input. The aggregate remains
/// the execution boundary; this helper only creates a narrow model view.
pub(crate) fn split_operation_schema(schema: &Value, operation: &str) -> Option<Value> {
    let mut branches = Vec::new();
    collect_operation_branches(schema, operation, &mut branches);
    match branches.len() {
        0 => None,
        1 => branches.into_iter().next(),
        _ => Some(serde_json::json!({
            "type": "object",
            "oneOf": branches,
        })),
    }
}

/// Extract one nested `scope` + `operation` branch, used by `system` views.
pub(crate) fn split_scope_operation_schema(
    schema: &Value,
    scope: &str,
    operation: &str,
) -> Option<Value> {
    let mut branches = Vec::new();
    collect_scope_operation_branches(schema, scope, operation, &mut branches);
    match branches.len() {
        0 => None,
        1 => branches.into_iter().next(),
        _ => Some(serde_json::json!({
            "type": "object",
            "oneOf": branches,
        })),
    }
}

fn collect_operation_branches(node: &Value, operation: &str, out: &mut Vec<Value>) {
    let Some(object) = node.as_object() else {
        return;
    };
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for branch in one_of {
            collect_operation_branches(branch, operation, out);
        }
        return;
    }
    let Some(operation_schema) = object
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("operation"))
    else {
        return;
    };
    let matches = operation_schema.get("const").and_then(Value::as_str) == Some(operation)
        || operation_schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(operation)));
    if !matches {
        return;
    }
    let mut branch = node.clone();
    remove_fixed_property(&mut branch, "operation");
    out.push(branch);
}

fn collect_scope_operation_branches(
    node: &Value,
    scope: &str,
    operation: &str,
    out: &mut Vec<Value>,
) {
    let Some(object) = node.as_object() else {
        return;
    };
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for branch in one_of {
            collect_scope_operation_branches(branch, scope, operation, out);
        }
        return;
    }
    let Some(properties) = object.get("properties").and_then(Value::as_object) else {
        return;
    };
    let scope_matches = properties
        .get("scope")
        .and_then(|value| value.get("const"))
        .and_then(Value::as_str)
        == Some(scope);
    let operation_matches = properties
        .get("operation")
        .and_then(|value| value.get("const"))
        .and_then(Value::as_str)
        == Some(operation);
    if scope_matches && operation_matches {
        let mut branch = node.clone();
        remove_fixed_property(&mut branch, "scope");
        remove_fixed_property(&mut branch, "operation");
        out.push(branch);
    }
}

fn remove_fixed_property(schema: &mut Value, name: &str) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        properties.remove(name);
    }
    if let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|value| value.as_str() != Some(name));
    }
}

#[async_trait]
impl Tool for OperationViewTool {
    fn name(&self) -> String {
        self.contract.name.to_string()
    }

    fn description(&self) -> String {
        self.contract.description.to_string()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match self.contract.risk_rule {
            Some(OperationViewRiskRule::ContentSearchMedium)
                if input.get("mode").and_then(Value::as_str) == Some("content") =>
            {
                RiskLevel::Medium
            }
            _ => self.contract.risk_level,
        }
    }

    fn idempotency(&self, _input: &Value) -> OperationIdempotency {
        self.contract.idempotency
    }

    fn operation_scope(&self, _input: &Value) -> ToolOperationScope {
        self.contract.scope
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        self.inner.timeout_outcome()
    }

    fn default_max_retries(&self) -> u32 {
        self.inner.default_max_retries()
    }

    fn default_retry_backoff_secs(&self) -> u64 {
        self.inner.default_retry_backoff_secs()
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        self.inner.execute(self.routed_input(&input), cancel).await
    }

    fn input_schema(&self) -> Value {
        self.contract.schema.clone()
    }

    fn tool_def(&self) -> ToolDef {
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.contract.risk_level,
        )
        .with_retry_safety(self.contract.idempotency.tool_retry_safety())
        .with_catalog_group(self.contract.catalog_group)
        .with_prompt(ToolPrompt {
            when_to_use: self.contract.prompt.clone(),
            when_not_to_use:
                "Use a different operation view for another action; do not add an operation field."
                    .into(),
            key_operations: vec![self.contract.name.into()],
        })
    }

    fn concurrency(&self, _input: &Value) -> ToolConcurrency {
        self.contract.concurrency.clone()
    }

    fn default_timeout_secs(&self) -> u64 {
        self.inner.default_timeout_secs()
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        self.inner.timeout_secs_for(&self.routed_input(input))
    }

    fn requires_session_id(&self) -> bool {
        self.inner.requires_session_id()
    }

    fn supports_live_output(&self) -> bool {
        self.inner.supports_live_output()
    }

    fn signals(&self, output: &Value) -> ToolSignals {
        self.inner.signals(output)
    }

    fn registrations(&self, output: &Value) -> Vec<ToolRegistration> {
        self.inner.registrations(output)
    }

    fn authorization_input(&self, input: &Value) -> Value {
        self.routed_input(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_operation_schema_keeps_only_the_selected_branch() {
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "read" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "type": "object",
                    "properties": { "operation": { "const": "write" }, "content": { "type": "string" } },
                    "required": ["operation", "content"]
                }
            ]
        });

        let view = split_operation_schema(&schema, "read").expect("read branch");
        assert_eq!(view["properties"]["path"]["type"], "string");
        assert!(view["properties"].get("operation").is_none());
        assert_eq!(view["required"], json!(["path"]));
    }

    #[test]
    fn operation_view_schema_is_self_describing() {
        let inner: ToolBox = Arc::new(crate::tool_contract::tests::MockTool::with_schema(
            "files",
            json!({
                "type": "object",
                "oneOf": [{
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"operation": {"const": "read"}, "path": {"type": "string"}},
                    "required": ["operation", "path"]
                }]
            }),
        ));
        let view = OperationViewTool::new(
            inner,
            OperationViewContract {
                name: "files.read",
                description: "Read text.",
                fixed: vec![("operation".into(), json!("read"))],
                schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
                risk_level: RiskLevel::Low,
                risk_rule: None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::ReadOnly,
                permission_key: "files.read".into(),
                catalog_group: ToolCatalogGroup::System,
                renderer: "files".into(),
                icon: "file".into(),
                prompt: "Read text.".into(),
            },
        );

        assert_eq!(view.input_schema()["title"], "files.read");
        assert_eq!(view.input_schema()["description"], "Read text.");
        let def = view.tool_def();
        assert_eq!(def.prompt.unwrap().key_operations, ["files.read"]);
    }

    #[test]
    fn split_scope_operation_schema_removes_both_fixed_discriminators() {
        let schema = json!({
            "oneOf": [{
                "type": "object",
                "properties": {
                    "scope": { "const": "env" },
                    "operation": { "const": "get" },
                    "name": { "type": "string" }
                },
                "required": ["scope", "operation", "name"]
            }]
        });

        let view = split_scope_operation_schema(&schema, "env", "get").expect("env.get branch");
        assert!(view["properties"].get("scope").is_none());
        assert!(view["properties"].get("operation").is_none());
        assert_eq!(view["required"], json!(["name"]));
    }
}
