use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct RegistryTool;

/// Normalize a registry path into `(hive_upper, subpath)`.
///
/// Accepts many common formats:
/// - `HKCU\Software\...`          — short form, backslash
/// - `HKCU:\Software\...`          — PowerShell with colon-backslash
/// - `HKCU:Software\...`           — PowerShell colon only
/// - `HKEY_CURRENT_USER\Software`  — long form
/// - `Computer\HKEY_LOCAL_MACHINE\Software` — Registry Editor address bar
/// - `hkey_current_user\Software` — lowercase
/// - `HKCU` / `HKEY_CURRENT_USER` — bare hive (root)
fn normalize_registry_path(path: &str) -> anyhow::Result<(String, String)> {
    let path = path
        .strip_prefix("Computer\\")
        .or_else(|| path.strip_prefix("computer\\"))
        .unwrap_or(path);

    // Use colon as the separator if it appears before the first backslash
    // (PowerShell style: HKCU:\... or HKLM:...). Otherwise use backslash.
    let colon_pos = path.find(':');
    let bs_pos = path.find('\\');
    let use_colon = matches!((colon_pos, bs_pos), (Some(c), Some(b)) if c < b)
        || matches!((colon_pos, bs_pos), (Some(_), None));

    let (hive_str, subpath) = if use_colon {
        path.split_once(':').unwrap()
    } else {
        path.split_once('\\').unwrap_or((path, ""))
    };

    // Strip a trailing colon from the hive part (handles "HKCU:" that
    // results from split_once('\\') on "HKCU:\\Software").
    let hive_str = hive_str.trim_end_matches(':');
    let subpath = subpath.trim_start_matches('\\');

    if hive_str.is_empty() {
        anyhow::bail!("invalid registry path: {}", path);
    }

    Ok((hive_str.to_uppercase(), subpath.to_string()))
}

/// Encode a string as UTF-16LE bytes (no trailing NUL).
fn utf16le_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

/// Parse a hex byte string like "DE AD BE EF", "deadbeef" or "0A-0B-0C"
/// (any non-hex separators are ignored). Returns `None`-style error for odd
/// nibble counts or non-hex input.
fn parse_hex_bytes(value: &str) -> anyhow::Result<Vec<u8>> {
    let hex: String = value
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.is_empty() {
        anyhow::bail!("invalid Binary value (no hex digits): '{}'", value);
    }
    if !hex.len().is_multiple_of(2) {
        anyhow::bail!("invalid Binary value (odd hex digit count): '{}'", value);
    }
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let s = std::str::from_utf8(pair).unwrap_or("");
            u8::from_str_radix(s, 16)
                .map_err(|_| anyhow::anyhow!("invalid Binary value: '{}'", value))
        })
        .collect()
}

