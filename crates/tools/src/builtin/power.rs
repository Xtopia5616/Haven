use tokio_util::sync::CancellationToken;

use crate::ToolResult;

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

/// Typed parameters for `PowerTool`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PowerParams {
    /// Operation to perform; defaults to `status`.
    #[serde(default)]
    pub operation: Option<PowerOperation>,
}

impl PowerTool {
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

        // SYSTEM_POWER_STATUS battery life fields are seconds (0xFFFFFFFF = unknown).
        let lifetime_secs = if status.BatteryLifeTime != 0xFFFFFFFF {
            Some(status.BatteryLifeTime)
        } else {
            None
        };

        let full_lifetime_secs = if status.BatteryFullLifeTime != 0xFFFFFFFF {
            Some(status.BatteryFullLifeTime)
        } else {
            None
        };

        let battery_saver = (status.SystemStatusFlag & 1) != 0;
        let no_battery = (status.BatteryFlag & 128) != 0;

        Ok(serde_json::json!({
            "ac_power": ac_line,
            "battery_percent": battery_pct,
            "battery_status": battery_flag_str,
            "battery_present": !no_battery,
            "battery_saver": battery_saver,
            "battery_life_remaining_secs": lifetime_secs,
            "battery_full_lifetime_secs": full_lifetime_secs,
        }))
    }

    pub fn lock_workstation() -> anyhow::Result<()> {
        unsafe { LockWorkStation() };
        Ok(())
    }

    pub fn sleep() -> anyhow::Result<()> {
        let ret = unsafe { SetSuspendState(false, true, false) };
        if !ret {
            anyhow::bail!("SetSuspendState (sleep) failed");
        }
        Ok(())
    }

    pub fn hibernate() -> anyhow::Result<()> {
        let ret = unsafe { SetSuspendState(true, true, false) };
        if !ret {
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

    #[tokio::test]
    async fn test_power_run_status() {
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
    async fn test_power_run_default_status() {
        let result = PowerTool
            .run(PowerParams { operation: None }, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        #[cfg(windows)]
        {
            assert!(result.output["ac_power"].as_str().is_some());
        }
    }

    #[tokio::test]
    async fn test_power_run_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = PowerTool
            .run(
                PowerParams {
                    operation: Some(PowerOperation::Status),
                },
                cancel,
            )
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
