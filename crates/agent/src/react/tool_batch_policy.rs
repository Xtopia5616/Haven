//! Failure policy and observation helpers for ReAct tool batches.
//!
//! These functions classify tool failures and shape the provider-only retry
//! hint. They do not execute tools or decide lifecycle transitions; keeping
//! them separate makes the batch executor a mechanical admission/execute/
//! commit pipeline.

use super::*;
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_tools::{OperationIdempotency, ToolErrorClass, ToolExecutionOutcome, ToolRetryability};

/// Failure classification used to shape the post-failure retry nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FailureKind {
    /// The environment cannot run the approach: missing command, wrong shell,
    /// network/proxy trouble, bad paths. The approach itself may be sound.
    Environmental,
    /// The approach/usage itself is flawed (bad params, parse failures).
    Logic,
    /// Cannot tell from the error text.
    Unknown,
}

/// A concrete failed invocation used by the run-scoped agent retry budget.
/// Keeping the normalized input in the key prevents a model from consuming
/// the same retry allowance by alternating irrelevant JSON field order or
/// switching to a different operation.
#[derive(Debug, Clone)]
pub(super) struct ToolFailureSignal {
    pub(super) tool_name: String,
    pub(super) tool_input: serde_json::Value,
    pub(super) error_class: ToolErrorClass,
    pub(super) retryability: ToolRetryability,
    pub(super) tool_call_id: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct ToolRetryBudget {
    attempts: std::collections::HashMap<(String, String, FailureKind, ToolRetryability), u8>,
}

const MAX_AGENT_RETRIES_PER_FAILURE: u8 = 2;

impl ToolRetryBudget {
    /// Record one model-level retry. Returns false after the bounded retry
    /// allowance is exhausted; tool-internal retries remain a separate policy
    /// owned by `ToolsManager`.
    pub(super) fn admit(&mut self, signal: &ToolFailureSignal) -> bool {
        let key = (
            signal.tool_name.clone(),
            normalize_tool_input(&signal.tool_input),
            failure_kind(signal.error_class),
            signal.retryability,
        );
        let attempts = self.attempts.entry(key).or_default();
        if *attempts >= MAX_AGENT_RETRIES_PER_FAILURE {
            return false;
        }
        *attempts += 1;
        true
    }
}

fn normalize_tool_input(input: &serde_json::Value) -> String {
    serde_json::to_string(input).unwrap_or_else(|_| "<invalid-json>".into())
}

/// `agent` operation=inbox result is an empty poll (`count: 0`): nothing for
/// the user to see, so the observation card is suppressed.
pub(crate) fn empty_inbox_output(result: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result)
        .ok()
        .and_then(|v| v.get("count").and_then(|c| c.as_u64()))
        == Some(0)
}

/// True when this is an `agent` inbox poll (check tool_input.operation).
pub(crate) fn is_agent_inbox_call(tool_name: &str, tool_input: &serde_json::Value) -> bool {
    tool_name == "agent" && tool_input.get("operation").and_then(|v| v.as_str()) == Some("inbox")
}

/// Only terminal failures with a known, completed outcome may produce an
/// agent-level retry nudge. `Cancelled` and `TimedOutUnknown` are deliberately
/// excluded because the external operation may still be in flight.
pub(super) fn is_retryable_failure_outcome(
    outcome: ToolExecutionOutcome,
    idempotency: OperationIdempotency,
    error_class: ToolErrorClass,
    retryability: ToolRetryability,
) -> bool {
    matches!(idempotency, OperationIdempotency::Idempotent)
        && matches!(retryability, ToolRetryability::Retryable)
        && matches!(error_class, ToolErrorClass::Transient)
        && matches!(
            outcome,
            ToolExecutionOutcome::Failed | ToolExecutionOutcome::TimedOutAndTerminated
        )
}

impl ReActEngine {
    /// Compose the retry nudge after a step where tool calls failed. The
    /// failure evidence is classified first: environment-type failures
    /// (missing command, wrong shell syntax, network/proxy, paths) must NOT
    /// push the model to abandon its approach — the correct move is to
    /// diagnose and fix the environment (different shell, different tool,
    /// corrected path) and retry. Logic failures get a fix-and-retry nudge
    /// with an explicit threshold before switching approach.
    ///
    /// The returned text is appended onto the last failed tool observation,
    /// never pushed as a synthetic User message into canonical/DB.
    pub(super) fn build_failure_nudge(failures: &[(String, ToolErrorClass)]) -> String {
        let has_env = failures
            .iter()
            .any(|(_, class)| failure_kind(*class) == FailureKind::Environmental);
        let has_logic = failures
            .iter()
            .any(|(_, class)| failure_kind(*class) == FailureKind::Logic);
        let has_permission = failures
            .iter()
            .any(|(_, class)| *class == ToolErrorClass::Permission);
        let has_unknown = failures.iter().any(|(_, class)| {
            matches!(
                class,
                ToolErrorClass::UnknownOutcome | ToolErrorClass::SideEffectMayHaveHappened
            )
        });
        if has_env {
            "The tool failures look ENVIRONMENTAL (missing command / wrong shell syntax / network / path), not logic errors. Do NOT abandon your approach. Diagnose the environment first: verify the command exists in the shell you chose (cmd vs PowerShell syntax differs; `&&` only works in cmd), check network/proxy/endpoints, fix paths and prerequisites. Switching tools (e.g. curl -> aria2) or shells is an environment fix, not a change of approach — keep the same approach and retry."
                .into()
        } else if has_logic {
            "The previous approach failed with logic errors. Analyze the exact error, fix the specific mistake, and retry. Only consider a completely different approach if the same method fails again after you fixed it."
                .into()
        } else if has_permission {
            "The tool call was blocked by a permission or safety policy. Do not retry it blindly; ask the user for authorization or choose a permitted read-only path."
                .into()
        } else if has_unknown {
            "The previous operation has an unknown outcome and may still have produced a side effect. Do not replay it automatically; verify the current state or ask the user."
                .into()
        } else {
            format!(
                "The previous approach encountered errors. {}",
                haven_common::prompts::TOOL_FAILURE_DIAGNOSIS
            )
        }
    }

