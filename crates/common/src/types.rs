use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Entity identifiers
// ---------------------------------------------------------------------------
//
// Unified ID convention (see AGENTS.md §ID 规范):
//   - Every persisted entity id is a `{prefix}-{uuid32}` string (hyphen +
//     lowercase-hex simple UUID), e.g. `ses-3f9a...`.
//   - Prefixes: `ses-` (sessions), `msg-` (messages and memory episodes —
//     memory_episodes shares the message id space), `step-` (session_steps),
//     `fact-` (facts), `task-` (tasks — unified background tasks and
//     scheduled tasks),
//     `conf-` (safety-gateway confirmations),
//     `rec-` (voice recording sessions), `file-` (temporary files),
//     `call-` (locally synthesized tool-call ids when the provider sends an
//     empty one).
//   - External ids (LLM `tool_call_id`, provider model ids, MCP session ids)
//     keep their provider formats; `run_id`/`gen_id` are in-process u64
//     run/generation counters, not persisted entity ids.
//   - Generate ids with `new_id(prefix)` — never build them by hand.
//   - Rust/DB/event fields use snake_case `xxx_id`; the frontend maps to
//     camelCase `xxxId` at the boundary.

/// Generate an entity id in the canonical `{prefix}-{uuid32}` format.
/// Every persisted entity id must come from here (see the module docs above),
/// then be converted into its newtype with `.into()`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Defines an entity-id newtype: `pub struct $name(pub String)` with the
/// standard derives plus the conversions/accessors the rest of the codebase
/// relies on. Serializes as the plain `{prefix}-{uuid32}` string on the wire,
/// so the frontend and the DB (which store ids as text) are unaffected.
macro_rules! id_newtype {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_newtype! {
    /// Unique identifier for a safety-gateway confirmation request
    /// (`conf-{uuid32}`). Ephemeral: lives only in the `confirm:requested`
    /// event payload and the executor's pending-wait map.
    ConfirmId
}

id_newtype! {
    /// Unique identifier for a voice-recording session (`rec-{uuid32}`).
    /// Ephemeral: one id per recording, generated at `recording:started` and
    /// shared by the `transcription:result`/`transcription:error` events of the
    /// same recording (held in `AppState.recording_session` between the two).
    SessionId
}

/// MCP transport type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportType {
    #[default]
    #[serde(alias = "Stdio")]
    Stdio,
    #[serde(alias = "Http")]
    Http,
}

impl McpTransportType {
    pub fn as_str(&self) -> &'static str {
        match self {
            McpTransportType::Stdio => "stdio",
            McpTransportType::Http => "http",
        }
    }
}

/// Hotkey activation mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyMode {
    #[default]
    Toggle,
    Hold,
}

/// Risk level for a tool invocation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, PartialOrd, Hash)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    #[default]
    Safe,
    Low,
    Medium,
    High,
    Critical,
}

/// Confirmation handling strategy for high-risk operations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationMode {
    #[default]
    Always,
}

// ---------------------------------------------------------------------------
// Provider-Neutral message format (refine §1.1)
// ---------------------------------------------------------------------------

/// A single content part in a provider-neutral message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentPart {
    Text(String),
    Image {
        #[serde(rename = "type")]
        content_type: String,
        media_type: String,
        data: String,
    },
    Audio {
        #[serde(rename = "type")]
        content_type: String,
        media_type: String,
        data: String,
    },
}

impl ContentPart {
    pub fn text(t: impl Into<String>) -> Self {
        ContentPart::Text(t.into())
    }
}

impl From<String> for ContentPart {
    fn from(s: String) -> Self {
        ContentPart::Text(s)
    }
}

impl From<&str> for ContentPart {
    fn from(s: &str) -> Self {
        ContentPart::Text(s.to_string())
    }
}

/// Provider-neutral role.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalRole {
    #[default]
    System,
    User,
    Assistant,
    Tool,
}

