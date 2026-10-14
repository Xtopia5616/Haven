//! Bounded serial execution of a statically validated Workflow plan.

use crate::workflow::{ValidatedWorkflow, WorkflowLimits};
use crate::{ToolErrorClass, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Schema and explanation for one capability captured from a trusted catalog
/// snapshot. It carries no adapter, target, proof, or permission grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowCapabilityDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// One child execution result. `result.output` stays in the evaluator for
/// explicit result references; only the normal per-tool observation is
/// returned to the model.
#[derive(Debug)]
pub struct WorkflowSubcallResult {
    pub result: ToolResult,
    pub observation: String,
}

/// The only execution authority passed into the Workflow evaluator.
#[async_trait]
pub trait WorkflowSubcallPort: Send + Sync {
    async fn call(
        &self,
        capability: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<WorkflowSubcallResult, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkflowStatus {
    Completed,
    Failed,
    Partial,
    Cancelled,
}

/// Model-visible step summary. Structured outputs are retained only inside
/// the active evaluator so subsequent nodes can resolve declared references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkflowStepObservation {
    pub step_id: String,
    pub capability: String,
    pub observation: String,
}

/// Bounded summary of one Workflow attempt. It intentionally carries neither
/// raw child outputs nor resumable continuation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkflowExecutionResult {
    pub(crate) status: WorkflowStatus,
    pub(crate) completed_step_ids: Vec<String>,
    pub(crate) steps: Vec<WorkflowStepObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failed_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) call_count: usize,
    #[serde(default)]
    pub(crate) truncated: bool,
    #[serde(skip)]
    pub(crate) error_class: Option<ToolErrorClass>,
}

impl WorkflowExecutionResult {
    fn new() -> Self {
        Self {
            status: WorkflowStatus::Completed,
            completed_step_ids: Vec::new(),
            steps: Vec::new(),
            failed_step_id: None,
            error: None,
            call_count: 0,
            truncated: false,
            error_class: None,
        }
    }

    fn stop(
        &mut self,
        step_id: Option<&str>,
        message: &str,
        error_class: ToolErrorClass,
        limits: WorkflowLimits,
    ) {
        self.status = if !self.completed_step_ids.is_empty() {
            WorkflowStatus::Partial
        } else {
            WorkflowStatus::Failed
        };
        self.failed_step_id = step_id.map(str::to_owned);
        self.error = Some(message.to_owned());
        self.error_class = Some(if self.completed_step_ids.is_empty() {
            error_class
        } else {
            // Earlier child operations may have produced external effects.
            // The enclosing call must not be automatically replayed.
            ToolErrorClass::UnknownOutcome
        });
        let output_budget = limits.max_final_observation_bytes.saturating_sub(8 * 1024);
        while serialized_step_observations_len(&self.steps) > output_budget {
            if let Some(last) = self.steps.last_mut() {
                if last.observation.is_empty() {
                    self.steps.pop();
                    break;
                }
                last.observation.clear();
                self.truncated = true;
            } else {
                break;
            }
        }
    }

    pub(crate) fn to_value(&self, limits: WorkflowLimits) -> Value {
        let value = serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "status": "failed",
                "error": "Workflow result could not be serialized",
                "completed_step_ids": [],
                "steps": [],
                "call_count": self.call_count,
                "truncated": true
            })
        });
        let encoded_size = serde_json::to_vec(&value).map_or(usize::MAX, |encoded| encoded.len());
        if encoded_size <= limits.max_final_observation_bytes {
            return value;
        }
        serde_json::json!({
            "status": "partial",
            "completed_step_ids": self.completed_step_ids,
            "failed_step_id": self.failed_step_id,
            "error": "Workflow result exceeded its output limit; completed step observations were omitted",
            "call_count": self.call_count,
            "truncated": true
        })
    }
}