    /// Append a failure-retry nudge onto the failed tool observation in a
    /// provider request buffer. Requires `failed_tool_call_id` so a parallel
    /// success cannot receive the nudge. Never invents a User row.
    pub(super) fn attach_failure_nudge(
        messages: &mut [CanonicalMessage],
        nudge: &str,
        failed_tool_call_id: Option<&str>,
    ) {
        let Some(id) = failed_tool_call_id else {
            return;
        };
        let idx = messages
            .iter()
            .rev()
            .position(|m| m.role == CanonicalRole::Tool && m.tool_call_id.as_deref() == Some(id))
            .map(|rev_i| messages.len() - 1 - rev_i);
        let Some(idx) = idx else {
            return;
        };
        let msg = &mut messages[idx];
        if let Some(ContentPart::Text(text)) = msg.content.last_mut() {
            text.push_str("\n\n");
            text.push_str(nudge);
        } else {
            msg.content.push(ContentPart::text(nudge));
        }
    }
}

pub(super) fn failure_kind(error_class: ToolErrorClass) -> FailureKind {
    match error_class {
        ToolErrorClass::Transient => FailureKind::Environmental,
        ToolErrorClass::Validation => FailureKind::Logic,
        ToolErrorClass::Permission
        | ToolErrorClass::UnknownOutcome
        | ToolErrorClass::SideEffectMayHaveHappened
        | ToolErrorClass::Other => FailureKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::is_retryable_failure_outcome;
    use haven_tools::{
        OperationIdempotency, ToolErrorClass, ToolExecutionOutcome, ToolRetryability,
    };

    #[test]
    fn unknown_and_cancelled_outcomes_never_request_retry() {
        assert!(!is_retryable_failure_outcome(
            ToolExecutionOutcome::Cancelled,
            OperationIdempotency::Idempotent,
            ToolErrorClass::UnknownOutcome,
            ToolRetryability::Unknown,
        ));
        assert!(!is_retryable_failure_outcome(
            ToolExecutionOutcome::TimedOutUnknown,
            OperationIdempotency::Idempotent,
            ToolErrorClass::UnknownOutcome,
            ToolRetryability::Unknown,
        ));
        assert!(is_retryable_failure_outcome(
            ToolExecutionOutcome::Failed,
            OperationIdempotency::Idempotent,
            ToolErrorClass::Transient,
            ToolRetryability::Retryable,
        ));
        assert!(is_retryable_failure_outcome(
            ToolExecutionOutcome::TimedOutAndTerminated,
            OperationIdempotency::Idempotent,
            ToolErrorClass::Transient,
            ToolRetryability::Retryable,
        ));
        assert!(!is_retryable_failure_outcome(
            ToolExecutionOutcome::Failed,
            OperationIdempotency::Idempotent,
            ToolErrorClass::Transient,
            ToolRetryability::NotRetryable,
        ));
        assert!(!is_retryable_failure_outcome(
            ToolExecutionOutcome::Failed,
            OperationIdempotency::NonIdempotent,
            ToolErrorClass::Transient,
            ToolRetryability::Retryable,
        ));
    }

    #[test]
    fn agent_retry_budget_is_scoped_by_tool_input_and_failure_kind() {
        let mut budget = super::ToolRetryBudget::default();
        let signal = super::ToolFailureSignal {
            tool_name: "files".into(),
            tool_input: serde_json::json!({"path":"a.txt", "operation":"read"}),
            error_class: ToolErrorClass::Validation,
            retryability: ToolRetryability::NotRetryable,
            tool_call_id: Some("call-1".into()),
        };
        assert!(budget.admit(&signal));
        assert!(budget.admit(&signal));
        assert!(!budget.admit(&signal));

        let different_input = super::ToolFailureSignal {
            tool_input: serde_json::json!({"path":"b.txt", "operation":"read"}),
            ..signal
        };
        assert!(budget.admit(&different_input));
    }
}
