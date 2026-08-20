use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::skill_runner::SkillRunner;
use crate::{SkillToolAdapter, Tool, ToolBox, ToolRegistry, ToolResult, ToolsManager};
use haven_skills::SkillsEngine;

pub struct LoadSkillTool {
    pub skills_engine: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub registry: ToolRegistry,
    pub session_registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>>,
    pub catalog_version: Arc<AtomicU64>,
    pub max_tools_per_request: usize,
}

/// Typed parameters for `LoadSkillTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `LoadSkillTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LoadSkillParams {
    /// The name of the skill to load.
    pub skill_name: String,
    /// Injected privately by ToolsManager when `requires_session_id` is set.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl LoadSkillTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: LoadSkillParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let skill_name = params.skill_name;
        if skill_name.is_empty() {
            anyhow::bail!("skill_name is required");
        }
        let session_id = params.session_id.filter(|s| !s.is_empty()).ok_or_else(|| {
            anyhow::anyhow!("session context required to load skill '{}'", skill_name)
        })?;
        let skill = self
            .skills_engine
            .get_skill(&skill_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("skill '{}' not found", skill_name))?;
        if !skill.enabled() {
            anyhow::bail!("skill '{}' is disabled", skill_name);
        }
        // The schema comes from the SkillToolAdapter's tool_def() so the
        // name / description / input_schema advertised to the model are the
        // exact ones the registered session adapter validates and executes.
        // Instructions are surfaced alongside (not inside the schema) so the
        // model still learns how to fill `params` at load time.
        let skill_instructions = skill.instructions().to_string();
        let skill_display_name = skill.name().to_string();
        let runner = self.skill_runner.read().await.clone();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
        let skill_def = self
            .activate_skill(&session_id, adapter)
            .await?;

        Ok(ToolResult::ok(serde_json::json!({
            "skill": skill_def,
            "instructions": skill_instructions,
            "status": "loaded",
            "skill_name": skill_display_name,
        })))
    }

    /// Atomically budget-check + register under the session write lock.
    async fn activate_skill(
        &self,
        session_id: &str,
        adapter: SkillToolAdapter,
    ) -> anyhow::Result<Value> {
        let max = self.max_tools_per_request.max(1);
        let global_count = self.registry.list().await.len();
        let name = adapter.name();
        let skill_def = adapter.tool_def().json();
        let mut map = self.session_registrations.write().await;
        let entry = map.entry(session_id.to_string()).or_default();
        let net_new = if entry.contains_key(&name) { 0 } else { 1 };
        if ToolsManager::tool_budget_would_exceed(max, global_count, entry.len(), net_new) {
            anyhow::bail!(
                "Cannot load skill '{}': adding 1 tool would exceed the per-request limit of {} (currently {} tools: {} builtin + {} session). Prefer unloading unused MCP servers, start a new session, or raise context_limits.max_tools_per_request.",
                name,
                max,
                global_count.saturating_add(entry.len()),
                global_count,
                entry.len()
            );
        }
        entry.insert(name, Arc::new(adapter));
        drop(map);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        Ok(skill_def)
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> String {
        "load_skill".into()
    }
    fn description(&self) -> String {
        "Load a skill's tools by skill name, activating them for this session. Use the raw skill name shown in the skills list, not a `skill__`-prefixed tool name. Prefer this over weaker built-in tools when the skill fits the session.".into()
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

    fn requires_session_id(&self) -> bool {
        true
    }

    /// Entry ②: LLM JSON entry — convert/validate into `LoadSkillParams`,
    /// then land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<LoadSkillParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }

    /// Registration is performed atomically inside `run` before success is
    /// returned. Resume restores from history via `register_skill_for_session`.
    fn registrations(&self, _output: &Value) -> Vec<crate::tool::ToolRegistration> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use haven_common::config::SkillsExecConfig;
    use haven_skills::VenvManager;
    use serde_json::json;
    use tempfile::TempDir;

    async fn make_engine_with_skill(
        enabled: bool,
    ) -> (SkillsEngine, Arc<RwLock<SkillRunner>>, tempfile::TempDir) {
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
        let filter = if enabled {
            Some(vec!["echo".to_string()])
        } else {
            Some(vec![])
        };
        engine
            .set_config(Some(skill_dir.parent().unwrap().to_path_buf()), filter)
            .await
            .unwrap();
        (engine, skill_runner, dir)
    }

    fn wrap(
        engine: SkillsEngine,
        runner: Arc<RwLock<SkillRunner>>,
        max_tools: usize,
    ) -> LoadSkillTool {
        LoadSkillTool {
            skills_engine: engine,
            skill_runner: runner,
            registry: ToolRegistry::new(),
            session_registrations: Arc::new(RwLock::new(HashMap::new())),
            catalog_version: Arc::new(AtomicU64::new(0)),
            max_tools_per_request: max_tools,
        }
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

        let tool = wrap(skills_engine, skill_runner, 350);
        assert_eq!(tool.name(), "load_skill");
    }

    #[tokio::test]
    async fn test_load_skill_rejects_disabled() {
        let (engine, runner, _dir) = make_engine_with_skill(false).await;
        let tool = wrap(engine, runner, 350);
        let result = tool
            .execute(
                json!({"skill_name": "echo", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "disabled skill should be rejected");
        assert!(result.unwrap_err().to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn test_load_skill_loads_enabled() {
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let tool = wrap(engine, runner, 350);
        let result = tool
            .execute(
                json!({"skill_name": "echo", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["skill"]["name"], "skill__echo");
        let regs = tool.session_registrations.read().await;
        assert!(regs.get("ses-x").unwrap().contains_key("skill__echo"));
    }

    #[tokio::test]
    async fn test_load_skill_schema_matches_adapter() {
        // The `skill` schema advertised to the model must be exactly the
        // SkillToolAdapter's tool_def() — same name, description and
        // input_schema — so the loaded tool the model sees is the one the
        // session adapter validates and executes. Instructions ride along as
        // a separate field instead of a second schema implementation.
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let engine_ref = engine.clone();
        let tool = wrap(engine, runner.clone(), 350);
        let result = tool
            .execute(
                json!({"skill_name": "echo", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let skill = engine_ref.get_skill("echo").await.unwrap();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner.read().await.clone());
        assert_eq!(result.output["skill"], adapter.tool_def().json());
        assert_eq!(result.output["instructions"], "do echo");
    }

    #[tokio::test]
    async fn test_load_skill_rejects_unknown() {
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let tool = wrap(engine, runner, 350);
        let result = tool
            .execute(
                json!({"skill_name": "nope", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "unknown skill should be rejected");
    }

    #[tokio::test]
    async fn test_load_skill_native_entry_lands_in_run() {
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let tool = wrap(engine, runner, 350);
        let result = tool
            .run(
                LoadSkillParams {
                    skill_name: "echo".into(),
                    session_id: Some("ses-x".into()),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["skill"]["name"], "skill__echo");
    }

    #[tokio::test]
    async fn test_load_skill_refuses_over_budget() {
        let (engine, runner, _dir) = make_engine_with_skill(true).await;
        let tool = wrap(engine, runner, 1);
        // Fill session to capacity first (max=1).
        {
            let mut map = tool.session_registrations.write().await;
            map.entry("ses-x".into()).or_default().insert(
                "pad".into(),
                Arc::new(crate::builtin::notify::NotifyTool) as ToolBox,
            );
        }
        let result = tool
            .execute(
                json!({"skill_name": "echo", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("per-request limit"),
            "should refuse over budget"
        );
    }
}
