use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::skills::SkillsEngine;
use crate::{Tool, ToolResult};
use crate::skills::runner::SkillRunner;

pub struct LoadSkillTool {
    pub skills_engine: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> String {
        "load_skill".into()
    }
    fn description(&self) -> String {
        "Load a skill's full tool schema by name. Use this when you need to access a skill listed in the Skill Index.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": { "type": "string", "description": "The name of the skill to load" }
            },
            "required": ["skill_name"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
        let skill_name = input["skill_name"].as_str().unwrap_or("");
        if skill_name.is_empty() {
            anyhow::bail!("skill_name is required");
        }
        let skill = self
            .skills_engine
            .get_skill(skill_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("skill '{}' not found", skill_name))?;
        if !skill.enabled() {
            anyhow::bail!("skill '{}' is disabled", skill_name);
        }
        let schema = serde_json::json!({
            "name": format!("skill::{}", skill.name()),
            "description": skill.description(),
            "input_schema": {
                "type": "object",
                "properties": {
                    "params": {
                        "type": "object",
                        "description": skill.instructions()
                    }
                },
                "required": ["params"]
            }
        });

        Ok(ToolResult::ok(serde_json::json!({"skill": schema, "status": "loaded", "skill_name": skill.name()})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::venv::VenvManager;
    use crate::Tool;
    use haven_common::config::SkillsExecConfig;
    use serde_json::json;
    use tempfile::TempDir;

    async fn make_engine_with_skill(enabled: bool) -> (SkillsEngine, Arc<RwLock<SkillRunner>>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let skill_dir = dir.path().join("echo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
        )
        .unwrap();
        let exec_config = SkillsExecConfig {
            venv_root: dir.path().to_path_buf(),
            work_dir: dir.path().to_path_buf(),
            timeout_secs: 30,
            max_output_lines: 5000,
            cpu_time_secs: None,
            max_memory_mb: None,
        };
        let venv_mgr = VenvManager::new(dir.path().to_path_buf());
        let skill_runner = Arc::new(RwLock::new(SkillRunner::new(venv_mgr, exec_config)));
        let engine = SkillsEngine::new();
        // `enabled` flag: Some(["echo"]) enables only echo; Some([]) disables all.
        let filter = if enabled { Some(vec!["echo".to_string()]) } else { Some(vec![]) };
        engine.set_config(Some(skill_dir.parent().unwrap().to_path_buf()), filter).await.unwrap();
        (engine, skill_runner, dir)
    }

    #[test]
    fn test_load_skill_name() {
        let skills_engine = SkillsEngine::new();
        let temp_dir = TempDir::new().unwrap();
        let exec_config = SkillsExecConfig {
            venv_root: temp_dir.path().to_path_buf(),
            work_dir: temp_dir.path().to_path_buf(),
            timeout_secs: 30,
            max_output_lines: 5000,
            cpu_time_secs: None,
            max_memory_mb: None,
        };
        let venv_mgr = VenvManager::new(temp_dir.path().to_path_buf());
        let skill_runner = Arc::new(RwLock::new(SkillRunner::new(venv_mgr, exec_config)));

        let tool = LoadSkillTool {
            skills_engine,
            skill_runner,
        };
        assert_eq!(tool.name(), "load_skill");
    }

    #[tokio::test]
    async fn test_load_skill_rejects_disabled() {
        let (engine, runner, _dir) = make_engine_with_skill(false).await;
        let tool = LoadSkillTool {
            skills_engine: engine,
            skill_runner: runner,
        };
        let result = tool
            .execute(json!({"skill_name": "echo"}), CancellationToken::new())
            .await;
        assert!(result.is_err(), "disabled skill should be rejected");
        assert!(result.unwrap_err().to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn test_load_skill_loads_enabled() {
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let tool = LoadSkillTool {
            skills_engine: engine,
            skill_runner: runner,
        };
        let result = tool
            .execute(json!({"skill_name": "echo"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["skill"]["name"], "skill::echo");
    }

    #[tokio::test]
    async fn test_load_skill_rejects_unknown() {
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let tool = LoadSkillTool {
            skills_engine: engine,
            skill_runner: runner,
        };
        let result = tool
            .execute(json!({"skill_name": "nope"}), CancellationToken::new())
            .await;
        assert!(result.is_err(), "unknown skill should be rejected");
    }
}
