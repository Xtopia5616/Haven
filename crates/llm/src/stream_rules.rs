use regex::Regex;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A rule that is evaluated against every chunk of the LLM response stream.
///
/// When the pattern matches the accumulated output text, the rule can either
/// abort the current stream and inject guidance before retrying, or send a
/// warning event to the UI.
#[derive(Debug, Clone)]
pub struct StreamRule {
    /// Name for identification in logs and UI.
    pub name: String,
    /// Compiled regex pattern.
    pub pattern: Regex,
    /// Raw pattern string for (de)serialization.
    pub pattern_str: String,
    /// Text to inject as a system message when the rule triggers in abort mode.
    pub inject: String,
    /// Whether to abort the stream or just warn.
    pub mode: StreamRuleMode,
}

impl Serialize for StreamRule {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("StreamRule", 4)?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("pattern_str", &self.pattern_str)?;
        st.serialize_field("inject", &self.inject)?;
        st.serialize_field("mode", &self.mode)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for StreamRule {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            name: String,
            pattern_str: String,
            inject: String,
            mode: StreamRuleMode,
        }
        let h = Helper::deserialize(d)?;
        let pattern = Regex::new(&h.pattern_str).map_err(|e| {
            de::Error::custom(format!(
                "invalid stream rule regex '{}': {}",
                h.pattern_str, e
            ))
        })?;
        Ok(StreamRule {
            name: h.name,
            pattern,
            pattern_str: h.pattern_str,
            inject: h.inject,
            mode: h.mode,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamRuleMode {
    /// Abort the current stream, inject the rule as a system message, and retry.
    Abort,
    /// Emit a warning event to the UI but allow the stream to continue.
    Warn,
}

/// A pending match result from checking accumulated output against all stream rules.
#[derive(Debug, Clone)]
pub struct StreamRuleMatch {
    pub rule_name: String,
    pub matched_text: String,
    pub inject: String,
    pub mode: StreamRuleMode,
}

/// Trailing byte window used when evaluating stream rules mid-stream so each
/// chunk does not re-scan the entire accumulated reply (O(n²)). Fence openers
/// and similar abort patterns are short; 256 bytes of UTF-8-safe suffix is
/// enough for the default `code_block_abort` rule and typical custom rules.
pub const STREAM_RULE_WINDOW: usize = 256;

/// UTF-8-safe trailing slice of at most `max_bytes`.
pub fn trailing_window(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
    &s[start..]
}

/// Evaluates stream rules against accumulated text chunks.
/// Returns the first matching rule (if any).
pub fn check_stream_rules(rules: &[StreamRule], accumulated_text: &str) -> Option<StreamRuleMatch> {
    if rules.is_empty() || accumulated_text.is_empty() {
        return None;
    }
    let window = trailing_window(accumulated_text, STREAM_RULE_WINDOW);
    for rule in rules {
        if let Some(mat) = rule.pattern.find(window) {
            return Some(StreamRuleMatch {
                rule_name: rule.name.clone(),
                matched_text: mat.as_str().to_string(),
                inject: rule.inject.clone(),
                mode: rule.mode.clone(),
            });
        }
    }
    None
}

impl StreamRule {
    /// Create a new stream rule. The pattern string is compiled on construction.
    pub fn new(
        name: &str,
        pattern_str: &str,
        inject: &str,
        mode: StreamRuleMode,
    ) -> Result<Self, regex::Error> {
        let pattern = Regex::new(pattern_str)?;
        Ok(Self {
            name: name.into(),
            pattern,
            pattern_str: pattern_str.into(),
            inject: inject.into(),
            mode,
        })
    }

    /// Convenience: abort when the model opens a fenced **programming** code
    /// block (common languages). Plain ``` / ```text / ```markdown fences are
    /// allowed so explanatory quotes are not killed. Production routers only
    /// evaluate this while tools are attached (see `aggregate_stream_cancellable`).
    pub fn code_block_abort() -> Result<Self, regex::Error> {
        Self::new(
            "no_code_blocks",
            r"(?s)```(?:python|py|rust|rs|javascript|js|typescript|ts|tsx|jsx|bash|sh|zsh|powershell|ps1|json|toml|yaml|yml|sql|go|java|c|cpp|csharp|cs|ruby|rb|php|swift|kotlin|lua|r|html|css|xml|dockerfile|makefile)\r?\n",
            "IMPORTANT: Do NOT output code blocks. Use available tools instead of writing code.",
            StreamRuleMode::Abort,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_block_rule_matches() {
        let rule = StreamRule::code_block_abort().unwrap();
        let text = "Let me write some code:\n```python\nprint('hello')\n```";
        let matched = check_stream_rules(&[rule], text);
        assert!(matched.is_some());
        assert_eq!(matched.as_ref().unwrap().mode, StreamRuleMode::Abort);
    }

    #[test]
    fn code_block_rule_allows_plain_and_text_fences() {
        let rule = StreamRule::code_block_abort().unwrap();
        assert!(check_stream_rules(std::slice::from_ref(&rule), "quote:\n```\nok\n```").is_none());
        assert!(
            check_stream_rules(&[rule], "note:\n```text\nplain\n```").is_none(),
            "```text should not abort"
        );
    }

    #[test]
    fn trailing_window_is_utf8_safe() {
        let s = "你好世界abcdef";
        let w = trailing_window(s, 8);
        assert!(w.is_char_boundary(0));
        assert!(s.ends_with(w));
    }

    #[test]
    fn no_match_when_rule_not_triggered() {
        let rule = StreamRule::new(
            "test",
            r"forbidden_pattern",
            "Don't do that",
            StreamRuleMode::Warn,
        )
        .unwrap();
        let text = "This is a safe response.";
        let matched = check_stream_rules(&[rule], text);
        assert!(matched.is_none());
    }

    #[test]
    fn warn_mode_returns_correct_mode() {
        let rule = StreamRule::new(
            "warn_test",
            r"warning_trigger",
            "Be careful",
            StreamRuleMode::Warn,
        )
        .unwrap();
        let text = "This contains warning_trigger in it.";
        let matched = check_stream_rules(&[rule], text);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().mode, StreamRuleMode::Warn);
    }

    #[test]
    fn first_rule_wins() {
        let r1 = StreamRule::new("first", r"apple", "Eat apple", StreamRuleMode::Abort).unwrap();
        let r2 = StreamRule::new("second", r"banana", "Eat banana", StreamRuleMode::Warn).unwrap();
        let text = "I like apple and banana.";
        let matched = check_stream_rules(&[r1, r2], text);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().rule_name, "first");
    }
}
