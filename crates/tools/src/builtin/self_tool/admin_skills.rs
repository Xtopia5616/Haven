use std::sync::Arc;

use anyhow::Result;
use haven_common::config::ConfigPatch;
use serde_json::Value;

use super::SelfParams;
use super::SelfTool;

impl SelfTool {
    pub(super) async fn op_skills_list(&self) -> Result<Value> {
        let skills: Vec<Value> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "enabled": s.enabled,
                    "description": s.description,
                    "root": s.root,
                })
            })
            .collect();
        Ok(serde_json::json!({ "skills": skills }))
    }

    pub(super) async fn op_skill_set(&self, params: &SelfParams, enabled: bool) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "name is required (the skill to {}able)",
                    if enabled { "en" } else { "dis" }
                )
            })?;
        let config_service = Arc::clone(
            self.context
                .config_service
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))?,
        );
        if self.skills_engine.get_skill(name).await.is_none() {
            anyhow::bail!("skill '{}' not found", name);
        }
        self.skills_engine.set_enabled(name, enabled).await?;
        let filter = self.skills_engine.enabled_filter().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = filter;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            // `set_enabled` is in-memory; restore it when durable config
            // persistence fails so the two views cannot diverge.
            if let Err(rollback) = self.skills_engine.set_enabled(name, !enabled).await {
                tracing::error!(
                    skill = name,
                    error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                    "skill enable rollback failed after config persistence error"
                );
            }
            return Err(error);
        }
        Ok(serde_json::json!({
            "name": name,
            "enabled": enabled,
            "saved": true,
            "note": "take effect immediately for new loads"
        }))
    }

    pub(super) async fn op_skill_create(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the new skill name)"))?;
        let config_service = Arc::clone(
            self.context
                .config_service
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))?,
        );
        validate_skill_name(name)?;
        let description = params
            .description
            .as_deref()
            .filter(|d| !d.is_empty())
            .ok_or_else(|| anyhow::anyhow!("description is required"))?;
        let instructions = params
            .instructions
            .as_deref()
            .filter(|i| !i.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("instructions are required (the '## Instructions' body)")
            })?;
        if instructions.len() > self.max_instructions_bytes {
            anyhow::bail!(
                "instructions too large (max {} bytes)",
                self.max_instructions_bytes
            );
        }
        let language = params.language.as_deref().unwrap_or("python");
        if language != "python" {
            anyhow::bail!("unsupported language '{language}': only 'python' is supported");
        }
        let version = params
            .version
            .as_deref()
            .map(|v| v.replace(['\n', '\r'], " ").trim().to_string())
            .filter(|v| !v.is_empty());
        if let Some(v) = &version
            && v.len() > 64
        {
            anyhow::bail!("version too long (max 64 characters)");
        }
        let script = params.script.clone();
        if let Some(s) = &script
            && s.len() > self.max_script_bytes
        {
            anyhow::bail!("script too large (max {} bytes)", self.max_script_bytes);
        }

        let root = self.skills_engine.resolved_root().await;
        let skill_dir = root.join(name);
        if skill_dir.exists() {
            anyhow::bail!("skill '{}' already exists at {}", name, skill_dir.display());
        }

        tokio::fs::create_dir_all(&skill_dir).await?;
        let desc_line = description.replace(['\n', '\r'], " ");
        let mut md =
            format!("# Skill: {name}\n\n## Metadata\n- name: {name}\n- description: {desc_line}\n");
        if let Some(v) = &version {
            md.push_str(&format!("- version: {v}\n"));
        }
        md.push_str(&format!(
            "- language: {language}\n\n## Instructions\n{instructions}\n"
        ));
        tokio::fs::write(skill_dir.join("SKILL.md"), md).await?;

        let mut has_script = false;
        if let Some(script) = script {
            let scripts = skill_dir.join("scripts");
            tokio::fs::create_dir_all(&scripts).await?;
            tokio::fs::write(scripts.join("main.py"), script).await?;
            has_script = true;
        }

        self.skills_engine.refresh_from_disk().await?;
        self.skills_engine.set_enabled(name, true).await?;
        let filter = self.skills_engine.enabled_filter().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = filter;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = tokio::fs::remove_dir_all(&skill_dir).await {
                tracing::warn!(
                    path = %skill_dir.display(),
                    error = %rollback,
                    "skill creation rollback failed after config persistence error"
                );
            }
            if let Err(refresh_error) = self.skills_engine.refresh_from_disk().await {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&refresh_error.to_string()),
                    "skill catalog refresh failed while rolling back skill creation"
                );
            }
            return Err(error);
        }

        Ok(serde_json::json!({
            "name": name,
            "created": true,
            "root": skill_dir.to_string_lossy(),
            "has_script": has_script,
        }))
    }
}

/// Validate a skill name for safe use as a directory and as the
/// `skill__<name>` tool identifier (after sanitization).
pub(super) fn validate_skill_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        anyhow::bail!(
            "invalid skill name '{}': use 1-128 characters of a-z, A-Z, 0-9, '-' or '_'",
            name
        );
    }
    Ok(())
}
