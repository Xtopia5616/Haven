use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Simulate keyboard and mouse input on the local desktop: type text
/// (Unicode-safe), press named keys or chords (ctrl+c), click/move/scroll the
/// mouse. Everything goes through SendInput, which behaves like real input
/// from the OS perspective. Requires a desktop session — headless/CI runs
/// will error.
pub struct InputTool;

#[async_trait]
impl Tool for InputTool {
    fn name(&self) -> String {
        "input".into()
    }

    fn description(&self) -> String {
        "Simulate keyboard/mouse input on the desktop: type, \
         key, click, \
         move, scroll. Coordinates are screen \
         pixels from the top-left."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            // move/scroll only steer the cursor/wheel — no click-through.
            Some("move") | Some("scroll") => RiskLevel::Low,
            // typing and clicking act on whatever is in focus: worth a confirm.
            _ => RiskLevel::Medium,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["type", "key", "click", "move", "scroll"],
                    "description": "What to send"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (type only; supports any Unicode)"
                },
                "key": {
                    "type": "string",
                    "description": "Key name or chord (enter, esc, tab, ctrl+c)"
                },
                "x": {
                    "type": "integer",
                    "description": "Screen x in pixels (click/move)"
                },
                "y": {
                    "type": "integer",
                    "description": "Screen y in pixels (click/move)"
                },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Mouse button (click only; default left)"
                },
                "delta": {
                    "type": "integer",
                    "description": "Wheel steps (scroll only; positive = up/away, negative = down/toward)"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let op = input["operation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("operation is required (type, key, click, move, scroll)"))?;
        let result = match op {
            "type" => {
                let text = input["text"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("text is required for type"))?;
                let chars = text.chars().count();
                imp::type_text(text)?;
                serde_json::json!({ "typed": text, "chars": chars })
            }
            "key" => {
                let key = input["key"]
                    .as_str()
                    .filter(|k| !k.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("key is required for key"))?;
                imp::press_key(key)?;
                serde_json::json!({ "pressed": key })
            }
            "click" => {
                let x = input["x"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("x is required for click"))?;
                let y = input["y"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("y is required for click"))?;
                let button = input["button"].as_str().unwrap_or("left");
                imp::click(x, y, button)?;
                serde_json::json!({ "clicked": [x, y], "button": button })
            }
            "move" => {
                let x = input["x"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("x is required for move"))?;
                let y = input["y"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("y is required for move"))?;
                imp::move_to(x, y)?;
                serde_json::json!({ "moved_to": [x, y] })
            }
            "scroll" => {
                let delta = input["delta"].as_i64().unwrap_or(1).clamp(-100, 100);
                imp::scroll(delta)?;
                serde_json::json!({ "scrolled": delta })
            }
            _ => anyhow::bail!("unknown input operation: {}", op),
        };
        Ok(ToolResult::ok(result))
    }
}

#[cfg(windows)]
mod imp {
    use anyhow::anyhow;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VK_RETURN, VK_TAB, VK_ESCAPE, VK_BACK,
        VK_SPACE, VK_DELETE, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_LEFT, VK_RIGHT, VK_UP,
        VK_DOWN, VK_CAPITAL, VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_F1,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    /// Named-key -> VK map (single keys; chords are handled separately).
    fn vk_map() -> &'static HashMap<&'static str, u16> {
        static MAP: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();
        MAP.get_or_init(|| {
            let mut m = HashMap::new();
            m.insert("enter", VK_RETURN);
            m.insert("return", VK_RETURN);
            m.insert("tab", VK_TAB);
            m.insert("esc", VK_ESCAPE);
            m.insert("escape", VK_ESCAPE);
            m.insert("backspace", VK_BACK);
            m.insert("space", VK_SPACE);
            m.insert("delete", VK_DELETE);
            m.insert("home", VK_HOME);
            m.insert("end", VK_END);
            m.insert("pageup", VK_PRIOR);
            m.insert("pagedown", VK_NEXT);
            m.insert("left", VK_LEFT);
            m.insert("right", VK_RIGHT);
            m.insert("up", VK_UP);
            m.insert("down", VK_DOWN);
            m.insert("capslock", VK_CAPITAL);
            m.insert("shift", VK_SHIFT);
            m.insert("ctrl", VK_CONTROL);
            m.insert("control", VK_CONTROL);
            m.insert("alt", VK_MENU);
            m.insert("win", VK_LWIN);
            m.insert("lwin", VK_LWIN);
            // F1..F12
            for i in 1..=12u16 {
                m.insert(
                    Box::leak(format!("f{i}").into_boxed_str()),
                    VK_F1 + i - 1,
                );
            }
            // 0..9, a..z (single chars)
            for c in '0'..='9' {
                m.insert(Box::leak(c.to_string().into_boxed_str()), c as u16);
            }
            for c in 'a'..='z' {
                m.insert(Box::leak(c.to_string().into_boxed_str()), c as u16 - 32);
            }
            m
        })
    }

    fn lookup_vk(name: &str) -> Option<u16> {
        let name = name.to_lowercase();
        vk_map().get(name.as_str()).copied()
    }

    #[cfg(test)]
    pub(crate) fn lookup_vk_test(name: &str) -> Option<u16> {
        lookup_vk(name)
    }

    fn build_key_input(vk: u16, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send_inputs(inputs: &[INPUT]) -> anyhow::Result<()> {
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            return Err(anyhow!(
                "SendInput only delivered {}/{} events",
                sent,
                inputs.len()
            ));
        }
        Ok(())
    }

