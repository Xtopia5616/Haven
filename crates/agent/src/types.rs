use std::collections::HashMap;

use haven_common::types::{
    CanonicalMessage, CanonicalToolCall, ContentPart, InjectSource, MessageAttachment,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool invocation within a [`ReActRound`] (parallel tools are siblings).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRecord {
    pub action: Action,
    pub observation: Option<String>,
}

/// One LLM step; parallel tools share `step_number` as siblings in `tools`
/// (no fake step inflation — Phase 8 / B2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReActRound {
    pub step_number: u32,
    pub thought: Option<String>,
    pub tools: Vec<ToolRecord>,
}

/// Append-only transcript record — sole snapshot authority (Phase 8 / B1-3).
/// Projected to canonical + [`ReActRound`] via [`project_transcript`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptRecord {
    Thought {
        step_number: u32,
        text: String,
        message_id: String,
    },
    ToolCall {
        step_number: u32,
        text: String,
        tool_calls: Vec<CanonicalToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        web_search_calls: Vec<Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        thinking_blocks: Vec<Value>,
    },
    ToolResult {
        step_number: u32,
        canonical_observation: String,
        history_observation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        action: Action,
    },
    UserInject {
        step_number: u32,
        source: InjectSource,
        /// Raw text — adapters prepend wire prefixes (Phase 8 / B3).
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    CompactSummary {
        compacted: Vec<CanonicalMessage>,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
        episode_id: String,
    },
}

/// Branch point saved before tool execution (§2 / Phase 8 F4).
/// Stores only an index into the parent snapshot's `events` — no Arc Vec copies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchPoint {
    /// Index into parent `events` — restored state is `events[..event_cursor]`.
    pub event_cursor: usize,
    pub step_number: u32,
    /// `created_at` of the most recent session message at save time. On
    /// rollback, messages after this timestamp are deleted.
    #[serde(default)]
    pub last_msg_at: Option<String>,
    /// Phase-7 upgrade only: per-BP canonical seed so step rollback can
    /// replace events with content instead of a useless length-1 cursor.
    /// New branch points leave this `None` (skipped in serde).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_canonical: Option<Vec<CanonicalMessage>>,
}

/// Pending `ask` tool state persisted in the snapshot (Phase 4 / C5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AskPending {
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_ids: Vec<String>,
}

/// One gated tool awaiting (or holding) a confirm decision (Phase 5 / E3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfirmPendingTool {
    pub confirm_id: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub step_id: String,
    pub risk_level: haven_common::types::RiskLevel,
    /// `None` = still waiting; `Some(true/false)` = user decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<bool>,
}

/// Pending safety-confirm batch persisted in the snapshot (Phase 5 / E3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConfirmPending {
    pub step_number: u32,
    #[serde(default)]
    pub tools: Vec<ConfirmPendingTool>,
}

impl ConfirmPending {
    pub fn all_decided(&self) -> bool {
        !self.tools.is_empty() && self.tools.iter().all(|t| t.decision.is_some())
    }

    pub fn any_approved(&self) -> bool {
        self.tools.iter().any(|t| t.decision == Some(true))
    }
}

/// Per-run step budget recorded on the snapshot for observability (R4 / J1).
///
/// Storage is diagnostic only — the live loop still reads `max_steps` /
/// `session_max_steps` from the engine. Resume grants another full per-run
/// budget; `session_max_steps` (when set) caps absolute `step_number`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunBudget {
    /// First step number this run will execute (`start_step`).
    pub start_step: u32,
    /// Inclusive last step this run may reach.
    pub effective_max: u32,
    /// Configured per-run `max_steps` at run start.
    pub max_steps: u32,
    /// Optional session-lifetime absolute step cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_max_steps: Option<u32>,
}

