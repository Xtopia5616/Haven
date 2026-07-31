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

                // The entries are sorted by memory desc, so when the JSON
                // exceeds the output budget, drop trailing entries until it
                // fits. Never parse a mid-string-truncated JSON back (that
                // could fall back to returning the full, uncapped output).
                let count = processes.len();
                let mut kept = processes;
                let (truncated, serialized) = loop {
                    let serialized = serde_json::to_string(&serde_json::json!({
                        "processes": kept,
                        "count": count,
                    }))
                    .unwrap_or_default();
                    if serialized.len() <= max_chars || kept.len() <= 1 {
                        break (kept.len() < count, serialized);
                    }
                    kept.pop();
                };
                let mut output: Value = serde_json::from_str(&serialized)
                    .unwrap_or_else(|_| serde_json::json!({"processes": kept, "count": count}));
                if truncated {
                    output["truncated"] = serde_json::Value::Bool(true);
                    output["hint"] = serde_json::json!(
                        "Output truncated to the max chars budget. Narrow by filtering processes, or reduce the returned fields."
                    );
                }
                Ok(ToolResult::ok(output))
            }
            "launch" => {
                let cmd = input["command"].as_str().unwrap_or("");
                if cmd.is_empty() {
                    anyhow::bail!("command is required for launch");
                }
                // Fire-and-forget: no kill_on_drop (dropping the Child would
                // terminate the launched process).
                let mut child = tokio::process::Command::new("cmd");
                child.args(["/c", cmd]);
                // Hide the console window when spawning GUI-less commands.
                #[cfg(windows)]
                {
                    const CREATE_NO_WINDOW: u32 = 0x08000000;
                    child.creation_flags(CREATE_NO_WINDOW);
                }
                child.spawn()?;
                Ok(ToolResult::ok(serde_json::json!({"launched": cmd})))
            }
            "kill" => {
                let pid = input["pid"].as_i64().unwrap_or(0) as u32;
                if pid == 0 {
                    anyhow::bail!("valid pid is required");
                }
                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    // new_all() refreshes the whole system; refresh_processes
                    // is enough to find a single process by pid.
                    let mut system = sysinfo::System::new();
                    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
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
