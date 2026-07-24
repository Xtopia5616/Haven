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
        "Execute a shell command on the user's PC. When silent is true, the command output is hidden from the user but still returned to the agent.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
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
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let mut std_cmd = std::process::Command::new("cmd");
        std_cmd.args(["/C", cmd]);
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

        let child = tokio::process::Command::from(std_cmd)
            .spawn()?;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let output = child.wait_with_output().await?;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

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

        let (text, _truncated) = truncate_output(&combined, max_chars);
        if output.status.success() {
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

fn truncate_output(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        (text.to_string(), false)
    } else {
        let truncated = format!(
            "{}[truncated ... {} chars omitted]",
            &text[..max_chars],
            text.len() - max_chars
        );
        (truncated, true)
    }
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
    }
}