/// Serializable snapshot of the ReAct loop state for pause/resume.
///
/// **Authority (Phase 8 / B1-3):** [`Self::events`] is the sole transcript.
/// Canonical and [`ReActRound`]s are derived via [`project_transcript`] /
/// [`Self::project`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReActSnapshot {
    pub events: Vec<TranscriptRecord>,
    pub step_number: u32,
    /// Branch points keyed by step number for tree-structured rollback (§2).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub branch_points: HashMap<u32, BranchPoint>,
    /// Wall-clock time the snapshot was written. Resume recovers messages
    /// submitted AFTER this by timestamp.
    #[serde(default)]
    pub saved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_answer: Option<AskPending>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_confirm: Option<ConfirmPending>,
    /// Last run's effective step budget (R4). Omitted on legacy snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_budget: Option<RunBudget>,
    /// Filled only by legacy `from_json` for one-shot tool restore on resume.
    /// Never serialized.
    #[serde(skip)]
    pub upgrade_tool_rounds: Vec<ReActRound>,
}

impl ReActSnapshot {
    /// Parse a snapshot JSON, accepting the current events-authority shape or
    /// the legacy Phase-7 `canonical`/`history` wire format.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        if let Ok(s) = serde_json::from_str::<ReActSnapshot>(json) {
            return Ok(s);
        }
        // Legacy Phase-7 shape: canonical + history + BranchPoint with Arc-ish arrays
        #[derive(Deserialize)]
        struct LegacyBp {
            #[serde(default)]
            canonical: Option<Vec<CanonicalMessage>>,
            step_number: u32,
            #[serde(default)]
            last_msg_at: Option<String>,
        }
        #[derive(Deserialize)]
        struct LegacySnap {
            canonical: Vec<CanonicalMessage>,
            #[serde(default)]
            history: serde_json::Value,
            step_number: u32,
            #[serde(default)]
            branch_points: HashMap<u32, LegacyBp>,
            #[serde(default)]
            saved_at: Option<String>,
            #[serde(default)]
            awaiting_answer: Option<AskPending>,
            #[serde(default)]
            awaiting_confirm: Option<ConfirmPending>,
        }
        let legacy: LegacySnap = serde_json::from_str(json).map_err(|e| {
            anyhow::anyhow!("corrupt or incompatible react_state: {e}")
        })?;
        tracing::warn!("react_state used legacy Phase-7 snapshot shape; upgrading on next save");
        let events = seed_events_from_canonical(legacy.canonical);
        let event_len = events.len();
        // Recover load_skill / load_mcp rounds from legacy history so resume
        // can re-register per-session tools (CompactSummary alone yields empty rounds).
        let upgrade_tool_rounds = legacy_history_to_tool_rounds(&legacy.history);
        let branch_points = legacy
            .branch_points
            .into_iter()
            .map(|(k, bp)| {
                // Keep the BP's own canonical as a restore seed. Cursor alone
                // is useless on a length-1 CompactSummary parent log.
                (
                    k,
                    BranchPoint {
                        event_cursor: event_len,
                        step_number: bp.step_number,
                        last_msg_at: bp.last_msg_at,
                        legacy_canonical: bp.canonical,
                    },
                )
            })
            .collect();
        Ok(ReActSnapshot {
            events,
            step_number: legacy.step_number,
            branch_points,
            saved_at: legacy.saved_at,
            awaiting_answer: legacy.awaiting_answer,
            awaiting_confirm: legacy.awaiting_confirm,
            run_budget: None,
            upgrade_tool_rounds,
        })
    }

    /// Project the full event log to canonical + rounds.
    pub fn project(&self) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
        project_transcript(&self.events)
    }

    /// Project `events[..cursor]` (cursor clamped to `events.len()`).
    pub fn project_at(&self, cursor: usize) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
        let end = cursor.min(self.events.len());
        project_transcript(&self.events[..end])
    }
}

