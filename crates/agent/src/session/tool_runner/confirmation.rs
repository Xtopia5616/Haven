use super::*;
use tracing::Instrument;

pub(in crate::session) fn confirmation_expiry_delay(
    expires_at: Option<&str>,
) -> Option<std::time::Duration> {
    let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at?)
        .ok()?
        .with_timezone(&chrono::Utc);
    let remaining = expires_at.signed_duration_since(chrono::Utc::now());
    Some(remaining.to_std().unwrap_or_default())
}

pub(super) fn interaction_owner_matches_request(
    owner: &crate::interaction::InteractionOwner,
    request: &crate::interaction::InteractionRequest,
) -> bool {
    match (owner, &request.details) {
        (
            crate::interaction::InteractionOwner::Session { session_id },
            crate::interaction::InteractionDetails::Confirm { .. },
        ) => request.session_id.as_deref() == Some(session_id.as_str()),
        (
            crate::interaction::InteractionOwner::ScheduledToolRun { tool_run_id },
            crate::interaction::InteractionDetails::ScheduledConfirm {
                tool_run_id: request_tool_run_id,
                ..
            },
        ) => tool_run_id == request_tool_run_id,
        _ => false,
    }
}

pub(super) fn scheduled_request_matches_route(
    request: &crate::interaction::InteractionRequest,
    tool_run_id: &str,
    request_id: &haven_common::types::ConfirmId,
) -> bool {
    request.request_id == request_id.as_str()
        && request.status == crate::interaction::InteractionStatus::Pending
        && matches!(
            &request.details,
            crate::interaction::InteractionDetails::ScheduledConfirm {
                tool_run_id: request_tool_run_id,
                ..
            } if request_tool_run_id == tool_run_id
        )
}

#[derive(Clone, Copy)]
pub(super) enum ScheduledConfirmDeadlineDecision {
    /// Determine expiry when the request is committed as terminal.
    CheckAtResolution,
    /// The owner gate already accepted this decision before the deadline.
    AcceptedBeforeDeadline,
    /// An owner timer or failed authorization explicitly expired the request.
    Expire,
}

pub(super) fn scheduled_confirmation_is_expired(
    request: &crate::interaction::InteractionRequest,
    decision: ScheduledConfirmDeadlineDecision,
) -> bool {
    match decision {
        ScheduledConfirmDeadlineDecision::CheckAtResolution => request
            .pending_permission_deadline()
            .map(|deadline| deadline <= chrono::Utc::now())
            .unwrap_or(true),
        ScheduledConfirmDeadlineDecision::AcceptedBeforeDeadline => false,
        ScheduledConfirmDeadlineDecision::Expire => true,
    }
}

struct ScheduledConfirmSpec {
    tool_run_id: String,
    session_id: Option<String>,
    tool_name: String,
    tool_args: Value,
    receipt: haven_tools::ConfirmationReceipt,
    nested_confirmation: Option<haven_tools::NestedConfirmationRequest>,
    title: String,
}

impl SessionSupervisor {
    pub(super) async fn scheduled_authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<haven_tools::AuthorizationRequest, String> {
        self.authorization
            .authorization_request(session_id, tool_name, input)
            .await
    }

