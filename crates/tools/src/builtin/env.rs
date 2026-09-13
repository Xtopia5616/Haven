use serde_json::Value;
use std::env;
use tokio_util::sync::CancellationToken;

use crate::ToolResult;

const MASKED_ENV_VALUE: &str = "[masked]";

pub struct EnvTool {
    /// Output cap (chars) for environment listings.
    pub max_output_chars: usize,
}

fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "API_KEY",
        "APIKEY",
        "AUTH_KEY",
        "AUTH_SECRET",
        "AUTH_TOKEN",
        "AUTHORIZATION",
        "CLIENT_SECRET",
        "PASSWORD",
        "PASSWD",
        "PRIVATE_KEY",
        "SECRET",
        "TOKEN",
    ]
    .iter()
    .any(|marker| upper.contains(marker))
}

/// Environment operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvOperation {
    Get,
    Set,
    Unset,
    List,
}

/// Where an environment variable lives. `process` is the legacy behavior;
/// the other scopes are persisted in the Windows environment registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvScope {
    Process,
    User,
    Machine,
}

impl EnvScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::User => "user",
            Self::Machine => "machine",
        }
    }
}

/// Typed parameters for `EnvTool`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EnvParams {
    /// Operation to perform; defaults to `list`.
    #[serde(default)]
    pub operation: Option<EnvOperation>,
    /// Environment variable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Value for set operation.
    #[serde(default)]
    pub value: Option<String>,
    /// Environment scope; defaults to the current process.
    #[serde(default)]
    pub scope: Option<EnvScope>,
}

impl EnvTool {
    pub async fn run(
        &self,
        params: EnvParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let scope = params.scope.unwrap_or(EnvScope::Process);
        match params.operation.unwrap_or(EnvOperation::List) {
            EnvOperation::Get => {
                let name = params
                    .name
                    .ok_or_else(|| anyhow::anyhow!("name is required for get"))?;
                match read_value(scope, &name)? {
                    Ok(val) => {
                        let masked = is_sensitive_env_name(&name);
                        Ok(ToolResult::ok(serde_json::json!({
                            "name": name,
                            "scope": scope.as_str(),
                            "value": if masked { MASKED_ENV_VALUE } else { &val },
                            "masked": masked,
                        })))
                    }
                    Err(env::VarError::NotPresent) => Ok(ToolResult {
                        success: true,
                        output: serde_json::json!({"name": name, "scope": scope.as_str(), "value": null, "masked": false}),
                        error: None,
                        error_class: None,
                        retryability: crate::ToolRetryability::Unknown,
                        truncated: false,
                        outcome: crate::ToolExecutionOutcome::Succeeded,
                        attempts: 1,
                        signals: crate::tool_contract::ToolSignals::default(),
                        llm_usage: Vec::new(),
                    }),
                    Err(e) => anyhow::bail!("failed to read env var '{}': {}", name, e),
                }
            }
            EnvOperation::Set => {
                let name = params
                    .name
                    .ok_or_else(|| anyhow::anyhow!("name is required for set"))?;
                let value = params
                    .value
                    .ok_or_else(|| anyhow::anyhow!("value is required for set"))?;
                write_value(scope, &name, &value)?;
                // Never echo a value back through the model-facing result.
                Ok(ToolResult::ok(serde_json::json!({
                    "set": true,
                    "name": name,
                    "scope": scope.as_str(),
                })))
            }
            EnvOperation::Unset => {
                let name = params
                    .name
                    .ok_or_else(|| anyhow::anyhow!("name is required for unset"))?;
                remove_value(scope, &name)?;
                Ok(ToolResult::ok(
                    serde_json::json!({"removed": true, "name": name, "scope": scope.as_str()}),
                ))
            }
            EnvOperation::List => {
                let prefix = params
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_uppercase());
                let mut vars: Vec<Value> = list_values(scope)?
                    .filter(|(k, _)| {
                        prefix
                            .as_ref()
                            .map(|p| k.to_ascii_uppercase().starts_with(p.as_str()))
                            .unwrap_or(true)
                    })
                    .map(|(k, _)| serde_json::json!({"name": k}))
                    .collect();
                vars.sort_by(|a, b| {
                    a["name"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(b["name"].as_str().unwrap_or(""))
                });
                let count = vars.len();
                let max_chars = self.max_output_chars;
                let (mut result, truncated) =
                    crate::util::json_list_within_budget("variables", vars, count, max_chars);
                if let Some(p) = prefix {
                    result["prefix"] = serde_json::json!(p);
                }
                result["scope"] = serde_json::json!(scope.as_str());
                if truncated {
                    result["hint"] = serde_json::json!(
                        "Environment listing truncated to the max chars budget. Use get with a specific variable name, or list with name as a prefix filter."
                    );
                }
                Ok(ToolResult::from_output(result, truncated))
            }
        }
    }
}