/// Run a validated plan serially. Authorization and live execution checks are
/// performed by the supplied port separately for every node.
pub(crate) async fn execute_workflow(
    plan: &ValidatedWorkflow,
    subcalls: Arc<dyn WorkflowSubcallPort>,
    cancel: CancellationToken,
) -> WorkflowExecutionResult {
    let limits = plan.limits();
    let mut execution = WorkflowExecutionResult::new();
    let mut completed_outputs = HashMap::new();
    let mut total_input_bytes = 0usize;
    let mut total_output_bytes = 0usize;
    let deadline = Instant::now() + Duration::from_secs(limits.max_execution_duration_secs);

    for step in plan.steps() {
        if cancel.is_cancelled() {
            execution.status = if execution.completed_step_ids.is_empty() {
                WorkflowStatus::Cancelled
            } else {
                WorkflowStatus::Partial
            };
            execution.error_class = Some(if execution.completed_step_ids.is_empty() {
                ToolErrorClass::Permission
            } else {
                ToolErrorClass::UnknownOutcome
            });
            execution.error = Some("Workflow execution was cancelled".into());
            break;
        }

        if Instant::now() >= deadline {
            execution.stop(
                Some(&step.id),
                "Workflow exceeded its total execution time limit before this step started",
                ToolErrorClass::Transient,
                limits,
            );
            break;
        }

        let input = match plan.resolve_input(&step.id, &completed_outputs) {
            Ok(input) => input,
            Err(_) => {
                execution.stop(
                    Some(&step.id),
                    "Workflow step input could not be materialized",
                    ToolErrorClass::Validation,
                    limits,
                );
                break;
            }
        };
        let input_size = serde_json::to_vec(&input).map_or(usize::MAX, |bytes| bytes.len());
        total_input_bytes = total_input_bytes.saturating_add(input_size);
        if input_size > limits.max_step_input_bytes
            || total_input_bytes > limits.max_total_input_bytes
        {
            execution.stop(
                Some(&step.id),
                "Workflow input budget was exhausted before the step executed",
                ToolErrorClass::Validation,
                limits,
            );
            break;
        }

        execution.call_count = execution.call_count.saturating_add(1);
        let step_cancel = cancel.child_token();
        let call_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                step_cancel.cancel();
                execution.status = if execution.completed_step_ids.is_empty() {
                    WorkflowStatus::Cancelled
                } else {
                    WorkflowStatus::Partial
                };
                execution.failed_step_id = Some(step.id.clone());
                execution.error = Some("Workflow execution was cancelled while a step was running".into());
                // The active child may have produced an effect before the
                // cancellation reached its handler.
                execution.error_class = Some(ToolErrorClass::UnknownOutcome);
                break;
            }
            _ = tokio::time::sleep_until(deadline) => {
                step_cancel.cancel();
                execution.stop(
                    Some(&step.id),
                    "Workflow exceeded its total execution time limit while a step was running",
                    ToolErrorClass::UnknownOutcome,
                    limits,
                );
                break;
            }
            result = subcalls.call(&step.capability, input, step_cancel.clone()) => result,
        };
        let call = match call_result {
            Ok(call) => call,
            Err(_) => {
                execution.stop(
                    Some(&step.id),
                    "Workflow step was blocked before its handler ran",
                    ToolErrorClass::Permission,
                    limits,
                );
                break;
            }
        };

        let output_size =
            serde_json::to_vec(&call.result.output).map_or(usize::MAX, |bytes| bytes.len());
        total_output_bytes = total_output_bytes.saturating_add(output_size);
        if output_size > limits.max_step_output_bytes
            || total_output_bytes > limits.max_total_output_bytes
        {
            execution.stop(
                Some(&step.id),
                "Workflow result budget was exhausted; no further steps were executed",
                ToolErrorClass::UnknownOutcome,
                limits,
            );
            break;
        }

        if !call.result.success {
            execution.steps.push(WorkflowStepObservation {
                step_id: step.id.clone(),
                capability: step.capability.clone(),
                observation: call.observation,
            });
            execution.stop(
                Some(&step.id),
                "Workflow stopped because a step did not complete successfully",
                call.result.error_class.unwrap_or(ToolErrorClass::Other),
                limits,
            );
            break;
        }

        execution.steps.push(WorkflowStepObservation {
            step_id: step.id.clone(),
            capability: step.capability.clone(),
            observation: call.observation,
        });
        if serialized_step_observations_len(&execution.steps)
            > limits.max_final_observation_bytes.saturating_sub(8 * 1024)
        {
            execution.steps.pop();
            // The handler succeeded. Keep its completion identity even when
            // there is no room to expose another observation or continue to
            // a node whose mappings might depend on omitted result data.
            execution.completed_step_ids.push(step.id.clone());
            execution.status = WorkflowStatus::Partial;
            execution.error = Some(
                "Workflow observation budget was exhausted; no further steps were executed".into(),
            );
            execution.error_class = Some(ToolErrorClass::UnknownOutcome);
            execution.truncated = true;
            break;
        }

        completed_outputs.insert(step.id.clone(), call.result.output);
        execution.completed_step_ids.push(step.id.clone());
    }
    execution
}

