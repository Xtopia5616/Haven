use haven_common::config::{StoredPermission, ToolConfig};
use haven_common::types::{
    ConfirmationMode, PermissionEffect, PermissionScope, RiskLevel, permission_key,
    permission_key_candidates,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;

/// Expected operation/risk rows used by the local-tool security regression
/// matrix. The registry-driven test below verifies that this contract covers
/// every operation actually advertised by every builtin tool, so schema and
/// risk-level changes cannot silently leave the matrix stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalToolSecurityCase {
    pub tool_name: &'static str,
    pub operation: &'static str,
    pub risk_level: RiskLevel,
}

macro_rules! security_case {
    ($tool:literal, $operation:literal, $risk:ident) => {
        LocalToolSecurityCase {
            tool_name: $tool,
            operation: $operation,
            risk_level: RiskLevel::$risk,
        }
    };
}

pub const LOCAL_TOOL_SECURITY_MATRIX: &[LocalToolSecurityCase] = &[
    security_case!("audio", "play", Low),
    security_case!("audio", "record", Medium),
    security_case!("audio", "volume_get", Low),
    security_case!("audio", "volume_set", Medium),
    security_case!("audio", "mute_get", Low),
    security_case!("audio", "mute_set", Medium),
    security_case!("ask", "ask", Safe),
    security_case!("files", "read", Low),
    security_case!("files", "write", Medium),
    security_case!("files", "edit", Medium),
    security_case!("files", "copy", Medium),
    security_case!("files", "move", Medium),
    security_case!("files", "delete", High),
    security_case!("files", "list", Low),
    security_case!("files", "summary", Low),
    security_case!("files", "search", Low),
    security_case!("files", "search:content", Medium),
    security_case!("process", "list", Low),
    security_case!("process", "kill", High),
    security_case!("clipboard", "read", Low),
    security_case!("clipboard", "write", Medium),
    security_case!("clipboard", "history", Low),
    security_case!("shell", "execute", High),
    security_case!("actions", "list", Safe),
    security_case!("input", "type", Medium),
    security_case!("input", "key", Medium),
    security_case!("input", "click", Medium),
    security_case!("input", "move", Low),
    security_case!("input", "scroll", Low),
    security_case!("schedule", "set", Low),
    security_case!("schedule", "list", Safe),
    security_case!("schedule", "cancel", Safe),
    security_case!("system", "info", Safe),
    security_case!("system", "overview", Safe),
    security_case!("system", "display", Safe),
    security_case!("system", "displays", Safe),
    security_case!("system", "env:list", High),
    security_case!("system", "env:get", Low),
    security_case!("system", "env:set", High),
    security_case!("system", "env:unset", High),
    security_case!("system", "registry:list", Medium),
    security_case!("system", "registry:get", Medium),
    security_case!("system", "registry:set", High),
    security_case!("system", "registry:delete", High),
    security_case!("system", "power:status", Safe),
    security_case!("system", "power:lock", High),
    security_case!("system", "power:sleep", High),
    security_case!("system", "power:hibernate", Critical),
    security_case!("window", "list", Low),
    security_case!("window", "foreground", Low),
    security_case!("window", "focus", Medium),
    security_case!("window", "close", High),
    security_case!("window", "screenshot", Low),
    security_case!("window", "ocr", High),
    security_case!("window", "ui_tree", Low),
    security_case!("window", "wait", Low),
    security_case!("http", "request", Medium),
    security_case!("notify", "notify", Safe),
    security_case!("agent", "list", Safe),
    security_case!("agent", "send", Safe),
    security_case!("agent", "inbox", Safe),
    security_case!("agent", "reply", Safe),
    security_case!("agent", "profile", Safe),
    security_case!("agent", "request", Safe),
    security_case!("agent", "spawn", Medium),
    security_case!("load_skill", "load", Safe),
    security_case!("load_mcp", "load", Safe),
    security_case!("memory", "search", Safe),
    security_case!("memory", "list", Safe),
    security_case!("memory", "remember", Medium),
    security_case!("memory", "forget", Medium),
    security_case!("memory", "recall", Safe),
    security_case!("haven_diagnostics", "status", Low),
    security_case!("haven_diagnostics", "logs_tail", Low),
    security_case!("haven_config", "config_get", Low),
    security_case!("haven_config", "logs_level", Medium),
    security_case!("haven_skills", "skills_list", Low),
    security_case!("haven_skills", "skill_enable", Medium),
    security_case!("haven_skills", "skill_disable", Medium),
    security_case!("haven_skills", "skill_create", High),
    security_case!("haven_tools", "tool_enable", Medium),
    security_case!("haven_tools", "tool_disable", Medium),
    security_case!("haven_mcp", "mcp_list", Low),
    security_case!("haven_mcp", "mcp_connect", Medium),
    security_case!("haven_mcp", "mcp_disconnect", Medium),
    security_case!("haven_mcp", "mcp_add", High),
    security_case!("haven_mcp", "mcp_update", High),
    security_case!("haven_mcp", "mcp_toggle", High),
    security_case!("haven_mcp", "mcp_remove", High),
    security_case!("haven_mcp", "mcp_reload", Medium),
    security_case!("haven_session_diagnostics", "sessions", Low),
    security_case!("haven_session_diagnostics", "errors", Low),
];