    /// Authorize a scheduled tool invocation through the supervisor's live
    /// authorization service. Confirmation queuing and execution remain with
    /// their existing scheduled ToolRun callers.
    pub(crate) async fn authorize_scheduled_tool(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> haven_tools::AuthorizationDecision {
        let request = self
            .scheduled_authorization_request(session_id, tool_name, input)
            .await;
        let request = match request {
            Ok(request) => request,
            Err(reason) => {
                return haven_tools::AuthorizationDecision::Blocked {
                    reason: format!("operation policy could not be resolved: {reason}"),
                    reason_code: haven_tools::AuthorizationReasonCode::DisabledOperation,
                };
            }
        };
        self.authorization.authorize(&request).await
    }

    /// Queue a scheduled ToolRun confirmation without blocking the fire
    /// consumer (R2). Stores the canonical interaction request and emits it
    /// through the supervisor event stream; a later owner-routed resolve or
    /// expiry executes or skips the ToolRun. Returns `None` when the ToolRun
    /// already owns a pending confirmation (fail closed).
    pub async fn request_scheduled_confirm(
        self: &Arc<Self>,
        tool_run_id: &str,
        session_id: Option<&str>,
        tool_name: &str,
        tool_args: Value,
        receipt: haven_tools::ConfirmationReceipt,
        title: &str,
    ) -> Option<haven_common::types::ConfirmId> {
        self.queue_scheduled_confirm(ScheduledConfirmSpec {
            tool_run_id: tool_run_id.to_owned(),
            session_id: session_id.map(str::to_owned),
            tool_name: tool_name.to_owned(),
            tool_args,
            receipt,
            nested_confirmation: None,
            title: title.to_owned(),
        })
        .await
    }

    pub async fn request_scheduled_nested_confirm(
        self: &Arc<Self>,
        tool_run_id: &str,
        session_id: Option<&str>,
        tool_name: &str,
        tool_args: Value,
        nested: haven_tools::NestedConfirmationRequest,
        title: &str,
    ) -> Option<haven_common::types::ConfirmId> {
        let receipt = nested.receipt.clone();
        self.queue_scheduled_confirm(ScheduledConfirmSpec {
            tool_run_id: tool_run_id.to_owned(),
            session_id: session_id.map(str::to_owned),
            tool_name: tool_name.to_owned(),
            tool_args,
            receipt,
            nested_confirmation: Some(nested),
            title: title.to_owned(),
        })
        .await
    }

    async fn queue_scheduled_confirm(
        self: &Arc<Self>,
        spec: ScheduledConfirmSpec,
    ) -> Option<haven_common::types::ConfirmId> {
        let ScheduledConfirmSpec {
            tool_run_id,
            session_id,
            tool_name,
            tool_args,
            receipt,
            nested_confirmation,
            title,
        } = spec;
        let step_id = receipt.confirmation_id.clone();
        let request = match nested_confirmation {
            Some(nested) => crate::interaction::InteractionRequest::scheduled_confirm_nested(
                tool_run_id.clone(),
                session_id.as_deref(),
                tool_name.clone(),
                tool_args,
                nested,
                title,
            ),
            None => crate::interaction::InteractionRequest::scheduled_confirm(
                tool_run_id.clone(),
                session_id.as_deref(),
                tool_name,
                tool_args,
                receipt,
                title,
            ),
        };
        if request.validate_new_pending_permission().is_err() {
            tracing::warn!(
                tool_run_id = %tool_run_id,
                request_id = %step_id,
                failure_kind = "invalid_confirmation_request",
                "scheduled confirmation has no valid future deadline"
            );
            return None;
        }
        let Some(expiry_delay) = confirmation_expiry_delay(request.expires_at.as_deref()) else {
            tracing::warn!(tool_run_id = %tool_run_id, request_id = %step_id, "scheduled confirmation deadline cannot be scheduled");
            return None;
        };
        {
            let mut scheduled_confirms = self.scheduled_confirms.lock().await;
            if scheduled_confirms.contains_key(&tool_run_id) {
                tracing::warn!(%tool_run_id, "scheduled ToolRun already owns a pending confirmation");
                return None;
            }
            scheduled_confirms.insert(tool_run_id.clone(), request.clone());
        }
        self.emit_event(SessionSupervisorEvent::InteractionRequested {
            envelope: Box::new(crate::interaction::InteractionEnvelope {
                owner: crate::interaction::InteractionOwner::ScheduledToolRun {
                    tool_run_id: tool_run_id.clone(),
                },
                request,
            }),
        });
        self.spawn_scheduled_confirmation_expiry(tool_run_id, step_id.clone(), expiry_delay);
        Some(step_id)
    }

    fn spawn_scheduled_confirmation_expiry(
        self: &Arc<Self>,
        tool_run_id: String,
        timeout_id: haven_common::types::ConfirmId,
        expiry_delay: std::time::Duration,
    ) {
        // The canonical request deadline is the hard lifetime for this
        // approval, including when the renderer is closed or crashed.
        let executor = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(expiry_delay).await;
            if executor
                .scheduled_confirmation_request(&tool_run_id, &timeout_id)
                .await
                .is_some()
            {
                tracing::warn!(
                    tool_run_id = %tool_run_id,
                    request_id = %timeout_id,
                    "scheduled confirmation timed out after {:?}; treating as rejected",
                    expiry_delay
                );
                let mut retry_delay = std::time::Duration::from_secs(1);
                loop {
                    match executor
                        .expire_scheduled_confirmation(&tool_run_id, &timeout_id)
                        .await
                    {
                        Ok(_) => break,
                        Err(error) => {
                            tracing::warn!(
                                tool_run_id = %tool_run_id,
                                request_id = %timeout_id,
                                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                                "failed to expire scheduled confirmation; retrying"
                            );
                            tokio::time::sleep(retry_delay).await;
                            retry_delay = retry_delay
                                .saturating_mul(2)
                                .min(std::time::Duration::from_secs(30));
                        }
                    }
                }
            }
        });
    }

    pub(super) async fn scheduled_confirmation_request(
        &self,
        tool_run_id: &str,
        request_id: &haven_common::types::ConfirmId,
    ) -> Option<crate::interaction::InteractionRequest> {
        self.scheduled_confirms
            .lock()
            .await
            .get(tool_run_id)
            .filter(|request| scheduled_request_matches_route(request, tool_run_id, request_id))
            .cloned()
    }

    pub async fn pending_confirmation_capability_for_owner(
        &self,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
    ) -> Option<haven_common::types::CapabilityScope> {
        let request = match owner {
            crate::interaction::InteractionOwner::ScheduledToolRun { tool_run_id } => {
                self.scheduled_confirmation_request(tool_run_id, request_id)
                    .await?
            }
            crate::interaction::InteractionOwner::AppCommand => return None,
            crate::interaction::InteractionOwner::Session { session_id } => {
                self.pending_session_confirmation_request(session_id, request_id)
                    .await?
            }
        };
        if !interaction_owner_matches_request(owner, &request) {
            return None;
        }
        match &request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm { receipt, .. } => {
                Some(receipt.capability.clone())
            }
            crate::interaction::InteractionDetails::Confirm {
                receipt,
                nested_confirmation,
                tool_name,
                tool_input,
                ..
            } => {
                if let Some(receipt) = receipt {
                    Some(receipt.capability.clone())
                } else if let Some(nested) = nested_confirmation {
                    Some(nested.receipt.capability.clone())
                } else {
                    let session_id = request.session_id.as_deref()?;
                    self.authorization
                        .authorization_request(Some(session_id), tool_name, tool_input)
                        .await
                        .ok()
                        .map(|request| request.policy.capability)
                }
            }
            crate::interaction::InteractionDetails::Ask { .. } => None,
        }
    }

    pub(super) async fn pending_session_confirmation_request(
        &self,
        session_id: &str,
        step_id: &haven_common::types::ConfirmId,
    ) -> Option<crate::interaction::InteractionRequest> {
        let actor = self.actor_for(session_id).await?;
        actor
            .interactions(Some(crate::interaction::InteractionKind::Confirm), true)
            .await
            .into_iter()
            .find(|request| request.request_id == step_id.as_str())
    }

    /// Persist an explicit session grant before resolving a pending
    /// confirmation. Resolving the interaction can wake a paused ReAct actor
    /// (or spawn a scheduled operation), so the durable decision must exist
    /// before that wake edge. The owner route selects either the session actor
    /// or the scheduled ToolRun registry; UI-only confirmations are handled by
    /// the app command because their typed action payload is app-owned.
    pub async fn resolve_confirmation_with_session_grant_for_owner(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
        target: haven_common::types::PermissionTarget,
        effect: haven_common::types::PermissionEffect,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        self.resolve_confirmation_with_session_grant_inner(owner, request_id, target, effect)
            .await
    }

    async fn resolve_confirmation_with_session_grant_inner(
        self: &Arc<Self>,
        expected_owner: &crate::interaction::InteractionOwner,
        step_id: &haven_common::types::ConfirmId,
        target: haven_common::types::PermissionTarget,
        effect: haven_common::types::PermissionEffect,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        let request = match expected_owner {
            crate::interaction::InteractionOwner::ScheduledToolRun { tool_run_id } => {
                self.scheduled_confirmation_request(tool_run_id, step_id)
                    .await
            }
            crate::interaction::InteractionOwner::AppCommand => None,
            crate::interaction::InteractionOwner::Session { session_id } => {
                self.pending_session_confirmation_request(session_id, step_id)
                    .await
            }
        };
        let Some(request) = request else {
            return Ok(None);
        };
        if self.request_session_is_closing(expected_owner, &request) {
            return Ok(None);
        }
        if !interaction_owner_matches_request(expected_owner, &request) {
            return Ok(None);
        }
        if request
            .pending_permission_deadline()
            .map(|deadline| deadline <= chrono::Utc::now())
            .unwrap_or(true)
        {
            if let Some(tool_run_id) = match expected_owner {
                crate::interaction::InteractionOwner::ScheduledToolRun { tool_run_id } => {
                    Some(tool_run_id.as_str())
                }
                _ => None,
            } {
                return self
                    .resolve_scheduled_confirmation_locked(
                        tool_run_id,
                        step_id,
                        false,
                        ScheduledConfirmDeadlineDecision::Expire,
                    )
                    .await;
            }
            let crate::interaction::InteractionOwner::Session { session_id } = expected_owner
            else {
                return Ok(None);
            };
            return self
                .resolve_confirmation_locked(session_id, step_id, false, true)
                .await;
        }
        let scheduled_tool_run_id = match &request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm { tool_run_id, .. } => {
                Some(tool_run_id.clone())
            }
            _ => None,
        };

        let (tool_name, tool_input, receipt) = match &request.details {
            crate::interaction::InteractionDetails::Confirm {
                tool_name,
                tool_input,
                receipt,
                ..
            } => (tool_name, tool_input, receipt.as_ref()),
            crate::interaction::InteractionDetails::ScheduledConfirm {
                tool_name,
                tool_input,
                receipt,
                nested_confirmation,
                ..
            } => match nested_confirmation {
                Some(nested) => (&nested.tool_name, &nested.input, Some(&nested.receipt)),
                None => (tool_name, tool_input, Some(receipt)),
            },
            _ => return Ok(None),
        };
        let session_id = request
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session scope requires an owning session"))?;

        let authorization_request = self
            .authorization
            .authorization_request(Some(session_id), tool_name, tool_input)
            .await
            .map_err(anyhow::Error::msg)?;
        let capability = receipt
            .map(|receipt| receipt.capability.clone())
            .unwrap_or_else(|| authorization_request.policy.capability.clone());
        let key = capability.target(target).ok_or_else(|| {
            anyhow::anyhow!("permission target is broader than the pending capability")
        })?;

        // For an allow, verify the exact receipt before making a durable trust
        // change. Denials do not execute the operation, but persist before the
        // wake so a retry cannot race past the user's session-scoped denial.
        if matches!(effect, haven_common::types::PermissionEffect::Allow) {
            let receipt = receipt.ok_or_else(|| {
                anyhow::anyhow!("confirmation is missing its authorization receipt")
            })?;
            if let Err(reason) = self
                .authorization
                .verify_receipt(&authorization_request, receipt)
                .await
            {
                // A stale approval must not leave its session paused forever.
                // Consume the request as a denial (without installing a grant)
                // before returning the validation error to the renderer.
                if let Some(tool_run_id) = &scheduled_tool_run_id {
                    self.resolve_scheduled_confirmation_locked(
                        tool_run_id,
                        step_id,
                        false,
                        ScheduledConfirmDeadlineDecision::Expire,
                    )
                    .await?;
                } else {
                    let crate::interaction::InteractionOwner::Session { session_id } =
                        expected_owner
                    else {
                        return Ok(None);
                    };
                    self.resolve_confirmation_locked(session_id, step_id, false, true)
                        .await?;
                }
                anyhow::bail!("confirmation request can no longer be executed: {reason}");
            }
        }
        if let Some(tool_run_id) = scheduled_tool_run_id.as_deref()
            && !self
                .tool_runs
                .claim_scheduled_execution(tool_run_id, step_id.as_str())
                .await?
        {
            // Consume and dismiss the request as stale after cancellation won;
            // this path deliberately does not persist the requested session
            // grant.
            self.resolve_scheduled_confirmation_locked(
                tool_run_id,
                step_id,
                true,
                ScheduledConfirmDeadlineDecision::CheckAtResolution,
            )
            .await?;
            return Ok(None);
        }
        if let Err(error) = self
            .grant_session_permission(session_id, key, target, effect)
            .await
        {
            if let Some(tool_run_id) = scheduled_tool_run_id.as_deref()
                && let Err(release_error) = self
                    .tool_runs
                    .release_scheduled_execution_claim(tool_run_id, step_id.as_str())
                    .await
            {
                tracing::warn!(
                tool_run_id = %tool_run_id,
                    request_id = %step_id,
                    error = %haven_common::error::sanitize_error_text(&release_error.to_string()),
                    "failed to release scheduled confirmation claim after grant persistence failed"
                );
            }
            return Err(error);
        }

        let confirmed = matches!(effect, haven_common::types::PermissionEffect::Allow);
        if let Some(tool_run_id) = scheduled_tool_run_id {
            self.resolve_scheduled_confirmation_locked(
                &tool_run_id,
                step_id,
                confirmed,
                ScheduledConfirmDeadlineDecision::AcceptedBeforeDeadline,
            )
            .await
        } else {
            let crate::interaction::InteractionOwner::Session { session_id } = expected_owner
            else {
                return Ok(None);
            };
            self.resolve_confirmation_locked(session_id, step_id, confirmed, false)
                .await
        }
    }

    pub async fn resolve_confirmation_for_owner(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        self.resolve_confirmation_for_owner_locked(owner, request_id, confirmed, false)
            .await
    }

    /// Expire one confirmation through its declared owner. Unlike the user
    /// resolve command, expiry is initiated only by the owner timer.
    pub async fn expire_confirmation_for_owner(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        self.resolve_confirmation_for_owner_locked(owner, request_id, false, true)
            .await
    }

    async fn resolve_confirmation_for_owner_locked(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
        confirmed: bool,
        expire: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        if self.owner_session_is_closing(owner, request_id).await {
            if expire {
                anyhow::bail!(
                    "session is closing; retry confirmation expiry after lifecycle cleanup"
                );
            }
            return Ok(None);
        }
        match owner {
            crate::interaction::InteractionOwner::ScheduledToolRun { tool_run_id } => {
                self.resolve_scheduled_confirmation_locked(
                    tool_run_id,
                    request_id,
                    confirmed,
                    if expire {
                        ScheduledConfirmDeadlineDecision::Expire
                    } else {
                        ScheduledConfirmDeadlineDecision::CheckAtResolution
                    },
                )
                .await
            }
            crate::interaction::InteractionOwner::AppCommand => Ok(None),
            crate::interaction::InteractionOwner::Session { session_id } => {
                let Some(request) = self
                    .pending_session_confirmation_request(session_id, request_id)
                    .await
                else {
                    return Ok(None);
                };
                if !interaction_owner_matches_request(owner, &request) {
                    return Ok(None);
                }
                let deadline_elapsed = request
                    .pending_permission_deadline()
                    .map(|deadline| deadline <= chrono::Utc::now())
                    .unwrap_or(true);
                self.resolve_confirmation_locked(
                    session_id,
                    request_id,
                    confirmed,
                    expire || deadline_elapsed,
                )
                .await
            }
        }
    }

    pub async fn expire_scheduled_confirmation(
        self: &Arc<Self>,
        tool_run_id: &str,
        request_id: &haven_common::types::ConfirmId,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        let owner = crate::interaction::InteractionOwner::ScheduledToolRun {
            tool_run_id: tool_run_id.to_string(),
        };
        if self.owner_session_is_closing(&owner, request_id).await {
            anyhow::bail!(
                "session is closing; retry scheduled confirmation expiry after lifecycle cleanup"
            );
        }
        self.resolve_scheduled_confirmation_locked(
            tool_run_id,
            request_id,
            false,
            ScheduledConfirmDeadlineDecision::Expire,
        )
        .await
    }

    async fn owner_session_is_closing(
        &self,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
    ) -> bool {
        match owner {
            crate::interaction::InteractionOwner::Session { session_id } => {
                self.is_session_closing(session_id)
            }
            crate::interaction::InteractionOwner::ScheduledToolRun { tool_run_id } => self
                .scheduled_confirmation_request(tool_run_id, request_id)
                .await
                .is_some_and(|request| self.request_session_is_closing(owner, &request)),
            crate::interaction::InteractionOwner::AppCommand => false,
        }
    }

    fn request_session_is_closing(
        &self,
        owner: &crate::interaction::InteractionOwner,
        request: &crate::interaction::InteractionRequest,
    ) -> bool {
        let Some(session_id) = request.session_id.as_deref() else {
            return false;
        };
        match owner {
            crate::interaction::InteractionOwner::Session {
                session_id: owner_id,
            } => owner_id == session_id && self.is_session_closing(owner_id),
            crate::interaction::InteractionOwner::ScheduledToolRun { .. } => {
                self.is_session_closing(session_id)
            }
            crate::interaction::InteractionOwner::AppCommand => false,
        }
    }

    async fn resolve_confirmation_locked(
        self: &Arc<Self>,
        session_id: &str,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
        expired: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        // Phase 5 / E3: pause-based confirm — record decision and wake when
        // every pending gated tool in the batch has been answered.
        if let Some(request) = self
            .resolve_interaction(
                session_id,
                step_id.as_str(),
                Value::Bool(confirmed),
                expired,
            )
            .await?
        {
            let status = request.status;
            let (session_id, tool_name, tool_input) = match request.details {
                crate::interaction::InteractionDetails::Confirm {
                    tool_name,
                    tool_input,
                    ..
                } => (request.session_id, tool_name, tool_input),
                _ => (None, String::new(), Value::Null),
            };
            return Ok(Some(crate::session::ConfirmResolution {
                session_id,
                tool_name,
                tool_input,
                status,
            }));
        }
        Ok(None)
    }

    async fn resolve_scheduled_confirmation_locked(
        self: &Arc<Self>,
        tool_run_id: &str,
        request_id: &haven_common::types::ConfirmId,
        confirmed: bool,
        deadline_decision: ScheduledConfirmDeadlineDecision,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let Some(pending_request) = self
            .scheduled_confirmation_request(tool_run_id, request_id)
            .await
        else {
            return Ok(None);
        };
        let (request_tool_run_id, tool_name, tool_input) = match &pending_request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm {
                tool_run_id,
                tool_name,
                tool_input,
                ..
            } => (tool_run_id.clone(), tool_name.clone(), tool_input.clone()),
            _ => return Ok(None),
        };
        if request_tool_run_id != tool_run_id {
            return Ok(None);
        }
        let expired = scheduled_confirmation_is_expired(&pending_request, deadline_decision);

        // An approval must win a durable claim against cancellation before it
        // can consume the request, persist a session grant, or start a tool.
        // If cancel already committed, dismiss the stale prompt without any
        // authorization or execution side effect.
        if confirmed
            && !expired
            && !self
                .tool_runs
                .claim_scheduled_execution(tool_run_id, request_id.as_str())
                .await?
        {
            let cancelled = {
                let mut scheduled_confirms = self.scheduled_confirms.lock().await;
                let Some(request) = scheduled_confirms.get(tool_run_id).filter(|request| {
                    scheduled_request_matches_route(request, tool_run_id, request_id)
                }) else {
                    return Ok(None);
                };
                let mut request = request.clone();
                scheduled_confirms.remove(tool_run_id);
                request.cancel();
                request
            };
            self.emit_event(
                crate::session::SessionSupervisorEvent::InteractionRequested {
                    envelope: Box::new(crate::interaction::InteractionEnvelope {
                        owner: crate::interaction::InteractionOwner::ScheduledToolRun {
                            tool_run_id: tool_run_id.to_string(),
                        },
                        request: cancelled,
                    }),
                },
            );
            return Ok(None);
        }

        let Some(mut request) = ({
            let mut scheduled_confirms = self.scheduled_confirms.lock().await;
            if !scheduled_confirms.get(tool_run_id).is_some_and(|request| {
                scheduled_request_matches_route(request, tool_run_id, request_id)
            }) {
                return Ok(None);
            }
            scheduled_confirms.remove(tool_run_id)
        }) else {
            if confirmed {
                self.tool_runs
                    .release_scheduled_execution_claim(tool_run_id, request_id.as_str())
                    .await?;
            }
            return Ok(None);
        };
        let session_id = request.session_id.clone();
        if expired {
            let _ = request.expire();
        } else {
            let _ = request.resolve(Value::Bool(confirmed));
        }
        self.emit_event(
            crate::session::SessionSupervisorEvent::InteractionRequested {
                envelope: Box::new(crate::interaction::InteractionEnvelope {
                    owner: crate::interaction::InteractionOwner::ScheduledToolRun {
                        tool_run_id: tool_run_id.to_string(),
                    },
                    request: request.clone(),
                }),
            },
        );
        let resolution = crate::session::ConfirmResolution {
            session_id,
            tool_name,
            tool_input,
            status: request.status,
        };
        let executor = Arc::clone(self);
        tokio::spawn(async move {
            executor
                .finish_scheduled_confirm(request, confirmed, expired)
                .await;
        });
        Ok(Some(resolution))
    }

    pub(super) async fn finish_scheduled_confirm(
        self: &Arc<Self>,
        request: crate::interaction::InteractionRequest,
        confirmed: bool,
        expired: bool,
    ) {
        let request_id = request.request_id.clone();
        let (tool_run_id, session_id, tool_name, tool_args, receipt, nested_confirmation, title) =
            match request.details {
                crate::interaction::InteractionDetails::ScheduledConfirm {
                    tool_run_id,
                    tool_name,
                    tool_input,
                    receipt,
                    nested_confirmation,
                    title,
                } => (
                    tool_run_id,
                    request.session_id.clone(),
                    tool_name,
                    tool_input,
                    receipt,
                    nested_confirmation,
                    title,
                ),
                _ => return,
            };
        if confirmed
            && let Some(live_session_id) = session_id.as_deref()
            && !self.session_is_live(live_session_id).await
        {
            let body = format!(
                "Scheduled tool '{}' was NOT executed: its session is no longer active.",
                tool_name
            );
            if self
                .persist_scheduled_terminal_state(&tool_run_id, Err("关联会话已结束或不存在"))
                .await
            {
                self.emit_event(SessionSupervisorEvent::ScheduledConfirmOutcome {
                    tool_run_id,
                    session_id,
                    title,
                    body,
                });
            }
            return;
        }
        if !confirmed {
            let reason = if expired {
                "confirmation timed out"
            } else {
                "confirmation was declined"
            };
            if self
                .persist_scheduled_terminal_state(
                    &tool_run_id,
                    Err(if expired {
                        "确认超时"
                    } else {
                        "确认被拒绝"
                    }),
                )
                .await
            {
                self.emit_event(SessionSupervisorEvent::ScheduledConfirmOutcome {
                    tool_run_id,
                    session_id,
                    title,
                    body: format!("Scheduled tool '{tool_name}' was NOT executed: {reason}."),
                });
            }
            return;
        }
        let scheduled_tool_input = tool_args.clone();
        let outcome = async {
            if tool_name == haven_tools::WORKFLOW_RUN_TOOL_NAME {
                let execution_cancellation = self
                    .tool_runs
                    .workflow_run_cancellation_token(&tool_run_id)
                    .await
                    .unwrap_or_else(CancellationToken::new);
                tokio::select! {
                    _ = execution_cancellation.cancelled() => {
                        Err(anyhow::anyhow!("定时 Workflow 已取消"))
                    }
                    outcome = self.execute_gated_with_nested_confirmation(
                        super::execution::GatedToolExecutionRequest {
                            session_id: session_id.as_deref(),
                            tool_name: &tool_name,
                            input: tool_args,
                            cancel: execution_cancellation.clone(),
                            receipt: nested_confirmation.is_none().then_some(receipt),
                            step_id: None,
                            nested_confirmation,
                            tool_run_id: Some(&tool_run_id),
                        }
                    ) => outcome,
                }
            } else {
                self.execute_gated(
                    session_id.as_deref(),
                    &tool_name,
                    tool_args,
                    CancellationToken::new(),
                    Some(receipt),
                    None,
                )
                .await
            }
        }
        .instrument(tracing::info_span!(
            "scheduled_tool_run_confirmation_execution",
            tool_run_id = %tool_run_id,
            session_id = ?session_id
        ))
        .await;
        if let Ok(execution) = &outcome
            && let Some(nested) = execution.result.signals.nested_confirmation.clone()
        {
            if let Err(error) = self
                .tool_runs
                .release_scheduled_execution_claim(&tool_run_id, &request_id)
                .await
            {
                tracing::warn!(
                    tool_run_id,
                    request_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to release previous scheduled claim before nested confirmation"
                );
                let reason = "无法续接 Workflow 确认";
                if self
                    .persist_scheduled_terminal_state(&tool_run_id, Err(reason))
                    .await
                {
                    self.emit_event(SessionSupervisorEvent::ScheduledConfirmOutcome {
                        tool_run_id,
                        session_id,
                        title,
                        body: format!("Scheduled tool '{tool_name}' failed: {reason}."),
                    });
                }
                return;
            }
            if self
                .request_scheduled_nested_confirm(
                    &tool_run_id,
                    session_id.as_deref(),
                    &tool_name,
                    scheduled_tool_input,
                    nested,
                    &title,
                )
                .await
                .is_some()
            {
                return;
            }
            let reason = "无法创建 Workflow 子调用确认";
            if self
                .persist_scheduled_terminal_state(&tool_run_id, Err(reason))
                .await
            {
                self.emit_event(SessionSupervisorEvent::ScheduledConfirmOutcome {
                    tool_run_id,
                    session_id,
                    title,
                    body: format!("Scheduled tool '{tool_name}' failed: {reason}."),
                });
            }
            return;
        }
        let summary_chars = self
            .notification_summary_chars
            .load(std::sync::atomic::Ordering::Relaxed);
        let succeeded = outcome
            .as_ref()
            .is_ok_and(crate::session::scheduled_tool_execution_succeeded);
        let mut result_summary = None;
        let body = match outcome {
            Ok(g) if succeeded => {
                let summary = crate::truncate_notification(&g.result.summary_text(), summary_chars);
                result_summary = Some(summary.clone());
                format!("schedule tool '{tool_name}':\n{summary}")
            }
            Ok(g) => {
                let summary = crate::truncate_notification(&g.result.summary_text(), summary_chars);
                format!("schedule tool '{tool_name}' failed: {summary}")
            }
            Err(e) => format!("schedule tool '{tool_name}' failed: {e}"),
        };
        let persisted = if succeeded {
            self.persist_scheduled_terminal_state(&tool_run_id, Ok(result_summary.as_deref()))
                .await
        } else {
            let failure_summary = crate::truncate_notification(&body, summary_chars);
            self.persist_scheduled_terminal_state(&tool_run_id, Err(&failure_summary))
                .await
        };
        if persisted {
            self.emit_event(SessionSupervisorEvent::ScheduledConfirmOutcome {
                tool_run_id,
                session_id,
                title,
                body,
            });
        }
    }

    async fn persist_scheduled_terminal_state(
        &self,
        tool_run_id: &str,
        outcome: Result<Option<&str>, &str>,
    ) -> bool {
        let transition = match outcome {
            Ok(Some(summary)) => {
                self.tool_runs
                    .complete_scheduled_with_result(tool_run_id, summary)
                    .await
            }
            Ok(None) => self.tool_runs.complete_scheduled(tool_run_id).await,
            Err(reason) => self.tool_runs.fail_scheduled(tool_run_id, reason).await,
        };
        match transition {
            Ok(true) => true,
            Ok(false) => {
                tracing::debug!(
                    tool_run_id,
                    "scheduled ToolRun terminal transition was already won"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    tool_run_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to persist scheduled ToolRun terminal state"
                );
                false
            }
        }
    }

    /// Check authorization using the turn's immutable catalog snapshot. The
    /// authorization engine remains live so grants, denies and policy
    /// revisions are never cached across the safety boundary.
    pub async fn check_tool_gate_with_catalog(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        catalog: &haven_tools::ToolCatalogSnapshot,
    ) -> haven_tools::AuthorizationDecision {
        let authorization_request = match self
            .authorization
            .authorization_request_from_catalog(catalog, Some(session_id), tool_name, input)
            .await
        {
            Ok(request) => request,
            Err(reason) => {
                return haven_tools::AuthorizationDecision::Blocked {
                    reason: format!("operation policy could not be resolved: {reason}"),
                    reason_code: haven_tools::AuthorizationReasonCode::DisabledOperation,
                };
            }
        };
        self.authorization.authorize(&authorization_request).await
    }

    /// Resume decision for a gated tool after a confirm pause (Phase 5 / E3).
    /// Returns the recorded decision and its exact authorization receipt.
    /// Siblings that were carried behind a confirm barrier have no receipt and
    /// are therefore rechecked normally on resume.
    pub async fn confirm_decision_for(
        &self,
        session_id: &str,
        step_id: &str,
        tool_index: u32,
        tool_call_id: Option<&str>,
    ) -> Option<super::RecordedConfirmationDecision> {
        self.interaction_requests(session_id)
            .await
            .into_iter()
            .filter(|request| request.kind == crate::interaction::InteractionKind::Confirm)
            .find_map(|request| {
                let decision = request.decision();
                match request.details {
                    crate::interaction::InteractionDetails::Confirm {
                        step_id: request_step_id,
                        tool_index: request_tool_index,
                        tool_call_id: request_tool_call_id,
                        receipt,
                        ..
                    } if request_step_id == step_id
                        && request_tool_index == tool_index
                        && request_tool_call_id == tool_call_id.unwrap_or_default() =>
                    {
                        decision.map(|approved| super::RecordedConfirmationDecision {
                            approved,
                            receipt,
                        })
                    }
                    _ => None,
                }
            })
    }
}