fn read_value(scope: EnvScope, name: &str) -> anyhow::Result<Result<String, env::VarError>> {
    match scope {
        EnvScope::Process => Ok(env::var(name)),
        EnvScope::User | EnvScope::Machine => {
            #[cfg(windows)]
            {
                Ok(read_persistent_value(scope, name))
            }
            #[cfg(not(windows))]
            {
                let _ = (scope, name);
                anyhow::bail!("user and machine environment scopes require Windows")
            }
        }
    }
}

fn write_value(scope: EnvScope, name: &str, value: &str) -> anyhow::Result<()> {
    match scope {
        EnvScope::Process => {
            // Rust 2024 marks process-global environment mutation unsafe.
            // The tool manager serializes this resource, so this is the one
            // deliberate process mutation boundary.
            unsafe { env::set_var(name, value) };
            Ok(())
        }
        EnvScope::User | EnvScope::Machine => {
            #[cfg(windows)]
            {
                write_persistent_value(scope, name, value)
            }
            #[cfg(not(windows))]
            {
                let _ = (scope, name, value);
                anyhow::bail!("user and machine environment scopes require Windows")
            }
        }
    }
}

fn remove_value(scope: EnvScope, name: &str) -> anyhow::Result<()> {
    match scope {
        EnvScope::Process => {
            unsafe { env::remove_var(name) };
            Ok(())
        }
        EnvScope::User | EnvScope::Machine => {
            #[cfg(windows)]
            {
                remove_persistent_value(scope, name)
            }
            #[cfg(not(windows))]
            {
                let _ = (scope, name);
                anyhow::bail!("user and machine environment scopes require Windows")
            }
        }
    }
}

fn list_values(scope: EnvScope) -> anyhow::Result<Box<dyn Iterator<Item = (String, String)>>> {
    match scope {
        EnvScope::Process => Ok(Box::new(env::vars())),
        EnvScope::User | EnvScope::Machine => {
            #[cfg(windows)]
            {
                Ok(Box::new(list_persistent_values(scope)?.into_iter()))
            }
            #[cfg(not(windows))]
            {
                let _ = scope;
                anyhow::bail!("user and machine environment scopes require Windows")
            }
        }
    }
}

#[cfg(windows)]
fn persistent_key(scope: EnvScope, write: bool) -> anyhow::Result<winreg::RegKey> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};
    let (hive, path, flags) = match scope {
        EnvScope::User => (
            winreg::RegKey::predef(HKEY_CURRENT_USER),
            "Environment",
            if write { KEY_WRITE } else { KEY_READ },
        ),
        EnvScope::Machine => (
            winreg::RegKey::predef(HKEY_LOCAL_MACHINE),
            "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
            if write { KEY_WRITE } else { KEY_READ },
        ),
        EnvScope::Process => anyhow::bail!("process scope has no persistent registry key"),
    };
    if write {
        Ok(hive.create_subkey(path)?.0)
    } else {
        Ok(hive.open_subkey_with_flags(path, flags)?)
    }
}

#[cfg(windows)]
fn read_persistent_value(scope: EnvScope, name: &str) -> Result<String, env::VarError> {
    let Ok(key) = persistent_key(scope, false) else {
        return Err(env::VarError::NotPresent);
    };
    match key.get_value::<String, _>(name) {
        Ok(value) => Ok(value),
        Err(_) => Err(env::VarError::NotPresent),
    }
}

#[cfg(windows)]
fn write_persistent_value(scope: EnvScope, name: &str, value: &str) -> anyhow::Result<()> {
    let key = persistent_key(scope, true)?;
    key.set_value(name, &value)?;
    broadcast_environment_change()?;
    Ok(())
}

