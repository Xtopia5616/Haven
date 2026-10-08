use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolConcurrency, ToolResult};

pub struct ProcessTool {
    /// Output cap (chars) for process listings.
    pub max_output_chars: usize,
}

const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: usize = 200;

/// Process operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOperation {
    List,
    Kill,
}

/// Typed parameters for `ProcessTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `ProcessTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProcessParams {
    /// Operation to perform; defaults to `list`.
    #[serde(default)]
    pub operation: Option<ProcessOperation>,
    /// Process id for the kill operation.
    #[serde(default)]
    pub pid: Option<i64>,
    /// Case-insensitive substring filter for process names.
    #[serde(default)]
    pub name_filter: Option<String>,
    /// Maximum matching processes to return (1..=200).
    #[serde(default)]
    pub limit: Option<usize>,
}

impl ProcessTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ProcessParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let max_chars = self.max_output_chars;

        match params.operation.unwrap_or(ProcessOperation::List) {
            ProcessOperation::List => {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let name_filter = params
                    .name_filter
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_lowercase);
                let limit = params
                    .limit
                    .unwrap_or(DEFAULT_LIST_LIMIT)
                    .clamp(1, MAX_LIST_LIMIT);
                let (processes, matching_count): (Vec<Value>, usize) =
                    tokio::task::spawn_blocking(move || {
                        // Process listings do not need disks, users, networks,
                        // or hardware refreshes. Refresh only the process table;
                        // `new_all` made this hot read path needlessly expensive.
                        let mut system = sysinfo::System::new();
                        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                        let mut processes: Vec<Value> = system
                            .processes()
                            .iter()
                            .filter_map(|(pid, proc)| {
                                let name = proc.name().to_string_lossy().into_owned();
                                if name_filter
                                    .as_ref()
                                    .is_some_and(|filter| !name.to_lowercase().contains(filter))
                                {
                                    return None;
                                }
                                Some(serde_json::json!({
                                    "pid": pid.as_u32(),
                                    "name": name,
                                    "cpu": proc.cpu_usage(),
                                    "memory": proc.memory(),
                                    "status": process_status_output(proc.status()),
                                }))
                            })
                            .collect();

                        processes.sort_by(|a, b| {
                            b["memory"]
                                .as_u64()
                                .unwrap_or(0)
                                .cmp(&a["memory"].as_u64().unwrap_or(0))
                        });
                        let matching_count = processes.len();
                        processes.truncate(limit);
                        (processes, matching_count)
                    })
                    .await?;

                // The entries are sorted by memory desc, so the tail holds the
                // least important entries and is dropped first when the JSON
                // exceeds the output budget.
                let capped =
                    crate::util::cap_json_list("processes", processes, matching_count, max_chars);
                let budget_truncated = capped.truncated;
                let mut output = capped.value;
                let returned = output["processes"].as_array().map_or(0, Vec::len);
                let truncated = budget_truncated || returned < matching_count;
                output["operation"] = serde_json::json!("list");
                output["returned"] = serde_json::json!(returned);
                output["name_filter"] = params
                    .name_filter
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .map(|name| serde_json::json!(name))
                    .unwrap_or(Value::Null);
                output["limit"] = serde_json::json!(limit);
                if truncated {
                    output["hint"] = serde_json::json!(format!(
                        "Returned {returned} of {matching_count} matching processes. Set name_filter or raise limit up to {MAX_LIST_LIMIT}; the output character budget may reduce the returned count."
                    ));
                }
                Ok(ToolResult::from_output(output, truncated))
            }
            ProcessOperation::Kill => {
                let raw_pid = params.pid.unwrap_or(0);
                if raw_pid <= 0 {
                    anyhow::bail!("valid pid is required");
                }
                let pid = raw_pid as u32;
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
                Ok(ToolResult::ok(
                    serde_json::json!({"operation": "kill", "killed": pid}),
                ))
            }
        }
    }
}

/// Keep process status values stable and finite across the dynamic ToolResult
/// boundary. `sysinfo::ProcessStatus::Unknown` carries a platform code, which
/// is intentionally normalized to the UI's generic unknown state.
fn process_status_output(status: sysinfo::ProcessStatus) -> &'static str {
    use sysinfo::ProcessStatus;

    match status {
        ProcessStatus::Idle => "Idle",
        ProcessStatus::Run => "Run",
        ProcessStatus::Sleep => "Sleep",
        ProcessStatus::Stop => "Stop",
        ProcessStatus::Zombie => "Zombie",
        ProcessStatus::Tracing => "Tracing",
        ProcessStatus::Dead => "Dead",
        ProcessStatus::Wakekill => "Wakekill",
        ProcessStatus::Waking => "Waking",
        ProcessStatus::Parked => "Parked",
        ProcessStatus::LockBlocked => "LockBlocked",
        ProcessStatus::UninterruptibleDiskSleep => "UninterruptibleDiskSleep",
        ProcessStatus::Suspended => "Suspended",
        ProcessStatus::Unknown(_) => "Unknown",
    }
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
        crate::prompts::PROCESS_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("kill") => RiskLevel::High,
            _ => RiskLevel::Low,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        match input["operation"].as_str() {
            Some("list") => ToolConcurrency::SharedResource("processes".into()),
            _ => ToolConcurrency::Resource("processes".into()),
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "kill"] }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "list" },
                        "name_filter": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "kill" }, "pid": { "type": "integer", "minimum": 1 } },
                    "required": ["operation", "pid"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `ProcessParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<ProcessParams>(&self.name(), input)?;
        self.run(params, cancel).await
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
    fn process_status_output_normalizes_platform_unknown_codes() {
        assert_eq!(
            process_status_output(sysinfo::ProcessStatus::Unknown(73)),
            "Unknown"
        );
        assert_eq!(
            process_status_output(sysinfo::ProcessStatus::UninterruptibleDiskSleep),
            "UninterruptibleDiskSleep"
        );
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
        assert!(ops.contains(&"kill"));
        assert!(
            ProcessTool::default()
                .validate_input(&json!({
                    "operation": "list",
                    "name_filter": "haven",
                    "limit": 20
                }))
                .is_ok()
        );
        assert!(
            ProcessTool::default()
                .validate_input(&json!({ "operation": "list", "limit": 201 }))
                .is_err()
        );
        assert!(
            ProcessTool::default()
                .validate_input(&json!({
                    "operation": "kill",
                    "pid": 1,
                    "name_filter": "haven"
                }))
                .is_err()
        );
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
            assert!(p["name"].as_str().is_some());
            assert!(p["memory"].is_number());
            assert!(p["cpu"].is_number());
            assert!(p["status"].is_string());
        }
        assert!(result.output["returned"].as_u64().unwrap() <= DEFAULT_LIST_LIMIT as u64);
    }

    #[tokio::test]
    async fn test_process_list_filters_by_name_case_insensitively() {
        let current_name = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let result = ProcessTool::default()
            .execute(
                json!({
                    "operation": "list",
                    "name_filter": current_name.to_ascii_uppercase(),
                    "limit": 200
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let processes = result.output["processes"].as_array().unwrap();
        assert!(!processes.is_empty());
        assert!(processes.iter().all(|process| {
            process["name"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains(&current_name.to_ascii_lowercase())
        }));
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
