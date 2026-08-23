//! Wire-protocol (`api_style`) capability helpers used by adapters.
//!
//! Canonical normalize / capability predicates live in
//! [`haven_common::config`] so `materialize_endpoint` can clear sticky
//! `web_search` without depending on this crate. This module re-exports them
//! and owns [`WebSearchMode`] resolution shared by every adapter.
//!
//! | Stored / derived style     | Normalized wire   | Built-in web search |
//! |----------------------------|-------------------|---------------------|
//! | `openai-chat`              | `openai-chat`     | no                  |
//! | `llama.cpp`                | `llama.cpp`       | no                  |
//! | `openai-responses`         | `openai-responses`| yes                 |
//! | `deepseek-responses`       | `openai-responses`| yes                 |
//! | `xai` / `grok`             | `xai`             | yes                 |
//! | `anthropic`                | `anthropic`       | yes                 |
//! | `gemini`                   | `gemini`          | yes                 |
//! | `deepgram` / `assemblyai`  | same              | n/a                 |

use haven_common::config::ModelEndpoint;

pub use haven_common::config::{
    api_style_from_provider, is_known_api_style, is_openai_family_wire_style, is_stt_only_style,
    is_tts_only_style, normalize_api_style, supports_builtin_web_search,
};

/// Web search mode for a provider's built-in search tool. Selected via the
/// endpoint's `web_search` config field or the `HAVEN_WEB_SEARCH` environment
/// variable (`off` | `auto` | `always`). Unconfigured defaults to `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchMode {
    /// Never expose / force the built-in search tool.
    Off,
    /// Let the model decide (`auto` / equivalent provider default).
    Auto,
    /// Force a search on every request when the protocol supports it.
    Always,
}

/// Parse a web search mode value (`off` | `auto` | `always`, case-insensitive).
/// Unset/empty/unrecognized values fall back to [`WebSearchMode::Off`].
pub fn parse_web_search_mode(value: Option<&str>) -> WebSearchMode {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "auto" => WebSearchMode::Auto,
        "always" | "required" | "on" | "1" | "true" => WebSearchMode::Always,
        _ => WebSearchMode::Off,
    }
}

fn web_search_mode_from_env() -> WebSearchMode {
    parse_web_search_mode(std::env::var("HAVEN_WEB_SEARCH").ok().as_deref())
}

/// Resolve the effective web search mode for an endpoint: the endpoint's
/// `web_search` config field wins, then `HAVEN_WEB_SEARCH`, then `off`.
///
/// Callers should only inject tools when [`supports_builtin_web_search`] is
/// true for the endpoint's style; `materialize_endpoint` already clears
/// sticky values for unsupported styles.
pub fn resolve_web_search_mode(endpoint: &ModelEndpoint) -> WebSearchMode {
    match endpoint.web_search.as_deref() {
        Some(v) if !v.trim().is_empty() => parse_web_search_mode(Some(v)),
        _ => web_search_mode_from_env(),
    }
}

/// xAI Live Search `search_parameters.mode` string for a [`WebSearchMode`].
pub fn xai_search_mode(mode: WebSearchMode) -> Option<&'static str> {
    match mode {
        WebSearchMode::Off => None,
        WebSearchMode::Auto => Some("auto"),
        WebSearchMode::Always => Some("on"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_aliases() {
        assert_eq!(
            normalize_api_style("deepseek-responses"),
            "openai-responses"
        );
        assert_eq!(normalize_api_style("OpenAI-Responses"), "openai-responses");
        assert_eq!(normalize_api_style("grok"), "xai");
        assert_eq!(normalize_api_style("xai"), "xai");
        assert_eq!(normalize_api_style("claude"), "anthropic");
        assert_eq!(normalize_api_style("google"), "gemini");
        assert_eq!(normalize_api_style("llama"), "llama.cpp");
    }

    #[test]
    fn known_style_rejects_typos() {
        assert!(is_known_api_style("anthropic"));
        assert!(is_known_api_style("deepseek-responses"));
        assert!(!is_known_api_style("antropic"));
        assert!(!is_known_api_style("openai-respones"));
    }

    #[test]
    fn web_search_capability_matrix() {
        assert!(supports_builtin_web_search("openai-responses"));
        assert!(supports_builtin_web_search("deepseek-responses"));
        assert!(supports_builtin_web_search("xai"));
        assert!(supports_builtin_web_search("grok"));
        assert!(supports_builtin_web_search("anthropic"));
        assert!(supports_builtin_web_search("gemini"));
        assert!(!supports_builtin_web_search("openai-chat"));
        assert!(!supports_builtin_web_search("llama.cpp"));
        assert!(!supports_builtin_web_search("deepgram"));
        assert!(!supports_builtin_web_search("assemblyai"));
    }

    #[test]
    fn parse_web_search_mode_maps_values() {
        assert_eq!(parse_web_search_mode(None), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("bogus")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("off")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("AUTO")), WebSearchMode::Auto);
        assert_eq!(parse_web_search_mode(Some("always")), WebSearchMode::Always);
        assert_eq!(parse_web_search_mode(Some("on")), WebSearchMode::Always);
    }

    #[test]
    fn xai_search_mode_mapping() {
        assert_eq!(xai_search_mode(WebSearchMode::Off), None);
        assert_eq!(xai_search_mode(WebSearchMode::Auto), Some("auto"));
        assert_eq!(xai_search_mode(WebSearchMode::Always), Some("on"));
    }

    #[test]
    fn resolve_prefers_endpoint_over_default() {
        let ep = ModelEndpoint {
            web_search: Some("auto".into()),
            ..Default::default()
        };
        assert_eq!(resolve_web_search_mode(&ep), WebSearchMode::Auto);
        let off = ModelEndpoint {
            web_search: None,
            ..Default::default()
        };
        assert_eq!(resolve_web_search_mode(&off), WebSearchMode::Off);
    }
}