#[cfg(windows)]
fn remove_persistent_value(scope: EnvScope, name: &str) -> anyhow::Result<()> {
    let key = persistent_key(scope, true)?;
    match key.delete_value(name) {
        Ok(()) => {
            broadcast_environment_change()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn list_persistent_values(scope: EnvScope) -> anyhow::Result<Vec<(String, String)>> {
    let key = persistent_key(scope, false)?;
    Ok(key
        .enum_values()
        .filter_map(|result| result.ok())
        .map(|(name, _)| (name, String::new()))
        .collect())
}

#[cfg(windows)]
fn broadcast_environment_change() -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let setting: Vec<u16> = std::ffi::OsStr::new("Environment")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            setting.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        )
    };
    if sent == 0 {
        anyhow::bail!("environment value persisted but WM_SETTINGCHANGE broadcast failed")
    }
    Ok(())
}

impl Default for EnvTool {
    fn default() -> Self {
        Self {
            max_output_chars: 20_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_var_name(tag: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("HAVEN_TEST_{}_{}_{}", tag, std::process::id(), n)
    }

    #[tokio::test]
    async fn test_env_get_existing() {
        let name = unique_var_name("GET");
        unsafe {
            env::set_var(&name, "hello");
        }
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Get),
                    name: Some(name.clone()),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["value"], "hello");
        unsafe {
            env::remove_var(&name);
        }
    }

    #[test]
    fn test_sensitive_env_name_detection() {
        assert!(is_sensitive_env_name("OPENAI_API_KEY"));
        assert!(is_sensitive_env_name("service_password"));
        assert!(is_sensitive_env_name("MY_AUTH_TOKEN"));
        assert!(!is_sensitive_env_name("HAVEN_TEST_MODE"));
    }

    #[tokio::test]
    async fn test_env_get_masks_sensitive_value() {
        let name = format!("{}_API_TOKEN", unique_var_name("MASK"));
        unsafe {
            env::set_var(&name, "do-not-leak");
        }
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Get),
                    name: Some(name.clone()),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["value"], MASKED_ENV_VALUE);
        assert_eq!(result.output["masked"], true);
        unsafe {
            env::remove_var(&name);
        }
    }

    #[tokio::test]
    async fn test_env_get_missing_returns_null() {
        let name = unique_var_name("MISSING");
        unsafe {
            env::remove_var(&name);
        }
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Get),
                    name: Some(name),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["value"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_env_get_requires_name() {
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Get),
                    name: None,
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_set_and_get_roundtrip() {
        let name = unique_var_name("SET");
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Set),
                    name: Some(name.clone()),
                    value: Some("v1".into()),
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["set"], true);
        assert!(result.output.get("value").is_none());
        assert_eq!(env::var(&name).unwrap(), "v1");
        unsafe {
            env::remove_var(&name);
        }
    }

    #[tokio::test]
    async fn test_env_set_requires_value() {
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Set),
                    name: Some(unique_var_name("SET")),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_unset() {
        let name = unique_var_name("UNSET");
        unsafe {
            env::set_var(&name, "temp");
        }
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Unset),
                    name: Some(name.clone()),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["removed"], true);
        assert!(env::var_os(&name).is_none());
    }

    #[tokio::test]
    async fn test_env_list_returns_variables() {
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::List),
                    name: None,
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let vars = result.output["variables"].as_array().unwrap();
        assert!(!vars.is_empty());
        assert!(vars[0]["name"].as_str().is_some());
        assert!(vars[0].get("value").is_none());
    }

    #[tokio::test]
    async fn test_env_list_prefix_filter() {
        let name = unique_var_name("PREFIX");
        unsafe {
            env::set_var(&name, "filtered");
        }
        let prefix = name[..name.len().saturating_sub(2)].to_string();
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::List),
                    name: Some(prefix.clone()),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(
            result.output["prefix"].as_str().unwrap(),
            prefix.to_ascii_uppercase()
        );
        let vars = result.output["variables"].as_array().unwrap();
        assert!(
            vars.iter()
                .any(|v| v["name"].as_str() == Some(name.as_str()))
        );
        unsafe {
            env::remove_var(&name);
        }
    }

    #[tokio::test]
    async fn test_env_run_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::List),
                    name: None,
                    value: None,
                    scope: None,
                },
                cancel,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_native_entry_lands_in_run() {
        let name = unique_var_name("NATIVE");
        unsafe {
            env::set_var(&name, "v2");
        }
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Get),
                    name: Some(name.clone()),
                    value: None,
                    scope: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["value"], "v2");
        unsafe {
            env::remove_var(&name);
        }
    }
}
