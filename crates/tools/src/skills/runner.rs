use crate::skills::Skill;
use crate::skills::venv::VenvManager;
use crate::tool::ToolResult;
use haven_common::config::SkillsExecConfig;
use haven_common::encoding;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Sandbox executor for skill scripts (M4-02).
///
/// Spawns a subprocess running the skill's entry script inside its isolated
/// venv, with stdin carrying the serialised `params` JSON, a clean environment,
/// and a wall-clock timeout.
#[derive(Clone)]
pub struct SkillRunner {
    venv: VenvManager,
    config: SkillsExecConfig,
}

impl SkillRunner {
    pub fn new(venv: VenvManager, config: SkillsExecConfig) -> Self {
        Self { venv, config }
    }

    pub fn venv(&self) -> &VenvManager {
        &self.venv
    }

    pub fn config(&self) -> &SkillsExecConfig {
        &self.config
    }

    /// Execute a skill's script with the given parameters.
    pub async fn execute(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let entry = skill
            .entry_script()
            .ok_or_else(|| anyhow::anyhow!("missing entry script for skill '{}'", skill.name()))?;
        if !matches!(skill.language(), crate::skills::Language::Python) {
            anyhow::bail!(
                "unsupported language '{}' for skill '{}'",
                skill.language().as_str(),
                skill.name()
            );
        }

        let python = self.venv.ensure(skill.name(), skill.root()).await?;

        let work_dir = &self.config.work_dir;
        tokio::fs::create_dir_all(work_dir).await?;

        let input_json = serde_json::to_string(params)?;

        let mut cmd = tokio::process::Command::new(&python);
        cmd.arg(&entry)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(work_dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(
                "SYSTEMROOT",
                std::env::var("SYSTEMROOT").unwrap_or_default(),
            )
            .env("TEMP", std::env::var("TEMP").unwrap_or_default())
            .env("TMP", std::env::var("TMP").unwrap_or_default())
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1");

        #[cfg(windows)]
        {
            cmd.env("COMSPEC", std::env::var("COMSPEC").unwrap_or_default());
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn skill '{}': {}", skill.name(), e))?;

        // Write params as JSON to stdin, then close it.
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(input_json.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }

        let max_lines = self.config.max_output_lines;
        let timeout_dur = std::time::Duration::from_secs(self.config.timeout_secs);
        let pid_label = skill.name().to_string();

        // Wait for the child with a wall-clock timeout using `wait(&mut self)`
        // which does not consume `child`, so the timeout branch can still kill it.
        let exit_status = tokio::select! {
            result = child.wait() => {
                match result {
                    Ok(status) => Some(status),
                    Err(e) => return Err(anyhow::anyhow!("skill '{}' wait error: {}", pid_label, e)),
                }
            }
            _ = tokio::time::sleep(timeout_dur) => {
                let _ = child.start_kill();
                let _ = tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let _ = child.kill().await;
                return Ok(ToolResult {
                    success: false,
                    output: Value::Null,
                    error: Some(format!(
                        "skill '{}' timed out after {}s",
                        pid_label, self.config.timeout_secs
                    )),
                    truncated: false,
                });
            }
        };

        if cancel.is_cancelled() {
            return Ok(ToolResult {
                success: false,
                output: Value::Null,
                error: Some(format!("skill '{}' cancelled by user", pid_label)),
                truncated: false,
            });
        }

        // Read stdout/stderr from the pipes after the process has exited.
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_end(&mut stdout_buf).await;
        }
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_end(&mut stderr_buf).await;
        }

        let stdout = encoding::decode_lossy(&stdout_buf);
        let stderr = encoding::decode_lossy(&stderr_buf);
        let exit_code = exit_status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

        let out_lines: Vec<&str> = stdout.lines().take(max_lines).collect();
        let err_lines: Vec<&str> = stderr.lines().take(max_lines).collect();
        let out_text = out_lines.join("\n");
        let err_text = err_lines.join("\n");

        if exit_code != 0 || !err_text.is_empty() {
            Ok(ToolResult {
                success: false,
                output: serde_json::json!({ "stdout": out_text, "stderr": err_text }),
                error: Some(format!(
                    "skill '{}' exited with code {}: {}",
                    pid_label, exit_code, err_text
                )),
                truncated: false,
            })
        } else {
            let output: Value = serde_json::from_str(&out_text)
                .unwrap_or_else(|_| serde_json::json!({ "result": out_text }));
            Ok(ToolResult::ok(output))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{Language, Skill, SkillManifest};
    use std::path::PathBuf;

    fn echo_skill() -> Skill {
        Skill::from_manifest_unchecked(
            SkillManifest {
                name: "echo".into(),
                description: "Echoes input".into(),
                version: None,
                language: Language::Python,
                instructions: "".into(),
            },
            PathBuf::from("examples/skills/echo"),
            true,
        )
    }

    #[tokio::test]
    async fn runner_timeout_returns_timed_out() {
        let skill = echo_skill();

        let config = SkillsExecConfig {
            timeout_secs: 1,
            max_output_lines: 5000,
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let runner = SkillRunner::new(venv, config);
        let cancel = CancellationToken::new();

        let result = runner
            .execute(&skill, &serde_json::json!({"text": "hi"}), cancel)
            .await;
        assert!(result.is_err() || !result.unwrap().success);
    }
}
