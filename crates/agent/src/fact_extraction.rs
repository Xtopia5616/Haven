//! Shared fact-extraction wire parsing and sanitization.
//!
//! The inference engine owns scheduling, prompts and persistence orchestration.
//! This module owns only the model-facing fact shape and the normalization
//! rules that protect extracted values before they enter prompts or storage.

use serde::Deserialize;

use haven_memory::repositories::facts::FactSourceRef;

/// A fact extracted by the LLM, deserialized from the model's JSON response.
#[derive(Clone, Deserialize)]
pub(crate) struct LlmFact {
    #[serde(default = "default_subject", deserialize_with = "coerce_to_string")]
    pub(crate) subject: String,
    #[serde(deserialize_with = "coerce_to_string")]
    pub(crate) predicate: String,
    #[serde(deserialize_with = "coerce_to_string")]
    pub(crate) object: String,
    #[serde(default, deserialize_with = "coerce_string_array")]
    pub(crate) tags: Vec<String>,
    #[serde(default = "default_confidence")]
    pub(crate) confidence: f64,
    /// 0..1 rating of how long this fact stays useful. Missing/unsure falls
    /// back to 0.6 (moderately durable) so an omitted field does not make a
    /// fact immortal by defaulting to 1.0.
    pub(crate) durability: Option<f64>,
    /// Index into the numbered conversation transcript of the message that
    /// supports this fact (the model is asked to fill this in).
    pub(crate) message_index: Option<usize>,
}

fn default_subject() -> String {
    "user".into()
}

/// Deserialize any JSON value into a string. The extraction model sometimes
/// emits booleans or numbers for fact fields (e.g. `"object": true`), which
/// would otherwise hard-fail the whole batch; coerce them to their string
/// form instead of dropping the fact.
fn coerce_value_to_string(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn coerce_to_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(coerce_value_to_string(value))
}

/// Deserialize an array of arbitrary JSON values into strings, coercing each
/// element the same way `coerce_to_string` does.
fn coerce_string_array<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(values.into_iter().map(coerce_value_to_string).collect())
}

fn default_confidence() -> f64 {
    0.7
}

/// One extracted fact ready for the shared persistence path:
/// (subject, predicate, object, confidence, tags, source reference, durability).
pub(crate) type FactDraft = (
    String,
    String,
    String,
    f64,
    Vec<String>,
    Option<FactSourceRef>,
    f64,
);

/// Fact tags allowed to enter long-term memory. The extraction prompt asks
/// the model to stick to these, but it may still emit arbitrary values; this
/// whitelist keeps the prompt-side grouping (`tags.first()`) clean and stops
/// tag drift from polluting the facts index.
const ALLOWED_FACT_TAGS: &[&str] = &["identity", "preference", "workspace", "project"];

/// Keep only tags from the allowed set, normalized to lowercase, capped in
/// number and length so a stray model output cannot inflate the tag column.
pub(crate) fn sanitize_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|tag| tag.trim().to_ascii_lowercase())
        .filter(|tag| ALLOWED_FACT_TAGS.contains(&tag.as_str()))
        .take(4)
        .collect()
}

/// Normalize a predicate to its canonical form (trim + lowercase + alias
/// mapping). Delegates to the memory layer so the inference path and the
/// repository write paths share ONE normalization policy.
pub(crate) fn normalize_predicate(predicate: &str) -> String {
    haven_memory::repositories::facts::normalize_predicate(predicate)
}

/// Sanitize a fact field before it is stored and later interpolated into the
/// agent's system prompt. Strips newlines and control characters that could be
/// used for indirect prompt injection, and caps the length.
pub(crate) fn sanitize_fact_field(value: &str, max_chars: usize) -> String {
    haven_common::text::sanitize_prompt_field(value, max_chars)
}

/// Extract the first JSON array `[...]` from a string that may contain
/// markdown code fences or surrounding text.
pub(crate) fn extract_json_array(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('[')
        && let Some(end) = trimmed.rfind(']')
        && end > start
    {
        return trimmed[start..=end].to_string();
    }
    trimmed.to_string()
}