/// Project an append-only event log into the LLM transcript and debug/tool
/// rounds. Pure — no I/O. Parallel `ToolResult`s with the same `step_number`
/// become siblings on one [`ReActRound`].
pub fn project_transcript(events: &[TranscriptRecord]) -> (Vec<CanonicalMessage>, Vec<ReActRound>) {
    let mut canonical: Vec<CanonicalMessage> = Vec::new();
    let mut rounds: Vec<ReActRound> = Vec::new();

    for ev in events {
        match ev {
            TranscriptRecord::Thought {
                step_number, text, ..
            } => {
                rounds.push(ReActRound {
                    step_number: *step_number,
                    thought: Some(text.clone()),
                    tools: Vec::new(),
                });
            }
            TranscriptRecord::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                ..
            } => {
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(text.clone())],
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls.clone())
                    },
                    reasoning.clone(),
                    web_search_calls.clone(),
                    thinking_blocks.clone(),
                ));
            }
            TranscriptRecord::ToolResult {
                step_number,
                canonical_observation,
                history_observation,
                tool_call_id,
                action,
            } => {
                // final_answer is rounds-only (mirrors pre-B1 history mutation;
                // the assistant text is pushed separately via finish_turn_end).
                let is_final = action.is_final || action.tool_name == "final_answer";
                if !is_final {
                    canonical.push(CanonicalMessage::tool(
                        vec![ContentPart::text(canonical_observation.clone())],
                        tool_call_id.clone(),
                    ));
                }
                if let Some(round) = rounds
                    .iter_mut()
                    .rev()
                    .find(|r| r.step_number == *step_number)
                {
                    round.tools.push(ToolRecord {
                        action: action.clone(),
                        observation: Some(history_observation.clone()),
                    });
                } else {
                    rounds.push(ReActRound {
                        step_number: *step_number,
                        thought: None,
                        tools: vec![ToolRecord {
                            action: action.clone(),
                            observation: Some(history_observation.clone()),
                        }],
                    });
                }
            }
            TranscriptRecord::UserInject {
                source,
                text,
                attachments,
                ..
            } => {
                let mut content = vec![ContentPart::text(text.clone())];
                content.extend(attachments.iter().map(attachment_to_content_part));
                canonical.push(CanonicalMessage::user_with_source(content, *source));
            }
            TranscriptRecord::CompactSummary { compacted, .. } => {
                canonical = compacted.clone();
            }
        }
    }

    (canonical, rounds)
}

fn attachment_to_content_part(att: &MessageAttachment) -> ContentPart {
    // Single helper shared with the live react path (via re-export).
    crate::react::attachment_to_content_part(att)
}

/// Recover load_skill / load_mcp rounds from a legacy Phase-7 `history` blob.
fn legacy_history_to_tool_rounds(history: &Value) -> Vec<ReActRound> {
    #[derive(Deserialize)]
    struct LegacyStep {
        step_number: u32,
        #[serde(default)]
        thought: Option<String>,
        #[serde(default)]
        action: Option<Action>,
        #[serde(default)]
        observation: Option<String>,
    }
    let Ok(steps) = serde_json::from_value::<Vec<LegacyStep>>(history.clone()) else {
        return Vec::new();
    };
    let mut rounds: Vec<ReActRound> = Vec::new();
    for step in steps {
        let Some(action) = step.action else {
            if step.thought.is_some() {
                rounds.push(ReActRound {
                    step_number: step.step_number,
                    thought: step.thought,
                    tools: Vec::new(),
                });
            }
            continue;
        };
        if action.tool_name != "load_skill" && action.tool_name != "load_mcp" {
            continue;
        }
        if let Some(last) = rounds
            .last_mut()
            .filter(|r| r.step_number == step.step_number)
        {
            last.tools.push(ToolRecord {
                action,
                observation: step.observation,
            });
        } else {
            rounds.push(ReActRound {
                step_number: step.step_number,
                thought: step.thought,
                tools: vec![ToolRecord {
                    action,
                    observation: step.observation,
                }],
            });
        }
    }
    rounds
}