/// Check an absolute local path without applying a tool-specific allowlist.
/// This is for native app entry points such as “open skills directory” and
/// “open external path”; the normal tool path goes through `check`, which adds
/// configured `allowed_paths` on top of this reparse-point check.
pub fn is_safe_local_path(path: &Path) -> bool {
    if !path.is_absolute() || is_unc_or_device_path(path) {
        return false;
    }
    resolve_path_without_reparse(path).is_some()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ConfirmationResult {
    AutoApproved,
    RequiresConfirmation {
        tool_name: String,
        params: Value,
        risk_level: RiskLevel,
        /// Stable key used for grant matching (`tool` / `tool:op`).
        permission_key: String,
    },
    /// Hard deny — permanent/session denylist, disabled operation, or path sandbox.
    Blocked {
        reason: String,
    },
}

/// Per-session allow/deny sets keyed by permission key.
#[derive(Clone, Default)]
struct SessionGrants {
    allow: HashSet<String>,
    deny: HashSet<String>,
}

/// Combined safety config under a single RwLock so `check` reads atomically.
#[derive(Clone)]
struct SafetyConfig {
    confirmation_mode: ConfirmationMode,
    min_risk_level: RiskLevel,
    /// Permanent (Always) grants from `SecurityConfig.permissions`.
    permanent: HashMap<String, PermissionEffect>,
    /// Per-conversation grants keyed by session id.
    session_grants: HashMap<String, SessionGrants>,
    /// Live copy of `tool_settings` for disabled_operations / risk_override /
    /// allowed_paths enforcement.
    tool_settings: HashMap<String, ToolConfig>,
}

pub struct SafetyGateway {
    config: RwLock<SafetyConfig>,
}

impl SafetyGateway {
    pub fn new(min_risk_level: RiskLevel) -> Self {
        Self {
            config: RwLock::new(SafetyConfig {
                confirmation_mode: ConfirmationMode::Ask,
                min_risk_level,
                permanent: HashMap::new(),
                session_grants: HashMap::new(),
                tool_settings: HashMap::new(),
            }),
        }
    }

    /// Replace threshold + mode + permanent grants from settings. Clears
    /// session grants so a policy change cannot leave stale trusts.
    pub async fn apply_security(
        &self,
        mode: ConfirmationMode,
        min_risk_level: RiskLevel,
        permissions: &[StoredPermission],
    ) {
        let mut cfg = self.config.write().await;
        cfg.confirmation_mode = mode;
        cfg.min_risk_level = min_risk_level;
        cfg.permanent.clear();
        for p in permissions {
            cfg.permanent.insert(p.key.clone(), p.effect);
        }
        cfg.session_grants.clear();
    }

    /// Update the minimum risk level threshold. Clears session grants.
    pub async fn set_min_risk_level(&self, level: RiskLevel) {
        let mut cfg = self.config.write().await;
        cfg.min_risk_level = level;
        cfg.session_grants.clear();
    }

    /// Refresh the live tool_settings mirror used by path/op/risk overrides.
    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        self.config.write().await.tool_settings = settings;
    }

    /// Effective risk after optional `tool_settings.risk_override`.
    pub async fn effective_risk(&self, tool_name: &str, reported: RiskLevel) -> RiskLevel {
        let cfg = self.config.read().await;
        effective_risk_from(&cfg, tool_name, reported)
    }

    pub async fn check(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        params: &Value,
        risk_level: RiskLevel,
    ) -> ConfirmationResult {
        let key = permission_key(tool_name, params);
        let cfg = self.config.read().await;
        let risk = effective_risk_from(&cfg, tool_name, risk_level);

        if let Some(reason) = disabled_operation_block(&cfg.tool_settings, tool_name, params) {
            return ConfirmationResult::Blocked { reason };
        }
        if let Some(reason) = path_sandbox_block(&cfg.tool_settings, tool_name, params) {
            return ConfirmationResult::Blocked { reason };
        }

        // Deny always wins over Allow (permanent deny → session deny →
        // permanent allow → session allow). Session deny can override a
        // permanent allow for the rest of that conversation.
        if match_grant(&cfg.permanent, &key) == Some(PermissionEffect::Deny) {
            return ConfirmationResult::Blocked {
                reason: format!("permanently denied: {key}"),
            };
        }
        if let Some(sid) = session_id
            && let Some(grants) = cfg.session_grants.get(sid)
            && match_key_set(&grants.deny, &key)
        {
            return ConfirmationResult::Blocked {
                reason: format!("denied for this session: {key}"),
            };
        }
        if match_grant(&cfg.permanent, &key) == Some(PermissionEffect::Allow) {
            return ConfirmationResult::AutoApproved;
        }
        if let Some(sid) = session_id
            && let Some(grants) = cfg.session_grants.get(sid)
            && match_key_set(&grants.allow, &key)
        {
            return ConfirmationResult::AutoApproved;
        }

        // Autopilot skips prompts except Critical — keep a hard floor for
        // irreversible ops (e.g. power hibernate).
        let needs_prompt = match cfg.confirmation_mode {
            ConfirmationMode::Autopilot => risk >= RiskLevel::Critical,
            ConfirmationMode::Paranoid => risk > RiskLevel::Safe,
            ConfirmationMode::Ask => risk >= cfg.min_risk_level,
        };

        if !needs_prompt {
            return ConfirmationResult::AutoApproved;
        }

        ConfirmationResult::RequiresConfirmation {
            tool_name: tool_name.into(),
            params: params.clone(),
            risk_level: risk,
            permission_key: key,
        }
    }

    /// Record a grant. `Once` is a no-op (caller already approved this call).
    /// `Always` updates the in-memory permanent map; the app layer must also
    /// persist to `SecurityConfig.permissions`.
    pub async fn grant(
        &self,
        session_id: Option<&str>,
        key: &str,
        effect: PermissionEffect,
        scope: PermissionScope,
    ) {
        if matches!(scope, PermissionScope::Once) || key.is_empty() {
            return;
        }
        let mut cfg = self.config.write().await;
        match scope {
            PermissionScope::Always => {
                cfg.permanent.insert(key.to_string(), effect);
            }
            PermissionScope::Session => {
                let Some(sid) = session_id else {
                    return;
                };
                let entry = cfg.session_grants.entry(sid.to_string()).or_default();
                match effect {
                    PermissionEffect::Allow => {
                        entry.deny.remove(key);
                        entry.allow.insert(key.to_string());
                    }
                    PermissionEffect::Deny => {
                        entry.allow.remove(key);
                        entry.deny.insert(key.to_string());
                    }
                }
            }
            PermissionScope::Once => {}
        }
    }

    /// Snapshot of permanent grants for the settings UI.
    pub async fn list_permanent(&self) -> Vec<StoredPermission> {
        let cfg = self.config.read().await;
        let mut out: Vec<_> = cfg
            .permanent
            .iter()
            .map(|(key, effect)| StoredPermission {
                key: key.clone(),
                effect: *effect,
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Remove one permanent grant from memory. App layer persists the change.
    pub async fn revoke_permanent(&self, key: &str) -> bool {
        self.config.write().await.permanent.remove(key).is_some()
    }

    /// Drop one session's grants (conversation ended / deleted).
    pub async fn clear_session_trust(&self, session_id: &str) {
        self.config.write().await.session_grants.remove(session_id);
    }

    /// Drop every session grant (history cleared / app reset).
    pub async fn clear_all_trust(&self) {
        self.config.write().await.session_grants.clear();
    }
}

fn effective_risk_from(cfg: &SafetyConfig, tool_name: &str, reported: RiskLevel) -> RiskLevel {
    cfg.tool_settings
        .get(tool_name)
        .and_then(|t| t.risk_override.as_deref())
        .and_then(parse_risk_override)
        .unwrap_or(reported)
}

fn parse_risk_override(raw: &str) -> Option<RiskLevel> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "safe" => Some(RiskLevel::Safe),
        "low" => Some(RiskLevel::Low),
        "medium" => Some(RiskLevel::Medium),
        "high" => Some(RiskLevel::High),
        "critical" => Some(RiskLevel::Critical),
        _ => None,
    }
}

fn match_grant(map: &HashMap<String, PermissionEffect>, key: &str) -> Option<PermissionEffect> {
    let candidates = permission_key_candidates(key);
    // A child Allow must never outrank a parent Deny. Check the entire
    // inheritance chain for denies before considering any allow, otherwise a
    // broad deny such as `files` could be bypassed by `files:read`.
    if candidates
        .iter()
        .any(|candidate| map.get(*candidate) == Some(&PermissionEffect::Deny))
    {
        return Some(PermissionEffect::Deny);
    }
    candidates
        .iter()
        .any(|candidate| map.get(*candidate) == Some(&PermissionEffect::Allow))
        .then_some(PermissionEffect::Allow)
}

fn match_key_set(set: &HashSet<String>, key: &str) -> bool {
    permission_key_candidates(key)
        .into_iter()
        .any(|c| set.contains(c))
}

fn disabled_operation_block(
    settings: &HashMap<String, ToolConfig>,
    tool_name: &str,
    params: &Value,
) -> Option<String> {
    let cfg = settings.get(tool_name)?;
    if cfg.disabled_operations.is_empty() {
        return None;
    }
    let op = params
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let scope = params.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    for disabled in &cfg.disabled_operations {
        let d = disabled.trim();
        if d.is_empty() {
            continue;
        }
        if d == op || d == scope || (!scope.is_empty() && d == format!("{scope}:{op}")) {
            return Some(format!(
                "operation '{disabled}' is disabled for tool '{tool_name}'"
            ));
        }
    }
    None
}

fn path_sandbox_block(
    settings: &HashMap<String, ToolConfig>,
    tool_name: &str,
    params: &Value,
) -> Option<String> {
    let cfg = settings.get(tool_name)?;
    if cfg.allowed_paths.is_empty() {
        return None;
    }
    let allowed: Vec<PathBuf> = cfg.allowed_paths.iter().map(PathBuf::from).collect();
    let paths = collect_path_params(params);
    if paths.is_empty() {
        return None;
    }
    for path in paths {
        if !path_is_allowed(&path, &allowed) {
            return Some(format!(
                "path '{}' is outside allowed_paths for tool '{tool_name}'",
                path.display()
            ));
        }
    }
    None
}

fn collect_path_params(params: &Value) -> Vec<PathBuf> {
    const KEYS: &[&str] = &[
        "path",
        "paths",
        "source",
        "destination",
        "target",
        "cwd",
        "file",
        "file_path",
        "dir",
        "directory",
    ];
    let mut out = Vec::new();
    let Some(obj) = params.as_object() else {
        return out;
    };
    for key in KEYS {
        match obj.get(*key) {
            Some(Value::String(s)) if !s.is_empty() => out.push(PathBuf::from(s)),
            Some(Value::Array(arr)) => {
                for v in arr {
                    if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                        out.push(PathBuf::from(s));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn path_is_allowed(path: &Path, allowed: &[PathBuf]) -> bool {
    // Relative paths and UNC/device paths are rejected even when the current
    // working directory happens to be inside an allowed root. Otherwise the
    // meaning of the same tool input changes with process launch context.
    if !path.is_absolute() || is_unc_or_device_path(path) {
        return false;
    }
    let Some(canon) = resolve_path_without_reparse(path) else {
        return false;
    };
    for base in allowed {
        if !base.is_absolute() || is_unc_or_device_path(base) {
            continue;
        }
        let Some(base_abs) = resolve_path_without_reparse(base) else {
            continue;
        };
        if path_is_within(&canon, &base_abs) {
            return true;
        }
    }
    false
}

/// Resolve the existing prefix of a path while rejecting every symlink or
/// Windows reparse point encountered on that prefix. The non-existing suffix
/// is appended only after the trusted prefix has been canonicalized. This is
/// intentionally fail-closed: a metadata/canonicalization error denies the
/// operation instead of falling back to lexical prefix matching.
fn resolve_path_without_reparse(path: &Path) -> Option<PathBuf> {
    let abs = normalize_path(path)?;
    let components: Vec<_> = abs.components().collect();
    let mut existing = PathBuf::new();
    let mut suffix: Vec<OsString> = Vec::new();
    let mut missing_started = false;

    for (index, component) in components.iter().enumerate() {
        if missing_started {
            suffix.push(component.as_os_str().to_owned());
            continue;
        }

        existing.push(component.as_os_str());
        match std::fs::symlink_metadata(&existing) {
            Ok(metadata) => {
                if is_reparse_point(&metadata) {
                    return None;
                }
                if index + 1 < components.len() && !metadata.file_type().is_dir() {
                    return None;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing.pop();
                missing_started = true;
                suffix.push(component.as_os_str().to_owned());
            }
            Err(_) => return None,
        }
    }

    let mut resolved = std::fs::canonicalize(&existing).ok()?;
    for component in suffix {
        resolved.push(component);
    }
    Some(resolved)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn is_unc_or_device_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("\\\\") || value.starts_with("//")
}

fn path_is_within(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().to_ascii_lowercase();
        let base = base.to_string_lossy().to_ascii_lowercase();
        Path::new(&path).starts_with(Path::new(&base))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(base)
    }
}

/// Lexically normalize `.` / `..` after making the path absolute so
/// `allowed\..\Windows` cannot prefix-match `allowed`.
fn normalize_path(path: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(path).ok()?;
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    #[tokio::test]
    async fn test_safety_gateway_new_default_threshold() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        // Safe is below Low → auto approved
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_below_threshold_auto_approved() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        // Low is below Medium → auto approved
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Low).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_at_threshold_requires_confirmation() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        // Medium is at the threshold → requires confirmation
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_above_threshold_requires_confirmation() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        // High is above Low → requires confirmation
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::High).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_allow_tool_key() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        let result = gw
            .check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_allow_is_per_session_and_per_tool() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(Some("ses-b"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(Some("ses-a"), "tool2", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(None, "tool1", &json!({}), RiskLevel::Medium).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_permanent_deny_blocks() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "shell",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;
        let result = gw.check(None, "shell", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_safety_gateway_parent_key_matches_operation() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "files",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        let result = gw
            .check(
                Some("ses-a"),
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_safety_gateway_clear_session_trust() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));

        gw.clear_session_trust("ses-a").await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_set_threshold_clears_session_grants() {
        let gw = SafetyGateway::new(RiskLevel::Low);
        gw.grant(
            Some("ses-a"),
            "tool1",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));

        gw.set_min_risk_level(RiskLevel::High).await;
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::Medium)
                .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(Some("ses-a"), "tool1", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_disabled_operation_blocks() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                disabled_operations: vec!["delete".into()],
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_safety_gateway_autopilot_skips_prompt_except_critical() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.apply_security(ConfirmationMode::Autopilot, RiskLevel::Medium, &[])
            .await;
        assert!(matches!(
            gw.check(None, "shell", &json!({}), RiskLevel::High).await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(
                None,
                "system",
                &json!({"scope":"power","operation":"hibernate"}),
                RiskLevel::Critical
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_deny_overrides_permanent_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "files",
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            "files",
            PermissionEffect::Deny,
            PermissionScope::Session,
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-a"),
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_child_allow_cannot_bypass_parent_deny() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "files",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            None,
            "files:read",
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;

        assert!(matches!(
            gw.check(None, "files", &json!({"operation": "read"}), RiskLevel::Low)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_child_deny_cannot_be_bypassed_by_parent_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "files",
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            None,
            "files:delete",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;

        assert!(matches!(
            gw.check(
                None,
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_session_parent_deny_beats_session_child_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-a"),
            "system:power",
            PermissionEffect::Deny,
            PermissionScope::Session,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            "system:power:lock",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;

        assert!(matches!(
            gw.check(
                Some("ses-a"),
                "system",
                &json!({"scope": "power", "operation": "lock"}),
                RiskLevel::High,
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_safety_gateway_permanent_deny_beats_session_allow() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.grant(
            None,
            "shell",
            PermissionEffect::Deny,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            "shell",
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;

        assert!(matches!(
            gw.check(Some("ses-a"), "shell", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_local_tool_security_matrix_gates_every_risk_bearing_case() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        for case in LOCAL_TOOL_SECURITY_MATRIX {
            let input = matrix_input(case);
            let result = gw
                .check(None, case.tool_name, &input, case.risk_level)
                .await;
            if case.risk_level >= RiskLevel::Medium {
                assert!(
                    matches!(result, ConfirmationResult::RequiresConfirmation { .. }),
                    "{}:{} should be gated, got {result:?}",
                    case.tool_name,
                    case.operation
                );
            } else {
                assert!(
                    matches!(result, ConfirmationResult::AutoApproved),
                    "{}:{} should be automatic, got {result:?}",
                    case.tool_name,
                    case.operation
                );
            }
        }
    }

    /// Extract the routing fields from the operation branches of a tool
    /// schema. This intentionally inspects the provider-facing schema rather
    /// than duplicating each tool's operation list in the test.
    fn schema_route_inputs(schema: &Value) -> Vec<Value> {
        let mut local_variants = vec![serde_json::Map::new()];
        for field in ["scope", "operation"] {
            let Some(property) = schema.get("properties").and_then(|p| p.get(field)) else {
                continue;
            };
            let values = if let Some(value) = property.get("const") {
                vec![value.clone()]
            } else {
                property
                    .get("enum")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            if values.is_empty() {
                continue;
            }
            let mut expanded = Vec::new();
            for variant in local_variants {
                for value in &values {
                    let mut next = variant.clone();
                    next.insert(field.to_string(), value.clone());
                    expanded.push(next);
                }
            }
            local_variants = expanded;
        }

        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            let mut routes = Vec::new();
            for branch in branches {
                routes.extend(schema_route_inputs(branch));
            }
            // A top-level schema often carries a broad operation enum while
            // its oneOf branches carry the authoritative route. Nested
            // oneOf branches (for example schedule.set's timing alternatives)
            // may carry no routing fields; in that case retain the local
            // const/enum route instead of producing a spurious empty route.
            if routes.iter().any(route_has_selector) {
                return dedupe_json(routes);
            }
        }
        dedupe_json(local_variants.into_iter().map(Value::Object).collect())
    }

    fn route_has_selector(value: &Value) -> bool {
        value.get("scope").is_some() || value.get("operation").is_some()
    }

    fn dedupe_json(values: Vec<Value>) -> Vec<Value> {
        let mut seen = HashSet::new();
        values
            .into_iter()
            .filter(|value| seen.insert(value.to_string()))
            .collect()
    }

    fn route_label(tool_name: &str, input: &Value) -> String {
        if tool_name == "system" {
            let scope = input["scope"].as_str().unwrap_or("info");
            return match input["operation"].as_str() {
                Some(operation) => format!("{scope}:{operation}"),
                None => scope.to_string(),
            };
        }
        if tool_name == "files"
            && input["operation"].as_str() == Some("search")
            && input["mode"].as_str() == Some("content")
        {
            return "search:content".into();
        }
        input["operation"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| match tool_name {
                "ask" => "ask".into(),
                "actions" => "list".into(),
                "shell" => "execute".into(),
                "http" => "request".into(),
                "notify" => "notify".into(),
                "load_skill" | "load_mcp" => "load".into(),
                other => other.to_string(),
            })
    }

    fn matrix_input(case: &LocalToolSecurityCase) -> Value {
        if case.tool_name == "system" {
            let mut parts = case.operation.split(':');
            let scope = parts.next().unwrap();
            let mut input = serde_json::json!({"scope": scope});
            if let Some(operation) = parts.next() {
                input["operation"] = Value::String(operation.into());
            }
            return input;
        }
        if case.operation == "search:content" {
            return serde_json::json!({"operation": "search", "mode": "content"});
        }
        match case.tool_name {
            "ask" | "actions" | "shell" | "http" | "notify" | "load_skill" | "load_mcp" => {
                serde_json::json!({})
            }
            _ => serde_json::json!({"operation": case.operation}),
        }
    }

    #[tokio::test]
    async fn test_builtin_registry_security_contract_covers_every_route() {
        use crate::ToolsManager;
        use crate::builtin::SelfToolContext;
        use haven_common::config::{ConfigLoader, ConfigService};
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let manager = ToolsManager::new();
        manager
            .set_admin_context(SelfToolContext {
                config_service: Some(Arc::new(ConfigService::new(loader))),
                db: None,
                router: None,
                log_path: None,
                set_log_level: None,
                tools_weak: None,
            })
            .await;

        let tools = manager.registry.list().await;
        let matrix_names: HashSet<_> = LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .map(|case| case.tool_name)
            .collect();
        let registry_names: HashSet<_> = tools.iter().map(|tool| tool.name()).collect();
        assert_eq!(
            registry_names,
            matrix_names
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            "security matrix tool families must match the actual builtin registry"
        );

        let gateway = SafetyGateway::new(RiskLevel::Medium);
        let mut seen = HashSet::new();
        for tool in tools {
            let name = tool.name();
            let mut inputs = schema_route_inputs(&tool.input_schema());
            // `files:search` has a mode-dependent risk level. The schema
            // operation is one route, but both risk-bearing modes need a
            // contract assertion.
            if name == "files" {
                let content_search = serde_json::json!({
                    "operation": "search",
                    "mode": "content"
                });
                if !inputs.iter().any(|input| input == &content_search) {
                    inputs.push(content_search);
                }
            }
            assert!(!inputs.is_empty(), "{name} must expose a contract route");

            for mut input in inputs {
                if name == "haven_config" && input["operation"].as_str() == Some("logs_level") {
                    // TypedToolAdapter parses the full operation args before
                    // consulting metadata, so provide the smallest valid
                    // non-routing field for this route.
                    input["level"] = Value::String("info".into());
                }
                let operation = route_label(&name, &input);
                let case = LOCAL_TOOL_SECURITY_MATRIX
                    .iter()
                    .find(|case| case.tool_name == name && case.operation == operation)
                    .unwrap_or_else(|| {
                        panic!("missing security matrix row for {name}:{operation}")
                    });
                let reported_risk = tool.risk_level(&input);
                assert_eq!(
                    reported_risk, case.risk_level,
                    "risk level drift for {name}:{operation}"
                );

                let key = permission_key(&name, &input);
                assert!(
                    !key.is_empty(),
                    "empty permission key for {name}:{operation}"
                );
                assert_eq!(
                    permission_key_candidates(&key).last().copied(),
                    Some(name.as_str()),
                    "permission key must remain rooted at the registered tool"
                );
                assert_eq!(
                    key,
                    permission_key(&name, &matrix_input(case)),
                    "permission key drift for {name}:{operation}"
                );

                let decision = gateway.check(None, &name, &input, reported_risk).await;
                if reported_risk >= RiskLevel::Medium {
                    assert!(
                        matches!(decision, ConfirmationResult::RequiresConfirmation { .. }),
                        "{name}:{operation} with {reported_risk:?} must be gated"
                    );
                } else {
                    assert!(
                        matches!(decision, ConfirmationResult::AutoApproved),
                        "{name}:{operation} with {reported_risk:?} must be automatic"
                    );
                }
                seen.insert((name.clone(), operation));
            }
        }

        for case in LOCAL_TOOL_SECURITY_MATRIX {
            assert!(
                seen.contains(&(case.tool_name.to_string(), case.operation.to_string())),
                "matrix row is not advertised by the builtin registry: {}:{}",
                case.tool_name,
                case.operation
            );
        }
    }

    #[tokio::test]
    async fn test_adapter_authorization_is_shared_but_session_scoped() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        let mcp_name = crate::McpToolAdapter::qualified_name_of("calendar", "create_event");
        let skill_name = crate::SkillToolAdapter::qualified_name_of("calendar");

        gw.grant(
            None,
            &mcp_name,
            PermissionEffect::Allow,
            PermissionScope::Always,
        )
        .await;
        gw.grant(
            Some("ses-a"),
            &skill_name,
            PermissionEffect::Allow,
            PermissionScope::Session,
        )
        .await;

        assert!(matches!(
            gw.check(None, &mcp_name, &json!({}), RiskLevel::High).await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(Some("ses-a"), &skill_name, &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(None, &skill_name, &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn test_local_tool_security_matrix_covers_every_builtin_family() {
        let names: HashSet<_> = LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .map(|case| case.tool_name)
            .collect();
        for expected in [
            "audio",
            "ask",
            "files",
            "process",
            "clipboard",
            "shell",
            "actions",
            "input",
            "schedule",
            "system",
            "window",
            "http",
            "notify",
            "agent",
            "load_skill",
            "load_mcp",
            "memory",
            "haven_diagnostics",
            "haven_config",
            "haven_skills",
            "haven_tools",
            "haven_mcp",
            "haven_session_diagnostics",
        ] {
            assert!(
                names.contains(expected),
                "missing matrix family: {expected}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_path_sandbox_rejects_symlink_reparse_escape() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = allowed.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        assert!(!path_is_allowed(
            &link.join("secret.txt"),
            &[allowed.path().to_path_buf()]
        ));
    }

    #[test]
    fn test_path_sandbox_allows_only_absolute_paths_inside_canonical_root() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = allowed.path().join("new").join("file.txt");

        assert!(path_is_allowed(&inside, &[allowed.path().to_path_buf()]));
        assert!(!path_is_allowed(
            &outside.path().join("file.txt"),
            &[allowed.path().to_path_buf()]
        ));
        assert!(!path_is_allowed(
            Path::new("relative.txt"),
            &[allowed.path().to_path_buf()]
        ));
        assert!(!path_is_allowed(
            Path::new("//server/share/file.txt"),
            &[allowed.path().to_path_buf()]
        ));
    }

    #[tokio::test]
    async fn test_path_sandbox_checks_source_and_destination_together() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                allowed_paths: vec![allowed.path().to_string_lossy().into_owned()],
                ..ToolConfig::default()
            },
        );
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        let result = gw
            .check(
                None,
                "files",
                &json!({
                    "operation": "copy",
                    "source": allowed.path().join("source.txt"),
                    "destination": outside.path().join("destination.txt"),
                }),
                RiskLevel::Medium,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_path_sandbox_checks_audio_file_path() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut settings = HashMap::new();
        settings.insert(
            "audio".into(),
            ToolConfig {
                allowed_paths: vec![allowed.path().to_string_lossy().into_owned()],
                ..ToolConfig::default()
            },
        );
        let gw = SafetyGateway::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        let result = gw
            .check(
                None,
                "audio",
                &json!({
                    "operation": "play",
                    "file_path": outside.path().join("outside.wav"),
                }),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_path_sandbox_rejects_parent_escape() {
        let gw = SafetyGateway::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                allowed_paths: vec!["C:\\allowed".into()],
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "read", "path": "C:\\allowed\\..\\Windows\\System32"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }
}
