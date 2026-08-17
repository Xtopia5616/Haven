//! Global hotkey bindings: neutral key-combo representation and parsing.
//!
//! App shells (Tauri, etc.) convert [`KeyCombo`] into their platform
//! shortcut type; parsing, validation and display never depend on the shell.

use std::fmt;

/// Modifier bit flags for [`KeyCombo::modifiers`].
pub const CTRL: u8 = 1 << 0;
pub const SHIFT: u8 = 1 << 1;
pub const ALT: u8 = 1 << 2;
pub const SUPER: u8 = 1 << 3;

/// The non-modifier key of a hotkey combo. The accepted names intentionally
/// cover the same vocabulary as the desktop simulation key map
/// (`simulate::press_key`) so a user-facing key name behaves identically in
/// both paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Space,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    CapsLock,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    /// Single letter `a`..=`z` (kept lowercase).
    Key(u8),
    /// Digit `0`..=`9`.
    Digit(u8),
    /// Function key `f1`..=`f12`.
    F(u8),
}

impl KeyCode {
    pub fn parse(name: &str) -> Option<Self> {
        let lower = name.to_lowercase();
        match lower.as_str() {
            "space" => return Some(Self::Space),
            "enter" | "return" => return Some(Self::Enter),
            "escape" | "esc" => return Some(Self::Escape),
            "tab" => return Some(Self::Tab),
            "backspace" => return Some(Self::Backspace),
            "delete" | "del" => return Some(Self::Delete),
            "capslock" => return Some(Self::CapsLock),
            "home" => return Some(Self::Home),
            "end" => return Some(Self::End),
            "pageup" | "pgup" => return Some(Self::PageUp),
            "pagedown" | "pgdn" => return Some(Self::PageDown),
            "left" => return Some(Self::ArrowLeft),
            "right" => return Some(Self::ArrowRight),
            "up" => return Some(Self::ArrowUp),
            "down" => return Some(Self::ArrowDown),
            _ => {}
        }
        let bytes = lower.as_bytes();
        if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
            return Some(Self::Key(bytes[0]));
        }
        if bytes.len() == 1 && bytes[0].is_ascii_digit() {
            return Some(Self::Digit(bytes[0]));
        }
        if let Some(rest) = lower.strip_prefix('f')
            && let Ok(n) = rest.parse::<u8>()
            && (1..=12).contains(&n)
        {
            return Some(Self::F(n));
        }
        None
    }

    /// Display name, e.g. `Space`, `A`, `F5`, `ArrowLeft`.
    pub fn name(&self) -> String {
        match self {
            Self::Space => "Space".into(),
            Self::Enter => "Enter".into(),
            Self::Escape => "Escape".into(),
            Self::Tab => "Tab".into(),
            Self::Backspace => "Backspace".into(),
            Self::Delete => "Delete".into(),
            Self::CapsLock => "CapsLock".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::PageUp => "PageUp".into(),
            Self::PageDown => "PageDown".into(),
            Self::ArrowLeft => "Left".into(),
            Self::ArrowRight => "Right".into(),
            Self::ArrowUp => "Up".into(),
            Self::ArrowDown => "Down".into(),
            Self::Key(c) => (c.to_ascii_uppercase() as char).to_string(),
            Self::Digit(d) => (*d as char).to_string(),
            Self::F(n) => format!("F{n}"),
        }
    }
}

/// A parsed hotkey combo, e.g. `Ctrl+Shift+Space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    modifiers: u8,
    key: KeyCode,
}

impl KeyCombo {
    pub fn new(modifiers: u8, key: KeyCode) -> Self {
        Self { modifiers, key }
    }

    pub fn modifiers(&self) -> u8 {
        self.modifiers
    }

    pub fn key(&self) -> KeyCode {
        self.key
    }

    pub fn has(&self, modifier: u8) -> bool {
        self.modifiers & modifier != 0
    }

