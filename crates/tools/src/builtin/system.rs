use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::env::{EnvOperation, EnvParams, EnvTool};
use super::power::{PowerOperation, PowerParams, PowerTool};
use super::registry::{RegistryOperation, RegistryParams, RegistryTool};
use crate::{Tool, ToolConcurrency, ToolResult};

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
    /// Info category when scope=info:
    /// overview/cpu/memory/disk/os/network/user/locale/all.
    #[serde(default)]
    pub category: Option<String>,
    /// Sub-operation for env/registry/power.
    #[serde(default)]
    pub operation: Option<String>,
    /// Env var name (or list prefix filter), registry value name, etc.
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
        let max_chars = self.max_output_chars;

        let info = tokio::task::spawn_blocking(move || collect_info(&category, max_chars)).await?;

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
        "System control and info. scope=info (default): machine snapshot — \
         category=overview|os|cpu|memory|disk|network|user|locale|all. \
         scope=env: get/set/unset/list (list accepts name as prefix filter). \
         scope=registry: get/set/delete/list Windows Registry. \
         scope=power: status/lock/sleep/hibernate. \
         scope=display: monitors with geometry, DPI/scale, refresh rate."
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
                // Lock/sleep are disruptive but recoverable; treat hibernate
                // as Critical so "Critical only" still gates the most severe
                // power action.
                "hibernate" => RiskLevel::Critical,
                "lock" | "sleep" => RiskLevel::High,
                _ => RiskLevel::Safe,
            },
            _ => RiskLevel::Safe,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        let scope = input["scope"].as_str().unwrap_or("info");
        let operation = input["operation"].as_str();
        match scope {
            "info" | "overview" | "display" | "displays" => {
                ToolConcurrency::SharedResource("system:info".into())
            }
            "env" if matches!(operation, None | Some("get") | Some("list")) => {
                ToolConcurrency::SharedResource("system:env".into())
            }
            "registry" if matches!(operation, None | Some("get") | Some("list")) => {
                ToolConcurrency::SharedResource("system:registry".into())
            }
            "power" if operation.is_none() || operation == Some("status") => {
                ToolConcurrency::SharedResource("system:power".into())
            }
            "env" => ToolConcurrency::Resource("system:env".into()),
            "registry" => ToolConcurrency::Resource("system:registry".into()),
            "power" => ToolConcurrency::Resource("system:power".into()),
            _ => ToolConcurrency::Exclusive,
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
                    "enum": ["overview", "cpu", "memory", "disk", "os", "network", "user", "locale", "all"],
                    "default": "overview",
                    "description": "Info category when scope=info"
                },
                "operation": {
                    "type": "string",
                    "description": "Sub-op: env get/set/unset/list; registry get/set/delete/list; power status/lock/sleep/hibernate"
                },
                "name": {
                    "type": "string",
                    "description": "Env var or registry value name; for env list, optional case-insensitive prefix filter"
                },
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
        let params = crate::tool_contract::parse_tool_input::<SystemParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

fn collect_info(category: &str, max_chars: usize) -> Value {
    match category {
        "os" => serde_json::json!({ "os": os_info() }),
        "cpu" => serde_json::json!({ "cpu": cpu_info(true, true) }),
        "memory" => serde_json::json!({ "memory": memory_info() }),
        "disk" => serde_json::json!({ "disks": disk_info() }),
        "network" => network_info_budgeted(max_chars),
        "user" => serde_json::json!({ "user": user_info(true) }),
        "locale" => serde_json::json!({ "locale": locale_info() }),
        "all" => {
            let mut out = serde_json::json!({
                "os": os_info(),
                "user": user_info(true),
                "locale": locale_info(),
                "cpu": cpu_info(true, true),
                "memory": memory_info(),
                "disks": disk_info(),
            });
            let networks = network_info_budgeted(max_chars);
            if let Some(obj) = out.as_object_mut() {
                for (k, v) in networks
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                {
                    obj.insert(k, v);
                }
            }
            out
        }
        // overview (default) and unknown → compact snapshot (no CPU sample sleep)
        _ => serde_json::json!({
            "os": os_info(),
            "user": user_info(false),
            "locale": locale_info(),
            "cpu": cpu_info(false, false),
            "memory": memory_info(),
            "disks": disk_info(),
            "network_summary": network_summary(),
        }),
    }
}

