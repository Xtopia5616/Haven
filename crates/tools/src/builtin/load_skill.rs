use async_trait::async_trait;
use haven_common::tools::{ToolCatalogGroup, ToolDef, ToolSource};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;

use crate::registry::{DeferredToolCatalog, SessionCatalog};
use crate::{Tool, ToolRegistry, ToolResult};

/// Activates enabled Skill adapters for one session. Skill discovery remains
/// global and cheap; the executable adapter and schema are session-local only
/// after this loader succeeds.
pub struct LoadSkillTool {
    pub deferred_catalog: DeferredToolCatalog,
    pub registry: ToolRegistry,
    pub session_catalog: SessionCatalog,
    pub max_tools_per_request: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LoadSkillParams {
    pub skill_names: Vec<String>,
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl LoadSkillTool {
    pub async fn run(
        &self,
        params: LoadSkillParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("cancelled"));
        }
        let session_id = params
            .session_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("session context required to load Skills"))?;
        let names = normalize_names(params.skill_names);
        if names.is_empty() {
            anyhow::bail!("skill_names must contain at least one Skill name");
        }

        let requested_names: HashSet<_> = names.iter().map(|name| qualified_name(name)).collect();
        let available: Vec<_> = self
            .deferred_catalog
            .list()
            .await
            .into_iter()
            .filter(|tool| is_skill(&tool.tool_def()))
            .collect();
        let selected: Vec<_> = available
            .iter()
            .filter(|tool| requested_names.contains(&tool.name()))
            .cloned()
            .collect();
        let selected_names: HashSet<_> = selected.iter().map(|tool| tool.name()).collect();
        let missing: Vec<_> = names
            .iter()
            .filter(|name| !selected_names.contains(&qualified_name(name)))
            .cloned()
            .collect();
        if selected.is_empty() {
            anyhow::bail!(
                "no matching enabled Skill; available Skills: {}",
                available
                    .iter()
                    .map(|tool| tool.name().trim_start_matches("skill__").to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let global_count = self.registry.list().await.len();
        let max = self.max_tools_per_request.max(1);
        match self
            .session_catalog
            .register_many_if_within_budget(&session_id, global_count, max, selected.clone())
            .await
        {
            Ok(loaded) => {
                let mut output = serde_json::json!({
                    "status": if loaded.is_empty() { "already_loaded" } else { "loaded" },
                    "skills": loaded
                        .iter()
                        .map(|name| name.trim_start_matches("skill__"))
                        .collect::<Vec<_>>(),
                });
                if !missing.is_empty() {
                    output["missing_skills"] = serde_json::json!(missing);
                }
                Ok(ToolResult::ok(output))
            }
            Err(net_new) => {
                let session_count = self.session_catalog.list_defs(&session_id).await.len();
                let remaining = max.saturating_sub(global_count.saturating_add(session_count));
                Ok(ToolResult::ok(serde_json::json!({
                    "status": "needs_selection",
                    "reason": format!(
                        "Loading these {} Skills would exceed the per-request limit of {}. Choose at most {} Skill(s).",
                        net_new, max, remaining
                    ),
                    "available_skills": selected
                        .iter()
                        .map(|tool| {
                            serde_json::json!({
                                "name": tool.name().trim_start_matches("skill__"),
                                "description": truncate_description(&tool.description()),
                            })
                        })
                        .collect::<Vec<_>>(),
                    "remaining_budget": remaining,
                    "max_tools_per_request": max,
                })))
            }
        }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> String {
        "load_skill".into()
    }

    fn description(&self) -> String {
        crate::prompts::LOAD_SKILL_DESCRIPTION.into()
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
                "skill_names": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "uniqueItems": true,
                    "items": { "type": "string", "minLength": 1, "maxLength": 128 },
                    "description": "Names from the available Skills index"
                }
            },
            "required": ["skill_names"]
        })
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<LoadSkillParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

fn normalize_names(values: Vec<String>) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let value = value.trim().trim_start_matches("skill__");
        if !value.is_empty() && seen.insert(value.to_string()) {
            names.push(value.to_string());
        }
    }
    names
}

fn qualified_name(name: &str) -> String {
    format!("skill__{name}")
}

fn is_skill(def: &ToolDef) -> bool {
    def.manifest
        .as_ref()
        .map(|manifest| manifest.identity.source == ToolSource::Skill)
        .unwrap_or_else(|| def.name.starts_with("skill__"))
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
    use crate::{ToolBox, ToolRegistry};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct SkillStub;

    #[async_trait]
    impl crate::Tool for SkillStub {
        fn name(&self) -> String {
            "skill__demo".into()
        }

        fn description(&self) -> String {
            "demo Skill".into()
        }

        fn risk_level(&self, _input: &Value) -> RiskLevel {
            RiskLevel::High
        }

        fn input_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[test]
    fn normalize_accepts_display_and_provider_skill_names() {
        assert_eq!(
            normalize_names(vec!["echo".into(), "skill__echo".into()]),
            ["echo"]
        );
        assert_eq!(qualified_name("echo"), "skill__echo");
    }

    #[tokio::test]
    async fn loader_registers_only_the_selected_skill() {
        let deferred_catalog = DeferredToolCatalog::new();
        let tool: ToolBox = Arc::new(SkillStub);
        deferred_catalog.replace(vec![tool]).await;
        let loader = LoadSkillTool {
            deferred_catalog,
            registry: ToolRegistry::new(),
            session_catalog: SessionCatalog::new(),
            max_tools_per_request: 4,
        };

        let result = loader
            .run(
                LoadSkillParams {
                    skill_names: vec!["demo".into()],
                    session_id: Some("ses-skill".into()),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["status"], "loaded");
        assert_eq!(result.output["skills"], serde_json::json!(["demo"]));
        assert_eq!(
            loader.session_catalog.list_defs("ses-skill").await[0].name,
            "skill__demo"
        );
    }
}
