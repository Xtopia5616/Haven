use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct ProcessTool {
    /// Output cap (chars) for process listings.
    pub max_output_chars: usize,
}

impl Default for ProcessTool {
    fn default() -> Self {
        Self {
            max_output_chars: 20_000,
        }
    }
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> String {
        "process".into()
    }
    fn description(&self) -> String {
        "List, launch, or kill processes".into()
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
                "pid": { "type": "integer" },
                "cwd": { "type": "string", "description": "Working directory for the launched command. Defaults to the shared Temp working directory.", "default": null }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("list");
        let max_chars = self.max_output_chars;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

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
                        b["memory"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&a["memory"].as_u64().unwrap_or(0))
                    });
                    processes.truncate(200);
                    processes
                })
                .await?;

                // The entries are sorted by memory desc, so the tail holds the
                // least important entries and is dropped first when the JSON
                // exceeds the output budget.
                let count = processes.len();
                let (mut output, truncated) =
                    crate::util::json_list_within_budget("processes", processes, count, max_chars);
                if truncated {
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
                // Default to the shared Temp working directory so launched
                // commands do not execute in the app's own working directory.
                child.current_dir(haven_common::default_work_dir());
                if let Some(cwd) = input["cwd"].as_str().filter(|s| !s.is_empty()) {
                    child.current_dir(cwd);
                }
                // Route launched commands through a locally detected proxy
                // (same detection as the shell tool).
                for (key, val) in crate::bg::proxy_env_vars() {
                    if std::env::var_os(&key).is_none() {
                        child.env(key, val);
                    }
                }
                // Hide the console window when spawning GUI-less commands.
                #[cfg(windows)]
                {
                    child.creation_flags(crate::bg::CREATE_NO_WINDOW);
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
                    let proc = system
                        .process(sysinfo::Pid::from_u32(pid))
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
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
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
        assert_eq!(ProcessTool::default().name(), "process");
    }

    #[test]
    fn test_process_tool_description() {
        assert!(ProcessTool::default().description().contains("kill"));
    }

    #[test]
    fn test_process_tool_risk_level() {
        assert_eq!(
            ProcessTool::default().risk_level(&json!({"operation": "kill"})),
            RiskLevel::High
        );
        assert_eq!(
            ProcessTool::default().risk_level(&json!({"operation": "list"})),
            RiskLevel::Low
        );
        assert_eq!(
            ProcessTool::default().risk_level(&json!({"operation": "launch"})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_process_tool_input_schema() {
        let schema = ProcessTool::default().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"list"));
        assert!(ops.contains(&"launch"));
        assert!(ops.contains(&"kill"));
    }

    #[tokio::test]
    async fn test_process_execute_list() {
        let result = ProcessTool::default()
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        let processes = result.output["processes"].as_array().unwrap();
        assert!(!processes.is_empty());
        for p in processes {
            assert!(p["pid"].as_u64().unwrap() > 0);
            assert!(p["memory"].is_number());
            assert!(p["cpu"].is_number());
            assert!(p["status"].is_string());
        }
    }

    #[tokio::test]
    async fn test_process_execute_launch() {
        let result = ProcessTool::default()
            .execute(
                json!({"operation": "launch", "command": "echo hello"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["launched"], "echo hello");
    }

    #[tokio::test]
    async fn test_process_execute_launch_requires_command() {
        let result = ProcessTool::default()
            .execute(
                json!({"operation": "launch", "command": ""}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_execute_kill_requires_pid() {
        let result = ProcessTool::default()
            .execute(json!({"operation": "kill"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_execute_kill_not_found() {
        let result = ProcessTool::default()
            .execute(
                json!({"operation": "kill", "pid": 999999999}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_execute_unknown_operation() {
        let result = ProcessTool::default()
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = ProcessTool::default()
            .execute(json!({"operation": "list"}), cancel)
            .await;
        assert!(result.is_err());
    }
}
