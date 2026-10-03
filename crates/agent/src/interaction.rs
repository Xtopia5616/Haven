//! Canonical lifecycle value for every interaction that pauses an agent.
//!
//! Ask questions, safety confirmations, and scheduled confirmations all have
//! the same lifecycle: a request is created, waits for an external decision,
//! then resolves, expires, or is cancelled. The request owns both the common
//! lifecycle and the typed execution data needed to resume the operation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn confirmation_expires_at(receipt: &haven_tools::ConfirmationReceipt) -> Option<String> {
    let timestamp = i64::try_from(receipt.expires_at).ok()?;
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0).map(|value| value.to_rfc3339())
}

/// Typed data carried by an interaction request.
///
/// This is deliberately separate from the renderer event projection. The
/// confirm variants contain the original tool input and authorization receipt
/// because resume must re-check and execute the exact invocation, but those
/// fields never cross the Tauri boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InteractionDetails {
    /// Used only by the generic constructor and by forward-compatible callers
    /// that need the common lifecycle before supplying typed execution data.
    Generic,
    Ask {
        options: Vec<String>,
        step_ids: Vec<String>,
    },
    Confirm {
        step_number: u32,
        tool_name: String,
        tool_input: Value,
        tool_call_id: String,
        step_id: String,
        action_index: u32,
        risk_level: haven_common::types::RiskLevel,
        #[serde(skip_serializing_if = "Option::is_none")]
        receipt: Option<haven_tools::ConfirmationReceipt>,
    },
    ScheduledConfirm {
        action_id: String,
        tool_name: String,
        tool_input: Value,
        receipt: haven_tools::ConfirmationReceipt,
        title: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Ask,
    Confirm,
    ScheduledConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    Pending,
    Resolved,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionRequest {
    pub id: String,
    pub session_id: String,
    pub kind: InteractionKind,
    pub status: InteractionStatus,
    /// Old event payloads included prompt text here. Keep reading that field
    /// so existing session_events can replay, but never serialize it again:
    /// transcript content and interaction lifecycle state now have one owner.
    #[allow(dead_code)]
    #[serde(default, rename = "prompt", skip_serializing)]
    legacy_prompt: Option<String>,
    pub details: InteractionDetails,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub correlation_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl InteractionRequest {
    pub fn new(
        session_id: impl Into<String>,
        kind: InteractionKind,
        correlation_ids: Vec<String>,
    ) -> Self {
        let id_prefix = match kind {
            InteractionKind::Ask => "step",
            InteractionKind::Confirm | InteractionKind::ScheduledConfirm => "conf",
        };
        Self {
            id: haven_common::types::new_id(id_prefix),
            session_id: session_id.into(),
            kind,
            status: InteractionStatus::Pending,
            legacy_prompt: None,
            details: InteractionDetails::Generic,
            correlation_ids,
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        }
    }

    pub fn ask(session_id: &str, options: Vec<String>, step_ids: Vec<String>) -> Self {
        let step_id = step_ids
            .first()
            .cloned()
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        Self {
            id: step_id,
            session_id: session_id.to_string(),
            kind: InteractionKind::Ask,
            status: InteractionStatus::Pending,
            legacy_prompt: None,
            details: InteractionDetails::Ask {
                options,
                step_ids: step_ids.clone(),
            },
            correlation_ids: step_ids,
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn confirm(
        session_id: &str,
        step_number: u32,
        tool_name: String,
        tool_input: Value,
        tool_call_id: String,
        step_id: String,
        action_index: u32,
        risk_level: haven_common::types::RiskLevel,
        receipt: Option<haven_tools::ConfirmationReceipt>,
    ) -> Self {
        let expires_at = receipt.as_ref().and_then(confirmation_expires_at);
        let id = receipt
            .as_ref()
            .map(|receipt| receipt.confirmation_id.to_string())
            .unwrap_or_else(|| haven_common::types::new_id("conf").to_string());
        Self {
            id,
            session_id: session_id.to_string(),
            kind: InteractionKind::Confirm,
            status: InteractionStatus::Pending,
            legacy_prompt: None,
            details: InteractionDetails::Confirm {
                step_number,
                tool_name,
                tool_input,
                tool_call_id,
                step_id: step_id.clone(),
                action_index,
                risk_level,
                receipt,
            },
            correlation_ids: vec![step_id],
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at,
        }
    }

    /// Build a confirmation raised by a direct renderer invocation.
    ///
    /// These requests do not belong to an agent session, but they still use
    /// the same interaction lifecycle and renderer projection as agent and
    /// scheduled confirmations. The app keeps the typed execution payload
    /// separately so it can resume the native command after resolution.
    pub fn ui_confirm(
        tool_name: String,
        tool_input: Value,
        receipt: haven_tools::ConfirmationReceipt,
    ) -> Self {
        let confirmation_id = receipt.confirmation_id.to_string();
        let expires_at = confirmation_expires_at(&receipt);
        Self {
            id: confirmation_id.clone(),
            session_id: "ui".into(),
            kind: InteractionKind::Confirm,
            status: InteractionStatus::Pending,
            legacy_prompt: None,
            details: InteractionDetails::Confirm {
                step_number: 0,
                tool_name,
                tool_input,
                tool_call_id: String::new(),
                step_id: String::new(),
                action_index: 0,
                risk_level: receipt.effective_risk,
                receipt: Some(receipt),
            },
            correlation_ids: Vec::new(),
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at,
        }
    }

    pub fn scheduled_confirm(
        action_id: String,
        session_id: &str,
        tool_name: String,
        tool_input: Value,
        receipt: haven_tools::ConfirmationReceipt,
        title: String,
    ) -> Self {
        let expires_at = confirmation_expires_at(&receipt);
        Self {
            id: receipt.confirmation_id.to_string(),
            session_id: session_id.to_string(),
            kind: InteractionKind::ScheduledConfirm,
            status: InteractionStatus::Pending,
            legacy_prompt: None,
            details: InteractionDetails::ScheduledConfirm {
                action_id,
                tool_name,
                tool_input,
                receipt,
                title,
            },
            correlation_ids: Vec::new(),
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at,
        }
    }

    pub fn resolve(&mut self, response: Value) -> bool {
        if self.status != InteractionStatus::Pending {
            return false;
        }
        self.status = InteractionStatus::Resolved;
        self.response = Some(response);
        true
    }

    pub fn expire(&mut self) -> bool {
        if self.status != InteractionStatus::Pending {
            return false;
        }
        self.status = InteractionStatus::Expired;
        true
    }

    pub fn cancel(&mut self) -> bool {
        if self.status != InteractionStatus::Pending {
            return false;
        }
        self.status = InteractionStatus::Cancelled;
        true
    }

    pub fn decision(&self) -> Option<bool> {
        match self.status {
            InteractionStatus::Expired => Some(false),
            InteractionStatus::Pending
            | InteractionStatus::Resolved
            | InteractionStatus::Cancelled => self.response.as_ref().and_then(Value::as_bool),
        }
    }
}

/// Rebuild the live interaction registry from the active durable session
/// stream. An ask's ToolResult and its InteractionRequested control event are
/// appended separately; if the process exits between those commits, the
/// transcript event is enough to restore the unanswered ask. A later Answer
/// injection or interaction-clear event closes that reconstructed request.
pub fn replay_session_interactions(
    session_id: &str,
    active_events: &[haven_memory::SessionEvent],
) -> anyhow::Result<Vec<InteractionRequest>> {
    use haven_memory::{
        INTERACTION_CLEARED_EVENT_TYPE, INTERACTION_REQUESTED_EVENT_TYPE,
        INTERACTION_RESOLVED_EVENT_TYPE, TRANSCRIPT_EVENT_TYPE,
    };

    let mut interactions: Vec<InteractionRequest> = Vec::new();
    for event in active_events {
        if event.event_type == TRANSCRIPT_EVENT_TYPE {
            let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
                continue;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("tool_result")
                    if payload.pointer("/action/tool_name").and_then(Value::as_str)
                        == Some("ask") =>
                {
                    let Some(step_id) = payload.get("step_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let options = payload
                        .pointer("/action/tool_input/options")
                        .and_then(Value::as_array)
                        .map(|options| {
                            options
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut request =
                        InteractionRequest::ask(session_id, options, vec![step_id.to_string()]);
                    request.created_at = event.created_at.clone();
                    interactions.retain(|existing| existing.id != request.id);
                    interactions.push(request);
                }
                Some("user_inject")
                    if payload.get("source").and_then(Value::as_str) == Some("answer") =>
                {
                    interactions.retain(|request| request.kind != InteractionKind::Ask);
                }
                _ => {}
            }
            continue;
        }

        match event.event_type.as_str() {
            INTERACTION_REQUESTED_EVENT_TYPE | INTERACTION_RESOLVED_EVENT_TYPE => {
                let request: InteractionRequest =
                    serde_json::from_str(&event.payload).map_err(|error| {
                        anyhow::anyhow!(
                            "invalid interaction event at sequence {}: {error}",
                            event.sequence
                        )
                    })?;
                if request.kind == InteractionKind::Ask {
                    let covered_ids = std::iter::once(request.id.as_str())
                        .chain(request.correlation_ids.iter().map(String::as_str))
                        .collect::<std::collections::HashSet<_>>();
                    interactions.retain(|existing| {
                        existing.id != request.id
                            && (existing.kind != InteractionKind::Ask
                                || !covered_ids.contains(existing.id.as_str()))
                    });
                } else {
                    interactions.retain(|existing| existing.id != request.id);
                }
                if request.status == InteractionStatus::Pending
                    || request.kind == InteractionKind::Confirm
                {
                    interactions.push(request);
                }
            }
            INTERACTION_CLEARED_EVENT_TYPE => {
                let payload: Value = serde_json::from_str(&event.payload).map_err(|error| {
                    anyhow::anyhow!(
                        "invalid interaction clear event at sequence {}: {error}",
                        event.sequence
                    )
                })?;
                let ids = payload
                    .get("ids")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "interaction clear event at sequence {} has no ids",
                            event.sequence
                        )
                    })?;
                interactions.retain(|request| {
                    !ids.iter()
                        .any(|id| id.as_str() == Some(request.id.as_str()))
                });
            }
            _ => {}
        }
    }
    Ok(interactions)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_ID: &str = "ses-0123456789abcdef0123456789abcdef";

    fn event(sequence: i64, event_type: &str, payload: Value) -> haven_memory::SessionEvent {
        haven_memory::SessionEvent {
            session_id: SESSION_ID.into(),
            sequence,
            event_type: event_type.into(),
            event_version: haven_memory::CURRENT_EVENT_VERSION,
            payload: serde_json::to_string(&payload).unwrap(),
            created_at: format!("2026-10-03T00:00:{sequence:02}Z"),
            run_id: Some(1),
            step_number: Some(sequence as u32),
        }
    }

    fn ask_result(sequence: i64, step_id: &str) -> haven_memory::SessionEvent {
        event(
            sequence,
            haven_memory::TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({
                "type": "tool_result",
                "step_number": sequence,
                "action_index": 0,
                "step_id": step_id,
                "action": {"tool_name": "ask", "tool_input": {"options": ["A", "B"]}}
            }),
        )
    }

    #[test]
    fn interaction_lifecycle_is_one_shot_and_roundtrips() {
        let mut request = InteractionRequest::new(
            "ses-0123456789abcdef0123456789abcdef",
            InteractionKind::Ask,
            vec!["step-0123456789abcdef0123456789abcdef".into()],
        );
        assert!(request.id.starts_with("step-"));
        assert_eq!(request.status, InteractionStatus::Pending);
        assert!(request.resolve(Value::String("notes.md".into())));
        assert!(!request.resolve(Value::String("other.md".into())));
        assert_eq!(request.status, InteractionStatus::Resolved);
        assert_eq!(request.response, Some(Value::String("notes.md".into())));

        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: InteractionRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, request);
        assert!(!request.clone().cancel());
    }

    #[test]
    fn interaction_cancel_and_expire_are_terminal() {
        let mut cancelled = InteractionRequest::new(
            "ses-0123456789abcdef0123456789abcdef",
            InteractionKind::Confirm,
            Vec::new(),
        );
        assert!(cancelled.cancel());
        assert!(!cancelled.expire());

        let mut expired = InteractionRequest::new(
            "ses-0123456789abcdef0123456789abcdef",
            InteractionKind::ScheduledConfirm,
            Vec::new(),
        );
        assert!(expired.expire());
        assert!(!expired.resolve(Value::Bool(true)));
    }

    #[test]
    fn direct_ui_confirmation_uses_the_canonical_confirm_shape() {
        let receipt = haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::new_id("conf").into(),
            capability: haven_tools::CapabilityScope::new("admin.test"),
            canonical_input_hash: "hash".into(),
            effective_risk: haven_common::types::RiskLevel::Medium,
            policy_revision: 1,
            expires_at: 1,
        };
        let request = InteractionRequest::ui_confirm(
            "haven.test".into(),
            serde_json::json!({"value": 1}),
            receipt.clone(),
        );

        assert_eq!(request.id, receipt.confirmation_id.to_string());
        assert_eq!(request.session_id, "ui");
        assert_eq!(request.kind, InteractionKind::Confirm);
        assert!(matches!(
            request.details,
            InteractionDetails::Confirm {
                step_number: 0,
                action_index: 0,
                ..
            }
        ));
    }

    #[test]
    fn replay_recovers_ask_committed_before_interaction_request() {
        let step_id = "step-1123456789abcdef0123456789abcdef";
        let replayed = replay_session_interactions(SESSION_ID, &[ask_result(1, step_id)]).unwrap();

        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].id, step_id);
        assert_eq!(replayed[0].session_id, SESSION_ID);
        assert_eq!(replayed[0].kind, InteractionKind::Ask);
        assert_eq!(replayed[0].status, InteractionStatus::Pending);
        assert_eq!(replayed[0].correlation_ids, [step_id]);
        assert_eq!(
            replayed[0].details,
            InteractionDetails::Ask {
                options: vec!["A".into(), "B".into()],
                step_ids: vec![step_id.into()],
            }
        );
        assert_eq!(replayed[0].created_at, "2026-10-03T00:00:01Z");
    }

    #[test]
    fn replay_groups_asks_and_only_answer_or_clear_closes_them() {
        use haven_memory::{INTERACTION_CLEARED_EVENT_TYPE, INTERACTION_REQUESTED_EVENT_TYPE};

        let first_step = "step-1123456789abcdef0123456789abcdef";
        let second_step = "step-2123456789abcdef0123456789abcdef";
        let mut grouped_request = InteractionRequest::ask(
            SESSION_ID,
            vec!["A".into(), "B".into()],
            vec![first_step.into(), second_step.into()],
        );
        grouped_request.created_at = "2026-10-03T00:00:03Z".into();
        let grouped_event = event(
            3,
            INTERACTION_REQUESTED_EVENT_TYPE,
            serde_json::to_value(&grouped_request).unwrap(),
        );
        let asks = [ask_result(1, first_step), ask_result(2, second_step)];
        let replayed = replay_session_interactions(
            SESSION_ID,
            &[asks[0].clone(), asks[1].clone(), grouped_event],
        )
        .unwrap();

        assert_eq!(replayed, [grouped_request.clone()]);

        let follow_up = event(
            3,
            haven_memory::TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "user_inject", "source": "follow_up"}),
        );
        let after_follow_up =
            replay_session_interactions(SESSION_ID, &[asks[0].clone(), follow_up]).unwrap();
        assert_eq!(
            after_follow_up.len(),
            1,
            "ordinary follow-up keeps Ask pending"
        );

        let answer = event(
            3,
            haven_memory::TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "user_inject", "source": "answer"}),
        );
        assert!(
            replay_session_interactions(SESSION_ID, &[asks[0].clone(), answer])
                .unwrap()
                .is_empty()
        );

        let cleared = event(
            3,
            INTERACTION_CLEARED_EVENT_TYPE,
            serde_json::json!({"ids": [first_step]}),
        );
        assert!(
            replay_session_interactions(SESSION_ID, &[asks[0].clone(), cleared])
                .unwrap()
                .is_empty()
        );
    }
}
