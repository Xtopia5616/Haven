use crate::process::read_stream_capped_with;
use crate::tool_contract::ToolResult;
use haven_common::config::SkillsExecConfig;
use haven_common::encoding;
use haven_common::error::sanitize_error_text;
use haven_platform::process_containment::ProcessContainment;
use haven_skills::{Skill, VenvManager};
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Bound retained output independently for stdout and stderr. Both streams
/// continue draining after the cap so a child can never deadlock on a full
/// pipe merely because its result is too large for Haven to keep.
const MAX_SKILL_STREAM_BYTES: usize = 8 * 1024 * 1024;

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

    /// The subprocess wall-clock timeout. Tool adapters use this to set an
    /// outer timeout with a small cleanup margin instead of racing the same
    /// deadline at two layers.
    pub fn timeout_secs(&self) -> u64 {
        self.config.timeout_secs
    }

    /// Execute a skill's script with the given parameters.
    pub async fn execute(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let entry = skill.entry_script().ok_or_else(|| {
            anyhow::anyhow!(
                "skill '{}' has no entry script; expected scripts/main.py or scripts/{}.py",
                skill.name(),
                skill.name()
            )
        })?;
        if !matches!(skill.language(), haven_skills::Language::Python) {
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
        let pid_label = skill.name().to_string();

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

        let containment = ProcessContainment::new().map_err(|error| {
            anyhow::anyhow!(
                "failed to create process containment for skill '{}': {}",
                pid_label,
                sanitize_error_text(&error.to_string())
            )
        })?;
        containment.prepare_command(cmd.as_std_mut(), 0);
        let mut child = cmd.kill_on_drop(true).spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn skill '{}': {}",
                pid_label,
                sanitize_error_text(&e.to_string())
            )
        })?;
        let pid = child.id().ok_or_else(|| {
            anyhow::anyhow!("skill '{}' did not expose a child process id", pid_label)
        })?;
        #[cfg(windows)]
        let attach_result = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("skill child process handle is unavailable"))
            .and_then(|handle| containment.attach_and_resume(pid, handle));
        #[cfg(not(windows))]
        let attach_result = containment.attach_and_resume(pid, ());
        if let Err(error) = attach_result {
            let _ = child.kill().await;
            anyhow::bail!(
                "failed to attach skill '{}' to process containment: {}",
                pid_label,
                sanitize_error_text(&error.to_string())
            );
        }

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let max_lines = self.config.max_output_lines;
        let timeout_dur = Duration::from_secs(self.config.timeout_secs);

        // Feed stdin, drain both output pipes, and wait for process exit as one
        // deadline-bound operation. Waiting first can deadlock when a child
        // fills either OS pipe before it exits.
        let (exit_status, stdout_buf, stdout_overflowed, stderr_buf, stderr_overflowed) = tokio::select! {
            result = async {
                use tokio::io::AsyncWriteExt;

                let write_input = async move {
                    if let Some(mut stdin) = stdin {
                        stdin.write_all(input_json.as_bytes()).await?;
                        stdin.shutdown().await?;
                    }
                    Ok::<(), std::io::Error>(())
                };
                let read_stdout = read_stream_capped_with(
                    stdout,
                    MAX_SKILL_STREAM_BYTES,
                    |_| {},
                );
                let read_stderr = read_stream_capped_with(
                    stderr,
                    MAX_SKILL_STREAM_BYTES,
                    |_| {},
                );
                let wait = child.wait();
                let (write_result, stdout, stderr, status) =
                    tokio::join!(write_input, read_stdout, read_stderr, wait);

                write_result.map_err(|error| {
                    anyhow::anyhow!(
                        "failed to write params to skill '{}': {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    )
                })?;
                let (stdout_buf, stdout_overflowed, stdout_error) = stdout;
                if let Some(error) = stdout_error {
                    anyhow::bail!(
                        "failed to read stdout from skill '{}': {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    );
                }
                let (stderr_buf, stderr_overflowed, stderr_error) = stderr;
                if let Some(error) = stderr_error {
                    anyhow::bail!(
                        "failed to read stderr from skill '{}': {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    );
                }
                let exit_status = status.map_err(|error| {
                    anyhow::anyhow!(
                        "skill '{}' wait error: {}",
                        pid_label,
                        sanitize_error_text(&error.to_string())
                    )
                })?;
                Ok::<_, anyhow::Error>((
                    exit_status,
                    stdout_buf,
                    stdout_overflowed,
                    stderr_buf,
                    stderr_overflowed,
                ))
            } => result?,
            _ = cancel.cancelled() => {
                // Dropping the child and its containment guard terminates the
                // process tree; no detached pipe reader survives this return.
                return Ok(ToolResult::cancelled(format!(
                    "skill '{}' cancelled",
                    pid_label
                )));
            }
            _ = tokio::time::sleep(timeout_dur) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("skill '{}' timed out after {}s", pid_label, self.config.timeout_secs),
                ));
            }
        };

        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled(format!(
                "skill '{}' cancelled by user",
                pid_label
            )));
        }

        if stdout_overflowed || stderr_overflowed {
            let stdout_preview =
                encoding::decode_lossy(&stdout_buf[..stdout_buf.len().min(8 * 1024)]);
            let stderr_preview = sanitize_error_text(&encoding::decode_lossy(
                &stderr_buf[..stderr_buf.len().min(8 * 1024)],
            ));
            return Ok(ToolResult::failed(
                serde_json::json!({
                    "stdout_preview": stdout_preview,
                    "stderr_preview": stderr_preview,
                    "truncated": true,
                }),
                format!(
                    "skill '{}' output exceeded its limit ({} bytes per stream)",
                    pid_label, MAX_SKILL_STREAM_BYTES
                ),
            ));
        }

        let stdout = encoding::decode_lossy(&stdout_buf);
        let stderr = encoding::decode_lossy(&stderr_buf);
        let exit_code = exit_status.code().unwrap_or(-1);

        let out_line_count = stdout.lines().take(max_lines.saturating_add(1)).count();
        let err_line_count = stderr.lines().take(max_lines.saturating_add(1)).count();
        let out_lines: Vec<&str> = stdout.lines().take(max_lines).collect();
        let err_lines: Vec<&str> = stderr.lines().take(max_lines).collect();
        let out_text = out_lines.join("\n");
        let err_text = sanitize_error_text(&err_lines.join("\n"));

        let over_line_limit = out_line_count > max_lines || err_line_count > max_lines;
        if over_line_limit {
            return Ok(ToolResult::failed(
                serde_json::json!({
                    "stdout": out_text,
                    "stderr": err_text,
                    "truncated": true,
                }),
                format!(
                    "skill '{}' output exceeded its limit ({} lines per stream)",
                    pid_label, max_lines
                ),
            ));
        }

        if exit_code != 0 || !err_text.is_empty() {
            Ok(ToolResult::failed(
                serde_json::json!({ "stdout": out_text, "stderr": err_text }),
                format!(
                    "skill '{}' exited with code {}: {}",
                    pid_label, exit_code, err_text
                ),
            ))
        } else {
            let output: Value = serde_json::from_str(&out_text).map_err(|error| {
                anyhow::anyhow!(
                    "skill '{}' returned invalid JSON: {}",
                    pid_label,
                    sanitize_error_text(&error.to_string())
                )
            })?;
            Ok(ToolResult::ok(output))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_skills::{Language, Skill, SkillManifest};
    use std::path::Path;

    fn scripted_skill(root: &Path, script: &str) -> Skill {
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("main.py"), script).unwrap();
        Skill::from_manifest_unchecked(
            SkillManifest {
                name: "test-skill".into(),
                description: "Temporary runner test".into(),
                version: None,
                language: Language::Python,
                instructions: "".into(),
            },
            root.to_path_buf(),
            true,
        )
    }

    #[tokio::test]
    async fn runner_timeout_returns_timed_out() {
        let temp = tempfile::tempdir().unwrap();
        let skill = scripted_skill(
            &temp.path().join("skill"),
            "import time\ntime.sleep(10)\nprint('{}')\n",
        );

        let config = SkillsExecConfig {
            timeout_secs: 1,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let runner = SkillRunner::new(venv, config);
        let cancel = CancellationToken::new();

        let result = runner
            .execute(&skill, &serde_json::json!({"text": "hi"}), cancel)
            .await;
        assert!(matches!(
            result.unwrap().outcome,
            crate::ToolExecutionOutcome::TimedOutUnknown
        ));
    }

    #[tokio::test]
    async fn runner_drains_both_pipes_and_rejects_bounded_output_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let skill = scripted_skill(
            &temp.path().join("skill"),
            concat!(
                "import sys, threading\n",
                "payload = b'x' * (9 * 1024 * 1024)\n",
                "out = threading.Thread(target=lambda: sys.stdout.buffer.write(payload))\n",
                "err = threading.Thread(target=lambda: sys.stderr.buffer.write(payload))\n",
                "out.start(); err.start(); out.join(); err.join()\n",
            ),
        );
        let config = SkillsExecConfig {
            timeout_secs: 15,
            max_output_lines: 5000,
            venv_root: temp.path().join("venvs"),
            work_dir: temp.path().join("work"),
            ..Default::default()
        };
        let venv = VenvManager::new(config.venv_root.clone());
        let runner = SkillRunner::new(venv, config);

        let result = tokio::time::timeout(
            Duration::from_secs(20),
            runner.execute(
                &skill,
                &serde_json::json!({"input": "ok"}),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("pipe readers must let a noisy child exit")
        .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("output exceeded its limit"));
        assert_eq!(result.output["truncated"], true);
    }
}