    /// Parse a binding string like `Ctrl+Shift+Space`. Modifier names are
    /// case-sensitive (`Ctrl`/`Control`, `Shift`, `Alt`, `Super`/`Win`/`Cmd`);
    /// each part is trimmed, repeated modifiers are idempotent, and a later
    /// key wins over an earlier one.
    pub fn parse(binding: &str) -> Option<Self> {
        let mut modifiers = 0u8;
        let mut key: Option<KeyCode> = None;
        for part in binding.split('+') {
            match part.trim() {
                "Ctrl" | "Control" => modifiers |= CTRL,
                "Shift" => modifiers |= SHIFT,
                "Alt" => modifiers |= ALT,
                "Super" | "Win" | "Cmd" => modifiers |= SUPER,
                other => key = KeyCode::parse(other),
            }
        }
        Some(Self::new(modifiers, key?))
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<&str> = Vec::new();
        if self.has(CTRL) {
            parts.push("Ctrl");
        }
        if self.has(SHIFT) {
            parts.push("Shift");
        }
        if self.has(ALT) {
            parts.push("Alt");
        }
        if self.has(SUPER) {
            parts.push("Super");
        }
        write!(f, "{}", parts.join("+"))?;
        if !parts.is_empty() {
            write!(f, "+")?;
        }
        write!(f, "{}", self.key.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_modifiers_and_key() {
        let combo = KeyCombo::parse("Ctrl+Shift+Space").unwrap();
        assert!(combo.has(CTRL));
        assert!(combo.has(SHIFT));
        assert!(!combo.has(ALT));
        assert!(!combo.has(SUPER));
        assert_eq!(combo.key(), KeyCode::Space);
        assert_eq!(combo.to_string(), "Ctrl+Shift+Space");
    }

    #[test]
    fn test_parse_case_insensitive_and_aliases() {
        assert_eq!(
            KeyCombo::parse("ctrl+alt+t").unwrap().key(),
            KeyCode::Key(b't')
        );
        let combo = KeyCombo::parse("Control+Win+f5").unwrap();
        assert!(combo.has(CTRL));
        assert!(combo.has(SUPER));
        assert_eq!(combo.key(), KeyCode::F(5));
        assert_eq!(combo.to_string(), "Ctrl+Super+F5");
    }

    #[test]
    fn test_parse_requires_key() {
        assert!(KeyCombo::parse("Ctrl+Shift").is_none());
        assert!(KeyCombo::parse("").is_none());
    }

    #[test]
    fn test_parse_rejects_unknown_keys() {
        assert!(KeyCombo::parse("Ctrl+F13").is_none());
        assert!(KeyCombo::parse("Ctrl+Foo").is_none());
        assert!(KeyCombo::parse("Ctrl+Enter+Bar").is_none());
    }

    #[test]
    fn test_parse_tolerates_whitespace_around_plus() {
        let combo = KeyCombo::parse("Ctrl + Space").unwrap();
        assert!(combo.has(CTRL));
        assert_eq!(combo.key(), KeyCode::Space);
        let combo = KeyCombo::parse(" Ctrl+Shift+Space ").unwrap();
        assert!(combo.has(CTRL) && combo.has(SHIFT));
        assert_eq!(combo.key(), KeyCode::Space);
    }

    #[test]
    fn test_parse_simulation_alias_parity() {
        // Names that `simulate::press_key` accepts must resolve here too, so
        // the hotkey and the key tool behave identically for the same input.
        assert_eq!(KeyCode::parse("esc"), Some(KeyCode::Escape));
        assert_eq!(KeyCode::parse("ESCAPE"), Some(KeyCode::Escape));
        assert_eq!(KeyCode::parse("return"), Some(KeyCode::Enter));
        assert_eq!(KeyCode::parse("backspace"), Some(KeyCode::Backspace));
        assert_eq!(KeyCode::parse("delete"), Some(KeyCode::Delete));
        assert_eq!(KeyCode::parse("home"), Some(KeyCode::Home));
        assert_eq!(KeyCode::parse("end"), Some(KeyCode::End));
        assert_eq!(KeyCode::parse("pageup"), Some(KeyCode::PageUp));
        assert_eq!(KeyCode::parse("pagedown"), Some(KeyCode::PageDown));
        assert_eq!(KeyCode::parse("left"), Some(KeyCode::ArrowLeft));
        assert_eq!(KeyCode::parse("up"), Some(KeyCode::ArrowUp));
        assert_eq!(KeyCode::parse("1"), Some(KeyCode::Digit(b'1')));
        assert_eq!(KeyCombo::parse("Ctrl+Esc").unwrap().key(), KeyCode::Escape);
        assert_eq!(
            KeyCombo::parse("Ctrl+1").unwrap().key(),
            KeyCode::Digit(b'1')
        );
    }

    #[test]
    fn test_parse_without_modifiers() {
        let combo = KeyCombo::parse("space").unwrap();
        assert_eq!(combo.modifiers(), 0);
        assert_eq!(combo.key(), KeyCode::Space);
        assert_eq!(combo.to_string(), "Space");
    }

    #[test]
    fn test_key_code_names() {
        assert_eq!(KeyCode::Space.name(), "Space");
        assert_eq!(KeyCode::Key(b'q').name(), "Q");
        assert_eq!(KeyCode::Digit(b'7').name(), "7");
        assert_eq!(KeyCode::F(12).name(), "F12");
        assert_eq!(KeyCode::ArrowLeft.name(), "Left");
    }

    #[cfg(windows)]
    #[test]
    fn test_keycode_parses_every_simulation_key_name() {
        // Names simulate treats as pressable keys that a hotkey combo resolves as
        // modifiers instead — the intentional exception list.
        const MODIFIER_EXCEPTIONS: &[&str] = &["shift", "ctrl", "control", "alt", "win", "lwin"];
        for name in crate::simulate::imp::accepted_key_names() {
            if MODIFIER_EXCEPTIONS.contains(&name) {
                continue;
            }
            assert!(
                KeyCode::parse(name).is_some(),
                "KeyCode::parse rejects simulation key name '{name}'"
            );
        }
    }
}
