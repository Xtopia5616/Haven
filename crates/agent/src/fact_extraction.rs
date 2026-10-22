//! Shared fact-extraction wire parsing and sanitization.
//!
//! The memory worker owns scheduling, prompts and persistence orchestration.
//! This module owns only the model-facing fact shape, strict field parsing and
//! sanitization before values enter prompts or storage. Canonical predicate
//! naming is owned by Memory because every fact write path shares that policy.

use serde::Deserialize;

/// A fact extracted by the LLM, deserialized from the model's JSON response.
#[derive(Clone, Deserialize)]
pub(crate) struct LlmFact {
    #[serde(default = "default_subject")]
    pub(crate) subject: String,
    pub(crate) predicate: String,
    pub(crate) object: String,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default = "default_confidence")]
    pub(crate) confidence: f64,
    /// 0..1 rating of how long this fact stays useful. Missing/unsure falls
    /// back to 0.6 (moderately durable) so an omitted field does not make a
    /// fact immortal by defaulting to 1.0.
    pub(crate) durability: Option<f64>,
    /// Explicit real-world start of this fact, never inferred from message time.
    #[serde(default)]
    pub(crate) valid_from: Option<String>,
    /// Exclusive real-world end of this fact, if stated.
    #[serde(default)]
    pub(crate) valid_to: Option<String>,
    /// Index into the numbered conversation transcript of the message that
    /// supports this fact (the model is asked to fill this in).
    pub(crate) message_index: Option<usize>,
}

fn default_subject() -> String {
    "user".into()
}

/// Parse a JSON fact batch while isolating malformed entries. Required text
/// fields and tag elements must have their declared string types; one invalid
/// model item is skipped without discarding valid facts from the same batch.
pub(crate) fn parse_llm_fact_batch(json: &str) -> Result<(Vec<LlmFact>, usize), serde_json::Error> {
    let values: Vec<serde_json::Value> = serde_json::from_str(json)?;
    let mut facts = Vec::with_capacity(values.len());
    let mut rejected = 0;
    for value in values {
        match serde_json::from_value(value) {
            Ok(fact) => facts.push(fact),
            Err(_) => rejected += 1,
        }
    }
    Ok((facts, rejected))
}

fn default_confidence() -> f64 {
    0.7
}

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
