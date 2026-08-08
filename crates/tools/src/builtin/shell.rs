use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::{self, BackgroundJobs};
use crate::{Tool, ToolResult};

pub struct ShellTool {
    /// Registry of background jobs for `background: true` invocations.
    pub jobs: Arc<BackgroundJobs>,
    /// Output cap (chars) for command output.
    pub max_output_chars: usize,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            jobs: Arc::new(BackgroundJobs::new()),
            max_output_chars: 20_000,
        }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> String {
        "shell".into()
    }
    fn description(&self) -> String {
        "Execute a shell command on the user's PC".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn input_schema(&self) -> Value {
        #[cfg(windows)]
        let shells = ["cmd", "powershell"];
        #[cfg(not(windows))]
        let shells = ["sh", "bash"];
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "shell": { "type": "string", "enum": shells, "description": "Which shell to run the command in" },
                "silent": { "type": "boolean", "description": "If true, hide output from the user (agent always sees it)", "default": false },
                "background": { "type": "boolean", "description": "Run the command in the background and return a job_id immediately", "default": false },
                "cwd": { "type": "string", "description": "Working directory to run the command in. Defaults to the shared Temp working directory.", "default": null }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let cmd = input["command"].as_str().unwrap_or("");
        if cmd.is_empty() {
            anyhow::bail!("command is required");
        }
        let silent = input["silent"].as_bool().unwrap_or(false);
        #[cfg(windows)]
        let shell = input["shell"].as_str().unwrap_or("cmd");
        #[cfg(not(windows))]
        let shell = input["shell"].as_str().unwrap_or("sh");
        let max_chars = self.max_output_chars;
        let cwd = input["cwd"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Background mode: hand the command to the job registry and return
        // immediately. The agent polls status with the returned job_id.
        if input["background"].as_bool().unwrap_or(false) {
            let job_id = self.jobs.spawn_shell(cmd, shell, max_chars, cwd).await?;
            return Ok(ToolResult::ok(serde_json::json!({
                "background": true,
                "job_id": job_id,
                "status": "running",
                "hint": "The command is running in the background. Its output will be delivered back to you automatically when it finishes — no need to poll.",
            })));
        }

        let mut std_cmd = bg::build_shell_command_silent(shell, cmd);
        if let Some(cwd) = cwd {
            std_cmd.current_dir(cwd);
        }

        // Suppress console window in silent mode
        if silent {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                std_cmd.creation_flags(CREATE_NO_WINDOW);
            }
        }

        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()?;

        if cancel.is_cancelled() {
            // kill_on_drop ensures the child is terminated when `child` drops,
            // but be explicit to avoid a race where the drop hasn't run yet.
            let _ = child.kill().await;
            anyhow::bail!("cancelled");
        }

        // Stream stdout/stderr into capped buffers so huge command output never
        // gets fully buffered (OOM protection). Mirrors network tool behavior.
        let max_collect = bg::collect_byte_cap(max_chars);
        let stdout_fut = bg::read_stream_capped(child.stdout.take(), max_collect);
        let stderr_fut = bg::read_stream_capped(child.stderr.take(), max_collect);
        // Read both pipes concurrently: reading stdout to EOF first can
        // deadlock when the child fills the stderr pipe buffer meanwhile.
        let ((stdout, stdout_overflow), (stderr, stderr_overflow)) =
            tokio::join!(stdout_fut, stderr_fut);
        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e.into());
            }
        };

        if cancel.is_cancelled() {
            let _ = child.kill().await;
            anyhow::bail!("cancelled");
        }

        let mut combined = String::new();
        if !stdout.is_empty() {
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }

        let (text, _) = haven_common::encoding::truncate_output(&combined, max_chars);
        let truncated = stdout_overflow || stderr_overflow;
        let mut output = serde_json::json!({"output": text});
        if truncated {
            output["truncated"] = serde_json::Value::Bool(true);
        }
        if status.success() {
            Ok(ToolResult::ok(output))
        } else {
            // Non-zero exit: report the failure. When the command produced no
            // stderr, fall back to the combined output so the result is never
            // an empty observation (the model would see a silent tool call).
            let err_text = if stderr.trim().is_empty() {
                text.clone()
            } else {
                stderr
            };
            Ok(ToolResult {
                success: false,
                output,
                error: Some(err_text),
                truncated,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_shell_tool_name() {
        assert_eq!(ShellTool::default().name(), "shell");
    }

    #[test]
    fn test_shell_tool_description() {
        assert!(ShellTool::default().description().contains("shell command"));
    }

    #[test]
    fn test_shell_tool_risk_level() {
        assert_eq!(ShellTool::default().risk_level(&json!({})), RiskLevel::High);
    }

    #[test]
    fn test_shell_tool_input_schema() {
        let schema = ShellTool::default().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"command"));
        let shell_enum = schema["properties"]["shell"]["enum"].as_array().unwrap();
        let shells: Vec<&str> = shell_enum.iter().map(|v| v.as_str().unwrap()).collect();
        #[cfg(windows)]
        {
            assert!(shells.contains(&"cmd"));
            assert!(shells.contains(&"powershell"));
        }
        #[cfg(not(windows))]
        {
            assert!(shells.contains(&"sh"));
            assert!(shells.contains(&"bash"));
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_large_output_reports_success_and_truncates() {
        // ~160KB of output exceeds the collection cap (80KB default). The child
        // must complete normally (no broken pipe from closing the read end
        // early) and the result must carry the truncation flag.
        let result = ShellTool::default()
            .execute(
                json!({
                    "command": "for /l %i in (1,1,5000) do @echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "shell": "cmd",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.success,
            "child must complete successfully: {:?}",
            result.error
        );
        let out = result.output["output"].as_str().unwrap();
        assert!(
            out.len() < 50_000,
            "output should be capped, got {} bytes",
            out.len()
        );
        assert!(
            result.output["truncated"].as_bool().unwrap_or(false),
            "capped output must carry the truncated flag"
        );
    }

    #[tokio::test]
    async fn test_shell_missing_command() {
        let result = ShellTool::default()
            .execute(json!({"command": ""}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = ShellTool::default()
            .execute(json!({"command": "echo hi"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_cmd_echo() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo hello", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert!(result.output["output"].as_str().unwrap().contains("hello"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_powershell_echo() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "Write-Output hello", "shell": "powershell"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success, "expected success: {:?}", result.error);
        assert!(result.output["output"].as_str().unwrap().contains("hello"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_nonzero_exit_reports_failure() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "exit 42", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.success, "non-zero exit must report failure");
        assert!(result.error.is_some());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_nonzero_exit_without_stderr_has_nonempty_error() {
        // A failing command with no stderr must not produce an empty error:
        // the result text flows straight into the model's observation, and an
        // empty string looks like the tool never returned anything.
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo out && exit 42", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            !err.trim().is_empty(),
            "error must not be empty, got: {:?}",
            err
        );
        assert!(
            err.contains("out"),
            "error should carry the stdout content when stderr is empty, got: {:?}",
            err
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_captures_stderr() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "echo boom 1>&2", "shell": "cmd"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            result.success,
            "echo to stderr still exits 0: {:?}",
            result.error
        );
        assert!(result.output["output"].as_str().unwrap().contains("boom"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_background_returns_job_id_and_completes() {
        let tool = ShellTool::default();
        let result = tool
            .execute(
                json!({"command": "echo bg-result", "shell": "cmd", "background": true}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["background"], true);
        assert_eq!(result.output["status"], "running");
        let job_id = result.output["job_id"].as_str().unwrap().to_string();
        assert!(!job_id.is_empty());

        // Poll the job registry until the job completes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            let v = tool.jobs.status(&job_id).await;
            if v["status"] != "running" || std::time::Instant::now() > deadline {
                break v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        assert_eq!(status["status"], "completed", "got: {}", status);
        assert!(status["output"].as_str().unwrap().contains("bg-result"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_shell_background_empty_command_rejected() {
        let result = ShellTool::default()
            .execute(
                json!({"command": "", "background": true}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_shell_background_schema_field() {
        let schema = ShellTool::default().input_schema();
        assert_eq!(
            schema["properties"]["background"]["type"], "boolean",
            "background field must be in the schema"
        );
    }
}
