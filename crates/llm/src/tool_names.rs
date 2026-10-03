//! Provider-specific function-name projection.
//!
//! Internal tool names are stable Haven contracts and may contain dots. Some
//! provider APIs reject those characters, and some require the name returned
//! by a tool call to be echoed exactly in the next request. This module keeps
//! the alias mapping local to one request while preserving canonical names in
//! the LLM types consumed by the Agent.

use crate::types::ToolDefinition;
use haven_common::types::{CanonicalMessage, CanonicalRole};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ToolNamePolicy {
    /// The provider's restricted function-name character set, with its
    /// documented maximum length.
    Restricted { max_len: usize },
    /// xAI's current function-calling reference requires unique names but
    /// documents no additional character or length restriction.
    Identity,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolNameMap {
    canonical_to_provider: HashMap<String, String>,
    provider_to_canonical: HashMap<String, String>,
}

impl ToolNameMap {
    pub(crate) fn for_request(
        tools: &[ToolDefinition],
        messages: &[CanonicalMessage],
        policy: ToolNamePolicy,
    ) -> Self {
        let ToolNamePolicy::Restricted { max_len } = policy else {
            return Self::default();
        };

        let mut names = BTreeSet::new();
        names.extend(tools.iter().map(|tool| tool.function.name.clone()));
        names.extend(
            messages
                .iter()
                .filter(|message| message.role == CanonicalRole::Assistant)
                .flat_map(|message| message.tool_calls.iter().flatten())
                .map(|call| call.name.clone()),
        );

        let names: Vec<_> = names.into_iter().collect();
        let mut aliases: Vec<_> = names
            .iter()
            .map(|name| normalize_name(name, max_len))
            .collect();

        // A dotted name can normalize to an existing underscore name (for
        // example `files.read` and `files_read`). Hash only colliding aliases
        // so normal, already-valid provider names remain unchanged.
        let truncated: Vec<_> = names.iter().map(|name| name.len() > max_len).collect();
        let initial_counts = alias_counts(&aliases);
        for index in 0..aliases.len() {
            if truncated[index] || initial_counts.get(&aliases[index]).copied().unwrap_or(0) > 1 {
                aliases[index] = hashed_alias(&names[index], max_len, 0);
            }
        }

        // Also guard against an intentionally chosen safe name colliding with
        // a hashed alias. Salt each duplicate deterministically until unique.
        loop {
            let counts = alias_counts(&aliases);
            let duplicates: Vec<_> = aliases
                .iter()
                .enumerate()
                .filter_map(|(index, alias)| {
                    (counts.get(alias).copied().unwrap_or(0) > 1).then_some(index)
                })
                .collect();
            if duplicates.is_empty() {
                break;
            }
            for index in duplicates {
                let mut salt = 1u32;
                loop {
                    let candidate = hashed_alias(&names[index], max_len, salt);
                    if !aliases
                        .iter()
                        .enumerate()
                        .any(|(other, alias)| other != index && alias == &candidate)
                    {
                        aliases[index] = candidate;
                        break;
                    }
                    salt = salt.saturating_add(1);
                }
            }
        }

        let canonical_to_provider: HashMap<_, _> =
            names.iter().cloned().zip(aliases.iter().cloned()).collect();
        let provider_to_canonical = aliases.into_iter().zip(names).collect();
        Self {
            canonical_to_provider,
            provider_to_canonical,
        }
    }

    pub(crate) fn to_provider(&self, canonical_name: &str) -> String {
        self.canonical_to_provider
            .get(canonical_name)
            .cloned()
            .unwrap_or_else(|| canonical_name.to_string())
    }

    pub(crate) fn to_canonical(&self, provider_name: &str) -> String {
        self.provider_to_canonical
            .get(provider_name)
            .cloned()
            .unwrap_or_else(|| provider_name.to_string())
    }
}

fn normalize_name(name: &str, max_len: usize) -> String {
    let mut normalized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if normalized.is_empty() {
        normalized.push_str("tool");
    }
    if normalized.len() > max_len {
        normalized.truncate(max_len);
    }
    normalized
}

fn alias_counts(aliases: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::with_capacity(aliases.len());
    for alias in aliases {
        *counts.entry(alias.clone()).or_default() += 1;
    }
    counts
}

fn hashed_alias(name: &str, max_len: usize, salt: u32) -> String {
    const DIGEST_HEX_LEN: usize = 12;
    let digest = Sha256::digest(format!("{name}\0{salt}").as_bytes());
    let suffix: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let prefix_len = max_len.saturating_sub(DIGEST_HEX_LEN + 1);
    let mut prefix = normalize_name(name, usize::MAX);
    prefix.truncate(prefix_len);
    format!("{prefix}_{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolFunction;

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }
    }

    #[test]
    fn restricted_policy_aliases_dotted_names_and_restores_them() {
        let tools = [tool("files.read")];
        let names =
            ToolNameMap::for_request(&tools, &[], ToolNamePolicy::Restricted { max_len: 128 });

        let provider_name = names.to_provider("files.read");
        assert_eq!(provider_name, "files_read");
        assert_eq!(names.to_canonical(&provider_name), "files.read");
        assert!(
            provider_name.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            )
        );
    }

    #[test]
    fn restricted_policy_disambiguates_normalized_name_collisions() {
        let tools = [tool("files.read"), tool("files_read")];
        let names =
            ToolNameMap::for_request(&tools, &[], ToolNamePolicy::Restricted { max_len: 64 });

        let dotted = names.to_provider("files.read");
        let underscored = names.to_provider("files_read");
        assert_ne!(dotted, underscored);
        assert_eq!(names.to_canonical(&dotted), "files.read");
        assert_eq!(names.to_canonical(&underscored), "files_read");
    }

    #[test]
    fn restricted_policy_caps_long_aliases() {
        let long_name = "a".repeat(140);
        let tools = [tool(&long_name)];
        let names =
            ToolNameMap::for_request(&tools, &[], ToolNamePolicy::Restricted { max_len: 128 });
        let provider_name = names.to_provider(&long_name);

        assert_eq!(provider_name.len(), 128);
        assert_eq!(names.to_canonical(&provider_name), long_name);
    }

    #[test]
    fn identity_policy_preserves_provider_names() {
        let tools = [tool("files.read")];
        let names = ToolNameMap::for_request(&tools, &[], ToolNamePolicy::Identity);

        assert_eq!(names.to_provider("files.read"), "files.read");
        assert_eq!(names.to_canonical("files.read"), "files.read");
    }

    #[test]
    fn request_map_includes_historical_assistant_tool_calls() {
        let messages = [CanonicalMessage::assistant(
            Vec::new(),
            Some(vec![haven_common::types::CanonicalToolCall {
                id: "call_1".into(),
                name: "files.read".into(),
                arguments: serde_json::json!({}),
            }]),
            None,
            Vec::new(),
            Vec::new(),
        )];
        let names =
            ToolNameMap::for_request(&[], &messages, ToolNamePolicy::Restricted { max_len: 128 });

        assert_eq!(names.to_provider("files.read"), "files_read");
        assert_eq!(names.to_canonical("files_read"), "files.read");
    }
}