fn os_info() -> Value {
    serde_json::json!({
        "name": sysinfo::System::name().unwrap_or_default(),
        "long_version": sysinfo::System::long_os_version().unwrap_or_default(),
        "os_version": sysinfo::System::os_version().unwrap_or_default(),
        "kernel": sysinfo::System::kernel_version().unwrap_or_default(),
        "kernel_long": sysinfo::System::kernel_long_version(),
        "distribution_id": sysinfo::System::distribution_id(),
        "hostname": sysinfo::System::host_name().unwrap_or_default(),
        "arch": sysinfo::System::cpu_arch(),
        "product_vendor": sysinfo::Product::vendor_name().unwrap_or_default(),
        "product_name": sysinfo::Product::name().unwrap_or_default(),
        "uptime_secs": sysinfo::System::uptime(),
        "boot_time_secs": sysinfo::System::boot_time(),
    })
}

fn cpu_info(detailed: bool, sample_usage: bool) -> Value {
    use sysinfo::{CpuRefreshKind, RefreshKind};
    let mut system = sysinfo::System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
    );
    // Second sample so usage_pct is meaningful. Skip on overview — the sleep
    // was a fixed ~120ms stall on every default system info call.
    if sample_usage {
        std::thread::sleep(std::time::Duration::from_millis(120));
        system.refresh_cpu_all();
    }
    let mut out = cpu_info_from(&system, detailed);
    if !sample_usage && let Some(obj) = out.as_object_mut() {
        obj.remove("usage_pct");
    }
    out
}

fn cpu_info_from(system: &sysinfo::System, detailed: bool) -> Value {
    let first = system.cpus().first();
    let brand = first.map(|c| c.brand().to_string()).unwrap_or_default();
    let vendor_id = first.map(|c| c.vendor_id().to_string()).unwrap_or_default();
    let frequency_mhz = first.map(|c| c.frequency()).unwrap_or(0);
    let mut out = serde_json::json!({
        "brand": brand,
        "vendor_id": vendor_id,
        "frequency_mhz": frequency_mhz,
        "cores": sysinfo::System::physical_core_count().unwrap_or(0),
        "logical_cpus": system.cpus().len(),
        "usage_pct": system.global_cpu_usage(),
    });
    if detailed {
        let per_core: Vec<Value> = system
            .cpus()
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name(),
                    "usage_pct": c.cpu_usage(),
                    "frequency_mhz": c.frequency(),
                })
            })
            .collect();
        out["per_core"] = Value::Array(per_core);
    }
    out
}

fn memory_info() -> Value {
    use sysinfo::{MemoryRefreshKind, RefreshKind};
    let system = sysinfo::System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
    );
    memory_info_from(&system)
}

fn memory_info_from(system: &sysinfo::System) -> Value {
    let total = system.total_memory();
    let used = system.used_memory();
    let usage_pct = if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) * 100.0
    };
    serde_json::json!({
        "total_bytes": total,
        "used_bytes": used,
        "available_bytes": system.available_memory(),
        "free_bytes": system.free_memory(),
        "usage_pct": (usage_pct * 10.0).round() / 10.0,
        "total_swap_bytes": system.total_swap(),
        "used_swap_bytes": system.used_swap(),
        "free_swap_bytes": system.free_swap(),
    })
}

fn disk_info() -> Value {
    let mut disks_info = Vec::new();
    for d in sysinfo::Disks::new_with_refreshed_list().iter() {
        let total = d.total_space();
        let available = d.available_space();
        let used = total.saturating_sub(available);
        let used_pct = if total == 0 {
            0.0
        } else {
            (used as f64 / total as f64) * 100.0
        };
        disks_info.push(serde_json::json!({
            "mount": d.mount_point().to_string_lossy(),
            "name": d.name().to_string_lossy(),
            "file_system": d.file_system().to_string_lossy().to_string(),
            "kind": d.kind().to_string(),
            "total_bytes": total,
            "available_bytes": available,
            "used_bytes": used,
            "used_pct": (used_pct * 10.0).round() / 10.0,
            "is_removable": d.is_removable(),
            "is_read_only": d.is_read_only(),
        }));
    }
    serde_json::json!(disks_info)
}

fn network_summary() -> Value {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut up = 0usize;
    let mut down = 0usize;
    for data in networks.list().values() {
        match data.operational_state() {
            sysinfo::InterfaceOperationalState::Up
            | sysinfo::InterfaceOperationalState::Dormant
            | sysinfo::InterfaceOperationalState::Unknown => up += 1,
            _ => down += 1,
        }
    }
    serde_json::json!({
        "interface_count": networks.list().len(),
        "up_or_unknown": up,
        "down": down,
        "hint": "Use category=network for per-interface details"
    })
}

