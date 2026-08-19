use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct SystemTool;

/// Typed parameters for `SystemTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `SystemTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SystemParams {
    /// Category to query; unknown values fall back to the `overview` set.
    #[serde(default)]
    pub category: Option<String>,
}

impl SystemTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: SystemParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let category = params
            .category
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "overview".to_string());

        let info = tokio::task::spawn_blocking(move || {
            // Build only what the requested category needs: the OS info is
            // cheap, CPU/memory need a system snapshot, disk needs a refresh.
            let (os, cpu, memory) = match category.as_str() {
                "os" => (Some(os_info()), None, None),
                "cpu" => (None, Some(cpu_info()), None),
                "memory" => (None, None, Some(memory_info())),
                "disk" => (None, None, None),
                _ => {
                    let system = sysinfo::System::new_all();
                    (
                        Some(os_info()),
                        Some(cpu_info_from(&system)),
                        Some(memory_info_from(&system)),
                    )
                }
            };

            match category.as_str() {
                "cpu" => serde_json::json!({"cpu": cpu}),
                "memory" => serde_json::json!({"memory": memory}),
                "disk" => serde_json::json!({"disks": disk_info()}),
                "os" => serde_json::json!({"os": os}),
                _ => serde_json::json!({
                    "os": os,
                    "cpu": cpu,
                    "memory": memory,
                    "disks": disk_info(),
                }),
            }
        })
        .await?;

        Ok(ToolResult::ok(info))
    }
}

#[async_trait]
impl Tool for SystemTool {
    fn name(&self) -> String {
        "system".into()
    }
    fn description(&self) -> String {
        "Query system information: CPU, memory, disk usage, OS info, hostname, uptime".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "enum": ["overview", "cpu", "memory", "disk", "os", "all"],
                    "default": "overview"
                }
            }
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `SystemParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<SystemParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

fn os_info() -> Value {
    let (os_name, os_kernel, os_version, os_hostname, os_long) = {
        let n = sysinfo::System::name();
        let k = sysinfo::System::kernel_version();
        let o = sysinfo::System::os_version();
        let h = sysinfo::System::host_name();
        let l = sysinfo::System::long_os_version();
        (
            n.unwrap_or_default(),
            k.unwrap_or_default(),
            o.unwrap_or_default(),
            h.unwrap_or_default(),
            l.unwrap_or_default(),
        )
    };
    serde_json::json!({
        "name": os_name,
        "kernel": os_kernel,
        "os_version": os_version,
        "hostname": os_hostname,
        "long_version": os_long,
        "uptime_secs": sysinfo::System::uptime(),
        "boot_time_secs": sysinfo::System::boot_time(),
    })
}

fn cpu_info() -> Value {
    use sysinfo::{CpuRefreshKind, RefreshKind};
    // Only CPU data (incl. usage) — not the full process enumeration that
    // System::new_all() performs.
    let system = sysinfo::System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
    );
    cpu_info_from(&system)
}

fn cpu_info_from(system: &sysinfo::System) -> Value {
    let brand = system
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();
    serde_json::json!({
        "brand": brand,
        "cores": sysinfo::System::physical_core_count().unwrap_or(0),
        "logical_cpus": system.cpus().len(),
        "usage_pct": system.global_cpu_usage(),
    })
}

fn memory_info() -> Value {
    use sysinfo::{MemoryRefreshKind, RefreshKind};
    let system = sysinfo::System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
    );
    memory_info_from(&system)
}

fn memory_info_from(system: &sysinfo::System) -> Value {
    serde_json::json!({
        "total_bytes": system.total_memory(),
        "used_bytes": system.used_memory(),
        "total_swap_bytes": system.total_swap(),
        "used_swap_bytes": system.used_swap(),
    })
}

fn disk_info() -> Value {
    let mut disks_info = Vec::new();
    for d in sysinfo::Disks::new_with_refreshed_list().iter() {
        disks_info.push(serde_json::json!({
            "mount": d.mount_point().to_string_lossy(),
            "total_bytes": d.total_space(),
            "available_bytes": d.available_space(),
            "file_system": d.file_system().to_string_lossy().to_string(),
            "name": d.name().to_string_lossy(),
        }));
    }
    serde_json::json!(disks_info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_system_tool_name() {
        assert_eq!(SystemTool.name(), "system");
    }

    #[test]
    fn test_system_tool_risk_level() {
        assert_eq!(SystemTool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_system_tool_input_schema() {
        let schema = SystemTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["category"]["enum"].as_array().unwrap();
        let cats: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(cats.contains(&"overview"));
        assert!(cats.contains(&"cpu"));
        assert!(cats.contains(&"memory"));
        assert!(cats.contains(&"disk"));
        assert!(cats.contains(&"os"));
        assert!(cats.contains(&"all"));
    }

    #[tokio::test]
    async fn test_system_execute_os() {
        let result = SystemTool
            .execute(json!({"category": "os"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["os"]["name"].is_string());
        assert!(result.output["os"]["hostname"].is_string());
        assert!(result.output["os"]["uptime_secs"].is_number());
        assert!(result.output.get("cpu").is_none());
    }

    #[tokio::test]
    async fn test_system_execute_cpu() {
        let result = SystemTool
            .execute(json!({"category": "cpu"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["cpu"]["cores"].is_number());
        assert!(result.output["cpu"]["logical_cpus"].as_u64().unwrap() >= 1);
        assert!(result.output.get("memory").is_none());
    }

    #[tokio::test]
    async fn test_system_execute_memory() {
        let result = SystemTool
            .execute(json!({"category": "memory"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["memory"]["total_bytes"].as_u64().unwrap() > 0);
        assert!(result.output["memory"]["used_bytes"].is_number());
        assert!(result.output.get("os").is_none());
    }

    #[tokio::test]
    async fn test_system_execute_disk() {
        let result = SystemTool
            .execute(json!({"category": "disk"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        let disks = result.output["disks"].as_array().unwrap();
        assert!(!disks.is_empty());
        assert!(disks[0]["mount"].as_str().is_some());
        assert!(disks[0]["total_bytes"].is_number());
    }

    #[tokio::test]
    async fn test_system_execute_default_overview() {
        let result = SystemTool
            .execute(json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["os"].is_object());
        assert!(result.output["cpu"].is_object());
        assert!(result.output["memory"].is_object());
        assert!(result.output["disks"].is_array());
    }

    #[tokio::test]
    async fn test_system_execute_unknown_category_falls_back_to_overview() {
        let result = SystemTool
            .execute(json!({"category": "bogus"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["os"].is_object());
    }

    #[tokio::test]
    async fn test_system_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = SystemTool.execute(json!({"category": "os"}), cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_system_native_entry_lands_in_run() {
        let result = SystemTool
            .run(
                SystemParams {
                    category: Some("os".into()),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["os"]["hostname"].is_string());
    }
}
