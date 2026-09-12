use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{
    OperationIdempotency, Tool, ToolBox, ToolConcurrency, ToolExecutionOutcome, ToolOperationScope,
    ToolRegistration, ToolResult, ToolSignals,
};

/// Declarative contract for a model-facing operation view. The grouped tool
/// remains the execution implementation, while this record is the one source
/// for the view's model schema and runtime policy metadata.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct OperationViewContract {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) fixed: (&'static str, Value),
    pub(crate) schema: Value,
    pub(crate) risk_level: RiskLevel,
    pub(crate) risk_rule: Option<OperationViewRiskRule>,
    pub(crate) idempotency: OperationIdempotency,
    pub(crate) scope: ToolOperationScope,
    pub(crate) concurrency: ToolConcurrency,
    pub(crate) permission_key: &'static str,
    pub(crate) renderer: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) prompt: &'static str,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum OperationViewRiskRule {
    ContentSearchMedium,
}

/// A narrow provider-facing view over a grouped tool.
///
/// The grouped implementation remains the single execution and policy
/// source. This adapter only fixes the operation discriminator and publishes
/// the smaller schema that the model needs for that operation. Native and
/// model-facing callers therefore continue to share the same implementation.
pub(crate) struct OperationViewTool {
    inner: ToolBox,
    contract: OperationViewContract,
    fixed: Map<String, Value>,
}

impl OperationViewTool {
    pub(crate) fn new(inner: ToolBox, contract: OperationViewContract) -> Arc<Self> {
        let fixed = Map::from_iter([(contract.fixed.0.to_string(), contract.fixed.1.clone())]);
        Arc::new(Self {
            inner,
            contract,
            fixed,
        })
    }

    fn routed_input(&self, input: &Value) -> Value {
        let mut object = match input {
            Value::Object(object) => object.clone(),
            _ => Map::new(),
        };
        for (key, value) in &self.fixed {
            object.insert(key.clone(), value.clone());
        }
        Value::Object(object)
    }
}

#[async_trait]
impl Tool for OperationViewTool {
    fn name(&self) -> String {
        self.contract.name.into()
    }

    fn description(&self) -> String {
        self.contract.description.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match self.contract.risk_rule {
            Some(OperationViewRiskRule::ContentSearchMedium)
                if input.get("mode").and_then(Value::as_str) == Some("content") =>
            {
                RiskLevel::Medium
            }
            _ => self.contract.risk_level,
        }
    }

    fn idempotency(&self, _input: &Value) -> OperationIdempotency {
        self.contract.idempotency
    }

    fn operation_scope(&self, _input: &Value) -> ToolOperationScope {
        self.contract.scope
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        self.inner.timeout_outcome()
    }

    fn default_max_retries(&self) -> u32 {
        self.inner.default_max_retries()
    }

    fn default_retry_backoff_secs(&self) -> u64 {
        self.inner.default_retry_backoff_secs()
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        self.inner.execute(self.routed_input(&input), cancel).await
    }

    fn input_schema(&self) -> Value {
        self.contract.schema.clone()
    }

    fn concurrency(&self, _input: &Value) -> ToolConcurrency {
        self.contract.concurrency.clone()
    }

    fn default_timeout_secs(&self) -> u64 {
        self.inner.default_timeout_secs()
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        self.inner.timeout_secs_for(&self.routed_input(input))
    }

    fn requires_session_id(&self) -> bool {
        self.inner.requires_session_id()
    }

    fn supports_live_output(&self) -> bool {
        self.inner.supports_live_output()
    }

    fn signals(&self, output: &Value) -> ToolSignals {
        self.inner.signals(output)
    }

    fn registrations(&self, output: &Value) -> Vec<ToolRegistration> {
        self.inner.registrations(output)
    }

    fn authorization_input(&self, input: &Value) -> Value {
        self.routed_input(input)
    }
}
