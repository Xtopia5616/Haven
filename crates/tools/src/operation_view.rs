use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{
    ConfirmationRequirement, OperationIdempotency, OperationPolicy, StructuredToolError, Tool,
    ToolConcurrency, ToolDef, ToolErrorMetadata, ToolExecutionOutcome, ToolHandle,
    ToolOperationScope, ToolRegistration, ToolResult, ToolSignals,
};
use haven_common::tools::{
    ToolAvailability, ToolCatalogGroup, ToolIdentity, ToolManifest, ToolModel, ToolPresentation,
    ToolPrompt, ToolRootPresentation, ToolSource,
};

/// Authored name, schema, presentation, and policy for one operation.
///
/// The spec has no handler. Builtin views, root tools, and MCP/Skill adapters
/// all publish this record. `policy_for` is the policy for one call.
/// `catalog_policy` is the model-visible upper bound. `ToolManifest`,
/// `ToolPolicy`, and `ToolPresentation` remain the IPC shapes produced by
/// `project_tool_manifest`.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct OperationSpec {
    pub(crate) name: Cow<'static, str>,
    pub(crate) description: Cow<'static, str>,
    pub(crate) fixed: Vec<(String, Value)>,
    pub(crate) schema: Value,
    pub(crate) policy: OperationPolicy,
    pub(crate) policy_rule: Option<OperationPolicyRule>,
    pub(crate) catalog_group: ToolCatalogGroup,
    pub(crate) presentation: ToolPresentation,
    pub(crate) prompt: ToolPrompt,
    /// `None` keeps the builtin projection: source is builtin, root is the
    /// name prefix, and root presentation comes from the builtin table.
    pub(crate) identity: Option<OperationIdentity>,
}

/// Optional catalog identity for operations that are not plain builtin views.
/// Builtin view specs leave this empty so their manifests stay stable.
#[derive(Debug, Clone)]
pub(crate) struct OperationIdentity {
    pub(crate) source: ToolSource,
    pub(crate) root: Cow<'static, str>,
    pub(crate) operation: Option<Cow<'static, str>>,
    pub(crate) root_presentation: ToolRootPresentation,
    pub(crate) availability: ToolAvailability,
}

/// Input-sensitive adjustment applied to one stored [`OperationSpec`] policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationPolicyRule {
    /// Filename search keeps the stored risk. `mode=content` discloses file
    /// text and raises that call to Medium. The catalog publishes the upper
    /// bound and its security-policy confirmation.
    ContentSearchMedium,
    /// The HTTP method changes replay safety and the request lock only.
    HttpVerb,
}

/// A narrow provider-facing view over an aggregate tool.
///
/// Execution stays on the aggregate. Policy, catalog group, and manifest come
/// from the spec captured at registration, not from a second projection.
pub(crate) struct OperationViewTool {
    handler: ToolHandle,
    spec: OperationSpec,
    fixed: Map<String, Value>,
}

impl OperationSpec {
    /// Policy for one invocation. Rules here must not invent a second
    /// confirmation table; content search only raises risk, and HTTP only
    /// changes idempotency and concurrency.
    pub(crate) fn policy_for(&self, input: &Value) -> OperationPolicy {
        let mut policy = self.policy.clone();
        match self.policy_rule {
            Some(OperationPolicyRule::ContentSearchMedium)
                if input.get("mode").and_then(Value::as_str) == Some("content") =>
            {
                policy.risk_level = RiskLevel::Medium;
            }
            Some(OperationPolicyRule::HttpVerb) => apply_http_verb(&mut policy, input),
            _ => {}
        }
        policy
    }

    /// Catalog upper bound. Only content search is wider than an empty call.
    pub(crate) fn catalog_policy(&self) -> OperationPolicy {
        let mut policy = self.policy.clone();
        if matches!(
            self.policy_rule,
            Some(OperationPolicyRule::ContentSearchMedium)
        ) {
            if policy.risk_level < RiskLevel::Medium {
                policy.risk_level = RiskLevel::Medium;
            }
            policy.confirmation = ConfirmationRequirement::SecurityPolicy;
        }
        policy
    }

