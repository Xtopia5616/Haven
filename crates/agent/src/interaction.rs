//! Canonical lifecycle value for every interaction that pauses an agent.
//!
//! Ask questions, safety confirmations, and scheduled confirmations all have
//! the same lifecycle: a request is created, waits for an external decision,
//! then resolves, expires, or is cancelled. The request owns both the common
//! lifecycle and the typed execution data needed to resume the operation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn require_canonical_id(id: &str, prefix: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        haven_common::types::is_canonical_id(id, prefix),
        "invalid {field}"
    );
    Ok(())
}

pub(crate) fn validate_interaction_ids(request: &InteractionRequest) -> anyhow::Result<()> {
    let (request_prefix, detail_ids) = match (&request.kind, &request.details) {
        (InteractionKind::Ask, InteractionDetails::Ask { step_ids, .. }) => {
            if let Some(first_step_id) = step_ids.first() {
                anyhow::ensure!(
                    request.request_id == *first_step_id,
                    "ask request ID does not match its first step ID"
                );
            }
            (
                "step",
                step_ids.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        }
        (
            InteractionKind::Confirm,
            InteractionDetails::Confirm {
                step_id, receipt, ..
            },
        ) => {
            require_canonical_id(step_id, "step", "confirmation step ID")?;
            if let Some(receipt) = receipt {
                anyhow::ensure!(
                    request.request_id == receipt.confirmation_id.to_string(),
                    "confirmation request ID does not match its receipt"
                );
            }
            ("conf", vec![step_id.as_str()])
        }
        (
            InteractionKind::ScheduledConfirm,
            InteractionDetails::ScheduledConfirm {
                tool_run_id,
                receipt,
                ..
            },
        ) => {
            require_canonical_id(tool_run_id, "toolrun", "scheduled tool run ID")?;
            anyhow::ensure!(
                request.request_id == receipt.confirmation_id.to_string(),
                "scheduled confirmation request ID does not match its receipt"
            );
            ("conf", Vec::new())
        }
        _ => anyhow::bail!("interaction kind does not match its details"),
    };

    require_canonical_id(
        &request.request_id,
        request_prefix,
        "interaction request ID",
    )?;
    if let Some(session_id) = request.session_id.as_deref() {
        require_canonical_id(session_id, "ses", "interaction session ID")?;
    }
    for id in detail_ids
        .into_iter()
        .chain(request.correlation_ids.iter().map(String::as_str))
    {
        require_canonical_id(id, "step", "interaction correlation ID")?;
    }
    Ok(())
}

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
        tool_index: u32,
        risk_level: haven_common::types::RiskLevel,
        #[serde(skip_serializing_if = "Option::is_none")]
        receipt: Option<haven_tools::ConfirmationReceipt>,
    },
    ScheduledConfirm {
        tool_run_id: String,
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

/// Identifies the runtime registry that owns a pending interaction.
///
/// This routing identity is process-local and must never be persisted in the
/// interaction continuation. A session's durable event stream determines its
/// owner during replay; other owner continuations stay in their own registries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionOwner {
    Session { session_id: String },
    ScheduledToolRun { tool_run_id: String },
    AppCommand,
}

/// Runtime delivery value coupling an interaction to its single lifecycle owner.
/// This envelope is intentionally not serialized into durable session events.
#[derive(Debug, Clone, PartialEq)]
pub struct InteractionEnvelope {
    pub owner: InteractionOwner,
    pub request: InteractionRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionRequest {
    /// Identity of this interaction request. Ask requests reuse their step ID;
    /// confirmation requests use a `conf-*` ID.
    #[serde(alias = "id")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub kind: InteractionKind,
    pub status: InteractionStatus,
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
    /// Return the owner-issued absolute expiry for a pending permission
    /// confirmation. Ask interactions and terminal confirmation projections do
    /// not require a deadline.
    pub fn pending_permission_deadline(&self) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
        anyhow::ensure!(
            self.status == InteractionStatus::Pending
                && matches!(
                    self.kind,
                    InteractionKind::Confirm | InteractionKind::ScheduledConfirm
                ),
            "interaction is not a pending permission confirmation"
        );
        let expires_at = self
            .expires_at
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("pending permission confirmation has no deadline"))?;
        chrono::DateTime::parse_from_rfc3339(expires_at)
            .map(|value| value.with_timezone(&chrono::Utc))
            .map_err(|error| anyhow::anyhow!("invalid permission confirmation deadline: {error}"))
    }

    /// Validate an owner registration before it becomes pending. Existing
    /// durable requests are replayed separately and may have an elapsed
    /// deadline; their owner immediately expires them instead.
    pub fn validate_new_pending_permission(&self) -> anyhow::Result<()> {
        let deadline = self.pending_permission_deadline()?;
        anyhow::ensure!(
            deadline > chrono::Utc::now(),
            "permission confirmation deadline has already elapsed"
        );
        Ok(())
    }
}

