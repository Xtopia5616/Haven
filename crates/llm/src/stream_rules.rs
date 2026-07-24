use regex::Regex;
use serde::{Deserialize, Serialize};

/// A rule that is evaluated against every chunk of the LLM response stream.
///
/// When the pattern matches the accumulated output text, the rule can either
/// abort the current stream and inject guidance before retrying, or send a
/// warning event to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRule {
    /// Name for identification in logs and UI.
    pub name: String,
    /// Regex pattern to match against the accumulated output text.
    #[serde(skip, default = "default_regex")]
    pub pattern: Regex,
    /// Raw pattern string for serialization.
    pub pattern_str: String,
    /// Text to inject as a system reminder when the rule triggers in abort mode.
    pub inject: String,
    /// Whether to abort the stream or just warn.
    pub mode: StreamRuleMode,
}

fn default_regex() -> Regex {
    Regex::new(r"^$").unwrap()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamRuleMode {
    /// Abort the current stream, inject the rule as a system reminder, and retry.
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

/// Evaluates stream rules against accumulated text chunks.
/// Returns the first matching rule (if any).
pub fn check_stream_rules(
    rules: &[StreamRule],
    accumulated_text: &str,
) -> Option<StreamRuleMatch> {
    for rule in rules {
        if let Some(mat) = rule.pattern.find(accumulated_text) {
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
    pub fn new(name: &str, pattern_str: &str, inject: &str, mode: StreamRuleMode) -> Result<Self, regex::Error> {
        let pattern = Regex::new(pattern_str)?;
        Ok(Self {
            name: name.into(),
            pattern,
            pattern_str: pattern_str.into(),
            inject: inject.into(),
            mode,
        })
    }

    /// Convenience: build a rule that aborts when the model starts writing code
    /// when it should only use tools.
    pub fn code_block_abort() -> Result<Self, regex::Error> {
        Self::new(
            "no_code_blocks",
            r"(?s)```\w*\n",
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
    fn no_match_when_rule_not_triggered() {
        let rule = StreamRule::new("test", r"forbidden_pattern", "Don't do that", StreamRuleMode::Warn).unwrap();
        let text = "This is a safe response.";
        let matched = check_stream_rules(&[rule], text);
        assert!(matched.is_none());
    }

    #[test]
    fn warn_mode_returns_correct_mode() {
        let rule = StreamRule::new("warn_test", r"warning_trigger", "Be careful", StreamRuleMode::Warn).unwrap();
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
