//! Shared MCP vocabulary for configuration, protocol and IPC.

use serde::{Deserialize, Serialize};

/// Connection transport supported by an MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpServerTransport {
    #[default]
    Stdio,
    Http,
}

impl McpServerTransport {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

/// Model-facing route for one MCP capability. Exposure controls discoverability
/// and invocation surface; it does not grant permission to perform the action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpToolExposure {
    /// Add the tool schema to the provider-native tool list.
    Direct,
    /// Add the tool schema to the provider-native tool list, but never permit
    /// it as a nested CodeMode or Workflow capability.
    #[serde(rename = "model_only")]
    ModelOnly,
    /// Require catalog discovery and explicit activation before native calls.
    #[default]
    Deferred,
    /// Permit calls only through the isolated CodeMode executor.
    #[serde(rename = "codemode")]
    CodeMode,
    /// Make the tool unavailable to model-driven execution.
    Hidden,
}

impl McpToolExposure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::ModelOnly => "model_only",
            Self::Deferred => "deferred",
            Self::CodeMode => "codemode",
            Self::Hidden => "hidden",
        }
    }

    /// Whether the tool belongs to the provider-native model surface.
    pub const fn is_provider_native(self) -> bool {
        matches!(self, Self::Direct | Self::ModelOnly)
    }

    /// Whether another Haven tool may invoke this capability as a bounded
    /// Workflow child. ModelOnly deliberately does not satisfy this policy.
    pub const fn is_workflow_callable(self) -> bool {
        matches!(self, Self::Direct | Self::Deferred)
    }

    /// Whether the capability may be described through Haven's MCP catalog.
    pub const fn is_catalog_visible(self) -> bool {
        matches!(self, Self::Direct | Self::ModelOnly | Self::Deferred)
    }
}

/// Resolve one MCP tool's effective exposure from its server default and
/// configured per-tool patterns. Exact names win. Otherwise, the matching
/// pattern with the most literal characters wins; ties use ascending
/// lexicographic order, independent of map insertion or TOML key order.
/// Patterns use `*` to match any number of characters.
pub fn resolve_mcp_tool_exposure(
    server_default: McpToolExposure,
    tool_overrides: &std::collections::BTreeMap<String, McpToolExposure>,
    tool_name: &str,
) -> McpToolExposure {
    if let Some(exact) = tool_overrides.get(tool_name) {
        return *exact;
    }

    tool_overrides
        .iter()
        .filter(|(pattern, _)| {
            pattern.contains('*') && mcp_tool_pattern_matches(pattern, tool_name)
        })
        .min_by(|(left, _), (right, _)| {
            let left_literal_count = left.chars().filter(|character| *character != '*').count();
            let right_literal_count = right.chars().filter(|character| *character != '*').count();
            right_literal_count
                .cmp(&left_literal_count)
                .then_with(|| left.cmp(right))
        })
        .map_or(server_default, |(_, exposure)| *exposure)
}

fn mcp_tool_pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut pattern_index = 0;
    let mut value_index = 0;
    let mut last_star = None;
    let mut value_after_star = 0;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && pattern[pattern_index] != '*'
            && pattern[pattern_index] == value[value_index]
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            last_star = Some(pattern_index);
            pattern_index += 1;
            value_after_star = value_index;
        } else if let Some(star_index) = last_star {
            value_after_star += 1;
            value_index = value_after_star;
            pattern_index = star_index + 1;
        } else {
            return false;
        }
    }

    pattern[pattern_index..]
        .iter()
        .all(|character| *character == '*')
}

#[cfg(test)]
mod tests {
    use super::{McpToolExposure as Exposure, resolve_mcp_tool_exposure};
    use std::collections::BTreeMap;

    #[test]
    fn exposure_serializes_as_closed_lowercase_values() {
        assert_eq!(
            serde_json::to_string(&Exposure::CodeMode).unwrap(),
            "\"codemode\""
        );
        assert!(serde_json::from_str::<Exposure>("\"legacy\"").is_err());
        assert_eq!(
            serde_json::to_string(&Exposure::ModelOnly).unwrap(),
            "\"model_only\""
        );
        assert!(!Exposure::ModelOnly.is_workflow_callable());
        assert!(Exposure::ModelOnly.is_provider_native());
    }

    #[test]
    fn exact_tool_override_wins_over_matching_patterns() {
        let overrides = BTreeMap::from([
            ("delete_*".into(), Exposure::Hidden),
            ("delete_user".into(), Exposure::Direct),
        ]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Deferred, &overrides, "delete_user"),
            Exposure::Direct
        );
        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Deferred, &overrides, "delete_project"),
            Exposure::Hidden
        );
    }

    #[test]
    fn most_specific_pattern_wins_independent_of_insertion_order() {
        let overrides = BTreeMap::from([
            ("get_*".into(), Exposure::Direct),
            ("get_user_*".into(), Exposure::Hidden),
            ("get_*_by_id".into(), Exposure::CodeMode),
        ]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Deferred, &overrides, "get_user_by_id"),
            Exposure::CodeMode
        );
    }

    #[test]
    fn equal_specificity_patterns_use_ascending_lexicographic_order() {
        let overrides = BTreeMap::from([
            ("read_*".into(), Exposure::Direct),
            ("*_file".into(), Exposure::Hidden),
        ]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Deferred, &overrides, "read_file"),
            Exposure::Hidden
        );
    }

    #[test]
    fn wildcard_matches_unicode_and_empty_segments() {
        let overrides = BTreeMap::from([("文件_*_读取".into(), Exposure::CodeMode)]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Deferred, &overrides, "文件_项目_读取"),
            Exposure::CodeMode
        );
        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Deferred, &overrides, "文件__读取"),
            Exposure::CodeMode
        );
    }

    #[test]
    fn unmatched_tool_uses_server_default() {
        let overrides = BTreeMap::from([("private_*".into(), Exposure::Hidden)]);
        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::Direct, &overrides, "public_info"),
            Exposure::Direct
        );
    }
}
