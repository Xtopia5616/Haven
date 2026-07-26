use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct ProcessTool;

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> String {
        "process".into()
    }
    fn description(&self) -> String {
        "Process operations: list, launch, kill".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("kill") => RiskLevel::High,
            Some("launch") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "launch", "kill"] },
                "command": { "type": "string" },
                "pid": { "type": "integer" }
            },
            "required": ["operation"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("list");
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        match op {
            "list" => {
                let processes: Vec<Value> = tokio::task::spawn_blocking(move || {
                    let system = sysinfo::System::new_all();
                    let mut processes: Vec<Value> = system
                        .processes()
                        .iter()
                        .map(|(pid, proc)| {
                            serde_json::json!({
                                "pid": pid.as_u32(),
                                "name": proc.name(),
                                "cpu": proc.cpu_usage(),
                                "memory": proc.memory(),
                                "status": format!("{:?}", proc.status()),
                            })
                        })
                        .collect();

                    processes.sort_by(|a, b| {
                        b["memory"].as_u64().unwrap_or(0)
                            .cmp(&a["memory"].as_u64().unwrap_or(0))
                    });
                    processes.truncate(200);
                    processes
                })
                .await?;

                let output = serde_json::json!({"processes": processes, "count": processes.len()});
                let text = serde_json::to_string(&output).unwrap_or_default();
                let (truncated_output, _) = truncate_output(&text, max_chars);
                Ok(ToolResult::ok(serde_json::from_str(&truncated_output).unwrap_or(output)))
            }
            "launch" => {
                let cmd = input["command"].as_str().unwrap_or("");
                if cmd.is_empty() {
                    anyhow::bail!("command is required for launch");
                }
                tokio::process::Command::new("cmd")
                    .args(["/c", cmd])
                    .spawn()?;
                Ok(ToolResult::ok(serde_json::json!({"launched": cmd})))
            }
            "kill" => {
                let pid = input["pid"].as_i64().unwrap_or(0) as u32;
                if pid == 0 {
                    anyhow::bail!("valid pid is required");
                }
                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let system = sysinfo::System::new_all();
                    let proc = system.process(sysinfo::Pid::from_u32(pid))
                        .ok_or_else(|| anyhow::anyhow!("process {} not found", pid))?;
                    #[cfg(target_os = "windows")]
                    if !proc.kill() {
                        anyhow::bail!("failed to kill process {}", pid);
                    }
                    #[cfg(not(target_os = "windows"))]
                    if !proc.kill_with(sysinfo::Signal::Term) {
                        anyhow::bail!("failed to kill process {}", pid);
                    }
                    Ok(())
                })
                .await??;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(serde_json::json!({"killed": pid})))
            }
            _ => anyhow::bail!("unknown process operation: {}", op),
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
    fn test_process_tool_name() {
        assert_eq!(ProcessTool.name(), "process");
    }

    #[test]
    fn test_process_tool_description() {
        assert!(ProcessTool.description().contains("Process operations"));
    }

    #[test]
    fn test_process_tool_risk_level() {
        assert_eq!(ProcessTool.risk_level(&json!({"operation": "kill"})), RiskLevel::High);
        assert_eq!(ProcessTool.risk_level(&json!({"operation": "list"})), RiskLevel::Low);
        assert_eq!(ProcessTool.risk_level(&json!({"operation": "launch"})), RiskLevel::Medium);
    }

    #[test]
    fn test_process_tool_input_schema() {
        let schema = ProcessTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"].as_array().unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"list"));
        assert!(ops.contains(&"launch"));
        assert!(ops.contains(&"kill"));
    }
}