fn network_info_budgeted(max_chars: usize) -> Value {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut items: Vec<Value> = networks
        .list()
        .iter()
        .map(|(name, data)| {
            let ips: Vec<String> = data.ip_networks().iter().map(|ip| ip.to_string()).collect();
            serde_json::json!({
                "name": name,
                "mac": data.mac_address().to_string(),
                "ips": ips,
                "mtu": data.mtu(),
                "state": data.operational_state().to_string(),
                "total_received_bytes": data.total_received(),
                "total_transmitted_bytes": data.total_transmitted(),
            })
        })
        .collect();
    // Prefer named / non-empty MAC interfaces first; keep unspecified last.
    items.sort_by(|a, b| {
        let a_mac = a["mac"].as_str().unwrap_or("00:00:00:00:00:00");
        let b_mac = b["mac"].as_str().unwrap_or("00:00:00:00:00:00");
        let a_unspec = a_mac == "00:00:00:00:00:00";
        let b_unspec = b_mac == "00:00:00:00:00:00";
        a_unspec.cmp(&b_unspec).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        })
    });
    let count = items.len();
    let (mut result, truncated) =
        crate::util::json_list_within_budget("networks", items, count, max_chars);
    if truncated {
        result["hint"] = serde_json::json!(
            "Network listing truncated to the max chars budget. Prefer category=network alone."
        );
    }
    result
}

fn user_info(detailed: bool) -> Value {
    let username = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let mut out = serde_json::json!({
        "username": username,
        "user_domain": std::env::var("USERDOMAIN").unwrap_or_default(),
        "computer_name": std::env::var("COMPUTERNAME")
            .or_else(|_| sysinfo::System::host_name().ok_or(std::env::VarError::NotPresent))
            .unwrap_or_default(),
        "home": std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default(),
        "temp_dir": std::env::temp_dir().to_string_lossy(),
        "cwd": std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    });
    if detailed {
        let users = sysinfo::Users::new_with_refreshed_list();
        let names: Vec<String> = users.list().iter().map(|u| u.name().to_string()).collect();
        out["local_users"] = Value::Array(names.into_iter().map(Value::String).collect());
        out["local_user_count"] = Value::from(users.list().len());
    }
    out
}

fn locale_info() -> Value {
    let now = chrono::Local::now();
    let offset = now.offset().local_minus_utc();
    let offset_hours = offset as f64 / 3600.0;
    serde_json::json!({
        "timezone_offset_hours": (offset_hours * 100.0).round() / 100.0,
        "timezone_offset_secs": offset,
        "local_time": now.to_rfc3339(),
        "utc_time": chrono::Utc::now().to_rfc3339(),
        "locale_name": locale_name(),
        "ui_language": ui_language(),
    })
}

fn locale_name() -> String {
    #[cfg(windows)]
    {
        locale_imp::user_locale_name().unwrap_or_else(|| {
            std::env::var("LANG")
                .or_else(|_| std::env::var("LC_ALL"))
                .unwrap_or_default()
        })
    }
    #[cfg(not(windows))]
    {
        std::env::var("LANG")
            .or_else(|_| std::env::var("LC_ALL"))
            .unwrap_or_default()
    }
}

fn ui_language() -> String {
    #[cfg(windows)]
    {
        locale_imp::user_ui_language().unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
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
mod locale_imp {
    use windows_sys::Win32::Globalization::{
        GetUserDefaultLocaleName, GetUserDefaultUILanguage, LCIDToLocaleName,
    };

    /// MSDN `LOCALE_NAME_MAX_LENGTH` (not always exported by windows-sys).
    const LOCALE_NAME_MAX_LENGTH: usize = 85;

    pub fn user_locale_name() -> Option<String> {
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if len <= 1 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..(len as usize - 1)]))
    }

    pub fn user_ui_language() -> Option<String> {
        let langid = unsafe { GetUserDefaultUILanguage() } as u32;
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        let len = unsafe { LCIDToLocaleName(langid, buf.as_mut_ptr(), buf.len() as i32, 0) };
        if len <= 1 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..(len as usize - 1)]))
    }
}

