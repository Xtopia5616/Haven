use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct SystemInfoTool;

#[async_trait]
impl Tool for SystemInfoTool {
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

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        let category = input["category"].as_str().unwrap_or("overview").to_string();

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
        }).await?;

        Ok(ToolResult::ok(info))
    }
}

fn os_info() -> Value {
    let (os_name, os_kernel, os_version, os_hostname, os_long) = {
        let n = sysinfo::System::name();
        let k = sysinfo::System::kernel_version();
        let o = sysinfo::System::os_version();
        let h = sysinfo::System::host_name();
        let l = sysinfo::System::long_os_version();
        (n.unwrap_or_default(), k.unwrap_or_default(), o.unwrap_or_default(), h.unwrap_or_default(), l.unwrap_or_default())
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
    cpu_info_from(&sysinfo::System::new_all())
}

fn cpu_info_from(system: &sysinfo::System) -> Value {
    let brand = system.cpus().first().map(|c| c.brand().to_string()).unwrap_or_default();
    serde_json::json!({
        "brand": brand,
        "cores": system.physical_core_count().unwrap_or(0),
        "logical_cpus": system.cpus().len(),
        "usage_pct": system.global_cpu_usage(),
    })
}

fn memory_info() -> Value {
    memory_info_from(&sysinfo::System::new_all())
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
        assert_eq!(SystemInfoTool.name(), "system");
    }

    #[test]
    fn test_system_tool_risk_level() {
        assert_eq!(SystemInfoTool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_system_tool_input_schema() {
        let schema = SystemInfoTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
    }
}
