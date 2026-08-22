use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::env::{EnvOperation, EnvParams, EnvTool};
use super::power::{PowerOperation, PowerParams, PowerTool};
use super::registry::{RegistryOperation, RegistryParams, RegistryTool};
use crate::{Tool, ToolResult};

/// Unified system tool: machine info, env vars, registry, power, displays.
pub struct SystemTool {
    pub max_output_chars: usize,
}

impl Default for SystemTool {
    fn default() -> Self {
        Self {
            max_output_chars: 20_000,
        }
    }
}

/// Typed parameters for `SystemTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `SystemTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SystemParams {
    /// Domain: info (default) | env | registry | power | display.
    #[serde(default)]
    pub scope: Option<String>,
    /// Info category when scope=info: overview/cpu/memory/disk/os/all.
    #[serde(default)]
    pub category: Option<String>,
    /// Sub-operation for env/registry/power.
    #[serde(default)]
    pub operation: Option<String>,
    /// Env var name, registry value name, etc.
    #[serde(default)]
    pub name: Option<String>,
    /// Env/registry value.
    #[serde(default)]
    pub value: Option<String>,
    /// Registry path.
    #[serde(default)]
    pub path: Option<String>,
    /// Registry value type.
    #[serde(default, rename = "type")]
    pub value_type: Option<String>,
}

impl SystemTool {
    /// Entry ①: structured native interface.
    pub async fn run(
        &self,
        params: SystemParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let scope = params
            .scope
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("info");

        match scope {
            "info" | "overview" => self.run_info(params.category, cancel).await,
            "env" => {
                let op = parse_env_op(params.operation.as_deref())?;
                EnvTool {
                    max_output_chars: self.max_output_chars,
                }
                .run(
                    EnvParams {
                        operation: Some(op),
                        name: params.name,
                        value: params.value,
                    },
                    cancel,
                )
                .await
            }
            "registry" => {
                let op = parse_registry_op(params.operation.as_deref())?;
                RegistryTool
                    .run(
                        RegistryParams {
                            operation: Some(op),
                            path: params.path,
                            name: params.name,
                            value: params.value,
                            value_type: params.value_type,
                        },
                        cancel,
                    )
                    .await
            }
            "power" => {
                let op = parse_power_op(params.operation.as_deref())?;
                PowerTool
                    .run(
                        PowerParams {
                            operation: Some(op),
                        },
                        cancel,
                    )
                    .await
            }
            "display" | "displays" => {
                let displays = tokio::task::spawn_blocking(list_displays).await??;
                Ok(ToolResult::ok(serde_json::json!({ "displays": displays })))
            }
            other => anyhow::bail!(
                "unknown scope '{}'; use info, env, registry, power, or display",
                other
            ),
        }
    }

    async fn run_info(
        &self,
        category: Option<String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let category = category
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "overview".to_string());

        let info = tokio::task::spawn_blocking(move || {
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

fn parse_env_op(op: Option<&str>) -> anyhow::Result<EnvOperation> {
    match op.unwrap_or("list") {
        "get" => Ok(EnvOperation::Get),
        "set" => Ok(EnvOperation::Set),
        "unset" => Ok(EnvOperation::Unset),
        "list" => Ok(EnvOperation::List),
        other => anyhow::bail!("unknown env operation '{}'", other),
    }
}

fn parse_registry_op(op: Option<&str>) -> anyhow::Result<RegistryOperation> {
    match op.unwrap_or("list") {
        "get" => Ok(RegistryOperation::Get),
        "set" => Ok(RegistryOperation::Set),
        "delete" => Ok(RegistryOperation::Delete),
        "list" => Ok(RegistryOperation::List),
        other => anyhow::bail!("unknown registry operation '{}'", other),
    }
}

fn parse_power_op(op: Option<&str>) -> anyhow::Result<PowerOperation> {
    match op.unwrap_or("status") {
        "status" => Ok(PowerOperation::Status),
        "lock" => Ok(PowerOperation::Lock),
        "sleep" => Ok(PowerOperation::Sleep),
        "hibernate" => Ok(PowerOperation::Hibernate),
        other => anyhow::bail!("unknown power operation '{}'", other),
    }
}

#[async_trait]
impl Tool for SystemTool {
    fn name(&self) -> String {
        "system".into()
    }
    fn description(&self) -> String {
        "System control and info. scope=info (default): CPU/memory/disk/OS. \
         scope=env: get/set/unset/list environment variables. \
         scope=registry: get/set/delete/list Windows Registry. \
         scope=power: status/lock/sleep/hibernate. \
         scope=display: list monitors."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        let scope = input["scope"].as_str().unwrap_or("info");
        // Match execution defaults: env/registry omit → list; power omit → status.
        let op = input["operation"].as_str().unwrap_or(match scope {
            "env" | "registry" => "list",
            "power" => "status",
            _ => "",
        });
        match scope {
            "env" => match op {
                "set" | "unset" | "list" => RiskLevel::High,
                _ => RiskLevel::Low,
            },
            "registry" => match op {
                "set" | "delete" => RiskLevel::High,
                _ => RiskLevel::Medium,
            },
            "power" => match op {
                "lock" | "sleep" | "hibernate" => RiskLevel::High,
                _ => RiskLevel::Safe,
            },
            _ => RiskLevel::Safe,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["info", "env", "registry", "power", "display"],
                    "default": "info",
                    "description": "Domain to operate on"
                },
                "category": {
                    "type": "string",
                    "enum": ["overview", "cpu", "memory", "disk", "os", "all"],
                    "default": "overview",
                    "description": "Info category when scope=info"
                },
                "operation": {
                    "type": "string",
                    "description": "Sub-op: env get/set/unset/list; registry get/set/delete/list; power status/lock/sleep/hibernate"
                },
                "name": { "type": "string", "description": "Env var or registry value name" },
                "value": { "type": "string", "description": "Value for env/registry set" },
                "path": { "type": "string", "description": "Registry path, e.g. HKCU:\\Software\\..." },
                "type": {
                    "type": "string",
                    "enum": ["String", "DWord", "QWord", "Binary", "MultiString", "ExpandString"],
                    "description": "Registry value type for set"
                }
            }
        })
    }

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

fn list_displays() -> anyhow::Result<Vec<Value>> {
    #[cfg(windows)]
    {
        display_imp::enumerate_displays()
    }
    #[cfg(not(windows))]
    {
        Ok(vec![serde_json::json!({
            "available": false,
            "note": "display enumeration requires Windows"
        })])
    }
}

#[cfg(windows)]
mod display_imp {
    use serde_json::Value;
    use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };

