use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> String {
        "shell".into()
    }
    fn description(&self) -> String {
        "Execute a shell command on the user's PC. When silent is true, the command output is hidden from the user but still returned to the agent. The shell parameter selects the interpreter: cmd (default) or powershell.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "shell": { "type": "string", "enum": ["cmd", "powershell"], "description": "Which shell to run the command in", "default": "cmd" },
                "silent": { "type": "boolean", "description": "If true, hide output from the user (agent always sees it)", "default": false }
            },
            "required": ["command"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let cmd = input["command"].as_str().unwrap_or("");
        if cmd.is_empty() {
            anyhow::bail!("command is required");
        }
        let silent = input["silent"].as_bool().unwrap_or(false);
        let shell = input["shell"].as_str().unwrap_or("cmd");
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let mut std_cmd = match shell {
            "powershell" => {
                let mut c = std::process::Command::new("powershell");
                c.args(["-NoProfile", "-Command", cmd]);
                c
            }
            _ => {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", cmd]);
                c
            }
        };
        std_cmd.stdout(std::process::Stdio::piped());
        std_cmd.stderr(std::process::Stdio::piped());

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
        let max_collect = max_chars.saturating_mul(4).max(8192);
        let stdout_fut = read_stream_capped(child.stdout.take(), max_collect);
        let stderr_fut = read_stream_capped(child.stderr.take(), max_collect);
        // Read both pipes concurrently: reading stdout to EOF first can
        // deadlock when the child fills the stderr pipe buffer meanwhile.
        let (stdout, stderr) = tokio::join!(stdout_fut, stderr_fut);
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

        let (text, _truncated) = haven_common::encoding::truncate_output(&combined, max_chars);
        if status.success() {
            Ok(ToolResult::ok(serde_json::json!({"output": text})))
        } else {
            Ok(ToolResult {
                success: false,
                output: serde_json::json!({"output": text}),
                error: Some(stderr.to_string()),
                truncated: false,
            })
        }
    }
}

/// Read a child stdout/stderr stream into a String, capping at `max_bytes`
/// so runaway output cannot exhaust memory.
async fn read_stream_capped<R>(stdout: Option<R>, max_bytes: usize) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut stream) = stdout else {
        return String::new();
    };
    let mut buf = Vec::with_capacity(max_bytes.min(8192));
    let mut tmp = [0u8; 8192];
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                let room = max_bytes.saturating_sub(buf.len());
                if room == 0 {
                    break;
                }
                let take = n.min(room);
                buf.extend_from_slice(&tmp[..take]);
                if take < n {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    haven_common::encoding::decode_lossy(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_shell_tool_name() {
        assert_eq!(ShellTool.name(), "shell");
    }

    #[test]
    fn test_shell_tool_description() {
        assert!(ShellTool.description().contains("shell command"));
    }

    #[test]
    fn test_shell_tool_risk_level() {
        assert_eq!(ShellTool.risk_level(&json!({})), RiskLevel::High);
    }

    #[test]
    fn test_shell_tool_input_schema() {
        let schema = ShellTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"command"));
        let shell_enum = schema["properties"]["shell"]["enum"].as_array().unwrap();
        let shells: Vec<&str> = shell_enum.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(shells.contains(&"cmd"));
        assert!(shells.contains(&"powershell"));
    }
}