/// Provider-neutral message used by the Agent internally.
/// Converted to provider-specific formats (e.g. `LlmMessage`) at the LLM call boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub role: CanonicalRole,
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CanonicalToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Internal reasoning/chain-of-thought from the model (e.g. DeepSeek's
    /// reasoning_content). Kept on assistant messages so it can be echoed
    /// back to APIs that require it in multi-turn requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Raw `web_search_call` output items produced by the provider's built-in
    /// web search tool. Carried on assistant messages so they are passed back
    /// verbatim in the next request's input (the server restores the search
    /// context from them). Never parsed or rewritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_calls: Vec<serde_json::Value>,
}

impl CanonicalMessage {
    pub fn system(content: Vec<ContentPart>) -> Self {
        Self {
            role: CanonicalRole::System,
            content,
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    pub fn user(content: Vec<ContentPart>) -> Self {
        Self {
            role: CanonicalRole::User,
            content,
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![ContentPart::text(text)])
    }

    pub fn assistant(
        content: Vec<ContentPart>,
        tool_calls: Option<Vec<CanonicalToolCall>>,
        reasoning: Option<String>,
        web_search_calls: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            role: CanonicalRole::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            reasoning,
            web_search_calls,
        }
    }

    pub fn tool(content: Vec<ContentPart>, tool_call_id: Option<String>) -> Self {
        Self {
            role: CanonicalRole::Tool,
            content,
            tool_calls: None,
            tool_call_id,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_role_all_variants() {
        let system = CanonicalRole::System;
        let user = CanonicalRole::User;
        let assistant = CanonicalRole::Assistant;
        let tool = CanonicalRole::Tool;
        assert_eq!(system, CanonicalRole::System);
        assert_eq!(user, CanonicalRole::User);
        assert_eq!(assistant, CanonicalRole::Assistant);
        assert_eq!(tool, CanonicalRole::Tool);
    }

    #[test]
    fn canonical_role_default_is_system() {
        assert_eq!(CanonicalRole::default(), CanonicalRole::System);
    }

    #[test]
    fn content_part_text_variant() {
        let part = ContentPart::Text("hello world".into());
        assert!(matches!(part, ContentPart::Text(_)));
    }

    #[test]
    fn content_part_image_variant() {
        let part = ContentPart::Image {
            content_type: "image_url".into(),
            media_type: "image/jpeg".into(),
            data: "base64data".into(),
        };
        match &part {
            ContentPart::Image {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "image/jpeg");
                assert_eq!(data, "base64data");
            }
            _ => panic!("expected Image variant"),
        }
    }

    #[test]
    fn content_part_text_helper() {
        let part = ContentPart::text("hello");
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "hello");
    }

    #[test]
    fn content_part_text_helper_empty() {
        let part = ContentPart::text("");
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "");
    }

