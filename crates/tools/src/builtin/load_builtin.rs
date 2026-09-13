use async_trait::async_trait;
use haven_common::tools::{ToolCatalogGroup, ToolDef, ToolSource};
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::registry::{DeferredToolCatalog, SessionCatalog};
use crate::{Tool, ToolBox, ToolRegistry, ToolResult};

/// Loads selected built-in operation views into one session. The executable
/// implementations stay host-owned in [`DeferredToolCatalog`]; only the
/// selected views enter the provider-facing `tools[]` list.
pub struct LoadBuiltinTool {
    pub deferred_catalog: DeferredToolCatalog,
    pub registry: ToolRegistry,
    pub session_catalog: SessionCatalog,
    pub max_tools_per_request: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LoadBuiltinParams {
    /// Exact dotted operation names such as `files.read` or `system.info`.
    #[serde(default)]
    pub operations: Option<Vec<String>>,
    /// Built-in roots such as `files`, `window`, or `media`.
    #[serde(default)]
    pub roots: Option<Vec<String>>,
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl LoadBuiltinTool {
    pub async fn run(
        &self,
        params: LoadBuiltinParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("cancelled"));
        }
        let session_id = params
            .session_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("session context required to load built-in tools"))?;
        let operations = normalize_names(params.operations);
        let roots = normalize_names(params.roots);
        if operations.is_empty() && roots.is_empty() {
            anyhow::bail!("provide at least one operation or root to load built-in tools");
        }

        let deferred = self.deferred_catalog.list().await;
        let builtin: Vec<_> = deferred
            .into_iter()
            .filter(|tool| is_builtin(&tool.tool_def()))
            .collect();
        let requested: Vec<_> = builtin
            .iter()
            .filter(|tool| {
                let name = tool.name();
                let root = tool_root(&name);
                (operations.is_empty() || operations.iter().any(|value| value == &name))
                    || (roots.iter().any(|value| value == root))
            })
            .cloned()
            .collect();
        let requested_names: std::collections::HashSet<_> =
            requested.iter().map(|tool| tool.name()).collect();
        let missing_operations: Vec<_> = operations
            .iter()
            .filter(|name| !requested_names.contains(*name))
            .cloned()
            .collect();
        if requested.is_empty() {
            anyhow::bail!(
                "no matching enabled built-in operation; available deferred operations: {}",
                builtin
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let global_count = self.registry.list().await.len();
        let max = self.max_tools_per_request.max(1);
        match self
            .session_catalog
            .register_many_if_within_budget(&session_id, global_count, max, requested.clone())
            .await
        {
            Ok(loaded) => {
                let mut output = serde_json::json!({
                    "status": if loaded.is_empty() { "already_loaded" } else { "loaded" },
                    "operations": loaded,
                    "available_now": requested.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
                });
                if !missing_operations.is_empty() {
                    output["missing_operations"] = serde_json::json!(missing_operations);
                }
                Ok(ToolResult::ok(output))
            }
            Err(net_new) => {
                let session_count = self.session_catalog.list_defs(&session_id).await.len();
                let remaining = max.saturating_sub(global_count.saturating_add(session_count));
                Ok(ToolResult::ok(serde_json::json!({
                    "status": "needs_selection",
                    "reason": format!(
                        "Loading these {} built-in operations would exceed the per-request limit of {}. Choose at most {} operation(s).",
                        net_new, max, remaining
                    ),
                    "available_operations": compact_entries(&requested),
                    "remaining_budget": remaining,
                    "max_tools_per_request": max,
                })))
            }
        }
    }
}

#[async_trait]
impl Tool for LoadBuiltinTool {
    fn name(&self) -> String {
        "load_builtin".into()
    }

    fn description(&self) -> String {
        crate::prompts::LOAD_BUILTIN_DESCRIPTION.into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Haven
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 64,
                    "uniqueItems": true,
                    "items": { "type": "string", "minLength": 1, "maxLength": 128 },
                    "description": "Exact dotted built-in operation names to load"
                },
                "roots": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "uniqueItems": true,
                    "items": { "type": "string", "minLength": 1, "maxLength": 64 },
                    "description": "Built-in roots to load when the whole root is needed"
                }
            },
            "anyOf": [
                { "required": ["operations"] },
                { "required": ["roots"] }
            ]
        })
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<LoadBuiltinParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

fn normalize_names(values: Option<Vec<String>>) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for value in values.into_iter().flatten() {
        let value = value.trim();
        if !value.is_empty() && seen.insert(value.to_string()) {
            names.push(value.to_string());
        }
    }
    names
}

fn is_builtin(def: &ToolDef) -> bool {
    def.manifest
        .as_ref()
        .map(|manifest| manifest.identity.source == ToolSource::Builtin)
        .unwrap_or_else(|| !def.name.starts_with("skill__") && !def.name.starts_with("mcp__"))
}

fn tool_root(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

fn compact_entries(tools: &[ToolBox]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name(),
                "description": truncate_description(&tool.description()),
            })
        })
        .collect()
}

fn truncate_description(description: &str) -> String {
    let description = description.trim();
    if description.chars().count() <= 160 {
        return description.into();
    }
    format!("{}...", description.chars().take(157).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::notify::NotifyTool;
    use haven_common::tools::ToolDef;
    use std::sync::Arc;

    #[test]
    fn builtin_filter_excludes_extensions() {
        let builtin = ToolDef::new("files.read", "read", serde_json::json!({}), RiskLevel::Low);
        let skill = ToolDef::new(
            "skill__echo",
            "echo",
            serde_json::json!({}),
            RiskLevel::High,
        );
        assert!(is_builtin(&builtin));
        assert!(!is_builtin(&skill));
    }

    #[tokio::test]
    async fn session_registration_is_atomic_when_budget_is_too_small() {
        let catalog = SessionCatalog::new();
        let tool = Arc::new(NotifyTool) as ToolBox;
        let result = catalog
            .register_many_if_within_budget("ses-test", 1, 1, vec![tool])
            .await;
        assert_eq!(result, Err(1));
        assert!(catalog.list_defs("ses-test").await.is_empty());
    }
}
