use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct AudioTool;

#[async_trait]
impl Tool for AudioTool {
    fn name(&self) -> String {
        "audio".into()
    }
    fn description(&self) -> String {
        "Control audio: list playback/capture devices, get/set volume (0-100), get/set mute".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("volume") | Some("mute") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "volume", "mute"] },
                "value": { "description": "Volume 0-100 (number), mute (boolean). Omit to query current value." },
                "device": { "type": "string", "description": "Target device ID (optional, defaults to default playback)" }
            },
            "required": ["operation"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let op = input["operation"].as_str().unwrap_or("list").to_string();

        match op.as_str() {
            "list" => {
                let devices = imp::list_devices()?;
                Ok(ToolResult::ok(serde_json::json!({"devices": devices})))
            }
            "volume" => {
                if let Some(val) = input["value"].as_f64() {
                    imp::set_volume(val as f32 / 100.0)?;
                    Ok(ToolResult::ok(serde_json::json!({"volume": val as u32, "set": true})))
                } else {
                    let vol = imp::get_volume()?;
                    Ok(ToolResult::ok(
                        serde_json::json!({"volume": (vol * 100.0).round() as u32}),
                    ))
                }
            }
            "mute" => {
                if let Some(muted) = input["value"].as_bool() {
                    imp::set_mute(muted)?;
                    Ok(ToolResult::ok(serde_json::json!({"muted": muted, "set": true})))
                } else {
                    let muted = imp::get_mute()?;
                    Ok(ToolResult::ok(serde_json::json!({"muted": muted})))
                }
            }
            _ => anyhow::bail!("unknown audio operation: {}", op),
        }
    }
}

#[cfg(windows)]
mod imp {
    use serde_json::Value;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::System::Com::*;

    fn get_endpoint_volume() -> anyhow::Result<IAudioEndpointVolume> {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)? };

        let device: IMMDevice = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };

        let ep: IAudioEndpointVolume =
            unsafe { device.Activate(CLSCTX_INPROC_SERVER, None)? };

        Ok(ep)
    }

    pub fn list_devices() -> anyhow::Result<Vec<Value>> {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)? };

        let mut all_devices = Vec::new();

        for &(flow, flow_name) in &[(eRender, "playback"), (eCapture, "capture")] {
            let collection: IMMDeviceCollection =
                unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)? };

            let count = unsafe { collection.GetCount()? };

            let default_id = unsafe {
                enumerator
                    .GetDefaultAudioEndpoint(flow, eConsole)
                    .ok()
                    .and_then(|dev| {
                        dev.GetId().ok()
                            .and_then(|id| id.to_string().ok())
                    })
                    .unwrap_or_default()
            };

            for i in 0..count {
                let device: IMMDevice = unsafe { collection.Item(i)? };

                let id = unsafe { device.GetId()? };
                let id_str = unsafe { id.to_string().unwrap_or_default() };

                all_devices.push(serde_json::json!({
                    "id": id_str,
                    "flow": flow_name,
                    "is_default": id_str == default_id,
                }));
            }
        }

        Ok(all_devices)
    }

    pub fn get_volume() -> anyhow::Result<f32> {
        let ep = get_endpoint_volume()?;
        let level: f32 = unsafe { ep.GetMasterVolumeLevelScalar()? };
        Ok(level)
    }

    pub fn set_volume(level: f32) -> anyhow::Result<()> {
        let level = level.clamp(0.0, 1.0);
        let ep = get_endpoint_volume()?;
        unsafe { ep.SetMasterVolumeLevelScalar(level, &Default::default())?; }
        Ok(())
    }

    pub fn get_mute() -> anyhow::Result<bool> {
        let ep = get_endpoint_volume()?;
        let muted: BOOL = unsafe { ep.GetMute()? };
        Ok(muted != BOOL(0))
    }

    pub fn set_mute(muted: bool) -> anyhow::Result<()> {
        let ep = get_endpoint_volume()?;
        unsafe { ep.SetMute(muted, &Default::default())?; }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use serde_json::Value;

    pub fn list_devices() -> anyhow::Result<Vec<Value>> {
        Ok(Vec::new())
    }

    pub fn get_volume() -> anyhow::Result<f32> {
        Ok(0.0)
    }

    pub fn set_volume(_level: f32) -> anyhow::Result<()> {
        anyhow::bail!("audio control requires Windows")
    }

    pub fn get_mute() -> anyhow::Result<bool> {
        Ok(false)
    }

    pub fn set_mute(_muted: bool) -> anyhow::Result<()> {
        anyhow::bail!("audio control requires Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_audio_tool_name() {
        assert_eq!(AudioTool.name(), "audio");
    }

    #[test]
    fn test_audio_tool_risk_level() {
        assert_eq!(
            AudioTool.risk_level(&json!({"operation": "list"})),
            RiskLevel::Low
        );
        assert_eq!(
            AudioTool.risk_level(&json!({"operation": "volume"})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_audio_tool_input_schema() {
        let schema = AudioTool.input_schema();
        assert!(schema["properties"]["operation"]["enum"]
            .as_array()
            .is_some());
    }
}
