//! Canonical lifecycle value for every interaction that pauses an agent.
//!
//! Ask questions, safety confirmations, and scheduled confirmations all have
//! the same lifecycle: a request is created, waits for an external decision,
//! then resolves, expires, or is cancelled. The request owns both the common
//! lifecycle and the typed execution data needed to resume the operation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub prompt: String,
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
            details: InteractionDetails::Generic,
            correlation_ids,
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        }
    }

    pub fn ask(
        session_id: &str,
        question: impl Into<String>,
        options: Vec<String>,
        step_ids: Vec<String>,
    ) -> Self {
        let step_id = step_ids
            .first()
            .cloned()
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        Self {
            id: step_id,
            session_id: session_id.to_string(),
            kind: InteractionKind::Ask,
            status: InteractionStatus::Pending,
            prompt: question.into(),
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
        let id = receipt
            .as_ref()
            .map(|receipt| receipt.confirmation_id.to_string())
            .unwrap_or_else(|| haven_common::types::new_id("conf").to_string());
        Self {
            id,
            session_id: session_id.to_string(),
            kind: InteractionKind::Confirm,
            status: InteractionStatus::Pending,
            prompt: "Waiting for confirmation".into(),
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
            expires_at: None,
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
        Self {
            id: receipt.confirmation_id.to_string(),
            session_id: session_id.to_string(),
            kind: InteractionKind::ScheduledConfirm,
            status: InteractionStatus::Pending,
            prompt: title.clone(),
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

    pub fn decision(&self) -> Option<bool> {
        self.response.as_ref().and_then(Value::as_bool)
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
