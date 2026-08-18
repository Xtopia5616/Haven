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
        }
    }

    /// Wire shape shared by the session schema listing and the UI tool list:
    /// `{name, description, risk_level, input_schema}`. Callers that need
    /// extra keys (e.g. `enabled`) merge them on top of the returned object.
    pub fn json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "risk_level": self.risk_level,
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
        assert!(json["input_schema"].is_object());
    }

    #[test]
    fn tool_def_new_and_fields() {
        let def = ToolDef::new("shell", "Run commands", Value::Null, RiskLevel::High);
        assert_eq!(def.name, "shell");
        assert_eq!(def.description, "Run commands");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert!(def.input_schema.is_null());
    }
}
