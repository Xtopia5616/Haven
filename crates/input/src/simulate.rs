//! Desktop input simulation: type text, press keys/chords, click/move/scroll
//! the mouse. Everything goes through SendInput, which behaves like real
//! input from the OS perspective. Requires a desktop session — headless/CI
//! runs will error (non-Windows stubs error unconditionally).

/// Type arbitrary Unicode text: one KEYEVENTF_UNICODE down/up pair per
/// char, so CJK and symbols work regardless of keyboard layout.
pub fn type_text(text: &str) -> anyhow::Result<()> {
    imp::type_text(text)
}

/// Press a key or a '+' chord like ctrl+c.
pub fn press_key(chord: &str) -> anyhow::Result<()> {
    imp::press_key(chord)
}

/// Click the mouse at screen coordinates (pixels from the top-left).
pub fn click(x: i64, y: i64, button: &str) -> anyhow::Result<()> {
    imp::click(x, y, button)
}

/// Move the mouse cursor to screen coordinates (pixels from the top-left).
pub fn move_to(x: i64, y: i64) -> anyhow::Result<()> {
    imp::move_to(x, y)
}

/// Scroll the wheel; positive delta scrolls up/away, negative down/toward.
pub fn scroll(delta: i64) -> anyhow::Result<()> {
    imp::scroll(delta)
}

#[cfg(windows)]
pub(crate) mod imp {
    use anyhow::anyhow;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE,
        VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR,
        VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
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
                m.insert(Box::leak(format!("f{i}").into_boxed_str()), VK_F1 + i - 1);
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

    pub(crate) fn lookup_vk(name: &str) -> Option<u16> {
        let name = name.to_lowercase();
        vk_map().get(name.as_str()).copied()
    }

    #[allow(dead_code)]
    pub(crate) fn accepted_key_names() -> impl Iterator<Item = &'static str> {
        vk_map().keys().copied()
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
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != inputs.len() {
            return Err(anyhow!(
                "SendInput only delivered {}/{} events",
                sent,
                inputs.len()
            ));
        }
        Ok(())
    }

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

    #[test]
    fn test_invalid_key_chord_rejected() {
        assert!(press_key("").is_err());
        assert!(press_key("ctrl+").is_err());
        assert!(press_key("boguskey").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn test_key_map_lookup() {
        // VK_RETURN=0x0D, VK_CONTROL=0x11, VK_F5=0x74, 'A'=0x41
        assert_eq!(imp::lookup_vk("enter"), Some(0x0D));
        assert_eq!(imp::lookup_vk("CTRL"), Some(0x11));
        assert_eq!(imp::lookup_vk("f5"), Some(0x74));
        assert_eq!(imp::lookup_vk("a"), Some(0x41));
        assert_eq!(imp::lookup_vk("nope"), None);
    }

    #[cfg(windows)]
    #[test]
    fn test_simulation_resolves_every_keycode_name() {
        use crate::hotkey::KeyCode;
        let variants: Vec<KeyCode> = vec![
            KeyCode::Space,
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Tab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::CapsLock,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
        ];
        for k in variants {
            let n = k.name().to_lowercase();
            assert!(
                imp::lookup_vk(&n).is_some(),
                "simulate rejects KeyCode name '{n}'"
            );
        }
        for c in b'a'..=b'z' {
            assert!(imp::lookup_vk(&(c as char).to_string()).is_some());
        }
        for c in b'0'..=b'9' {
            assert!(imp::lookup_vk(&(c as char).to_string()).is_some());
        }
        for n in 1..=12u8 {
            assert!(imp::lookup_vk(&format!("f{n}")).is_some());
        }
    }
}