    /// UI/IPC projection of [`Self::catalog_policy`].
    pub(crate) fn manifest(&self, schema: Value) -> ToolManifest {
        let name = self.name.to_string();
        let (source, root, operation, root_presentation, availability) = match &self.identity {
            Some(identity) => (
                identity.source,
                identity.root.to_string(),
                identity.operation.as_ref().map(ToString::to_string),
                identity.root_presentation.clone(),
                identity.availability.clone(),
            ),
            None => {
                let root = name.split('.').next().unwrap_or(&name).to_string();
                let operation = name
                    .strip_prefix(&format!("{root}."))
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string);
                (
                    ToolSource::Builtin,
                    root.clone(),
                    operation,
                    crate::tool_contract::default_root_presentation(&root, ToolSource::Builtin),
                    ToolAvailability::default(),
                )
            }
        };
        let policy = self.catalog_policy();
        crate::tool_contract::project_tool_manifest(
            ToolIdentity {
                source,
                catalog_group: self.catalog_group,
                root,
                operation,
                stable_name: name.clone(),
            },
            ToolModel {
                name,
                description: self.description.to_string(),
                input_schema: schema,
            },
            &policy,
            self.presentation.clone(),
            root_presentation,
            self.prompt.clone(),
            availability,
        )
    }
}

fn apply_http_verb(policy: &mut OperationPolicy, input: &Value) {
    match input.get("method").and_then(Value::as_str) {
        None | Some("GET") => {
            policy.idempotency = OperationIdempotency::Idempotent;
            policy.concurrency = ToolConcurrency::SharedResource("http".into());
        }
        Some("POST") => {
            policy.idempotency = OperationIdempotency::NonIdempotent;
            policy.concurrency = ToolConcurrency::Resource("http".into());
        }
        _ => {
            policy.idempotency = OperationIdempotency::Unknown;
            policy.concurrency = ToolConcurrency::Resource("http".into());
        }
    }
}

/// Static policy for a root tool whose attributes come from its name.
pub(crate) fn root_policy(
    name: &str,
    risk_level: RiskLevel,
    idempotency: OperationIdempotency,
    scope: ToolOperationScope,
    concurrency: ToolConcurrency,
) -> OperationPolicy {
    let attributes = crate::tool_contract::operation_attributes(name, concurrency.clone());
    OperationPolicy {
        risk_level,
        capability: name.into(),
        confirmation: crate::tool_contract::confirmation_for(
            risk_level,
            matches!(attributes.effect, crate::OperationEffect::ReadOnly),
        ),
        idempotency,
        scope,
        concurrency,
        effect: attributes.effect,
        data_sensitivity: attributes.data_sensitivity,
        network_access: attributes.network_access,
    }
}

