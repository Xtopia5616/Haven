use tokio_util::sync::CancellationToken;

use crate::ToolResult;

/// Maximum registry entries examined for each collection returned by `list`.
/// One additional entry is probed to report whether that collection was cut off.
#[cfg(windows)]
const MAX_REGISTRY_LIST_ENTRIES: usize = 256;

pub struct RegistryTool;

#[cfg(any(windows, test))]
#[derive(Debug)]
struct BoundedRegistryEntries<T> {
    entries: Vec<T>,
    truncated: bool,
}

/// Collect at most `limit` registry enumeration results and probe one more
/// result so callers can distinguish a complete list from a capped one.
/// Failed registry entries retain the previous behavior and are omitted.
#[cfg(any(windows, test))]
fn collect_bounded_registry_entries<I, T, E>(
    entries: I,
    limit: usize,
    is_cancelled: impl Fn() -> bool,
) -> anyhow::Result<BoundedRegistryEntries<T>>
where
    I: IntoIterator<Item = Result<T, E>>,
{
    let mut entries = entries.into_iter();
    let mut collected = Vec::with_capacity(limit);

    for _ in 0..limit {
        if is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let next = entries.next();
        if is_cancelled() {
            anyhow::bail!("cancelled");
        }
        match next {
            Some(Ok(entry)) => collected.push(entry),
            Some(Err(_)) => {}
            None => {
                return Ok(BoundedRegistryEntries {
                    entries: collected,
                    truncated: false,
                });
            }
        }
    }

    if is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let truncated = entries.next().is_some();
    if is_cancelled() {
        anyhow::bail!("cancelled");
    }

    Ok(BoundedRegistryEntries {
        entries: collected,
        truncated,
    })
}

#[cfg(any(windows, test))]
fn registry_get_output(path: &str, name: &str, value: String) -> serde_json::Value {
    serde_json::json!({"path": path, "name": name, "value": value})
}

#[cfg(any(windows, test))]
fn registry_list_output(
    path: &str,
    values: BoundedRegistryEntries<String>,
    subkeys: BoundedRegistryEntries<String>,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "values": values.entries,
        "subkeys": subkeys.entries,
        "truncated": {
            "values": values.truncated,
            "subkeys": subkeys.truncated,
        },
    })
}

#[cfg(windows)]
fn registry_value_names(
    key: &winreg::RegKey,
    cancel: &CancellationToken,
) -> anyhow::Result<BoundedRegistryEntries<String>> {
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{Foundation::ERROR_NO_MORE_ITEMS, System::Registry::RegEnumValueW};

    // Windows limits a value name to 16,383 UTF-16 characters. Include room
    // for the terminating NUL required by RegEnumValueW's output buffer.
    let mut name_buffer = vec![0u16; 16_384];
    let mut index = 0u32;
    let entries = std::iter::from_fn(|| {
        let current_index = index;
        index = index.saturating_add(1);
        let mut name_length = name_buffer.len() as u32;
        // SAFETY: `key` owns a valid HKEY for this call, `name_buffer` is
        // writable for the supplied character count, and the optional data,
        // data length, type, and reserved pointers are null because list only
        // needs names. RegEnumValueW permits null data pointers when data is
        // not requested.
        let status = unsafe {
            RegEnumValueW(
                key.raw_handle(),
                current_index,
                name_buffer.as_mut_ptr(),
                &mut name_length,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            return None;
        }
        if status != 0 {
            return Some(Err(io::Error::from_raw_os_error(status as i32)));
        }

        let name_length = name_length as usize;
        if name_length > name_buffer.len() {
            return Some(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "registry returned an invalid value name length",
            )));
        }
        let name = OsString::from_wide(&name_buffer[..name_length]);
        Some(name.into_string().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "registry value name is not valid Unicode",
            )
        }))
    });

    collect_bounded_registry_entries(entries, MAX_REGISTRY_LIST_ENTRIES, || cancel.is_cancelled())
}

/// Registry operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryOperation {
    Get,
    List,
}

/// Typed parameters for `RegistryTool`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RegistryParams {
    /// Operation to perform; defaults to `list`.
    #[serde(default)]
    pub operation: Option<RegistryOperation>,
    /// Registry path, e.g. HKCU:\Software\Microsoft\Windows\CurrentVersion.
    #[serde(default)]
    pub path: Option<String>,
    /// Value name.
    #[serde(default)]
    pub name: Option<String>,
}

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

