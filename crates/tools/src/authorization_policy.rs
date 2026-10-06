//! Builds the canonical policy input presented to the live authorization
//! engine. The engine remains the authority for allow, deny, and confirmation
//! decisions; tool execution remains in `AuthorizedExecutor`.

use crate::ToolCatalogSnapshot;
use crate::tool_contract::{
    ConfirmationRequirement, DataSensitivity, NetworkAccess, OperationEffect, OperationIdempotency,
    OperationPolicy, ToolBox, ToolConcurrency, ToolOperationScope,
};
use crate::{AuthorizationRequest, RiskLevel, ToolsFacade};
use haven_common::types::permission_key;
use serde_json::Value;

/// Resolves the operation contract and canonical input used by authorization.
/// It does not authorize or execute the operation.
pub(crate) struct ToolAuthorizationRequestResolver<'a> {
    tools: &'a ToolsFacade,
}

impl<'a> ToolAuthorizationRequestResolver<'a> {
    fn new(tools: &'a ToolsFacade) -> Self {
        Self { tools }
    }

    pub(crate) fn operation_policy_for(
        tool: Option<&ToolBox>,
        tool_name: &str,
        input: &Value,
    ) -> OperationPolicy {
        tool.map(|tool| tool.operation_policy(input))
            .unwrap_or_else(|| unknown_operation_policy(tool_name, input))
    }

    fn authorization_input_for(tool: Option<&ToolBox>, input: &Value) -> Value {
        tool.map(|tool| tool.authorization_input(input))
            .unwrap_or_else(|| input.clone())
    }

    async fn operation_policy(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> OperationPolicy {
        let tool = self.tools.get_tool_for_session(session_id, tool_name).await;
        Self::operation_policy_for(tool.as_ref(), tool_name, input)
    }

    async fn authorization_input(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Value {
        let tool = self.tools.get_tool_for_session(session_id, tool_name).await;
        Self::authorization_input_for(tool.as_ref(), input)
    }

    async fn risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> RiskLevel {
        let reported = self
            .operation_policy(session_id, tool_name, input)
            .await
            .risk_level;
        self.tools
            .coordinator
            .core
            .authorization
            .effective_risk(tool_name, reported)
            .await
    }

    async fn authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        // Keep the existing live lookup order: the operation contract and
        // canonical input are each resolved through the session-first lookup.
        let policy = self.operation_policy(session_id, tool_name, input).await;
        let authorization_input = self.authorization_input(session_id, tool_name, input).await;
        AuthorizationRequest::new(session_id, tool_name, authorization_input, policy)
    }

    fn authorization_request_from_snapshot(
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        let tool = catalog.get(tool_name);
        let authorization_input = Self::authorization_input_for(tool, input);
        let policy = Self::operation_policy_for(tool, tool_name, input);
        AuthorizationRequest::new(session_id, tool_name, authorization_input, policy)
    }
}

fn unknown_operation_policy(tool_name: &str, input: &Value) -> OperationPolicy {
    OperationPolicy {
        risk_level: RiskLevel::Safe,
        capability: permission_key(tool_name, input).into(),
        confirmation: ConfirmationRequirement::None,
        idempotency: OperationIdempotency::Unknown,
        scope: ToolOperationScope::Session,
        concurrency: ToolConcurrency::Exclusive,
        effect: OperationEffect::ExternalEffect,
        data_sensitivity: DataSensitivity::None,
        network_access: NetworkAccess::None,
    }
}

impl ToolsFacade {
    pub async fn resolve_risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> RiskLevel {
        ToolAuthorizationRequestResolver::new(self)
            .risk_level(session_id, tool_name, input)
            .await
    }

    /// Return the canonical intrinsic operation policy. Security overrides
    /// are applied by `AuthorizationEngine` when it evaluates a request.
    pub async fn resolve_operation_policy(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> OperationPolicy {
        ToolAuthorizationRequestResolver::new(self)
            .operation_policy(session_id, tool_name, input)
            .await
    }

    /// Build the typed request evaluated by the live authorization engine.
    pub async fn resolve_authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        ToolAuthorizationRequestResolver::new(self)
            .authorization_request(session_id, tool_name, input)
            .await
    }

    /// Build a request from the turn's immutable tool lookup view. The live
    /// authorization engine still evaluates current grants and policy.
    pub fn resolve_authorization_request_from_snapshot(
        &self,
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        ToolAuthorizationRequestResolver::authorization_request_from_snapshot(
            catalog, session_id, tool_name, input,
        )
    }
}
