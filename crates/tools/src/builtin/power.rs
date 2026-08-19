use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct PowerTool;

/// Power operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerOperation {
    Status,
    Lock,
    Sleep,
    Hibernate,
}

/// Typed parameters for `PowerTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `PowerTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PowerParams {
    /// Operation to perform; defaults to `status`.
    #[serde(default)]
    pub operation: Option<PowerOperation>,
}

impl PowerTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: PowerParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        match params.operation.unwrap_or(PowerOperation::Status) {
            PowerOperation::Status => {
                let status = imp::get_power_status()?;
                Ok(ToolResult::ok(status))
            }
            PowerOperation::Lock => {
                imp::lock_workstation()?;
                Ok(ToolResult::ok(serde_json::json!({"locked": true})))
            }
            PowerOperation::Sleep => {
                imp::sleep()?;
                Ok(ToolResult::ok(serde_json::json!({"sleep": true})))
            }
            PowerOperation::Hibernate => {
                imp::hibernate()?;
                Ok(ToolResult::ok(serde_json::json!({"hibernate": true})))
            }
        }
    }
}

#[async_trait]
impl Tool for PowerTool {
    fn name(&self) -> String {
        "power".into()
    }
    fn description(&self) -> String {
        "Lock, sleep, hibernate, or query battery status".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("lock") | Some("sleep") | Some("hibernate") => RiskLevel::High,
            _ => RiskLevel::Safe,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["status", "lock", "sleep", "hibernate"] }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `PowerParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<PowerParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(windows)]
mod imp {
    use serde_json::Value;
    use windows_sys::Win32::System::Power::{
        GetSystemPowerStatus, SYSTEM_POWER_STATUS, SetSuspendState,
    };

    #[link(name = "user32")]
    unsafe extern "system" {
        fn LockWorkStation() -> i32;
    }

    pub fn get_power_status() -> anyhow::Result<Value> {
        let mut status = SYSTEM_POWER_STATUS {
            ACLineStatus: 0,
            BatteryFlag: 0,
            BatteryLifePercent: 0,
            SystemStatusFlag: 0,
            BatteryLifeTime: 0,
            BatteryFullLifeTime: 0,
        };

        let ret = unsafe { GetSystemPowerStatus(&mut status) };
        if ret == 0 {
            anyhow::bail!("GetSystemPowerStatus failed");
        }

        let ac_line = match status.ACLineStatus {
            0 => "offline",
            1 => "online",
            _ => "unknown",
        };

        let battery_pct = if status.BatteryLifePercent <= 100 {
            Some(status.BatteryLifePercent)
        } else {
            None
        };

        let battery_flag_str = match status.BatteryFlag & 0x0f {
            1 => "high",
            2 => "low",
            4 => "critical",
            8 => "charging",
            _ => "unknown",
        };

        let lifetime_min = if status.BatteryLifeTime != 0xFFFFFFFF {
            Some(status.BatteryLifeTime)
        } else {
            None
        };

        let full_lifetime_min = if status.BatteryFullLifeTime != 0xFFFFFFFF {
            Some(status.BatteryFullLifeTime)
        } else {
            None
        };

        Ok(serde_json::json!({
            "ac_power": ac_line,
            "battery_percent": battery_pct,
            "battery_status": battery_flag_str,
            "battery_life_remaining_minutes": lifetime_min,
            "battery_full_lifetime_minutes": full_lifetime_min,
        }))
    }

    pub fn lock_workstation() -> anyhow::Result<()> {
        unsafe { LockWorkStation() };
        Ok(())
    }

    pub fn sleep() -> anyhow::Result<()> {
        let ret = unsafe { SetSuspendState(0, 1, 0) };
        if ret == 0 {
            anyhow::bail!("SetSuspendState (sleep) failed");
        }
        Ok(())
    }

    pub fn hibernate() -> anyhow::Result<()> {
        let ret = unsafe { SetSuspendState(1, 1, 0) };
        if ret == 0 {
            anyhow::bail!("SetSuspendState (hibernate) failed");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use serde_json::Value;

    pub fn get_power_status() -> anyhow::Result<Value> {
        Ok(serde_json::json!({"available": false, "note": "power management requires Windows"}))
    }

    pub fn lock_workstation() -> anyhow::Result<()> {
        anyhow::bail!("power management requires Windows")
    }

    pub fn sleep() -> anyhow::Result<()> {
        anyhow::bail!("power management requires Windows")
    }

    pub fn hibernate() -> anyhow::Result<()> {
        anyhow::bail!("power management requires Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_power_tool_name() {
        assert_eq!(PowerTool.name(), "power");
    }

    #[test]
    fn test_power_tool_risk_level() {
        assert_eq!(
            PowerTool.risk_level(&json!({"operation": "status"})),
            RiskLevel::Safe
        );
        assert_eq!(
            PowerTool.risk_level(&json!({"operation": "lock"})),
            RiskLevel::High
        );
        assert_eq!(
            PowerTool.risk_level(&json!({"operation": "sleep"})),
            RiskLevel::High
        );
        assert_eq!(
            PowerTool.risk_level(&json!({"operation": "hibernate"})),
            RiskLevel::High
        );
    }

    #[test]
    fn test_power_tool_input_schema() {
        let schema = PowerTool.input_schema();
        assert!(
            schema["properties"]["operation"]["enum"]
                .as_array()
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_power_execute_status() {
        let result = PowerTool
            .execute(json!({"operation": "status"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        #[cfg(windows)]
        {
            assert!(result.output["ac_power"].as_str().is_some());
            assert!(
                result.output["battery_percent"].is_null()
                    || result.output["battery_percent"].is_number()
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(result.output["available"], false);
        }
    }

    #[tokio::test]
    async fn test_power_execute_default_status() {
        let result = PowerTool
            .execute(json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        #[cfg(windows)]
        {
            assert!(result.output["ac_power"].as_str().is_some());
        }
    }

    #[tokio::test]
    async fn test_power_execute_unknown_operation() {
        let result = PowerTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_power_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = PowerTool
            .execute(json!({"operation": "status"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_power_native_entry_lands_in_run() {
        let result = PowerTool
            .run(
                PowerParams {
                    operation: Some(PowerOperation::Status),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
    }
}