/// Test/helper: wrap a pre-built canonical list as a single CompactSummary
/// seed event so snapshots can be constructed without replaying applies.
pub fn seed_events_from_canonical(canonical: Vec<CanonicalMessage>) -> Vec<TranscriptRecord> {
    vec![TranscriptRecord::CompactSummary {
        compacted: canonical,
        summary: String::new(),
        tokens_before: 0,
        tokens_after: 0,
        episode_id: haven_common::types::new_id("msg"),
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub tool_name: String,
    pub tool_input: Value,
    pub is_final: bool,
    pub tool_call_id: Option<String>,
}

/// Result of [`crate::AgentLayer::process_input`]. Carries the persisted
/// user-message id so the UI can replace its optimistic temp id with the
/// canonical `msg-*` without content/timestamp guessing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProcessResult {
    SessionCreated {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    Supplemented {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
}

impl ProcessResult {
    pub fn session_created(session_id: impl Into<String>, message_id: Option<String>) -> Self {
        Self::SessionCreated {
            session_id: session_id.into(),
            message_id,
        }
    }

    pub fn supplemented(message_id: Option<String>) -> Self {
        Self::Supplemented { message_id }
    }

    pub fn message_id(&self) -> Option<&str> {
        match self {
            Self::SessionCreated { message_id, .. } | Self::Supplemented { message_id } => {
                message_id.as_deref()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::CanonicalRole;

    fn canonical_msg(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
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
    fn project_parallel_tools_one_round() {
        let events = vec![
            TranscriptRecord::Thought {
                step_number: 1,
                text: "run both".into(),
                message_id: "step-1".into(),
            },
            TranscriptRecord::ToolCall {
                step_number: 1,
                text: "calling".into(),
                tool_calls: vec![
                    CanonicalToolCall {
                        id: "c1".into(),
                        name: "a".into(),
                        arguments: serde_json::json!({}),
                    },
                    CanonicalToolCall {
                        id: "c2".into(),
                        name: "b".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
                reasoning: None,
                web_search_calls: vec![],
                thinking_blocks: vec![],
            },
            TranscriptRecord::ToolResult {
                step_number: 1,
                canonical_observation: "ra".into(),
                history_observation: "ra".into(),
                tool_call_id: Some("c1".into()),
                action: Action {
                    tool_name: "a".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("c1".into()),
                },
            },
            TranscriptRecord::ToolResult {
                step_number: 1,
                canonical_observation: "rb".into(),
                history_observation: "rb".into(),
                tool_call_id: Some("c2".into()),
                action: Action {
                    tool_name: "b".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("c2".into()),
                },
            },
        ];
        let (canonical, rounds) = project_transcript(&events);
        assert_eq!(rounds.len(), 1, "parallel tools must share one round");
        assert_eq!(rounds[0].tools.len(), 2);
        assert_eq!(canonical.len(), 3); // assistant + 2 tool
    }

    #[test]
    fn branch_point_missing_last_msg_at_defaults_to_none() {
        let json = r#"{"event_cursor": 2, "step_number": 2}"#;
        let bp: BranchPoint = serde_json::from_str(json).unwrap();
        assert_eq!(bp.step_number, 2);
        assert_eq!(bp.event_cursor, 2);
        assert_eq!(bp.last_msg_at, None);
    }

    #[test]
    fn branch_point_roundtrip_with_last_msg_at() {
        let bp = BranchPoint {
            event_cursor: 4,
            step_number: 5,
            last_msg_at: Some("2026-07-31T12:00:00Z".into()),
            legacy_canonical: None,
        };
        let json = serde_json::to_string(&bp).unwrap();
        let back: BranchPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_cursor, 4);
        assert_eq!(back.step_number, 5);
        assert_eq!(back.last_msg_at.as_deref(), Some("2026-07-31T12:00:00Z"));
    }

    #[test]
    fn snapshot_roundtrip_with_branch_points() {
        let mut snapshot = ReActSnapshot {
            events: seed_events_from_canonical(vec![canonical_msg(CanonicalRole::System, "sys")]),
            step_number: 7,
            ..Default::default()
        };
        snapshot.branch_points.insert(
            4,
            BranchPoint {
                event_cursor: 1,
                step_number: 4,
                last_msg_at: None,
                legacy_canonical: None,
            },
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("branch_points"));
        assert!(json.contains("events"));
        assert!(!json.contains("\"canonical\""));
        assert!(!json.contains("\"history\""));
        let back = ReActSnapshot::from_json(&json).unwrap();
        assert_eq!(back.step_number, 7);
        assert_eq!(back.branch_points.len(), 1);
        assert_eq!(back.branch_points.get(&4).unwrap().event_cursor, 1);
        let (canonical, _) = back.project();
        assert_eq!(canonical.len(), 1);
    }

    #[test]
    fn snapshot_from_json_accepts_legacy_phase7_shape() {
        // ContentPart::Text is an untagged string on the wire.
        let legacy = serde_json::json!({
            "canonical": [
                {
                    "role": "user",
                    "content": ["hi"]
                }
            ],
            "history": [],
            "step_number": 3,
            "branch_points": {
                "2": {
                    "canonical": [
                        {
                            "role": "user",
                            "content": ["hi"]
                        }
                    ],
                    "step_number": 2,
                    "last_msg_at": "2026-08-01T00:00:00Z"
                }
            },
            "saved_at": "2026-08-01T00:01:00Z"
        });
        let snap = ReActSnapshot::from_json(&legacy.to_string()).unwrap();
        assert_eq!(snap.step_number, 3);
        assert_eq!(snap.events.len(), 1);
        assert_eq!(snap.branch_points.get(&2).unwrap().event_cursor, 1);
        assert_eq!(
            snap.branch_points.get(&2).unwrap().last_msg_at.as_deref(),
            Some("2026-08-01T00:00:00Z")
        );
        let (canonical, _) = snap.project();
        assert_eq!(canonical.len(), 1);
    }

    #[test]
    fn snapshot_empty_branch_points_skipped_in_json() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 1,
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("branch_points"));
        assert!(!json.contains("awaiting_answer"));
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        assert!(back.branch_points.is_empty());
        assert!(back.awaiting_answer.is_none());
    }

    #[test]
    fn snapshot_awaiting_answer_roundtrip() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 2,
            awaiting_answer: Some(AskPending {
                question: "which file?".into(),
                step_ids: vec!["step-abc".into()],
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        let pending = back.awaiting_answer.expect("flag restored");
        assert_eq!(pending.question, "which file?");
        assert_eq!(pending.step_ids, vec!["step-abc".to_string()]);
    }

    #[test]
    fn snapshot_run_budget_roundtrip() {
        let snapshot = ReActSnapshot {
            events: vec![],
            step_number: 3,
            run_budget: Some(RunBudget {
                start_step: 1,
                effective_max: 20,
                max_steps: 20,
                session_max_steps: Some(100),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("run_budget"));
        let back: ReActSnapshot = serde_json::from_str(&json).unwrap();
        let budget = back.run_budget.expect("budget restored");
        assert_eq!(budget.start_step, 1);
        assert_eq!(budget.effective_max, 20);
        assert_eq!(budget.max_steps, 20);
        assert_eq!(budget.session_max_steps, Some(100));
    }

    #[test]
    fn snapshot_project_at_truncates() {
        let events = vec![
            TranscriptRecord::UserInject {
                step_number: 1,
                source: InjectSource::FollowUp,
                text: "a".into(),
                attachments: vec![],
                message_id: None,
            },
            TranscriptRecord::UserInject {
                step_number: 1,
                source: InjectSource::FollowUp,
                text: "b".into(),
                attachments: vec![],
                message_id: None,
            },
        ];
        let snapshot = ReActSnapshot {
            events,
            step_number: 1,
            ..Default::default()
        };
        let (full, _) = snapshot.project();
        assert_eq!(full.len(), 2);
        let (at1, _) = snapshot.project_at(1);
        assert_eq!(at1.len(), 1);
    }

    #[test]
    fn process_result_variants_roundtrip() {
        for result in [
            ProcessResult::session_created("ses-1", Some("msg-abc".into())),
            ProcessResult::session_created("ses-2", None),
            ProcessResult::supplemented(Some("msg-def".into())),
            ProcessResult::supplemented(None),
        ] {
            let json = serde_json::to_string(&result).unwrap();
            let back: ProcessResult = serde_json::from_str(&json).unwrap();
            assert_eq!(back, result);
        }
    }
}