#[cfg(windows)]
mod display_imp {
    use serde_json::Value;
    use windows_sys::Win32::Foundation::{LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateDCW, DEVMODEW, DeleteDC, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors,
        EnumDisplaySettingsW, GetDeviceCaps, GetMonitorInfoW, HDC, HMONITOR, LOGPIXELSX,
        MONITORINFOEXW,
    };
    use windows_sys::core::BOOL;

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

                    let (dpi, scale_pct) = dpi_for_device(&info.szDevice);
                    let (refresh_hz, bits_per_pel) = display_mode(&info.szDevice);

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
                        "dpi": dpi,
                        "scale_pct": scale_pct,
                        "refresh_hz": refresh_hz,
                        "bits_per_pel": bits_per_pel,
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

    unsafe fn dpi_for_device(device: &[u16; 32]) -> (u32, u32) {
        unsafe {
            // CreateDCW(L"DISPLAY", deviceName, …) is the documented form for monitors.
            let driver: Vec<u16> = "DISPLAY\0".encode_utf16().collect();
            let hdc = CreateDCW(
                driver.as_ptr(),
                device.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            );
            if hdc.is_null() {
                return (96, 100);
            }
            let dpi = GetDeviceCaps(hdc, LOGPIXELSX as i32) as u32;
            DeleteDC(hdc);
            let scale = if dpi == 0 { 100 } else { (dpi * 100) / 96 };
            (dpi.max(1), scale)
        }
    }

    unsafe fn display_mode(device: &[u16; 32]) -> (Option<u32>, Option<u32>) {
        unsafe {
            let mut mode: DEVMODEW = std::mem::zeroed();
            mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            let ok = EnumDisplaySettingsW(device.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode);
            if ok == 0 {
                return (None, None);
            }
            let refresh = if mode.dmDisplayFrequency > 1 {
                Some(mode.dmDisplayFrequency)
            } else {
                None
            };
            let bpp = if mode.dmBitsPerPel > 0 {
                Some(mode.dmBitsPerPel)
            } else {
                None
            };
            (refresh, bpp)
        }
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
            tool.risk_level(&json!({"scope": "power", "operation": "hibernate"})),
            RiskLevel::Critical
        );
        assert_eq!(
            tool.risk_level(&json!({"scope": "env", "operation": "set"})),
            RiskLevel::High
        );
        // Omitted operation defaults to list for both risk and execution.
        assert_eq!(tool.risk_level(&json!({"scope": "env"})), RiskLevel::High);
    }

    #[test]
    fn test_system_tool_input_schema() {
        let schema = SystemTool::default().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let scopes = schema["properties"]["scope"]["enum"].as_array().unwrap();
        assert!(scopes.iter().any(|v| v == "env"));
        assert!(scopes.iter().any(|v| v == "display"));
        let cats = schema["properties"]["category"]["enum"].as_array().unwrap();
        assert!(cats.iter().any(|v| v == "network"));
        assert!(cats.iter().any(|v| v == "user"));
        assert!(cats.iter().any(|v| v == "locale"));
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
        assert!(result.output["os"]["arch"].is_string());
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
        assert!(result.output["user"].is_object());
        assert!(result.output["locale"].is_object());
        assert!(result.output["cpu"].is_object());
        assert!(result.output["memory"].is_object());
        assert!(result.output["disks"].is_array());
        assert!(result.output["network_summary"].is_object());
        assert!(result.output["memory"]["available_bytes"].is_number());
        assert!(result.output["disks"].as_array().unwrap()[0]["kind"].is_string());
    }

    #[tokio::test]
    async fn test_system_execute_network() {
        let result = SystemTool::default()
            .execute(json!({"category": "network"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["networks"].is_array());
        assert!(result.output["count"].is_number());
    }

    #[tokio::test]
    async fn test_system_execute_user_and_locale() {
        let user = SystemTool::default()
            .execute(json!({"category": "user"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(user.success);
        assert!(user.output["user"]["username"].is_string());
        assert!(user.output["user"]["local_users"].is_array());

        let locale = SystemTool::default()
            .execute(json!({"category": "locale"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(locale.success);
        assert!(locale.output["locale"]["local_time"].is_string());
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
        assert!(result.output.get("ac_power").is_some());
    }

    #[tokio::test]
    async fn test_system_execute_display() {
        let result = SystemTool::default()
            .execute(json!({"scope": "display"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["displays"].is_array());
        if let Some(first) = result.output["displays"].as_array().and_then(|a| a.first()) {
            assert!(first.get("dpi").is_some() || first.get("available") == Some(&json!(false)));
        }
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
