use std::collections::HashMap;

use haven_common::types::CanonicalMessage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Branch point saved before tool execution, used for rollback (§2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchPoint {
    pub canonical: Vec<CanonicalMessage>,
    pub history: Vec<ReActStep>,
    pub step_number: u32,
}

/// Serializable snapshot of the ReAct loop state for pause/resume (§1.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActSnapshot {
    pub canonical: Vec<CanonicalMessage>,
    pub history: Vec<ReActStep>,
    pub step_number: u32,
    /// Branch points keyed by step number for tree-structured rollback (§2).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub branch_points: HashMap<u32, BranchPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActStep {
    pub step_number: u32,
    pub thought: Option<String>,
    pub action: Option<Action>,
    pub observation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub tool_name: String,
    pub tool_input: Value,
    pub is_final: bool,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ProcessResult {
    TaskCreated(String),
    Supplemented,
}
