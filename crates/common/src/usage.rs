use serde::{Deserialize, Serialize};

/// How a provider accounts for prompt-cache tokens in `prompt_tokens`.
///
/// The Rust, event, and TypeScript contracts use this closed vocabulary. The
/// database stores its snake_case representation at the SQLite boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheAccounting {
    Inclusive,
    Exclusive,
    #[default]
    Unknown,
}

impl CacheAccounting {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inclusive => "inclusive",
            Self::Exclusive => "exclusive",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "inclusive" => Self::Inclusive,
            "exclusive" => Self::Exclusive,
            _ => Self::Unknown,
        }
    }
}

/// The runtime owner of one LLM usage record.
///
/// Runtime inputs and the live `agent:usage` event use this closed category.
/// Its snake_case JSON representation matches the persisted `llm_usage`
/// string column, which remains a storage-boundary projection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmCallKind {
    Agent,
    Media,
    Tool,
}

impl LlmCallKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Media => "media",
            Self::Tool => "tool",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "media" => Some(Self::Media),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// Effective strategy used to make a prompt-cacheable request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheStrategy {
    Off,
    Key,
    Split,
    Implicit,
    Explicit,
}

impl PromptCacheStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Key => "key",
            Self::Split => "split",
            Self::Implicit => "implicit",
            Self::Explicit => "explicit",
        }
    }
}

/// Cache-use result reported by the provider for one call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheDiagnosticOutcome {
    Disabled,
    Unknown,
    Hit,
    Miss,
}

impl CacheDiagnosticOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Unknown => "unknown",
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

/// Source of the per-call prompt-cache usage measurement.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheUsageSource {
    Provider,
    Unavailable,
}

impl CacheUsageSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Non-sensitive prompt-cache request and provider outcome metadata.
///
/// This provider-neutral value is shared by the LLM adapter, Agent events,
/// persisted usage projection, and UI. It never contains a cache key or prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheDiagnostics {
    /// Effective strategy used on the provider wire.
    pub strategy: PromptCacheStrategy,
    /// Configured provider identity, independent of its wire protocol adapter.
    pub provider: String,
    pub key_requested: bool,
    pub system_split: bool,
    /// True when an optional cache extension was rejected and safely retried.
    pub downgraded: bool,
    /// Provider-reported cache result; absence is kept as `unknown`.
    pub outcome: CacheDiagnosticOutcome,
    /// Whether the provider returned a cache usage field, including zero.
    pub usage_source: CacheUsageSource,
}

impl Default for CacheDiagnostics {
    fn default() -> Self {
        Self {
            strategy: PromptCacheStrategy::Off,
            provider: String::new(),
            key_requested: false,
            system_split: false,
            downgraded: false,
            outcome: CacheDiagnosticOutcome::Unknown,
            usage_source: CacheUsageSource::Unavailable,
        }
    }
}