    /// Type arbitrary Unicode text: one KEYEVENTF_UNICODE down/up pair per
    /// char, so CJK and symbols work regardless of keyboard layout.
    pub fn type_text(text: &str) -> anyhow::Result<()> {
        let mut inputs = Vec::with_capacity(text.chars().count() * 2);
        for c in text.chars() {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: c as u16,
                        dwFlags: KEYEVENTF_UNICODE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: c as u16,
                        dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
        send_inputs(&inputs)
    }

    /// Press a key or a '+' chord like ctrl+c. Modifiers are held in order
    /// then released in reverse; a plain key gets one down/up pair.
    pub fn press_key(chord: &str) -> anyhow::Result<()> {
        let parts: Vec<&str> = chord.split('+').map(|p| p.trim()).collect();
        if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
            anyhow::bail!("invalid key chord '{}'", chord);
        }
        let mut vks = Vec::with_capacity(parts.len());
        for part in &parts {
            let vk = lookup_vk(part).ok_or_else(|| {
                anyhow!(
                    "unknown key '{}' (try: enter, esc, tab, ctrl+c, win+r, alt+tab, f5, a, 1, ...)",
                    part
                )
            })?;
            vks.push(vk);
        }

        let mut inputs = Vec::new();
        for vk in &vks {
            inputs.push(build_key_input(*vk, 0));
        }
        for vk in vks.iter().rev() {
            inputs.push(build_key_input(*vk, KEYEVENTF_KEYUP));
        }
        send_inputs(&inputs)
    }

    fn screen_size() -> (i64, i64) {
        unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN) as i64,
                GetSystemMetrics(SM_CYSCREEN) as i64,
            )
        }
    }

    fn absolute_xy(x: i64, y: i64) -> (u32, u32) {
        let (sw, sh) = screen_size();
        let x = x.clamp(0, sw - 1);
        let y = y.clamp(0, sh - 1);
        // SendInput absolute coords are 0..65535 across the whole virtual
        // screen; the primary screen starts at the origin.
        (
            (x * 65535 / sw.max(1)) as u32,
            (y * 65535 / sh.max(1)) as u32,
        )
    }

    fn build_mouse_input(flags: u32, dx: u32, dy: u32, data: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                mi: MOUSEINPUT {
                    dx: dx as i32,
                    dy: dy as i32,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    pub fn click(x: i64, y: i64, button: &str) -> anyhow::Result<()> {
        let (ax, ay) = absolute_xy(x, y);
        let (down, up) = match button {
            "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
            _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        };
        let (sw, sh) = screen_size();
        let _ = (sw, sh); // screen bounds validated inside absolute_xy
        let inputs = [
            build_mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, ax, ay, 0),
            build_mouse_input(down | MOUSEEVENTF_ABSOLUTE, ax, ay, 0),
            build_mouse_input(up | MOUSEEVENTF_ABSOLUTE, ax, ay, 0),
        ];
        send_inputs(&inputs)
    }

    pub fn move_to(x: i64, y: i64) -> anyhow::Result<()> {
        let (ax, ay) = absolute_xy(x, y);
        send_inputs(&[build_mouse_input(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
            ax,
            ay,
            0,
        )])
    }

    pub fn scroll(delta: i64) -> anyhow::Result<()> {
        // One wheel notch = 120 units; positive scrolls away from the user.
        let data = (delta * 120) as u32;
        send_inputs(&[build_mouse_input(MOUSEEVENTF_WHEEL, 0, 0, data)])
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::anyhow;

    pub fn type_text(_text: &str) -> anyhow::Result<()> {
        Err(anyhow!("input simulation requires Windows"))
    }

    pub fn press_key(_key: &str) -> anyhow::Result<()> {
        Err(anyhow!("input simulation requires Windows"))
    }

    pub fn click(_x: i64, _y: i64, _button: &str) -> anyhow::Result<()> {
        Err(anyhow!("input simulation requires Windows"))
    }

    pub fn move_to(_x: i64, _y: i64) -> anyhow::Result<()> {
        Err(anyhow!("input simulation requires Windows"))
    }

    pub fn scroll(_delta: i64) -> anyhow::Result<()> {
        Err(anyhow!("input simulation requires Windows"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_input_name() {
        assert_eq!(InputTool.name(), "input");
    }

    #[test]
    fn test_input_risk_levels() {
        let tool = InputTool;
        assert_eq!(
            tool.risk_level(&json!({"operation": "move"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "scroll"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "click"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "type"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "key"})),
            RiskLevel::Medium
        );
    }

    #[tokio::test]
    async fn test_type_requires_text() {
        let err = InputTool
            .execute(json!({"operation": "type"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
        let err = InputTool
            .execute(
                json!({"operation": "type", "text": "   "}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_click_requires_coordinates() {
        let err = InputTool
            .execute(json!({"operation": "click"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_unknown_operation_rejected() {
        let err = InputTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = InputTool
            .execute(json!({"operation": "move", "x": 1, "y": 1}), cancel)
            .await;
        assert!(err.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn test_key_map_lookup() {
        // VK_RETURN=0x0D, VK_CONTROL=0x11, VK_F5=0x74, 'A'=0x41
        assert_eq!(imp::lookup_vk_test("enter"), Some(0x0D));
        assert_eq!(imp::lookup_vk_test("CTRL"), Some(0x11));
        assert_eq!(imp::lookup_vk_test("f5"), Some(0x74));
        assert_eq!(imp::lookup_vk_test("a"), Some(0x41));
        assert_eq!(imp::lookup_vk_test("nope"), None);
    }
}
