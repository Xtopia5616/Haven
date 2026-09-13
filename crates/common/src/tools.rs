//! Canonical, provider-agnostic tool definition.
//!
//! This is the single structured tool abstraction shared by the whole
//! workspace: the registry (`haven-tools`) and the session schema builder
//! (`haven-agent`) produce [`ToolDef`]s, the LLM boundary (`haven-llm`)
//! converts them into its provider-facing [`ToolDefinition`], and the UI
//! boundary consumes the [`ToolDef::json`] wire shape. Nothing hand-assembles
//! or re-parses loose `{name, description, risk_level, input_schema}` JSON.

use crate::types::RiskLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON key / value Haven tools emit when a background action is still running
/// and the agent should end the turn to await auto-wake. Kept in common so
/// producers (`haven-tools`) and the ReAct response policy (`haven-agent`)
/// cannot drift.
pub const BACKGROUND_WAIT_NEXT_STEP_KEY: &str = "next_step";
pub const BACKGROUND_WAIT_NEXT_STEP: &str = "end_turn";

/// Static retry metadata exposed beside a tool definition. Grouped tools may
/// still refine this policy per operation at execution time; `unknown` is the
/// honest value when the default input does not identify one operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRetrySafety {
    SafeToRetry,
    UnsafeToRetry,
    #[default]
    Unknown,
}

/// Stable high-level catalog grouping shared by the Agent prompt and the UI.
///
/// This is presentation metadata only: it never changes the model-facing tool
/// name, authorization key, or execution boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCatalogGroup {
    Haven,
    System,
    Agent,
    Skills,
    Mcp,
    #[default]
    Other,
}

impl ToolCatalogGroup {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Haven => "haven",
            Self::System => "system",
            Self::Agent => "agent",
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Other => "other",
        }
    }
}

/// Prompt-only orientation for a tool definition.
///
/// This is deliberately not part of [`ToolDef::json`] or provider tool
/// parameters. It helps the system-prompt catalog explain a capability
/// without duplicating that prose in every provider-facing description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPrompt {
    pub when_to_use: String,
    pub when_not_to_use: String,
    pub key_operations: Vec<String>,
}

/// Start a background-wait observation object with `next_step` first, then
/// `hint`. Callers insert the rest (`background` / `action_id` / `actions`…).
/// Centralized so producers cannot forget the wait marker the ReAct policy
/// keys on.
pub fn background_wait_object(hint: impl Into<String>) -> serde_json::Map<String, Value> {
    let mut body = serde_json::Map::new();
    body.insert(
        BACKGROUND_WAIT_NEXT_STEP_KEY.into(),
        Value::String(BACKGROUND_WAIT_NEXT_STEP.into()),
    );
    body.insert("hint".into(), Value::String(hint.into()));
    body
}

/// Structured definition of a tool, independent of both the execution runtime
/// (builtin / MCP / skill) and the LLM provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool's arguments (the `input_schema` wire
    /// key). Providers map this onto their own parameter schema.
    pub input_schema: Value,
    /// Default risk level, computed without concrete input. The runtime
    /// safety gateway may refine per-call risk via the tool's `risk_level`.
    pub risk_level: RiskLevel,
    /// Whether replaying the default/concrete operation is safe after a
    /// transient failure. The execution result carries the per-call value.
    #[serde(default)]
    pub retry_safety: ToolRetrySafety,
    /// Stable high-level grouping for the Agent prompt and UI catalog. This
    /// is intentionally omitted from provider-facing tool JSON.
    #[serde(skip)]
    pub catalog_group: ToolCatalogGroup,
    /// Short catalog guidance used by the Agent system prompt. This is not
    /// serialized into the UI/provider wire shape; the schema and description
    /// remain the executable model-facing contract.
    #[serde(skip)]
    pub prompt: Option<ToolPrompt>,
}

impl ToolDef {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        risk_level: RiskLevel,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            risk_level,
            retry_safety: ToolRetrySafety::Unknown,
            catalog_group: ToolCatalogGroup::Other,
            prompt: None,
        }
    }

    pub fn with_retry_safety(mut self, retry_safety: ToolRetrySafety) -> Self {
        self.retry_safety = retry_safety;
        self
    }

    pub fn with_prompt(mut self, prompt: ToolPrompt) -> Self {
        self.prompt = Some(prompt);
        self
    }

    pub fn with_catalog_group(mut self, catalog_group: ToolCatalogGroup) -> Self {
        self.catalog_group = catalog_group;
        self
    }

    /// Wire shape shared by the session schema listing and the UI tool list:
    /// `{name, description, risk_level, retry_safety, input_schema}`. Callers that need
    /// extra keys (e.g. `enabled`) merge them on top of the returned object.
    pub fn json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "risk_level": self.risk_level,
            "retry_safety": self.retry_safety,
            "input_schema": self.input_schema,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_def_json_shape() {
        let def = ToolDef::new(
            "files",
            "Read and write files",
            Value::Object(Default::default()),
            RiskLevel::Low,
        );
        let json = def.json();
        assert_eq!(json["name"], "files");
        assert_eq!(json["description"], "Read and write files");
        assert_eq!(json["risk_level"], "low");
        assert_eq!(json["retry_safety"], "unknown");
        assert!(json["input_schema"].is_object());
        assert_eq!(def.catalog_group, ToolCatalogGroup::Other);
        assert!(json.get("catalog_group").is_none());
        assert!(json.get("prompt").is_none());
    }

    #[test]
    fn tool_prompt_is_available_to_prompt_builders_but_not_wire_json() {
        let def = ToolDef::new(
            "files.read",
            "Read a file",
            serde_json::json!({"type": "object"}),
            RiskLevel::Low,
        )
        .with_prompt(ToolPrompt {
            when_to_use: "Read source text".into(),
            when_not_to_use: "Do not use for edits".into(),
            key_operations: vec!["files.read".into()],
        });

        assert_eq!(def.prompt.as_ref().unwrap().key_operations, ["files.read"]);
        assert!(def.json().get("prompt").is_none());
    }

    #[test]
    fn tool_def_new_and_fields() {
        let def = ToolDef::new("shell", "Run commands", Value::Null, RiskLevel::High);
        assert_eq!(def.name, "shell");
        assert_eq!(def.description, "Run commands");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert!(def.input_schema.is_null());
        assert!(def.prompt.is_none());
    }
}