impl CacheDiagnostics {
    pub fn for_request(key_requested: bool, system_split: bool) -> Self {
        Self {
            strategy: if system_split {
                PromptCacheStrategy::Split
            } else if key_requested {
                PromptCacheStrategy::Key
            } else {
                PromptCacheStrategy::Off
            },
            provider: String::new(),
            key_requested,
            system_split,
            downgraded: false,
            outcome: if key_requested || system_split {
                CacheDiagnosticOutcome::Unknown
            } else {
                CacheDiagnosticOutcome::Disabled
            },
            usage_source: CacheUsageSource::Unavailable,
        }
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Use for providers with cache controls or automatic prefix caching but
    /// without an explicit routing key.
    pub fn for_provider_cache(system_split: bool) -> Self {
        Self {
            strategy: if system_split {
                PromptCacheStrategy::Split
            } else {
                PromptCacheStrategy::Implicit
            },
            provider: String::new(),
            key_requested: false,
            system_split,
            downgraded: false,
            outcome: CacheDiagnosticOutcome::Unknown,
            usage_source: CacheUsageSource::Unavailable,
        }
    }

    /// Use when a provider resource explicitly owns the reusable prompt prefix.
    pub fn for_explicit_provider_cache(system_split: bool) -> Self {
        Self {
            strategy: PromptCacheStrategy::Explicit,
            provider: String::new(),
            key_requested: false,
            system_split,
            downgraded: false,
            outcome: CacheDiagnosticOutcome::Unknown,
            usage_source: CacheUsageSource::Unavailable,
        }
    }

    pub fn with_provider_usage(mut self, cached_tokens: Option<u32>, usage_reported: bool) -> Self {
        self.usage_source = if usage_reported {
            CacheUsageSource::Provider
        } else {
            CacheUsageSource::Unavailable
        };
        if usage_reported {
            self.outcome = match cached_tokens {
                Some(tokens) if tokens > 0 => CacheDiagnosticOutcome::Hit,
                Some(_) => CacheDiagnosticOutcome::Miss,
                None => CacheDiagnosticOutcome::Unknown,
            };
        } else if self.outcome != CacheDiagnosticOutcome::Disabled {
            self.outcome = CacheDiagnosticOutcome::Unknown;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_diagnostics_distinguish_missing_usage_from_explicit_zero() {
        let base = CacheDiagnostics::for_request(true, false);
        let unavailable = base.clone().with_provider_usage(None, false);
        assert_eq!(unavailable.outcome, CacheDiagnosticOutcome::Unknown);
        assert_eq!(unavailable.usage_source, CacheUsageSource::Unavailable);

        let zero = base.with_provider_usage(Some(0), true);
        assert_eq!(zero.outcome, CacheDiagnosticOutcome::Miss);
        assert_eq!(zero.usage_source, CacheUsageSource::Provider);

        let hit = CacheDiagnostics::for_provider_cache(false).with_provider_usage(Some(8), true);
        assert_eq!(hit.outcome, CacheDiagnosticOutcome::Hit);
        assert_eq!(hit.usage_source, CacheUsageSource::Provider);

        let off_but_reported =
            CacheDiagnostics::for_request(false, false).with_provider_usage(Some(8), true);
        assert_eq!(off_but_reported.outcome, CacheDiagnosticOutcome::Hit);
        assert_eq!(off_but_reported.usage_source, CacheUsageSource::Provider);

        let disabled_without_usage =
            CacheDiagnostics::for_request(false, false).with_provider_usage(None, false);
        assert_eq!(
            disabled_without_usage.outcome,
            CacheDiagnosticOutcome::Disabled
        );
    }

    #[test]
    fn cache_diagnostics_roundtrip_uses_the_current_typed_metadata_shape() {
        let diagnostics = CacheDiagnostics::for_request(true, false)
            .with_provider("openai")
            .with_provider_usage(Some(12), true);
        let encoded = serde_json::to_value(&diagnostics).unwrap();
        assert_eq!(encoded["strategy"], "key");
        assert_eq!(encoded["outcome"], "hit");
        assert!(encoded.get("mode").is_none());
        let decoded: CacheDiagnostics = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, diagnostics);
    }

    #[test]
    fn llm_call_kind_string_and_serde_contract() {
        for (kind, value) in [
            (LlmCallKind::Agent, "agent"),
            (LlmCallKind::Media, "media"),
            (LlmCallKind::Tool, "tool"),
        ] {
            assert_eq!(kind.as_str(), value);
            assert_eq!(LlmCallKind::parse(value), Some(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{value}\"")
            );
            assert_eq!(
                serde_json::from_str::<LlmCallKind>(&format!("\"{value}\"")).unwrap(),
                kind
            );
        }
        assert_eq!(LlmCallKind::parse("unknown"), None);
        assert!(serde_json::from_str::<LlmCallKind>("\"unknown\"").is_err());
    }
}
