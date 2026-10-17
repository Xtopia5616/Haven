use async_trait::async_trait;
use haven_common::tools::{ToolCatalogGroup, ToolDef, ToolSource};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;

use super::name_list::ordered_unique_non_empty_names;
use crate::registry::{DeferredToolCatalog, SessionToolOverlay};
use crate::{Tool, ToolRegistry, ToolResult};
use haven_skills::SkillRegistry;

/// Activates enabled Skill adapters for one session. Skill discovery remains
/// global and cheap; the executable adapter and schema are session-local only
/// after this loader succeeds.
pub struct LoadSkillTool {
    pub deferred_catalog: DeferredToolCatalog,
    pub registry: ToolRegistry,
    pub skill_registry: SkillRegistry,
    pub session_tool_overlay: SessionToolOverlay,
    pub max_tools_per_request: usize,
    pub max_observation_chars: usize,
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
        let names = normalize_skill_names(params.skill_names);
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
        let selected_tools: Vec<_> = available
            .iter()
            .filter(|tool| requested_names.contains(&tool.name()))
            .cloned()
            .collect();
        let selected_names: HashSet<_> = selected_tools.iter().map(|tool| tool.name()).collect();
        let missing: Vec<_> = names
            .iter()
            .filter(|name| !selected_names.contains(&qualified_name(name)))
            .cloned()
            .collect();
        if selected_tools.is_empty() {
            anyhow::bail!(
                "no matching enabled executable Skill; available Skills: {}",
                available
                    .iter()
                    .map(|tool| tool.name().trim_start_matches("skill__").to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // Resolve the complete selected bodies before registering any adapter.
        // The result is persisted as the loader observation, so it must fit the
        // same bounded context budget as every other tool observation.
        let mut selected_skills = Vec::with_capacity(selected_tools.len());
        for name in &names {
            if selected_names.contains(&qualified_name(name)) {
                selected_skills.push(self.skill_registry.get_executable_skill(name).await?);
            }
        }

        let skill_instructions = selected_skills
            .iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name(),
                    "content": skill.instructions(),
                })
            })
            .collect::<Vec<_>>();
        let instructions_output = serde_json::json!({
            // Use the longest possible status value when checking the budget.
            "status": "already_loaded",
            "skills": selected_skills.iter().map(|skill| skill.name()).collect::<Vec<_>>(),
            "skill_instructions": skill_instructions.clone(),
            "missing_skills": missing.clone(),
        });
        let output_chars = serde_json::to_string(&instructions_output)
            .map(|output| output.chars().count())
            .unwrap_or(usize::MAX);
        if output_chars > self.max_observation_chars {
            return Ok(ToolResult::ok(serde_json::json!({
                "status": "needs_selection",
                "reason": "The selected Skills' instructions exceed the tool observation budget. Select fewer Skills or shorten their SKILL.md bodies; no Skill was loaded.",
                "available_skills": selected_skills
                    .iter()
                    .map(|skill| {
                        serde_json::json!({
                            "name": skill.name(),
                            "description": truncate_description(skill.description()),
                        })
                    })
                    .collect::<Vec<_>>(),
                "max_observation_chars": self.max_observation_chars,
            })));
        }

        let global_count = self.registry.list().await.len();
        let max = self.max_tools_per_request.max(1);
        match self
            .session_tool_overlay
            .register_many_if_within_budget(&session_id, global_count, max, selected_tools.clone())
            .await
        {
            Ok(loaded) => {
                let mut output = serde_json::json!({
                    "status": if loaded.is_empty() { "already_loaded" } else { "loaded" },
                    "skills": selected_skills.iter().map(|skill| skill.name()).collect::<Vec<_>>(),
                    "skill_instructions": skill_instructions,
                });
                if !missing.is_empty() {
                    output["missing_skills"] = serde_json::json!(missing);
                }
                Ok(ToolResult::ok(output))
            }
            Err(budget) => {
                let remaining = budget
                    .max
                    .saturating_sub(budget.global_count.saturating_add(budget.session_count));
                Ok(ToolResult::ok(serde_json::json!({
                    "status": "needs_selection",
                    "reason": format!(
                        "Loading these {} Skills would exceed the per-request limit of {}. Choose at most {} Skill(s).",
                        budget.net_new, budget.max, remaining
                    ),
                    "available_skills": selected_tools
                        .iter()
                        .map(|tool| {
                            serde_json::json!({
                                "name": tool.name().trim_start_matches("skill__"),
                                "description": truncate_description(&tool.description()),
                            })
                        })
                        .collect::<Vec<_>>(),
                    "remaining_budget": remaining,
                    "max_tools_per_request": budget.max,
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

    fn operation_spec(&self) -> Option<crate::operation_view::OperationSpec> {
        Some(crate::operation_view::root_tool_spec(
            "load_skill",
            crate::prompts::LOAD_SKILL_DESCRIPTION,
            self.input_schema(),
            RiskLevel::Safe,
            crate::OperationIdempotency::Unknown,
            crate::ToolOperationScope::Session,
            crate::ToolConcurrency::Exclusive,
            ToolCatalogGroup::Haven,
            None,
        ))
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

fn normalize_skill_names(values: Vec<String>) -> Vec<String> {
    ordered_unique_non_empty_names(
        values
            .into_iter()
            .map(|value| value.trim().trim_start_matches("skill__").to_string()),
    )
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
    use crate::{ToolHandle, ToolRegistry};
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
            normalize_skill_names(vec!["echo".into(), "skill__echo".into()]),
            ["echo"]
        );
        assert_eq!(qualified_name("echo"), "skill__echo");
    }

    #[tokio::test]
    async fn loader_registers_only_the_selected_skill() {
        let deferred_catalog = DeferredToolCatalog::new();
        let tool: ToolHandle = Arc::new(SkillStub);
        deferred_catalog.replace(vec![tool]).await;
        let skills_root = tempfile::tempdir().expect("temporary Skills root");
        let demo_root = skills_root.path().join("demo");
        std::fs::create_dir_all(demo_root.join("scripts")).expect("create Skill fixture");
        std::fs::write(
            demo_root.join("SKILL.md"),
            "---\nname: demo\ndescription: Demo Skill\n---\n\nRun the skill.\n",
        )
        .expect("write Skill manifest");
        std::fs::write(demo_root.join("scripts/main.py"), "print('{}')\n")
            .expect("write Skill entry script");
        let skill_registry = SkillRegistry::new();
        skill_registry
            .set_config(
                Some(skills_root.path().to_path_buf()),
                Some(vec!["demo".into()]),
            )
            .await
            .expect("load Skill fixture");
        let loader = LoadSkillTool {
            deferred_catalog,
            registry: ToolRegistry::new(),
            skill_registry,
            session_tool_overlay: SessionToolOverlay::new(),
            max_tools_per_request: 4,
            max_observation_chars: 16_000,
        };

        let result = loader
            .run(
                LoadSkillParams {
                    skill_names: vec!["demo".into()],
                    session_id: Some("ses-2e946a977a2c0524b9f157354337a7d8".into()),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["status"], "loaded");
        assert_eq!(result.output["skills"], serde_json::json!(["demo"]));
        assert_eq!(
            result.output["skill_instructions"][0]["content"],
            "Run the skill."
        );
        assert_eq!(
            loader
                .session_tool_overlay
                .list_tool_definitions("ses-2e946a977a2c0524b9f157354337a7d8")
                .await[0]
                .name,
            "skill__demo"
        );
    }
}