pub(crate) fn validate_session_association(
    interaction_session_id: Option<&str>,
    owner_session_id: &str,
) -> anyhow::Result<()> {
    match interaction_session_id {
        Some(interaction_session_id) if interaction_session_id == owner_session_id => Ok(()),
        Some(_) => anyhow::bail!("interaction session association does not match its owner"),
        None => anyhow::bail!("session-owned interaction has no session association"),
    }
}

impl InteractionRequest {
    pub fn ask(session_id: &str, options: Vec<String>, step_ids: Vec<String>) -> Self {
        let step_id = step_ids
            .first()
            .cloned()
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        Self {
            request_id: step_id,
            session_id: Some(session_id.to_string()),
            kind: InteractionKind::Ask,
            status: InteractionStatus::Pending,
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
        tool_index: u32,
        risk_level: haven_common::types::RiskLevel,
        receipt: Option<haven_tools::ConfirmationReceipt>,
    ) -> Self {
        let expires_at = receipt.as_ref().and_then(confirmation_expires_at);
        let request_id = receipt
            .as_ref()
            .map(|receipt| receipt.confirmation_id.to_string())
            .unwrap_or_else(|| haven_common::types::new_id("conf").to_string());
        Self {
            request_id,
            session_id: Some(session_id.to_string()),
            kind: InteractionKind::Confirm,
            status: InteractionStatus::Pending,
            details: InteractionDetails::Confirm {
                step_number,
                tool_name,
                tool_input,
                tool_call_id,
                step_id: step_id.clone(),
                tool_index,
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
            request_id: confirmation_id.clone(),
            session_id: None,
            kind: InteractionKind::Confirm,
            status: InteractionStatus::Pending,
            details: InteractionDetails::Confirm {
                step_number: 0,
                tool_name,
                tool_input,
                tool_call_id: String::new(),
                step_id: String::new(),
                tool_index: 0,
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
        tool_run_id: String,
        session_id: Option<&str>,
        tool_name: String,
        tool_input: Value,
        receipt: haven_tools::ConfirmationReceipt,
        title: String,
    ) -> Self {
        let expires_at = confirmation_expires_at(&receipt);
        Self {
            request_id: receipt.confirmation_id.to_string(),
            session_id: session_id.map(str::to_owned),
            kind: InteractionKind::ScheduledConfirm,
            status: InteractionStatus::Pending,
            details: InteractionDetails::ScheduledConfirm {
                tool_run_id,
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

    require_canonical_id(session_id, "ses", "replay session ID")?;

    let mut interactions: Vec<InteractionRequest> = Vec::new();
    for event in active_events {
        if event.session_id != session_id {
            anyhow::bail!(
                "active event at sequence {} belongs to a different session",
                event.sequence
            );
        }

        if event.event_type == TRANSCRIPT_EVENT_TYPE {
            let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
                continue;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("tool_result")
                    if payload.pointer("/action/tool_name").and_then(Value::as_str)
                        == Some("ask") =>
                {
                    let Some(step_id_value) = payload.get("step_id") else {
                        continue;
                    };
                    let Some(step_id) = step_id_value.as_str() else {
                        anyhow::bail!(
                            "invalid ask transcript at sequence {}: step ID is not a string",
                            event.sequence
                        );
                    };
                    require_canonical_id(step_id, "step", "ask transcript step ID").map_err(
                        |error| {
                            anyhow::anyhow!(
                                "invalid ask transcript at sequence {}: {error}",
                                event.sequence
                            )
                        },
                    )?;
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
                    interactions.retain(|existing| existing.request_id != request.request_id);
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
                validate_session_association(request.session_id.as_deref(), session_id).map_err(
                    |error| {
                        anyhow::anyhow!(
                            "invalid interaction event at sequence {}: {error}",
                            event.sequence
                        )
                    },
                )?;
                validate_interaction_ids(&request).map_err(|error| {
                    anyhow::anyhow!(
                        "invalid interaction event at sequence {}: {error}",
                        event.sequence
                    )
                })?;
                if request.kind == InteractionKind::Ask {
                    let covered_ids = std::iter::once(request.request_id.as_str())
                        .chain(request.correlation_ids.iter().map(String::as_str))
                        .collect::<std::collections::HashSet<_>>();
                    interactions.retain(|existing| {
                        existing.request_id != request.request_id
                            && (existing.kind != InteractionKind::Ask
                                || !covered_ids.contains(existing.request_id.as_str()))
                    });
                } else {
                    interactions.retain(|existing| existing.request_id != request.request_id);
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
                    .get("request_ids")
                    .or_else(|| payload.get("ids"))
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "interaction clear event at sequence {} has no request_ids",
                            event.sequence
                        )
                    })?;
                if !ids.iter().all(|id| {
                    id.as_str().is_some_and(|id| {
                        haven_common::types::is_canonical_id(id, "step")
                            || haven_common::types::is_canonical_id(id, "conf")
                    })
                }) {
                    anyhow::bail!(
                        "interaction clear event at sequence {} has invalid request IDs",
                        event.sequence
                    );
                }
                interactions.retain(|request| {
                    !ids.iter()
                        .any(|id| id.as_str() == Some(request.request_id.as_str()))
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

    fn confirmation_request(session_id: &str) -> InteractionRequest {
        InteractionRequest::confirm(
            session_id,
            1,
            "haven.test".into(),
            Value::Null,
            "call-test".into(),
            "step-0123456789abcdef0123456789abcdef".into(),
            0,
            haven_common::types::RiskLevel::Safe,
            Some(haven_tools::ConfirmationReceipt {
                confirmation_id: haven_common::types::ConfirmId::new(),
                capability: haven_common::types::CapabilityScope::try_new("haven.test").unwrap(),
                canonical_input_hash: String::new(),
                effective_risk: haven_common::types::RiskLevel::Safe,
                policy_revision: 1,
                expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
            }),
        )
    }

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
                "tool_index": 0,
                "step_id": step_id,
                "action": {"tool_name": "ask", "tool_input": {"options": ["A", "B"]}}
            }),
        )
    }

    #[test]
    fn interaction_lifecycle_is_one_shot_and_roundtrips() {
        let mut request = InteractionRequest::ask(
            SESSION_ID,
            Vec::new(),
            vec!["step-0123456789abcdef0123456789abcdef".into()],
        );
        assert!(request.request_id.starts_with("step-"));
        assert_eq!(request.status, InteractionStatus::Pending);
        assert_eq!(request.session_id.as_deref(), Some(SESSION_ID));
        assert!(request.resolve(Value::String("notes.md".into())));
        assert!(!request.resolve(Value::String("other.md".into())));
        assert_eq!(request.status, InteractionStatus::Resolved);
        assert_eq!(request.response, Some(Value::String("notes.md".into())));

        let encoded = serde_json::to_string(&request).unwrap();
        let wire: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(wire["request_id"], request.request_id);
        assert!(wire.get("id").is_none());
        assert_eq!(wire["session_id"], SESSION_ID);
        assert!(wire.get("owner").is_none());
        let decoded: InteractionRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, request);

        let mut legacy_wire = wire;
        let request_id = legacy_wire
            .as_object_mut()
            .unwrap()
            .remove("request_id")
            .unwrap();
        legacy_wire["id"] = request_id;
        assert_eq!(
            serde_json::from_value::<InteractionRequest>(legacy_wire).unwrap(),
            request
        );
        assert!(!request.clone().cancel());
    }

    #[test]
    fn session_association_requires_the_exact_owner_session() {
        assert!(validate_session_association(Some(SESSION_ID), SESSION_ID).is_ok());
        assert!(validate_session_association(Some("ses-other"), SESSION_ID).is_err());
        assert!(validate_session_association(None, SESSION_ID).is_err());
    }

    #[test]
    fn owner_wire_is_typed_and_non_session_context_is_omitted() {
        let owner = InteractionOwner::ScheduledToolRun {
            tool_run_id: "toolrun-0123456789abcdef0123456789abcdef".into(),
        };
        assert_eq!(
            serde_json::to_value(owner).unwrap(),
            serde_json::json!({
                "kind": "scheduled_tool_run",
                "tool_run_id": "toolrun-0123456789abcdef0123456789abcdef"
            })
        );

        let receipt = haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: haven_common::types::CapabilityScope::try_new("admin.test").unwrap(),
            canonical_input_hash: "hash".into(),
            effective_risk: haven_common::types::RiskLevel::Medium,
            policy_revision: 1,
            expires_at: 1,
        };
        let request = InteractionRequest::ui_confirm(
            "haven.test".into(),
            serde_json::json!({"value": 1}),
            receipt,
        );
        let wire = serde_json::to_value(request).unwrap();
        assert!(wire.get("session_id").is_none());
        assert!(wire.get("owner").is_none());
    }

    #[test]
    fn interaction_cancel_and_expire_are_terminal() {
        let mut cancelled = InteractionRequest::ask(SESSION_ID, Vec::new(), Vec::new());
        assert!(cancelled.cancel());
        assert!(!cancelled.expire());

        let mut expired = confirmation_request(SESSION_ID);
        assert!(expired.expire());
        assert!(!expired.resolve(Value::Bool(true)));
    }

    #[test]
    fn direct_ui_confirmation_uses_the_canonical_confirm_shape() {
        let receipt = haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: haven_common::types::CapabilityScope::try_new("admin.test").unwrap(),
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

        assert_eq!(request.request_id, receipt.confirmation_id.to_string());
        assert_eq!(request.session_id, None);
        assert_eq!(request.kind, InteractionKind::Confirm);
        assert!(matches!(
            request.details,
            InteractionDetails::Confirm {
                step_number: 0,
                tool_index: 0,
                ..
            }
        ));
    }

    #[test]
    fn replay_recovers_ask_committed_before_interaction_request() {
        let step_id = "step-1123456789abcdef0123456789abcdef";
        let replayed = replay_session_interactions(SESSION_ID, &[ask_result(1, step_id)]).unwrap();

        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].request_id, step_id);
        assert_eq!(replayed[0].session_id.as_deref(), Some(SESSION_ID));
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
    fn replay_rejects_an_interaction_from_another_session() {
        let request = InteractionRequest::ask(
            "ses-1123456789abcdef0123456789abcdef",
            Vec::new(),
            Vec::new(),
        );
        let event = event(
            1,
            haven_memory::INTERACTION_REQUESTED_EVENT_TYPE,
            serde_json::to_value(request).unwrap(),
        );

        let error = replay_session_interactions(SESSION_ID, &[event])
            .expect_err("mismatched session association must fail closed");
        assert!(error.to_string().contains("does not match its owner"));
    }

    #[test]
    fn replay_rejects_noncanonical_session_and_transcript_ids() {
        assert!(replay_session_interactions("ses-short", &[]).is_err());

        let error = replay_session_interactions(SESSION_ID, &[ask_result(1, "step-short")])
            .expect_err("malformed ask IDs must not become pending interactions");
        assert!(error.to_string().contains("ask transcript step ID"));

        let malformed_type = event(
            2,
            haven_memory::TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({
                "type": "tool_result",
                "step_id": 42,
                "action": {"tool_name": "ask"}
            }),
        );
        let error = replay_session_interactions(SESSION_ID, &[malformed_type])
            .expect_err("non-string ask IDs must fail replay");
        assert!(error.to_string().contains("step ID is not a string"));
    }

    #[test]
    fn replay_rejects_malformed_interaction_identity_fields() {
        use haven_memory::INTERACTION_REQUESTED_EVENT_TYPE;

        let mut request = InteractionRequest::ask(
            SESSION_ID,
            Vec::new(),
            vec!["step-1123456789abcdef0123456789abcdef".into()],
        );
        request.correlation_ids = vec!["step-short".into()];
        let malformed_correlation = event(
            1,
            INTERACTION_REQUESTED_EVENT_TYPE,
            serde_json::to_value(request).unwrap(),
        );
        let error = replay_session_interactions(SESSION_ID, &[malformed_correlation])
            .expect_err("malformed correlation IDs must fail replay");
        assert!(error.to_string().contains("interaction correlation ID"));

        let receipt = haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: haven_common::types::CapabilityScope::try_new("haven.test").unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: haven_common::types::RiskLevel::Safe,
            policy_revision: 1,
            expires_at: 1,
        };
        let request = InteractionRequest::scheduled_confirm(
            "toolrun-short".into(),
            Some(SESSION_ID),
            "haven.test".into(),
            Value::Null,
            receipt,
            "Scheduled confirmation".into(),
        );
        let malformed_tool_run = event(
            2,
            INTERACTION_REQUESTED_EVENT_TYPE,
            serde_json::to_value(request).unwrap(),
        );
        let error = replay_session_interactions(SESSION_ID, &[malformed_tool_run])
            .expect_err("malformed ToolRun IDs must fail replay");
        assert!(error.to_string().contains("scheduled tool run ID"));
    }

    #[test]
    fn replay_rejects_malformed_interaction_clear_ids() {
        let cleared = event(
            1,
            haven_memory::INTERACTION_CLEARED_EVENT_TYPE,
            serde_json::json!({"request_ids": ["step-short"]}),
        );

        let error = replay_session_interactions(SESSION_ID, &[cleared])
            .expect_err("malformed clear IDs must not silently close requests");
        assert!(error.to_string().contains("invalid request IDs"));
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
            serde_json::json!({"request_ids": [first_step]}),
        );
        assert!(
            replay_session_interactions(SESSION_ID, &[asks[0].clone(), cleared])
                .unwrap()
                .is_empty()
        );

        let legacy_cleared = event(
            3,
            INTERACTION_CLEARED_EVENT_TYPE,
            serde_json::json!({"ids": [first_step]}),
        );
        assert!(
            replay_session_interactions(SESSION_ID, &[asks[0].clone(), legacy_cleared])
                .unwrap()
                .is_empty()
        );
    }
}
