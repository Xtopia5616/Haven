//! One interaction port for permissions discovered while executing a capability.
//! The application owns presentation and user decisions; Tools owns validation.

use super::{
    AuthorizationDecision, AuthorizationEngine, AuthorizationExecutionProof, AuthorizationRequest,
    ConfirmationReceipt,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// User interaction adapter. Returning a receipt is only valid after its exact
/// request has been approved by the canonical interaction owner.
#[async_trait::async_trait]
pub trait PermissionInteractionPort: Send + Sync {
    async fn request_permission(
        &self,
        request: AuthorizationRequest,
        receipt: ConfirmationReceipt,
        cancel: CancellationToken,
    ) -> Result<ConfirmationReceipt, String>;
}

#[cfg(test)]
pub(crate) struct ApprovingPermissionInteraction;

#[cfg(test)]
#[async_trait::async_trait]
impl PermissionInteractionPort for ApprovingPermissionInteraction {
    async fn request_permission(
        &self,
        _request: AuthorizationRequest,
        receipt: ConfirmationReceipt,
        _cancel: CancellationToken,
    ) -> Result<ConfirmationReceipt, String> {
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::{PermissionEffect, PermissionScope};

    #[tokio::test]
    async fn unknown_private_and_loopback_destinations_request_the_exact_origin() {
        let engine = AuthorizationEngine::new();
        engine
            .set_permission_interaction(Arc::new(ApprovingPermissionInteraction))
            .await;
        for url in [
            "http://127.0.0.1:8123/",
            "http://10.1.2.3:8123/",
            "http://192.168.1.2:8123/",
        ] {
            let destination = engine
                .request_network_destination(None, "http", url, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(destination.url.as_str(), url);
        }
    }

    #[tokio::test]
    async fn decline_and_cancellation_never_produce_execution_authority() {
        struct Decline;
        #[async_trait::async_trait]
        impl PermissionInteractionPort for Decline {
            async fn request_permission(
                &self,
                _: AuthorizationRequest,
                _: ConfirmationReceipt,
                _: CancellationToken,
            ) -> Result<ConfirmationReceipt, String> {
                Err("declined".into())
            }
        }
        let engine = AuthorizationEngine::new();
        engine.set_permission_interaction(Arc::new(Decline)).await;
        assert!(
            engine
                .request_network_destination(
                    None,
                    "http",
                    "http://127.0.0.1:8123/",
                    CancellationToken::new()
                )
                .await
                .unwrap_err()
                .contains("declined")
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            engine
                .request_network_destination(None, "http", "http://127.0.0.1:8123/", cancel)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn approved_critical_operations_execute_but_each_invocation_still_asks() {
        let engine = AuthorizationEngine::new();
        let mut request = AuthorizationEngine::runtime_permission_request(
            None,
            "system.power.shutdown",
            serde_json::json!({}),
            crate::NetworkAccess::None,
            Vec::new(),
        )
        .unwrap();
        request.policy.risk_level = haven_common::types::RiskLevel::Critical;
        request.policy.confirmation = crate::ConfirmationRequirement::Required;
        let first = match engine.authorize(&request).await {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("critical operation must ask: {decision:?}"),
        };
        let proof = engine
            .confirm_for_execution(&request, &first)
            .await
            .unwrap();
        engine
            .verify_execution_proof(&request, &proof)
            .await
            .unwrap();
        let second = match engine.authorize(&request).await {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("a new invocation still requires confirmation: {decision:?}"),
        };
        assert_ne!(first.confirmation_id, second.confirmation_id);
    }

    #[tokio::test]
    async fn saved_resource_authority_overrides_default_readonly_and_can_be_revoked() {
        let engine = AuthorizationEngine::new();
        let root = tempfile::tempdir().unwrap();
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "shell",
            serde_json::json!({"command":"git status"}),
            crate::NetworkAccess::Opaque,
            vec![super::super::DeclaredResource::LocalPath {
                path: root.path().to_path_buf(),
                access: haven_common::types::LocalPathAccess::Write,
            }],
        )
        .unwrap();
        {
            let mut config = engine.config.write().await;
            config.sandbox_mode = haven_common::types::SandboxMode::ReadOnly;
        }
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
        let scopes = engine
            .resource_scope_for_request(
                &request,
                haven_common::types::PermissionResourceTarget::Exact,
            )
            .await
            .unwrap();
        engine
            .grant(
                None,
                request.capability().clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scopes,
            )
            .await;
        let proof = engine
            .request_execution_permission(&request, CancellationToken::new())
            .await
            .unwrap();
        assert!(proof.binding.resource_boundary_extension);
        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_ok()
        );
        engine.revoke_permanent(request.capability().as_str()).await;
        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn saved_refusal_invalidates_existing_proof_and_signals_running_processes() {
        let engine = AuthorizationEngine::new();
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "shell",
            serde_json::json!({"command":"echo test"}),
            crate::NetworkAccess::Opaque,
            Vec::new(),
        )
        .unwrap();
        engine
            .set_permission_interaction(Arc::new(ApprovingPermissionInteraction))
            .await;
        let proof = engine
            .request_execution_permission(&request, CancellationToken::new())
            .await
            .unwrap();
        let changes = engine.process_revocations.subscribe();
        engine
            .grant(
                None,
                request.capability().clone(),
                PermissionEffect::Deny,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
        assert!(changes.has_changed().unwrap());
        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_err()
        );
    }
}

impl AuthorizationEngine {
    pub(crate) async fn runtime_process_permit(
        self: &Arc<Self>,
        session_id: Option<&str>,
        tool_name: &str,
        input: serde_json::Value,
        resources: Vec<super::DeclaredResource>,
        cancel: CancellationToken,
    ) -> Result<super::ExecutionPolicyPermit, String> {
        let request = Self::runtime_permission_request(
            session_id,
            tool_name,
            input,
            crate::NetworkAccess::Opaque,
            resources,
        )?;
        let proof = self.request_execution_permission(&request, cancel).await?;
        self.execution_context_for_proof(&request, &proof)
            .await?
            .clone_execution_authorization()
            .ok_or_else(|| "process permission has no execution authority".to_owned())?
            .acquire()
            .await
    }
    pub(crate) async fn request_network_destination(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        raw_url: &str,
        cancel: CancellationToken,
    ) -> Result<haven_common::network::ResolvedHttpDestination, String> {
        let url = url::Url::parse(raw_url).map_err(|_| "invalid network destination".to_owned())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("network destination must be HTTP(S) without embedded credentials".into());
        }
        let host = url
            .host_str()
            .ok_or_else(|| "network destination has no host".to_owned())?
            .to_owned();
        // Resolve metadata first, then ask once for the origin and any
        // additional private address. No target connection is opened here.
        let destination = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("network permission cancelled".into()),
            result = haven_common::network::resolve_authorized_http_destination(raw_url) => result.map_err(|error| error.to_string())?,
        };
        let mut resources = vec![super::DeclaredResource::NetworkDestination {
            scheme: url.scheme().into(),
            host: host.clone(),
            port: url.port_or_known_default(),
        }];
        if let Some(address) = destination.pinned_address
            && haven_common::network::http_host_is_local_or_special(&address.ip().to_string())
            && !haven_common::network::http_host_is_local_or_special(&host)
        {
            resources.push(super::DeclaredResource::NetworkDestination {
                scheme: url.scheme().into(),
                host: address.ip().to_string(),
                port: Some(address.port()),
            });
        }
        let request = Self::runtime_permission_request(
            session_id,
            tool_name,
            serde_json::json!({"destination":url.origin().ascii_serialization(), "resolved_address":destination.pinned_address.map(|address| address.to_string())}),
            crate::NetworkAccess::Public,
            resources,
        )?;
        let proof = self.request_execution_permission(&request, cancel).await?;
        self.verify_execution_proof(&request, &proof).await?;
        Ok(destination)
    }
    pub(crate) fn runtime_permission_request(
        session_id: Option<&str>,
        tool_name: &str,
        input: serde_json::Value,
        network_access: crate::NetworkAccess,
        resources: Vec<super::DeclaredResource>,
    ) -> Result<AuthorizationRequest, String> {
        let mut policy = crate::OperationPolicy::external(
            haven_common::types::CapabilityScope::try_new(format!("{tool_name}.permission"))?,
            haven_common::types::RiskLevel::High,
        );
        policy.confirmation = crate::ConfirmationRequirement::SecurityPolicy;
        policy.network_access = network_access;
        if resources.iter().any(|resource| {
            matches!(
                resource,
                super::DeclaredResource::LocalPath {
                    access: haven_common::types::LocalPathAccess::Write,
                    ..
                }
            )
        }) {
            policy.effect = crate::OperationEffect::WorkspaceWrite;
        }
        policy.execution_boundary = if network_access == crate::NetworkAccess::Opaque {
            crate::ExecutionBoundary::SandboxedHost
        } else {
            crate::ExecutionBoundary::InProcess
        };
        Ok(
            AuthorizationRequest::new(session_id, tool_name, input, policy)
                .with_resources(resources),
        )
    }
    pub(crate) async fn request_execution_permission(
        &self,
        request: &AuthorizationRequest,
        cancel: CancellationToken,
    ) -> Result<AuthorizationExecutionProof, String> {
        if cancel.is_cancelled() {
            return Err("permission request cancelled".into());
        }
        let check = self.authorize_for_execution(request).await;
        match check.decision {
            AuthorizationDecision::AutoApproved => check
                .execution_proof
                .ok_or_else(|| "approved permission has no execution proof".into()),
            AuthorizationDecision::Blocked { reason, .. } => Err(reason),
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
                let _slot = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("permission request cancelled".into()),
                    slot = self.permission_slots.acquire() => slot.map_err(|_| "permission queue is closed".to_owned())?,
                };
                let interaction_cancel = cancel.child_token();
                let _cancel_guard = interaction_cancel.clone().drop_guard();
                let remaining = receipt
                    .expires_at
                    .saturating_sub(super::engine::confirmation_now());
                let interaction = self.permission_interaction.read().await.clone()
                    .ok_or_else(|| "permission requires user interaction, but no interaction adapter is connected".to_owned())?;
                let approved = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("permission request cancelled".into()),
                    result = tokio::time::timeout(std::time::Duration::from_secs(remaining), interaction.request_permission(request.clone(), receipt.clone(), interaction_cancel)) =>
                        result.map_err(|_| "permission request expired".to_owned())??,
                };
                if approved != receipt {
                    return Err("permission interaction returned a different receipt".into());
                }
                self.confirm_for_execution(request, &approved).await
            }
        }
    }

    pub(crate) async fn set_permission_interaction(
        &self,
        interaction: Arc<dyn PermissionInteractionPort>,
    ) {
        *self.permission_interaction.write().await = Some(interaction);
    }
}