    pub fn enumerate_displays() -> anyhow::Result<Vec<Value>> {
        let mut displays: Vec<Value> = Vec::new();

        unsafe extern "system" fn callback(
            monitor: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            unsafe {
                let out = &mut *(lparam as *mut Vec<Value>);
                let mut info: MONITORINFOEXW = std::mem::zeroed();
                info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
                if GetMonitorInfoW(monitor, &mut info as *mut _ as *mut _) != 0 {
                    let rc = info.monitorInfo.rcMonitor;
                    let work = info.monitorInfo.rcWork;
                    let primary = (info.monitorInfo.dwFlags & 1) != 0;
                    let name = {
                        let len = info
                            .szDevice
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(info.szDevice.len());
                        String::from_utf16_lossy(&info.szDevice[..len])
                    };
                    out.push(serde_json::json!({
                        "name": name,
                        "primary": primary,
                        "left": rc.left,
                        "top": rc.top,
                        "right": rc.right,
                        "bottom": rc.bottom,
                        "width": rc.right - rc.left,
                        "height": rc.bottom - rc.top,
                        "work_left": work.left,
                        "work_top": work.top,
                        "work_right": work.right,
                        "work_bottom": work.bottom,
                    }));
                }
                TRUE
            }
        }

        unsafe {
            let ok = EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(callback),
                &mut displays as *mut _ as LPARAM,
            );
            if ok == 0 {
                anyhow::bail!("EnumDisplayMonitors failed");
            }
        }
        Ok(displays)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_system_tool_name() {
        assert_eq!(SystemTool::default().name(), "system");
    }

    #[test]
    fn test_system_tool_risk_level() {
        let tool = SystemTool::default();
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
        assert_eq!(
            tool.risk_level(&json!({"scope": "power", "operation": "lock"})),
            RiskLevel::High
        );
        assert_eq!(
            tool.risk_level(&json!({"scope": "env", "operation": "set"})),
            RiskLevel::High
        );
        // Omitted operation defaults to list for both risk and execution.
        assert_eq!(
            tool.risk_level(&json!({"scope": "env"})),
            RiskLevel::High
        );
    }

    #[test]
    fn test_system_tool_input_schema() {
        let schema = SystemTool::default().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let scopes = schema["properties"]["scope"]["enum"].as_array().unwrap();
        assert!(scopes.iter().any(|v| v == "env"));
        assert!(scopes.iter().any(|v| v == "display"));
    }

    #[tokio::test]
    async fn test_system_execute_os() {
        let result = SystemTool::default()
            .execute(json!({"category": "os"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["os"]["name"].is_string());
        assert!(result.output["os"]["hostname"].is_string());
        assert!(result.output.get("cpu").is_none());
    }

    #[tokio::test]
    async fn test_system_execute_default_overview() {
        let result = SystemTool::default()
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
    async fn test_system_execute_power_status() {
        let result = SystemTool::default()
            .execute(
                json!({"scope": "power", "operation": "status"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_system_execute_display() {
        let result = SystemTool::default()
            .execute(json!({"scope": "display"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["displays"].is_array());
    }

    #[tokio::test]
    async fn test_system_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = SystemTool::default()
            .execute(json!({"category": "os"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_system_native_entry_lands_in_run() {
        let result = SystemTool::default()
            .run(
                SystemParams {
                    scope: Some("info".into()),
                    category: Some("os".into()),
                    operation: None,
                    name: None,
                    value: None,
                    path: None,
                    value_type: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["os"]["hostname"].is_string());
    }
}
