use super::engine::{canonical_authorization_hash, execution_proof_expiry};
use crate::OperationPolicy;
use haven_common::config::{SecurityConfig, StoredPermission};
use haven_common::types::{
    CapabilityScope, NetworkPolicy, PermissionEffect, PermissionMode, PermissionScope, RiskLevel,
    SandboxMode,
};
use serde_json::Value;
use std::path::PathBuf;

/// A one-shot authorization proof bound to the exact request that was shown
/// to the user.  A receipt is deliberately invalidated by any policy change,
/// input change, risk increase, expiry, or Critical classification.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ConfirmationReceipt {
    pub confirmation_id: haven_common::types::ConfirmId,
    pub capability: CapabilityScope,
    pub canonical_input_hash: String,
    pub effective_risk: RiskLevel,
    pub policy_revision: u64,
    pub expires_at: u64,
}

/// Machine-readable reason for a gate result. User-facing text remains
/// backend-generated, but callers no longer need to parse that text to decide
/// whether a result came from a hard boundary, a rule, or an approval mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationReasonCode {
    DisabledOperation,
    NetworkPolicy,
    SandboxBoundary,
    PlanMode,
    PermanentDeny,
    SessionDeny,
    CriticalOperation,
    ApprovalPolicy,
    SensitiveData,
}

impl AuthorizationReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DisabledOperation => "disabled_operation",
            Self::NetworkPolicy => "network_policy",
            Self::SandboxBoundary => "sandbox_boundary",
            Self::PlanMode => "plan_mode",
            Self::PermanentDeny => "permanent_deny",
            Self::SessionDeny => "session_deny",
            Self::CriticalOperation => "critical_operation",
            Self::ApprovalPolicy => "approval_policy",
            Self::SensitiveData => "sensitive_data",
        }
    }
}

#[derive(Debug, Clone)]
pub enum AuthorizationDecision {
    AutoApproved,
    RequiresConfirmation {
        capability: CapabilityScope,
        risk_level: RiskLevel,
        receipt: ConfirmationReceipt,
        reason_code: AuthorizationReasonCode,
    },
    /// Hard deny — permanent/session denylist, disabled operation, or path sandbox.
    Blocked {
        reason: String,
        reason_code: AuthorizationReasonCode,
    },
}

/// The complete typed input to [`super::AuthorizationEngine`]. The engine owns the
/// decision; callers only provide the concrete operation contract and its
/// canonical input once.
#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    pub policy: OperationPolicy,
    /// Opaque, non-secret identity of the concrete execution target. MCP
    /// adapters use this to bind consent to one live server generation.
    pub target_binding: Option<String>,
}

/// The active process security boundaries owned by the authorization engine.
/// This narrow snapshot supports truthful, renderer-safe security status
/// without exposing mutable policy internals or workspace paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityBoundarySnapshot {
    pub sandbox_mode: SandboxMode,
    pub network_policy: NetworkPolicy,
}

#[derive(Debug)]
pub(super) struct AuthorizationExecutionBinding {
    pub(super) session_id: Option<String>,
    pub(super) tool_name: String,
    pub(super) capability: CapabilityScope,
    pub(super) canonical_input_hash: String,
    pub(super) target_binding: Option<String>,
    pub(super) policy: OperationPolicy,
    pub(super) policy_revision: u64,
    pub(super) expires_at: u64,
}

#[derive(Debug)]
pub(super) enum ExecutionAuthorizationKind {
    AutoApproved,
    Confirmed(ConfirmationReceipt),
}

/// Opaque proof carried from the Agent authorization gate to the Tools
/// execution owner. A proof is move-only, and only the authorization owner can construct it;
/// confirmation receipts cannot be wrapped into executable authority by
/// consumers. A proof authorizes one exact operation, never its child calls.
#[derive(Debug)]
pub struct AuthorizationExecutionProof {
    pub(super) binding: AuthorizationExecutionBinding,
    pub(super) authorization: ExecutionAuthorizationKind,
}