fn serialized_step_observations_len(steps: &[WorkflowStepObservation]) -> usize {
    serde_json::to_vec(steps).map_or(usize::MAX, |encoded| encoded.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{WorkflowDefinition, WorkflowStep, WorkflowValue, validate_workflow};
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    struct EchoPort;

    #[async_trait]
    impl WorkflowSubcallPort for EchoPort {
        async fn call(
            &self,
            _capability: &str,
            input: Value,
            _cancel: CancellationToken,
        ) -> Result<WorkflowSubcallResult, String> {
            Ok(WorkflowSubcallResult {
                result: ToolResult::ok(input.clone()),
                observation: serde_json::to_string(&input).unwrap(),
            })
        }
    }

    fn step(id: &str, capability: &str) -> WorkflowStep {
        WorkflowStep {
            id: id.into(),
            capability: capability.into(),
            depends_on: Vec::new(),
            input: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn executes_serially_and_keeps_structured_results_private_to_mappings() {
        let mut first = step("read", "mcp__notes__read");
        first.input.insert(
            "id".into(),
            WorkflowValue::Literal {
                value: json!("abc"),
            },
        );
        let mut second = step("summarize", "mcp__notes__summarize");
        second.input.insert(
            "text".into(),
            WorkflowValue::Result {
                step_id: "read".into(),
                json_pointer: "/id".into(),
            },
        );
        let allowed = BTreeSet::from([
            "mcp__notes__read".to_owned(),
            "mcp__notes__summarize".to_owned(),
        ]);
        let plan = validate_workflow(
            &WorkflowDefinition {
                version: 1,
                steps: vec![first, second],
            },
            &allowed,
            WorkflowLimits::default(),
        )
        .unwrap();

        let result = execute_workflow(&plan, Arc::new(EchoPort), CancellationToken::new()).await;
        assert_eq!(result.status, WorkflowStatus::Completed);
        assert_eq!(result.completed_step_ids, ["read", "summarize"]);
        assert_eq!(result.call_count, 2);
        assert!(result.steps[1].observation.contains("abc"));
        let visible = result.to_value(plan.limits());
        assert!(visible["steps"][0].get("raw_output").is_none());
    }

    struct StopAfterFirst;

    #[async_trait]
    impl WorkflowSubcallPort for StopAfterFirst {
        async fn call(
            &self,
            capability: &str,
            input: Value,
            _cancel: CancellationToken,
        ) -> Result<WorkflowSubcallResult, String> {
            if capability == "mcp__notes__write" {
                return Err("confirmation required".into());
            }
            Ok(WorkflowSubcallResult {
                result: ToolResult::ok(input),
                observation: "read complete".into(),
            })
        }
    }

    #[tokio::test]
    async fn stops_at_a_blocked_node_and_marks_prior_steps_partial_non_replayable() {
        let definition = WorkflowDefinition {
            version: 1,
            steps: vec![
                step("read", "mcp__notes__read"),
                step("write", "mcp__notes__write"),
            ],
        };
        let allowed = BTreeSet::from([
            "mcp__notes__read".to_owned(),
            "mcp__notes__write".to_owned(),
        ]);
        let plan = validate_workflow(&definition, &allowed, WorkflowLimits::default()).unwrap();
        let result =
            execute_workflow(&plan, Arc::new(StopAfterFirst), CancellationToken::new()).await;
        assert_eq!(result.status, WorkflowStatus::Partial);
        assert_eq!(result.completed_step_ids, ["read"]);
        assert_eq!(result.failed_step_id.as_deref(), Some("write"));
        assert_eq!(result.error_class, Some(ToolErrorClass::UnknownOutcome));
        assert_eq!(result.call_count, 2);
    }

    #[tokio::test]
    async fn expired_workflow_budget_prevents_the_first_child_from_running() {
        let definition = WorkflowDefinition {
            version: 1,
            steps: vec![step("read", "mcp__notes__read")],
        };
        let allowed = BTreeSet::from(["mcp__notes__read".to_owned()]);
        let plan = validate_workflow(
            &definition,
            &allowed,
            WorkflowLimits {
                max_execution_duration_secs: 0,
                ..WorkflowLimits::default()
            },
        )
        .unwrap();

        let result = execute_workflow(&plan, Arc::new(EchoPort), CancellationToken::new()).await;
        assert_eq!(result.status, WorkflowStatus::Failed);
        assert_eq!(result.failed_step_id.as_deref(), Some("read"));
        assert_eq!(result.call_count, 0);
        assert_eq!(result.error_class, Some(ToolErrorClass::Transient));
    }

    #[tokio::test]
    async fn cancellation_during_a_child_stops_the_graph_with_unknown_outcome() {
        struct BlockingPort(tokio::sync::mpsc::UnboundedSender<()>);

        #[async_trait]
        impl WorkflowSubcallPort for BlockingPort {
            async fn call(
                &self,
                _capability: &str,
                _input: Value,
                cancel: CancellationToken,
            ) -> Result<WorkflowSubcallResult, String> {
                self.0.send(()).unwrap();
                cancel.cancelled().await;
                Err("cancelled".into())
            }
        }

        let definition = WorkflowDefinition {
            version: 1,
            steps: vec![step("read", "mcp__notes__read")],
        };
        let allowed = BTreeSet::from(["mcp__notes__read".to_owned()]);
        let plan = validate_workflow(&definition, &allowed, WorkflowLimits::default()).unwrap();
        let cancel = CancellationToken::new();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let plan = Arc::new(plan);
        let run = tokio::spawn({
            let cancel = cancel.clone();
            async move { execute_workflow(&plan, Arc::new(BlockingPort(started_tx)), cancel).await }
        });

        started_rx.recv().await.unwrap();
        cancel.cancel();
        let result = run.await.unwrap();

        assert_eq!(result.status, WorkflowStatus::Cancelled);
        assert_eq!(result.failed_step_id.as_deref(), Some("read"));
        assert_eq!(result.call_count, 1);
        assert_eq!(result.error_class, Some(ToolErrorClass::UnknownOutcome));
    }

    #[tokio::test]
    async fn observation_budget_preserves_the_identity_of_a_completed_step() {
        let definition = WorkflowDefinition {
            version: 1,
            steps: vec![step("read", "mcp__notes__read")],
        };
        let allowed = BTreeSet::from(["mcp__notes__read".to_owned()]);
        let plan = validate_workflow(
            &definition,
            &allowed,
            WorkflowLimits {
                max_final_observation_bytes: 8 * 1024,
                ..WorkflowLimits::default()
            },
        )
        .unwrap();

        let result = execute_workflow(&plan, Arc::new(EchoPort), CancellationToken::new()).await;
        assert_eq!(result.status, WorkflowStatus::Partial);
        assert_eq!(result.completed_step_ids, ["read"]);
        assert_eq!(result.failed_step_id, None);
        assert_eq!(result.error_class, Some(ToolErrorClass::UnknownOutcome));
        assert!(result.steps.is_empty());
    }
}
