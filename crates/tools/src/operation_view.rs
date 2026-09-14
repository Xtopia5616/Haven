use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{
    ConfirmationRequirement, OperationIdempotency, OperationPolicy, Tool, ToolBox, ToolConcurrency,
    ToolDef, ToolExecutionOutcome, ToolOperationScope, ToolRegistration, ToolResult, ToolSignals,
};
use haven_common::tools::{
    ToolAvailability, ToolCatalogGroup, ToolIdentity, ToolManifest, ToolModel, ToolPresentation,
    ToolPrompt, ToolSource,
};

/// Declarative specification for a model-facing operation view. The aggregate tool
/// remains the execution implementation, while this record is the one source
/// for the view's model schema and runtime policy metadata.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct OperationSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) fixed: Vec<(String, Value)>,
    pub(crate) schema: Value,
    pub(crate) policy: OperationPolicy,
    pub(crate) risk_rule: Option<OperationViewRiskRule>,
    pub(crate) catalog_group: ToolCatalogGroup,
    pub(crate) presentation: ToolPresentation,
    pub(crate) prompt: ToolPrompt,
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
    spec: OperationSpec,
    fixed: Map<String, Value>,
}

impl OperationViewTool {
    pub(crate) fn new(inner: ToolBox, mut spec: OperationSpec) -> Arc<Self> {
        annotate_schema(&mut spec);
        let fixed = Map::from_iter(spec.fixed.iter().cloned());
        Arc::new(Self { inner, spec, fixed })
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
fn annotate_schema(spec: &mut OperationSpec) {
    let Some(schema) = spec.schema.as_object_mut() else {
        return;
    };
    schema.insert("title".into(), Value::String(spec.name.into()));
    schema.insert("description".into(), Value::String(spec.description.into()));
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
        self.spec.name.to_string()
    }

    fn description(&self) -> String {
        self.spec.description.to_string()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match self.spec.risk_rule {
            Some(OperationViewRiskRule::ContentSearchMedium)
                if input.get("mode").and_then(Value::as_str) == Some("content") =>
            {
                RiskLevel::Medium
            }
            _ => self.spec.policy.risk_level,
        }
    }

    fn operation_policy(&self, input: &Value) -> OperationPolicy {
        let mut policy = self.spec.policy.clone();
        policy.risk_level = self.risk_level(input);
        policy.confirmation = if policy.risk_level >= RiskLevel::Critical {
            ConfirmationRequirement::Required
        } else if policy.is_read_only() || policy.risk_level == RiskLevel::Safe {
            ConfirmationRequirement::None
        } else {
            ConfirmationRequirement::SecurityPolicy
        };
        policy
    }

    fn idempotency(&self, _input: &Value) -> OperationIdempotency {
        self.spec.policy.idempotency
    }

    fn operation_scope(&self, _input: &Value) -> ToolOperationScope {
        self.spec.policy.scope
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
        self.spec.schema.clone()
    }

    fn tool_def(&self) -> ToolDef {
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.spec.policy.risk_level,
        )
        .with_retry_safety(self.spec.policy.idempotency.tool_retry_safety())
        .with_catalog_group(self.spec.catalog_group)
        .with_prompt(self.spec.prompt.clone())
        .with_manifest(self.tool_manifest())
    }

    fn concurrency(&self, _input: &Value) -> ToolConcurrency {
        self.spec.policy.concurrency.clone()
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

    fn tool_manifest(&self) -> ToolManifest {
        let name = self.name();
        let root = name.split('.').next().unwrap_or(&name).to_string();
        let operation = name
            .strip_prefix(&format!("{root}."))
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        // A manifest has no concrete input, so expose the conservative upper
        // bound for input-dependent risk rules. Runtime calls still refine the
        // same policy through `operation_policy(input)`.
        let mut manifest_policy = self.spec.policy.clone();
        if self.spec.risk_rule.is_some() {
            if manifest_policy.risk_level < RiskLevel::Medium {
                manifest_policy.risk_level = RiskLevel::Medium;
            }
            manifest_policy.confirmation = ConfirmationRequirement::SecurityPolicy;
        }
        ToolManifest {
            identity: ToolIdentity {
                source: ToolSource::Builtin,
                catalog_group: self.spec.catalog_group,
                root: root.clone(),
                operation,
                stable_name: name.clone(),
            },
            model: ToolModel {
                name: name.clone(),
                description: self.description(),
                input_schema: self.input_schema(),
            },
            policy: manifest_policy.to_catalog_policy(),
            presentation: self.spec.presentation.clone(),
            root_presentation: crate::tool_contract::default_root_presentation(
                &root,
                ToolSource::Builtin,
            ),
            prompt: self.spec.prompt.clone(),
            availability: ToolAvailability {
                requires_permission: manifest_policy.risk_level >= RiskLevel::Medium,
                ..ToolAvailability::default()
            },
        }
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
            OperationSpec {
                name: "files.read",
                description: "Read text.",
                fixed: vec![("operation".into(), json!("read"))],
                schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
                policy: OperationPolicy {
                    risk_level: RiskLevel::Low,
                    permission_key: "files.read".into(),
                    confirmation: ConfirmationRequirement::SecurityPolicy,
                    idempotency: OperationIdempotency::Idempotent,
                    scope: ToolOperationScope::Session,
                    concurrency: ToolConcurrency::ReadOnly,
                    effect: crate::OperationEffect::ReadOnly,
                    data_sensitivity: crate::DataSensitivity::UserData,
                    network_access: crate::NetworkAccess::None,
                },
                risk_rule: None,
                catalog_group: ToolCatalogGroup::System,
                presentation: ToolPresentation {
                    label: "读取文件".into(),
                    renderer: "files".into(),
                    icon: "file".into(),
                    represented_source: ToolSource::Builtin,
                },
                prompt: ToolPrompt {
                    when_to_use: "Read text.".into(),
                    when_not_to_use: "Use another operation view.".into(),
                    key_operations: vec!["files.read".into()],
                },
            },
        );

        assert_eq!(view.input_schema()["title"], "files.read");
        assert_eq!(view.input_schema()["description"], "Read text.");
        let def = view.tool_def();
        assert_eq!(def.prompt.as_ref().unwrap().key_operations, ["files.read"]);
        let manifest = view.tool_manifest();
        assert_eq!(manifest.identity.stable_name, "files.read");
        assert_eq!(manifest.presentation.renderer, "files");
        assert_eq!(manifest.presentation.label, "读取文件");
        assert_eq!(manifest.root_presentation.label, "文件");
        assert!(def.json().get("manifest").is_none());
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