/// A policy decision plus the execution proof, if the operation can proceed
/// without a user confirmation.
#[derive(Debug)]
pub struct AuthorizationCheck {
    pub decision: AuthorizationDecision,
    pub execution_proof: Option<AuthorizationExecutionProof>,
}

impl AuthorizationCheck {
    pub(super) fn without_proof(decision: AuthorizationDecision) -> Self {
        Self {
            decision,
            execution_proof: None,
        }
    }

    pub(super) fn auto_approved(request: &AuthorizationRequest, policy_revision: u64) -> Self {
        Self {
            decision: AuthorizationDecision::AutoApproved,
            execution_proof: Some(AuthorizationExecutionProof {
                binding: AuthorizationExecutionBinding {
                    session_id: request.session_id.clone(),
                    tool_name: request.tool_name.clone(),
                    capability: request.capability().clone(),
                    canonical_input_hash: canonical_authorization_hash(request),
                    target_binding: request.target_binding.clone(),
                    policy: request.policy.clone(),
                    policy_revision,
                    expires_at: execution_proof_expiry(),
                },
                authorization: ExecutionAuthorizationKind::AutoApproved,
            }),
        }
    }
}

/// Stable capability boundary for authorization consumers outside `haven-tools`.
///
/// The policy engine is deliberately kept private to this crate. Callers can
/// request a decision or manage grants through this contract without gaining
/// access to the mutable policy implementation.
#[async_trait::async_trait]
pub trait AuthorizationPort: Send + Sync {
    async fn apply_security(&self, security: &SecurityConfig) -> Result<(), String>;

    async fn set_permission_mode(&self, mode: PermissionMode);

    async fn set_boundaries(
        &self,
        sandbox_mode: SandboxMode,
        writable_roots: Vec<PathBuf>,
        network_policy: NetworkPolicy,
    );

    async fn security_boundary_snapshot(&self) -> SecurityBoundarySnapshot;

    async fn authorize(&self, request: &AuthorizationRequest) -> AuthorizationDecision;

    async fn authorize_for_execution(&self, request: &AuthorizationRequest) -> AuthorizationCheck;

    async fn verify_receipt(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<(), String>;

    /// Turn a receipt selected by the trusted interaction owner into the
    /// opaque proof consumed by execution. Callers must invoke this only
    /// after the matching user interaction has resolved as approved.
    async fn confirm_for_execution(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<AuthorizationExecutionProof, String>;

    async fn verify_execution_proof(
        &self,
        request: &AuthorizationRequest,
        proof: &AuthorizationExecutionProof,
    ) -> Result<(), String>;

    async fn grant(
        &self,
        session_id: Option<&str>,
        capability: CapabilityScope,
        effect: PermissionEffect,
        scope: PermissionScope,
    );

    async fn list_permanent(&self) -> Vec<StoredPermission>;

    async fn revoke_permanent(&self, capability: &str) -> bool;

    async fn revoke_session_grant(&self, session_id: &str, capability: &CapabilityScope) -> bool;

    async fn clear_permanent(&self) -> usize;

    async fn clear_session_trust(&self, session_id: &str);

    async fn clear_all_trust(&self);

    async fn prompt_summary(&self) -> String;
}

impl AuthorizationRequest {
    pub fn new(
        session_id: Option<&str>,
        tool_name: impl Into<String>,
        input: Value,
        policy: OperationPolicy,
    ) -> Self {
        Self {
            session_id: session_id.map(ToOwned::to_owned),
            tool_name: tool_name.into(),
            input,
            policy,
            target_binding: None,
        }
    }

    pub fn with_target_binding(mut self, target_binding: Option<String>) -> Self {
        self.target_binding = target_binding;
        self
    }

    pub fn capability(&self) -> &CapabilityScope {
        &self.policy.capability
    }
}
