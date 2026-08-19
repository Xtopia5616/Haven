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
    /// `created_at` of the most recent session message at the time this branch
    /// point was saved. On rollback, all session messages after this timestamp
    /// are deleted so the conversation context matches the restored snapshot.
    #[serde(default)]
    pub last_msg_at: Option<String>,
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
    /// Wall-clock time the snapshot was written. Resume uses it to recover
    /// messages submitted AFTER the snapshot (supplements/steering/answers
    /// persisted to the DB while paused or after a crash) by TIMESTAMP —
    /// anything newer than this cannot be in the canonical, so no content
    /// comparison is needed. Missing on legacy snapshots: the canonical is
    /// trusted as complete and nothing is re-seeded.
    #[serde(default)]
    pub saved_at: Option<String>,
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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProcessResult {
    SessionCreated(String),
    Supplemented,
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::{CanonicalRole, ContentPart};

    fn canonical_msg(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }
    }

    #[test]
    fn action_serde_roundtrip() {
        let action = Action {
            tool_name: "file".into(),
            tool_input: serde_json::json!({"path": "C:/tmp/a.txt"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: Action = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_name, "file");
        assert_eq!(back.tool_input, serde_json::json!({"path": "C:/tmp/a.txt"}));
        assert!(!back.is_final);
        assert_eq!(back.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn action_missing_tool_call_id_defaults_to_none() {
        let json = r#"{"tool_name":"shell","tool_input":{"cmd":"dir"},"is_final":true}"#;
        let action: Action = serde_json::from_str(json).unwrap();
        assert!(action.is_final);
        assert_eq!(action.tool_call_id, None);
    }

    #[test]
    fn react_step_serde_roundtrip_full() {
        let step = ReActStep {
            step_number: 3,
            thought: Some("need to read the file".into()),
            action: Some(Action {
                tool_name: "file".into(),
                tool_input: serde_json::json!({}),
                is_final: false,
                tool_call_id: None,
            }),
            observation: Some("file not found".into()),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: ReActStep = serde_json::from_str(&json).unwrap();
        assert_eq!(back.step_number, 3);
        assert_eq!(back.thought.as_deref(), Some("need to read the file"));
        assert_eq!(back.observation.as_deref(), Some("file not found"));
        let action = back.action.unwrap();
        assert_eq!(action.tool_name, "file");
    }

    #[test]
    fn react_step_serde_roundtrip_sparse() {
        let step = ReActStep {
            step_number: 1,
            thought: None,
            action: None,
            observation: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: ReActStep = serde_json::from_str(&json).unwrap();
        assert_eq!(back.step_number, 1);
        assert!(back.thought.is_none());
        assert!(back.action.is_none());
        assert!(back.observation.is_none());
    }

    #[test]
    fn branch_point_missing_last_msg_at_defaults_to_none() {
        let json = r#"{
            "canonical": [{"role":"user","content":["hi"]}],
            "history": [],
            "step_number": 2
        }"#;
        let bp: BranchPoint = serde_json::from_str(json).unwrap();
        assert_eq!(bp.step_number, 2);
        assert_eq!(bp.last_msg_at, None);
    }

    #[test]
    fn branch_point_roundtrip_with_last_msg_at() {
        let bp = BranchPoint {
            canonical: vec![canonical_msg(CanonicalRole::User, "hello")],
            history: vec![ReActStep {
                step_number: 1,
                thought: None,
                action: None,
                observation: None,
            }],
            step_number: 5,
            last_msg_at: Some("2026-07-31T12:00:00Z".into()),
        };
        let json = serde_json::to_string(&bp).unwrap();
        let back: BranchPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.canonical.len(), 1);
        assert_eq!(back.history.len(), 1);
        assert_eq!(back.step_number, 5);
        assert_eq!(back.last_msg_at.as_deref(), Some("2026-07-31T12:00:00Z"));
    }

    #[test]
    fn snapshot_roundtrip_with_branch_points() {
        let mut snapshot = ReActSnapshot {
            canonical: vec![canonical_msg(CanonicalRole::System, "sys")],
            history: vec![],
            step_number: 7,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        snapshot.branch_points.insert(
            4,
            BranchPoint {
                canonical: vec![],
                history: vec![],
                step_number: 4,
                last_msg_at: None,
            },
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("branch_points"));
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.step_number, 7);
        assert_eq!(back.branch_points.len(), 1);
        assert_eq!(back.branch_points.get(&4).unwrap().step_number, 4);
    }

    #[test]
    fn snapshot_empty_branch_points_skipped_in_json() {
        let snapshot = ReActSnapshot {
            canonical: vec![],
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("branch_points"));
        // Deserializing without the field works thanks to #[serde(default)].
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        assert!(back.branch_points.is_empty());
    }

    #[test]
    fn process_result_variants_roundtrip() {
        for result in [
            ProcessResult::SessionCreated("ses-1".into()),
            ProcessResult::Supplemented,
        ] {
            let json = serde_json::to_string(&result).unwrap();
            let back: ProcessResult = serde_json::from_str(&json).unwrap();
            assert_eq!(back, result);
        }
    }
}
