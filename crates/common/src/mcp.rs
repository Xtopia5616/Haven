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

/// Whether a configured MCP tool is callable through any model-facing adapter.
/// Availability does not grant permission to perform the remote operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpToolAvailability {
    #[default]
    Enabled,
    Disabled,
}

/// How the tool appears to model-driven discovery.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpCatalogPresentation {
    /// List the tool in the general catalog and expose it without an explicit load.
    Direct,
    /// List the tool for discovery; native calls require an explicit load.
    #[default]
    Deferred,
    /// Omit the tool from the general catalog. Invocation adapters remain independent.
    Hidden,
}

/// Invocation adapters that can route the capability to the MCP server.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpInvocationAdapters {
    #[default]
    None,
    Native,
    #[serde(rename = "codemode")]
    CodeMode,
    #[serde(rename = "native_and_codemode")]
    NativeAndCodeMode,
}

impl McpInvocationAdapters {
    pub const fn supports_any(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn supports_native(self) -> bool {
        matches!(self, Self::Native | Self::NativeAndCodeMode)
    }

    pub const fn supports_codemode(self) -> bool {
        matches!(self, Self::CodeMode | Self::NativeAndCodeMode)
    }
}

/// Independent call availability, catalog presentation, and adapter support
/// for one MCP capability. These fields control routing only; authorization
/// remains per invocation in Tools.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct McpToolExposure {
    #[serde(default)]
    pub availability: McpToolAvailability,
    #[serde(default)]
    pub catalog: McpCatalogPresentation,
    #[serde(default)]
    pub invocation: McpInvocationAdapters,
}

impl Default for McpToolExposure {
    fn default() -> Self {
        Self {
            availability: McpToolAvailability::Enabled,
            catalog: McpCatalogPresentation::Deferred,
            invocation: McpInvocationAdapters::Native,
        }
    }
}

impl McpToolExposure {
    pub const fn is_available(self) -> bool {
        matches!(self.availability, McpToolAvailability::Enabled)
    }

    /// Whether an enabled native adapter is sent directly to the provider.
    /// Deferred tools require explicit loading; hidden only affects the general catalog.
    pub const fn is_provider_native(self) -> bool {
        self.is_available()
            && !matches!(self.catalog, McpCatalogPresentation::Deferred)
            && self.invocation.supports_native()
    }

    /// Whether the capability may be described through Haven's MCP catalog.
    pub const fn is_catalog_visible(self) -> bool {
        !matches!(self.catalog, McpCatalogPresentation::Hidden)
    }

    pub const fn has_invocation_adapter(self) -> bool {
        self.invocation.supports_any()
    }

    pub const fn requires_native_load(self) -> bool {
        self.is_available()
            && matches!(self.catalog, McpCatalogPresentation::Deferred)
            && self.invocation.supports_native()
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
    use super::{
        McpCatalogPresentation as Catalog, McpInvocationAdapters as Adapters,
        McpToolAvailability as Availability, McpToolExposure as Exposure,
        resolve_mcp_tool_exposure,
    };
    use std::collections::BTreeMap;

    fn exposure(catalog: Catalog, invocation: Adapters) -> Exposure {
        Exposure {
            catalog,
            invocation,
            ..Exposure::default()
        }
    }

    #[test]
    fn exposure_serializes_its_three_independent_axes() {
        let value = exposure(Catalog::Direct, Adapters::NativeAndCodeMode);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"availability":"enabled","catalog":"direct","invocation":"native_and_codemode"}"#
        );
        assert!(serde_json::from_str::<Exposure>(r#""legacy""#).is_err());
        assert!(value.is_provider_native());
        assert!(value.invocation.supports_codemode());
    }

    #[test]
    fn availability_and_catalog_visibility_do_not_imply_an_invocation_adapter() {
        let value = Exposure {
            invocation: Adapters::None,
            ..Exposure::default()
        };
        assert!(value.is_available());
        assert!(value.is_catalog_visible());
        assert!(!value.is_provider_native());
        assert!(!value.requires_native_load());

        let hidden_native = exposure(Catalog::Hidden, Adapters::Native);
        assert!(!hidden_native.is_catalog_visible());
        assert!(hidden_native.is_provider_native());

        let disabled = Exposure {
            availability: Availability::Disabled,
            ..exposure(Catalog::Direct, Adapters::Native)
        };
        assert!(!disabled.is_provider_native());
    }

    #[test]
    fn exact_tool_override_wins_over_matching_patterns() {
        let overrides = BTreeMap::from([
            ("delete_*".into(), exposure(Catalog::Hidden, Adapters::None)),
            (
                "delete_user".into(),
                exposure(Catalog::Direct, Adapters::Native),
            ),
        ]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::default(), &overrides, "delete_user"),
            exposure(Catalog::Direct, Adapters::Native)
        );
        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::default(), &overrides, "delete_project"),
            exposure(Catalog::Hidden, Adapters::None)
        );
    }

    #[test]
    fn most_specific_pattern_wins_independent_of_insertion_order() {
        let overrides = BTreeMap::from([
            ("get_*".into(), exposure(Catalog::Direct, Adapters::Native)),
            (
                "get_user_*".into(),
                exposure(Catalog::Hidden, Adapters::None),
            ),
            (
                "get_*_by_id".into(),
                exposure(Catalog::Hidden, Adapters::CodeMode),
            ),
        ]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::default(), &overrides, "get_user_by_id"),
            exposure(Catalog::Hidden, Adapters::CodeMode)
        );
    }

    #[test]
    fn equal_specificity_patterns_use_ascending_lexicographic_order() {
        let overrides = BTreeMap::from([
            ("read_*".into(), exposure(Catalog::Direct, Adapters::Native)),
            ("*_file".into(), exposure(Catalog::Hidden, Adapters::None)),
        ]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::default(), &overrides, "read_file"),
            exposure(Catalog::Hidden, Adapters::None)
        );
    }

    #[test]
    fn wildcard_matches_unicode_and_empty_segments() {
        let overrides = BTreeMap::from([(
            "文件_*_读取".into(),
            exposure(Catalog::Hidden, Adapters::CodeMode),
        )]);

        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::default(), &overrides, "文件_项目_读取"),
            exposure(Catalog::Hidden, Adapters::CodeMode)
        );
        assert_eq!(
            resolve_mcp_tool_exposure(Exposure::default(), &overrides, "文件__读取"),
            exposure(Catalog::Hidden, Adapters::CodeMode)
        );
    }

    #[test]
    fn unmatched_tool_uses_server_default() {
        let overrides = BTreeMap::from([(
            "private_*".into(),
            exposure(Catalog::Hidden, Adapters::None),
        )]);
        assert_eq!(
            resolve_mcp_tool_exposure(
                exposure(Catalog::Direct, Adapters::Native),
                &overrides,
                "public_info"
            ),
            exposure(Catalog::Direct, Adapters::Native)
        );
    }
}