/// Root-tool spec whose manifest matches the historical default projection.
pub(crate) fn root_operation_spec(
    name: &'static str,
    description: &'static str,
    schema: Value,
    policy: OperationPolicy,
    catalog_group: ToolCatalogGroup,
    policy_rule: Option<OperationPolicyRule>,
) -> OperationSpec {
    let represented = crate::tool_contract::display_source_for_name(name);
    let root = crate::tool_contract::default_tool_root(name, represented);
    let operation = crate::tool_contract::default_tool_operation(name, &root, represented);
    let source = crate::tool_contract::tool_source_for_name(name);
    let root_presentation = crate::tool_contract::default_root_presentation(&root, represented);
    OperationSpec {
        name: name.into(),
        description: description.into(),
        fixed: Vec::new(),
        schema,
        policy,
        policy_rule,
        catalog_group,
        presentation: ToolPresentation {
            label: crate::tool_contract::default_tool_label(name),
            renderer: root.clone(),
            icon: "tools".into(),
            represented_source: represented,
        },
        prompt: ToolPrompt {
            when_to_use: description.into(),
            when_not_to_use: "Use a narrower operation when one is available.".into(),
            key_operations: vec![name.into()],
        },
        identity: Some(OperationIdentity {
            source,
            root: root.into(),
            operation: operation.map(Cow::from),
            root_presentation,
            availability: ToolAvailability::default(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn root_tool_spec(
    name: &'static str,
    description: &'static str,
    schema: Value,
    risk_level: RiskLevel,
    idempotency: OperationIdempotency,
    scope: ToolOperationScope,
    concurrency: ToolConcurrency,
    catalog_group: ToolCatalogGroup,
    policy_rule: Option<OperationPolicyRule>,
) -> OperationSpec {
    root_operation_spec(
        name,
        description,
        schema,
        root_policy(name, risk_level, idempotency, scope, concurrency),
        catalog_group,
        policy_rule,
    )
}

impl OperationViewTool {
    pub(crate) fn new(handler: ToolHandle, mut spec: OperationSpec) -> Arc<Self> {
        annotate_schema(&mut spec);
        let fixed = Map::from_iter(spec.fixed.iter().cloned());
        Arc::new(Self {
            handler,
            spec,
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
fn annotate_schema(spec: &mut OperationSpec) {
    close_object_schemas(&mut spec.schema);
    let Some(schema) = spec.schema.as_object_mut() else {
        return;
    };
    // Provider function parameters require an object root. A selected
    // operation branch may omit `type` even when its aggregate schema has
    // `type: object` at the root, so restore the invariant after projection.
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("title".into(), Value::String(spec.name.to_string()));
    schema.insert(
        "description".into(),
        Value::String(spec.description.to_string()),
    );
}

/// Operation branches are the provider boundary. Some aggregate schemas use
/// a nested `oneOf` and omit `additionalProperties: false` on those inner
/// branches; once a branch is split into a view, accepting an extra field
/// would silently discard caller input before the aggregate receives it.
fn close_object_schemas(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if object.get("properties").is_some() {
        object.insert("additionalProperties".into(), Value::Bool(false));
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for branch in branches {
                close_object_schemas(branch);
            }
        }
    }
}

/// Extract one operation branch from an aggregate tool schema and remove the
/// fixed discriminator from the provider-facing input. The aggregate remains
/// the execution boundary; this helper only creates a narrow model view.
pub(crate) fn split_operation_schema(schema: &Value, operation: &str) -> Option<Value> {
    let mut branches = Vec::new();
    collect_operation_branches(schema, operation, &mut branches, &Map::new());
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
    collect_scope_operation_branches(schema, scope, operation, &mut branches, &Map::new());
    match branches.len() {
        0 => None,
        1 => branches.into_iter().next(),
        _ => Some(serde_json::json!({
            "type": "object",
            "oneOf": branches,
        })),
    }
}

fn collect_operation_branches(
    node: &Value,
    operation: &str,
    out: &mut Vec<Value>,
    inherited_properties: &Map<String, Value>,
) {
    let Some(object) = node.as_object() else {
        return;
    };
    let mut properties = inherited_properties.clone();
    if let Some(local_properties) = object.get("properties").and_then(Value::as_object) {
        properties.extend(local_properties.clone());
    }
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for branch in one_of {
            collect_operation_branches(branch, operation, out, &properties);
        }
        return;
    }
    let Some(operation_schema) = properties.get("operation") else {
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
    if !properties.is_empty() {
        branch["properties"] = Value::Object(properties);
    }
    remove_fixed_property(&mut branch, "operation");
    out.push(branch);
}

fn collect_scope_operation_branches(
    node: &Value,
    scope: &str,
    operation: &str,
    out: &mut Vec<Value>,
    inherited_properties: &Map<String, Value>,
) {
    let Some(object) = node.as_object() else {
        return;
    };
    let mut properties = inherited_properties.clone();
    if let Some(local_properties) = object.get("properties").and_then(Value::as_object) {
        properties.extend(local_properties.clone());
    }
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for branch in one_of {
            collect_scope_operation_branches(branch, scope, operation, out, &properties);
        }
        return;
    }
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
        if !properties.is_empty() {
            branch["properties"] = Value::Object(properties);
        }
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

    fn operation_spec(&self) -> Option<OperationSpec> {
        Some(self.spec.clone())
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        self.spec.policy_for(input).risk_level
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        self.handler.timeout_outcome()
    }

    fn default_max_retries(&self) -> u32 {
        self.handler.default_max_retries()
    }

    fn default_retry_backoff_secs(&self) -> u64 {
        self.handler.default_retry_backoff_secs()
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        // ToolsFacade validates the model-facing arguments, then injects
        // trusted execution metadata such as `_session_id` before calling the
        // view. Validate the public shape again without those private fields;
        // the original input is retained for routing into the aggregate
        // implementation, which consumes the trusted metadata.
        let mut validation_input = input.clone();
        crate::tool_contract::strip_private_tool_fields(&mut validation_input);
        if let Err(error) = self.validate_input(&validation_input) {
            return Err(anyhow::Error::new(StructuredToolError::new(
                error.to_string(),
                ToolErrorMetadata::validation(),
            )));
        }
        self.handler
            .execute(self.routed_input(&input), cancel)
            .await
    }

    fn input_schema(&self) -> Value {
        self.spec.schema.clone()
    }

    fn tool_def(&self) -> ToolDef {
        // The default tool_def does not copy ToolDef.prompt. Views still need
        // that prompt, and its risk is the empty-input policy rather than the
        // catalog upper bound.
        let empty = Value::Object(Map::new());
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.risk_level(&empty),
        )
        .with_retry_safety(self.idempotency(&empty).tool_retry_safety())
        .with_catalog_group(self.catalog_group())
        .with_prompt(self.spec.prompt.clone())
        .with_manifest(self.tool_manifest())
    }

    fn default_timeout_secs(&self) -> u64 {
        self.handler.default_timeout_secs()
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        self.handler.timeout_secs_for(&self.routed_input(input))
    }

    fn requires_session_id(&self) -> bool {
        self.handler.requires_session_id()
    }

    fn supports_live_output(&self) -> bool {
        self.handler.supports_live_output()
    }

    fn signals(&self, output: &Value) -> ToolSignals {
        self.handler.signals(output)
    }

    fn registrations(&self, output: &Value) -> Vec<ToolRegistration> {
        self.handler.registrations(output)
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
        let aggregate_schema = json!({
            "type": "object",
            "oneOf": [{
                "additionalProperties": false,
                "properties": {"operation": {"const": "read"}, "path": {"type": "string"}},
                "required": ["operation", "path"]
            }]
        });
        let projected_schema =
            split_operation_schema(&aggregate_schema, "read").expect("read branch");
        let inner: ToolHandle = Arc::new(crate::tool_contract::tests::MockTool::with_schema(
            "files",
            aggregate_schema,
        ));
        let view = OperationViewTool::new(
            inner,
            OperationSpec {
                name: "files.read".into(),
                description: "Read text.".into(),
                fixed: vec![("operation".into(), json!("read"))],
                schema: projected_schema,
                policy: OperationPolicy {
                    risk_level: RiskLevel::Low,
                    capability: "files.read".into(),
                    confirmation: ConfirmationRequirement::SecurityPolicy,
                    idempotency: OperationIdempotency::Idempotent,
                    scope: ToolOperationScope::Session,
                    concurrency: ToolConcurrency::ReadOnly,
                    effect: crate::OperationEffect::ReadOnly,
                    data_sensitivity: crate::DataSensitivity::UserData,
                    network_access: crate::NetworkAccess::None,
                },
                policy_rule: None,
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
                identity: None,
            },
        );

        assert_eq!(view.input_schema()["title"], "files.read");
        assert_eq!(view.input_schema()["description"], "Read text.");
        assert_eq!(view.input_schema()["type"], "object");
        let def = view.tool_def();
        assert_eq!(def.prompt.as_ref().unwrap().key_operations, ["files.read"]);
        let manifest = view.tool_manifest();
        assert_eq!(manifest.identity.stable_name, "files.read");
        assert_eq!(manifest.presentation.renderer, "files");
        assert_eq!(manifest.presentation.label, "读取文件");
        assert_eq!(manifest.policy.permission_key, "files.read");
        assert_eq!(manifest.policy.effect, crate::OperationEffect::ReadOnly);
        assert_eq!(
            manifest.policy.concurrency,
            crate::ToolConcurrencyMode::ReadOnly
        );
        assert_eq!(
            manifest.policy.data_sensitivity,
            crate::DataSensitivity::UserData
        );
        assert_eq!(manifest.model.description, "Read text.");
        assert_eq!(manifest.root_presentation.label, "文件");
        assert_eq!(
            view.tool_manifest(),
            view.spec.manifest(view.input_schema())
        );
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
