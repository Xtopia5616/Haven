//! Canonical lifecycle value for every interaction that pauses an agent.
//!
//! Ask questions, safety confirmations, and scheduled confirmations all have
//! the same durable shape: a request is created, waits for an external
//! decision, then resolves, expires, or is cancelled. The legacy snapshot
//! fields remain readable for compatibility; new checkpoints also emit this
//! normalized projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub prompt: String,
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
        prompt: impl Into<String>,
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
            prompt: prompt.into(),
            correlation_ids,
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
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

    pub fn from_ask(session_id: &str, pending: &crate::types::AskPending) -> Self {
        let id = pending
            .step_ids
            .first()
            .cloned()
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        Self {
            id,
            session_id: session_id.to_string(),
            kind: InteractionKind::Ask,
            status: InteractionStatus::Pending,
            prompt: pending.question.clone(),
            correlation_ids: pending.step_ids.clone(),
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        }
    }

    pub fn from_confirm(session_id: &str, pending: &crate::types::ConfirmPending) -> Self {
        let ids = pending
            .tools
            .iter()
            .map(|tool| tool.confirm_id.clone())
            .collect::<Vec<_>>();
        Self {
            id: ids
                .first()
                .cloned()
                .unwrap_or_else(|| haven_common::types::new_id("conf")),
            session_id: session_id.to_string(),
            kind: InteractionKind::Confirm,
            status: InteractionStatus::Pending,
            prompt: "Waiting for confirmation".into(),
            correlation_ids: ids,
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_lifecycle_is_one_shot_and_roundtrips() {
        let mut request = InteractionRequest::new(
            "ses-0123456789abcdef0123456789abcdef",
            InteractionKind::Ask,
            "Which file should I use?",
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
            "Allow the operation?",
            Vec::new(),
        );
        assert!(cancelled.cancel());
        assert!(!cancelled.expire());

        let mut expired = InteractionRequest::new(
            "ses-0123456789abcdef0123456789abcdef",
            InteractionKind::ScheduledConfirm,
            "Allow the scheduled operation?",
            Vec::new(),
        );
        assert!(expired.expire());
        assert!(!expired.resolve(Value::Bool(true)));
    }
}
