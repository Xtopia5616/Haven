use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{
    OperationIdempotency, Tool, ToolBox, ToolConcurrency, ToolExecutionOutcome, ToolOperationScope,
    ToolRegistration, ToolResult, ToolSignals,
};

/// A narrow provider-facing view over a grouped tool.
///
/// The grouped implementation remains the single execution and policy
/// source. This adapter only fixes the operation discriminator and publishes
/// the smaller schema that the model needs for that operation. Native and
/// model-facing callers therefore continue to share the same implementation.
pub(crate) struct OperationViewTool {
    inner: ToolBox,
    name: String,
    description: String,
    fixed: Map<String, Value>,
    schema: Value,
}

impl OperationViewTool {
    pub(crate) fn new(
        inner: ToolBox,
        name: impl Into<String>,
        description: impl Into<String>,
        fixed: impl IntoIterator<Item = (&'static str, Value)>,
        schema: Value,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            name: name.into(),
            description: description.into(),
            fixed: fixed
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            schema,
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
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        self.inner.risk_level(&self.routed_input(input))
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        self.inner.idempotency(&self.routed_input(input))
    }

    fn operation_scope(&self, input: &Value) -> ToolOperationScope {
        self.inner.operation_scope(&self.routed_input(input))
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
        self.schema.clone()
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        self.inner.concurrency(&self.routed_input(input))
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
}
