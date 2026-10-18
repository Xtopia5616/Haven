//! Builds the canonical policy input presented to the live authorization
//! engine. The engine remains the authority for allow, deny, and confirmation
//! decisions; tool execution remains in `AuthorizedExecutor`.

use crate::ToolCatalogSnapshot;
use crate::tool_contract::{
    ConfirmationRequirement, DataSensitivity, NetworkAccess, OperationEffect, OperationIdempotency,
    OperationPolicy, ToolConcurrency, ToolHandle, ToolOperationScope,
};
use crate::{AuthorizationRequest, RiskLevel, ToolsFacade};
use haven_common::types::CapabilityScope;
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
        tool: Option<&ToolHandle>,
        tool_name: &str,
        input: &Value,
    ) -> Result<OperationPolicy, String> {
        match tool {
            Some(tool) => tool.try_operation_policy(input),
            None => unknown_operation_policy(tool_name),
        }
    }

    pub(crate) fn catalog_operation_policy_for(
        tool: Option<&ToolHandle>,
        tool_name: &str,
        input: &Value,
    ) -> OperationPolicy {
        Self::operation_policy_for(tool, tool_name, input).unwrap_or_else(|_error| {
            tracing::warn!("invalid operation policy; using restrictive catalog projection");
            crate::tool_contract::restrictive_catalog_policy()
        })
    }

    fn authorization_input_for(tool: Option<&ToolHandle>, input: &Value) -> Value {
        tool.map(|tool| tool.authorization_input(input))
            .unwrap_or_else(|| input.clone())
    }

    async fn operation_policy(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<OperationPolicy, String> {
        let tool = self.tools.get_tool_for_session(session_id, tool_name).await;
        match tool {
            Some(tool) => Ok(tool.authorization_contract(input).await?.policy),
            None => Self::operation_policy_for(None, tool_name, input),
        }
    }

    async fn risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<RiskLevel, String> {
        let reported = self
            .operation_policy(session_id, tool_name, input)
            .await?
            .risk_level;
        Ok(self
            .tools
            .coordinator
            .core
            .authorization
            .effective_risk(tool_name, reported)
            .await)
    }

    async fn authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        let tool = self.tools.get_tool_for_session(session_id, tool_name).await;
        let contract = match tool.as_ref() {
            Some(tool) => tool.authorization_contract(input).await?,
            None => crate::ToolAuthorizationContract {
                policy: unknown_operation_policy(tool_name)?,
                target_binding: None,
            },
        };
        let authorization_input = Self::authorization_input_for(tool.as_ref(), input);
        Ok(
            AuthorizationRequest::new(session_id, tool_name, authorization_input, contract.policy)
                .with_target_binding(contract.target_binding),
        )
    }

    async fn codemode_subcall_authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        let tool = self
            .tools
            .get_codemode_subcall_tool_for_session(session_id, tool_name)
            .await
            .ok_or_else(|| {
                "MCP capability is not enabled for CodeMode in this session".to_owned()
            })?;
        let contract = tool.authorization_contract(input).await?;
        let authorization_input = Self::authorization_input_for(Some(&tool), input);
        Ok(
            AuthorizationRequest::new(session_id, tool_name, authorization_input, contract.policy)
                .with_target_binding(contract.target_binding),
        )
    }

    async fn authorization_request_from_snapshot(
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        let tool = catalog.get(tool_name);
        let authorization_input = Self::authorization_input_for(tool, input);
        let contract = match tool {
            Some(tool) => tool.authorization_contract(input).await?,
            None => crate::ToolAuthorizationContract {
                policy: unknown_operation_policy(tool_name)?,
                target_binding: None,
            },
        };
        Ok(
            AuthorizationRequest::new(session_id, tool_name, authorization_input, contract.policy)
                .with_target_binding(contract.target_binding),
        )
    }
}

fn unknown_operation_policy(tool_name: &str) -> Result<OperationPolicy, String> {
    Ok(OperationPolicy {
        risk_level: RiskLevel::Safe,
        capability: CapabilityScope::try_new(crate::llm_tool_name(tool_name))
            .map_err(|error| format!("invalid capability for unknown tool: {error}"))?,
        confirmation: ConfirmationRequirement::None,
        idempotency: OperationIdempotency::Unknown,
        scope: ToolOperationScope::Session,
        concurrency: ToolConcurrency::Exclusive,
        effect: OperationEffect::ExternalEffect,
        data_sensitivity: DataSensitivity::None,
        network_access: NetworkAccess::None,
        execution_boundary: crate::ExecutionBoundary::HostProcess,
    })
}

impl ToolsFacade {
    pub async fn resolve_risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<RiskLevel, String> {
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
    ) -> Result<OperationPolicy, String> {
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
    ) -> Result<AuthorizationRequest, String> {
        ToolAuthorizationRequestResolver::new(self)
            .authorization_request(session_id, tool_name, input)
            .await
    }

    /// Build an authorization request only for a currently enabled MCP tool
    /// whose exposure resolver selects CodeMode.
    pub async fn resolve_codemode_subcall_authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        ToolAuthorizationRequestResolver::new(self)
            .codemode_subcall_authorization_request(session_id, tool_name, input)
            .await
    }

    /// Build a request from the turn's immutable tool lookup view. The live
    /// authorization engine still evaluates current grants and policy.
    pub async fn resolve_authorization_request_from_snapshot(
        &self,
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        ToolAuthorizationRequestResolver::authorization_request_from_snapshot(
            catalog, session_id, tool_name, input,
        )
        .await
    }
}