#[async_trait]
impl Tool for RegistryTool {
    fn name(&self) -> String {
        "registry".into()
    }
    fn description(&self) -> String {
        "Read and write Windows Registry. Operations: get, set, delete, list".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("set") | Some("delete") => RiskLevel::High,
            _ => RiskLevel::Medium,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["get", "set", "delete", "list"] },
                "path": { "type": "string", "description": "Registry path, e.g. HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion" },
                "name": { "type": "string", "description": "Value name" },
                "value": { "type": "string", "description": "Value data (for set)" },
                "type": { "type": "string", "enum": ["String", "DWord", "QWord", "Binary", "MultiString", "ExpandString"], "description": "Value type for set" }
            },
            "required": ["operation", "path"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        #[cfg(windows)]
        {
            use winreg::enums::*;
            use winreg::RegKey;

            let op = input["operation"].as_str().unwrap_or("list");
            let path = input["path"].as_str().ok_or_else(|| anyhow::anyhow!("path is required"))?;

            // Parse hive from path using the shared normalizer.
            fn parse_hive(path: &str) -> anyhow::Result<(RegKey, String)> {
                let (hive_upper, subpath) = normalize_registry_path(path)?;
                let hive = match hive_upper.as_str() {
                    "HKCU" | "HKEY_CURRENT_USER" => RegKey::predef(HKEY_CURRENT_USER),
                    "HKLM" | "HKEY_LOCAL_MACHINE" => RegKey::predef(HKEY_LOCAL_MACHINE),
                    "HKCR" | "HKEY_CLASSES_ROOT" => RegKey::predef(HKEY_CLASSES_ROOT),
                    "HKU" | "HKEY_USERS" => RegKey::predef(HKEY_USERS),
                    "HKCC" | "HKEY_CURRENT_CONFIG" => RegKey::predef(HKEY_CURRENT_CONFIG),
                    _ => anyhow::bail!("unknown registry hive: {}", hive_upper),
                };
                Ok((hive, subpath))
            }

            match op {
                "get" => {
                    let name = input["name"].as_str().ok_or_else(|| anyhow::anyhow!("name is required for get"))?;
                    let (hive, subpath) = parse_hive(path)?;
                    let key = hive.open_subkey_with_flags(&subpath, KEY_READ)?;
                    let val: String = key.get_value(name)?;
                    Ok(ToolResult::ok(serde_json::json!({"path": path, "name": name, "value": val})))
                }
                "set" => {
                    let name = input["name"].as_str().ok_or_else(|| anyhow::anyhow!("name is required for set"))?;
                    let value = input["value"].as_str().ok_or_else(|| anyhow::anyhow!("value is required for set"))?;
                    let val_type = input["type"].as_str().unwrap_or("String");
                    let (hive, subpath) = parse_hive(path)?;
                    let key = hive.open_subkey_with_flags(&subpath, KEY_WRITE)?;

                    match val_type {
                        "String" => key.set_value(name, &value)?,
                        "DWord" => {
                            let v: u32 = value.parse().map_err(|_| anyhow::anyhow!("invalid DWord value: {}", value))?;
                            key.set_value(name, &v)?;
                        }
                        "QWord" => {
                            let v: u64 = value.parse().map_err(|_| anyhow::anyhow!("invalid QWord value: {}", value))?;
                            key.set_value(name, &v)?;
                        }
                        "Binary" => {
                            let bytes = parse_hex_bytes(value)?;
                            key.set_raw_value(name, &winreg::RegValue {
                                bytes,
                                vtype: winreg::enums::REG_BINARY,
                            })?;
                        }
                        "ExpandString" => {
                            // REG_EXPAND_SZ: same text storage as String, but
                            // marked expandable so %VAR% resolves on read.
                            let mut bytes = utf16le_bytes(value);
                            bytes.extend_from_slice(&[0, 0]); // trailing NUL
                            key.set_raw_value(name, &winreg::RegValue {
                                bytes,
                                vtype: winreg::enums::REG_EXPAND_SZ,
                            })?;
                        }
                        "MultiString" => {
                            // Semicolon-separated list, like reg.exe /d.
                            let parts: Vec<&str> = value.split(';').collect();
                            let mut bytes = Vec::new();
                            for part in parts {
                                bytes.extend_from_slice(&utf16le_bytes(part));
                                bytes.extend_from_slice(&[0, 0]);
                            }
                            bytes.extend_from_slice(&[0, 0]); // final empty string
                            key.set_raw_value(name, &winreg::RegValue {
                                bytes,
                                vtype: winreg::enums::REG_MULTI_SZ,
                            })?;
                        }
                        _ => anyhow::bail!("unsupported type: {}", val_type),
                    }
                    Ok(ToolResult::ok(serde_json::json!({"set": true, "path": path, "name": name})))
                }
                "delete" => {
                    let (hive, subpath) = parse_hive(path)?;
                    let key = hive.open_subkey_with_flags(&subpath, KEY_WRITE)?;
                    if let Some(name) = input["name"].as_str() {
                        key.delete_value(name)?;
                    } else {
                        drop(key);
                        hive.delete_subkey(&subpath)?;
                    }
                    Ok(ToolResult::ok(serde_json::json!({"deleted": true, "path": path})))
                }
                "list" => {
                    let (hive, subpath) = parse_hive(path)?;
                    let key = hive.open_subkey_with_flags(&subpath, KEY_READ)?;
                    let names: Vec<String> = key.enum_values()
                        .filter_map(|r| r.ok().map(|(n, _)| n))
                        .collect();
                    let subkeys: Vec<String> = key.enum_keys()
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(ToolResult::ok(serde_json::json!({
                        "path": path,
                        "values": names,
                        "subkeys": subkeys
                    })))
                }
                _ => anyhow::bail!("unknown registry operation: {}", op),
            }
        }

        #[cfg(not(windows))]
        {
            let _ = input;
            anyhow::bail!("registry operations are only supported on Windows")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_registry_tool_name() {
        assert_eq!(RegistryTool.name(), "registry");
    }

    #[test]
    fn test_registry_tool_risk_level() {
        assert_eq!(RegistryTool.risk_level(&json!({"operation": "get"})), RiskLevel::Medium);
        assert_eq!(RegistryTool.risk_level(&json!({"operation": "set"})), RiskLevel::High);
    }

    #[test]
    fn test_registry_tool_input_schema() {
        let schema = RegistryTool.input_schema();
        assert!(schema["properties"]["operation"]["enum"].as_array().is_some());
    }

    #[test]
    fn test_normalize_short_backslash() {
        let (h, s) = normalize_registry_path("HKCU\\Software\\Microsoft").unwrap();
        assert_eq!(h, "HKCU");
        assert_eq!(s, "Software\\Microsoft");
    }

    #[test]
    fn test_normalize_long_form() {
        let (h, s) = normalize_registry_path("HKEY_LOCAL_MACHINE\\Software").unwrap();
        assert_eq!(h, "HKEY_LOCAL_MACHINE");
        assert_eq!(s, "Software");
    }

    #[test]
    fn test_normalize_lowercase() {
        let (h, _) = normalize_registry_path("hkey_current_user\\Software").unwrap();
        assert_eq!(h, "HKEY_CURRENT_USER");
    }

    #[test]
    fn test_normalize_powershell_colon_backslash() {
        // "HKCU:\\Software" — JSON-escaped, real path is HKCU:\Software
        let (h, s) = normalize_registry_path("HKCU:\\Software\\Microsoft").unwrap();
        assert_eq!(h, "HKCU");
        assert_eq!(s, "Software\\Microsoft");
    }

    #[test]
    fn test_normalize_powershell_colon_only() {
        let (h, s) = normalize_registry_path("HKLM:Software\\Microsoft").unwrap();
        assert_eq!(h, "HKLM");
        assert_eq!(s, "Software\\Microsoft");
    }

    #[test]
    fn test_normalize_computer_prefix() {
        let (h, s) =
            normalize_registry_path("Computer\\HKEY_CURRENT_USER\\Software\\Test").unwrap();
        assert_eq!(h, "HKEY_CURRENT_USER");
        assert_eq!(s, "Software\\Test");
    }

    #[test]
    fn test_normalize_bare_hive() {
        let (h, s) = normalize_registry_path("HKCU").unwrap();
        assert_eq!(h, "HKCU");
        assert_eq!(s, "");

        let (h, s) = normalize_registry_path("HKEY_LOCAL_MACHINE").unwrap();
        assert_eq!(h, "HKEY_LOCAL_MACHINE");
        assert_eq!(s, "");
    }

    #[test]
    fn test_normalize_all_hives() {
        for (input, expect) in [
            ("HKCU", "HKCU"),
            ("HKLM", "HKLM"),
            ("HKCR", "HKCR"),
            ("HKU", "HKU"),
            ("HKCC", "HKCC"),
            ("HKEY_CURRENT_USER", "HKEY_CURRENT_USER"),
            ("HKEY_LOCAL_MACHINE", "HKEY_LOCAL_MACHINE"),
            ("HKEY_CLASSES_ROOT", "HKEY_CLASSES_ROOT"),
            ("HKEY_USERS", "HKEY_USERS"),
            ("HKEY_CURRENT_CONFIG", "HKEY_CURRENT_CONFIG"),
        ] {
            let (h, _) = normalize_registry_path(input).unwrap();
            assert_eq!(h, expect, "failed for input {}", input);
        }
    }

    #[test]
    fn test_normalize_empty() {
        assert!(normalize_registry_path("").is_err());
    }

    #[test]
    fn test_parse_hex_bytes_spaces() {
        assert_eq!(parse_hex_bytes("DE AD BE EF").unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn test_parse_hex_bytes_compact() {
        assert_eq!(parse_hex_bytes("deadbeef").unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn test_parse_hex_bytes_dash_separated() {
        assert_eq!(parse_hex_bytes("0A-0B-0C").unwrap(), vec![0x0A, 0x0B, 0x0C]);
    }

    #[test]
    fn test_parse_hex_bytes_odd_nibbles() {
        assert!(parse_hex_bytes("ABC").is_err());
    }

    #[test]
    fn test_parse_hex_bytes_empty() {
        assert!(parse_hex_bytes("").is_err());
        assert!(parse_hex_bytes("--").is_err());
    }
}
