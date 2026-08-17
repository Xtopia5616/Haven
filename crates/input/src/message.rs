//! User message input: the unit of text + attachment input injected into a
//! conversation (supplement or steering).
//!
//! Owned by the input crate so every user input — typed messages, voice
//! transcripts, hotkeys, attachments — is represented here; the agent crate's
//! `session` module re-exports [`Supplement`] for its queue/ReAct injection.

use haven_common::types::MessageAttachment;

/// A user message queued for injection into the ReAct loop (supplement or
/// steering). `text` is the plain-text content; `attachments` hold binary
/// payloads (e.g. images) for multimodal requests.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct Supplement {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    /// True when this message is the user's reply to a pending `ask`
    /// question. The ReAct loop injects it as a paired answer ("Answer to
    /// your previous question") instead of generic additional context, so
    /// the model does not treat the old question as still open and answer
    /// stale questions again.
    #[serde(default)]
    pub is_answer: bool,
}

impl Supplement {
    pub fn new(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
            is_answer: false,
        }
    }

    pub fn answer(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
            is_answer: true,
        }
    }
}

impl From<String> for Supplement {
    fn from(text: String) -> Self {
        Supplement::new(text, vec![])
    }
}

impl From<&str> for Supplement {
    fn from(text: &str) -> Self {
        Supplement::new(text, vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supplement_new() {
        let s = Supplement::new("hello", vec![]);
        assert_eq!(s.text, "hello");
        assert!(!s.is_answer);
        assert!(s.attachments.is_empty());
    }

    #[test]
    fn test_supplement_answer() {
        let s = Supplement::answer("yes", vec![]);
        assert!(s.is_answer);
    }

    #[test]
    fn test_supplement_from_string() {
        let s: Supplement = "hi".into();
        assert_eq!(s.text, "hi");
        assert!(!s.is_answer);
    }

    #[test]
    fn test_supplement_serde_roundtrip() {
        let s = Supplement {
            text: "任务完成了吗".into(),
            attachments: vec![],
            is_answer: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Supplement = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_supplement_default() {
        let s = Supplement::default();
        assert!(s.text.is_empty());
        assert!(!s.is_answer);
    }
}