pub(super) fn canonical_registry_path(path: &str) -> anyhow::Result<(String, String)> {
    let (hive, subpath) = normalize_registry_path(path)?;
    if subpath.contains('\0') {
        anyhow::bail!("registry path contains a NUL character");
    }
    let hive = match hive.as_str() {
        "HKCU" | "HKEY_CURRENT_USER" => "HKCU",
        "HKLM" | "HKEY_LOCAL_MACHINE" => "HKLM",
        "HKCR" | "HKEY_CLASSES_ROOT" => "HKCR",
        "HKU" | "HKEY_USERS" => "HKU",
        "HKCC" | "HKEY_CURRENT_CONFIG" => "HKCC",
        _ => anyhow::bail!("unknown registry hive: {}", hive),
    };
    Ok((hive.to_owned(), subpath))
}

impl RegistryTool {
    pub async fn run(
        &self,
        params: RegistryParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        #[cfg(windows)]
        {
            let blocking_cancel = cancel.clone();
            let task = tokio::task::spawn_blocking(move || {
                run_registry_operation(params, blocking_cancel)
            });
            let result = tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                result = task => result.map_err(|error| anyhow::anyhow!("registry worker failed: {error}"))?,
            };
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            result
        }

        #[cfg(not(windows))]
        {
            let _ = params;
            anyhow::bail!("registry operations are only supported on Windows")
        }
    }
}

#[cfg(windows)]
fn run_registry_operation(
    params: RegistryParams,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    use winreg::RegKey;
    use winreg::enums::*;

    fn parse_hive(path: &str) -> anyhow::Result<(RegKey, String)> {
        let (hive, subpath) = canonical_registry_path(path)?;
        let hive = match hive.as_str() {
            "HKCU" => RegKey::predef(HKEY_CURRENT_USER),
            "HKLM" => RegKey::predef(HKEY_LOCAL_MACHINE),
            "HKCR" => RegKey::predef(HKEY_CLASSES_ROOT),
            "HKU" => RegKey::predef(HKEY_USERS),
            "HKCC" => RegKey::predef(HKEY_CURRENT_CONFIG),
            _ => unreachable!("canonical registry paths contain only supported hives"),
        };
        Ok((hive, subpath))
    }

    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    let op = params.operation.unwrap_or(RegistryOperation::List);
    let path = params
        .path
        .ok_or_else(|| anyhow::anyhow!("path is required"))?;

    match op {
        RegistryOperation::Get => {
            let name = params
                .name
                .ok_or_else(|| anyhow::anyhow!("name is required for get"))?;
            let (hive, subpath) = parse_hive(&path)?;
            let key = hive.open_subkey_with_flags(&subpath, KEY_READ)?;
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            let value: String = key.get_value(&name)?;
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            Ok(ToolResult::ok(registry_get_output(&path, &name, value)))
        }
        RegistryOperation::List => {
            let (hive, subpath) = parse_hive(&path)?;
            let key = hive.open_subkey_with_flags(&subpath, KEY_READ)?;
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            let values = registry_value_names(&key, &cancel)?;
            let subkeys = collect_bounded_registry_entries(
                key.enum_keys(),
                MAX_REGISTRY_LIST_ENTRIES,
                || cancel.is_cancelled(),
            )?;
            Ok(ToolResult::ok(registry_list_output(&path, values, subkeys)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_list_enumeration_is_bounded_and_reports_truncation() {
        let values = collect_bounded_registry_entries(
            (0..5).map(|index| Ok::<_, ()>(format!("value-{index}"))),
            3,
            || false,
        )
        .unwrap();
        let subkeys = collect_bounded_registry_entries(
            [Ok::<_, ()>("child".to_string())].into_iter(),
            3,
            || false,
        )
        .unwrap();

        let output = registry_list_output("HKCU", values, subkeys);
        assert_eq!(
            output,
            serde_json::json!({
                "path": "HKCU",
                "values": ["value-0", "value-1", "value-2"],
                "subkeys": ["child"],
                "truncated": {"values": true, "subkeys": false},
            })
        );

        let exact_limit =
            collect_bounded_registry_entries((0..3).map(|index| Ok::<_, ()>(index)), 3, || false)
                .unwrap();
        assert_eq!(exact_limit.entries, [0, 1, 2]);
        assert!(!exact_limit.truncated);
    }

    #[test]
    fn test_registry_list_enumeration_checks_cancellation_between_entries() {
        let cancel = CancellationToken::new();
        let iterator_cancel = cancel.clone();
        let mut index = 0;
        let entries = std::iter::from_fn(move || {
            if index == 1 {
                iterator_cancel.cancel();
            }
            let current = index;
            index += 1;
            Some(Ok::<_, ()>(current))
        });

        let error =
            collect_bounded_registry_entries(entries, 10, || cancel.is_cancelled()).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn test_registry_get_output_preserves_existing_shape() {
        assert_eq!(
            registry_get_output("HKCU\\Software", "Theme", "dark".into()),
            serde_json::json!({
                "path": "HKCU\\Software",
                "name": "Theme",
                "value": "dark",
            })
        );
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
}