    #[test]
    fn content_part_from_string() {
        let part: ContentPart = String::from("test string").into();
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "test string");
    }

    #[test]
    fn content_part_from_str() {
        let part: ContentPart = "static str".into();
        let ContentPart::Text(s) = &part else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "static str");
    }

    #[test]
    fn canonical_message_system_role() {
        let msg = CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("system prompt")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };

        assert_eq!(msg.role, CanonicalRole::System);
        assert_eq!(msg.content.len(), 1);
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn canonical_message_user_role_with_tool_call_id() {
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("use this tool")],
            tool_calls: None,
            tool_call_id: Some("call_abc".into()),
            reasoning: None,
            web_search_calls: Vec::new(),
        };
        assert!(msg.tool_call_id.is_some());
    }

    #[test]
    fn canonical_message_assistant_with_tool_calls() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![],
            tool_calls: Some(vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "run".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };

        assert_eq!(msg.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn canonical_message_tool_role() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("tool output")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };

        assert_eq!(msg.role, CanonicalRole::Tool);
    }

    #[test]
    fn canonical_tool_call_construction() {
        let call = CanonicalToolCall {
            id: "tc_123".into(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust"}),
        };
        assert_eq!(call.id, "tc_123");
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments["query"], "rust");
    }

    #[test]
    fn canonical_message_constructors_set_role_and_content() {
        assert_eq!(CanonicalMessage::user_text("u").role, CanonicalRole::User);
    }

    #[test]
    fn canonical_message_constructors_leave_optional_fields_empty() {
        let msg = CanonicalMessage::user_text("hello");
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
        assert!(msg.reasoning.is_none());
        let tool = CanonicalMessage::tool(vec![ContentPart::text("out")], Some("call_1".into()));
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn canonical_message_assistant_constructor_keeps_tool_calls_and_reasoning() {
        let msg = CanonicalMessage::assistant(
            vec![ContentPart::text("thinking")],
            Some(vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "run".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }]),
            Some("chain of thought".into()),
            Vec::new(),
        );
        assert_eq!(msg.tool_calls.unwrap().len(), 1);
        assert_eq!(msg.reasoning.as_deref(), Some("chain of thought"));
    }

    #[test]
    fn id_newtype_conversions() {
        let confirm: ConfirmId = new_id("conf").into();
        assert!(confirm.as_str().starts_with("conf-"));
        assert_eq!(confirm.to_string(), confirm.0);
        assert_eq!(AsRef::<str>::as_ref(&confirm), confirm.as_str());
        let restored: String = confirm.clone().into();
        assert_eq!(restored, confirm.0);
        let from_str: SessionId = "rec-abc".into();
        assert_eq!(from_str.0, "rec-abc");
    }

    #[test]
    fn id_newtype_serde_roundtrip() {
        let id: ConfirmId = "conf-1234".into();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"conf-1234\"");
        let decoded: ConfirmId = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn risk_level_ordering() {
        let safe = RiskLevel::Safe;
        let low = RiskLevel::Low;
        let medium = RiskLevel::Medium;
        let high = RiskLevel::High;
        let critical = RiskLevel::Critical;
        let values = [&safe, &low, &medium, &high, &critical];
        for i in 0..(values.len() - 1) {
            assert!(
                std::mem::discriminant(values[i]) != std::mem::discriminant(values[i + 1]),
                "adjacent variants must differ"
            );
        }
    }

    #[test]
    fn risk_level_default_is_safe() {
        assert_eq!(RiskLevel::default(), RiskLevel::Safe);
    }

    #[test]
    fn hotkey_mode_variants() {
        let toggle = HotkeyMode::Toggle;
        let hold = HotkeyMode::Hold;
        assert_ne!(toggle, hold);
    }

    #[test]
    fn hotkey_mode_default_is_toggle() {
        assert_eq!(HotkeyMode::default(), HotkeyMode::Toggle);
    }

    #[test]
    fn serde_roundtrip_content_part_text() {
        let part = ContentPart::Text("hello".into());
        let json = serde_json::to_string(&part).unwrap();
        let decoded: ContentPart = serde_json::from_str(&json).unwrap();
        let ContentPart::Text(s) = &decoded else {
            panic!("expected Text variant");
        };
        assert_eq!(s, "hello");
    }

    #[test]
    fn serde_roundtrip_content_part_image() {
        let part = ContentPart::Image {
            content_type: "image_url".into(),
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        };
        let json = serde_json::to_string(&part).unwrap();
        let decoded: ContentPart = serde_json::from_str(&json).unwrap();
        match decoded {
            ContentPart::Image {
                ref media_type,
                ref data,
                ..
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "aGVsbG8=");
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn serde_roundtrip_canonical_message() {
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hello")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: CanonicalMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.role, CanonicalRole::User);
        assert_eq!(decoded.content.len(), 1);
    }

    #[test]
    fn serde_roundtrip_canonical_message_with_tool_calls() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![],
            tool_calls: Some(vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "exec".into(),
                arguments: serde_json::json!({"path": "/tmp"}),
            }]),
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: CanonicalMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tool_calls.unwrap()[0].name, "exec");
    }

    #[test]
    fn serde_roundtrip_canonical_role() {
        let role = CanonicalRole::Assistant;
        let json = serde_json::to_string(&role).unwrap();
        assert!(json.contains("assistant"));
        let decoded: CanonicalRole = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, CanonicalRole::Assistant);
    }

    #[test]
    fn serde_roundtrip_risk_level() {
        let risk = RiskLevel::High;
        let json = serde_json::to_string(&risk).unwrap();
        assert!(json.contains("high"));
        let decoded: RiskLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, RiskLevel::High);
    }
}
