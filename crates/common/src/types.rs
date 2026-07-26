use serde::{Deserialize, Serialize};

/// Unique identifier for a task.
pub type TaskId = String;

/// Unique identifier for a recording/confirmation request.
pub type ConfirmId = String;

/// Unique identifier for a session.
pub type SessionId = String;

/// MCP transport type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportType {
    #[default]
    Stdio,
}

/// Hotkey activation mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyMode {
    #[default]
    Toggle,
    Hold,
}

/// Task priority assigned by the classifier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
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
    /// Parent message ID for tree-structured conversation history (§2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,
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
                media_type,
                data,
                ..
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
            parent_message_id: None,
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
            parent_message_id: None,
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
            parent_message_id: None,
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
            parent_message_id: None,
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
    fn task_priority_ordering() {
        let low = TaskPriority::Low;
        let normal = TaskPriority::Normal;
        let high = TaskPriority::High;
        let critical = TaskPriority::Critical;
        let values = [&low, &normal, &high, &critical];
        for i in 0..(values.len() - 1) {
            assert!(
                std::mem::discriminant(values[i]) != std::mem::discriminant(values[i + 1]),
                "adjacent variants must differ"
            );
        }
    }

    #[test]
    fn task_priority_default_is_normal() {
        assert_eq!(TaskPriority::default(), TaskPriority::Normal);
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
            parent_message_id: None,
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
            parent_message_id: None,
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

    #[test]
    fn serde_roundtrip_task_priority() {
        let prio = TaskPriority::Critical;
        let json = serde_json::to_string(&prio).unwrap();
        assert!(json.contains("critical"));
        let decoded: TaskPriority = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, TaskPriority::Critical);
    }
}
