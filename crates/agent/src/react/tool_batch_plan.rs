//! Immutable plan for one assistant tool batch.
//!
//! The model's tool-call array is the protocol order. Execution may complete
//! in any order, but identities, tool_call indexes and transcript materialization
//! must all derive from that one array. Building this plan before any safety
//! gate or future is created gives the batch a stable identity map and keeps
//! the executor from minting related ids in several branches.

use super::transcript::ToolCallCard;
use crate::interaction::{InteractionDetails, InteractionRequest};
use crate::types::ToolCall;
use haven_common::types::CanonicalToolCall;
use haven_tools::{ToolCatalogSnapshot, is_silent_tool_call};

/// Durable continuation for a confirmation barrier. Only requests that
/// actually require a user decision are linked; every sibling remains in the
/// ordered plan without being represented as a synthetic interaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct ConfirmationBatchPlan {
    pub(crate) step_number: u32,
    pub(crate) tools: Vec<ConfirmationBatchTool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct ConfirmationBatchTool {
    pub(crate) step_id: String,
    pub(crate) tool_index: u32,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) confirmation_request_id: Option<String>,
}

impl ConfirmationBatchPlan {
    pub(crate) fn validate_shape(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.tools.is_empty(), "confirmation batch plan is empty");
        let mut step_ids = std::collections::HashSet::new();
        let mut tool_indexes = std::collections::HashSet::new();
        let mut request_ids = std::collections::HashSet::new();
        let mut previous_index = None;
        for tool in &self.tools {
            anyhow::ensure!(
                !tool.step_id.is_empty()
                    && step_ids.insert(tool.step_id.as_str())
                    && tool_indexes.insert(tool.tool_index),
                "confirmation batch plan contains a duplicate or empty tool identity"
            );
            anyhow::ensure!(
                previous_index.is_none_or(|index| index < tool.tool_index),
                "confirmation batch plan is not ordered by provider tool index"
            );
            previous_index = Some(tool.tool_index);
            anyhow::ensure!(
                tool.tool_call_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty()),
                "confirmation batch plan contains an empty tool_call_id"
            );
            if let Some(request_id) = &tool.confirmation_request_id {
                anyhow::ensure!(
                    !request_id.is_empty() && request_ids.insert(request_id.as_str()),
                    "confirmation batch plan contains a duplicate or empty request id"
                );
            }
        }
        anyhow::ensure!(
            !request_ids.is_empty(),
            "confirmation batch plan has no confirmation request"
        );
        Ok(())
    }

    pub(super) fn from_plan(
        plan_step_number: u32,
        plan: &ToolBatchPlan,
        requests: &[InteractionRequest],
    ) -> anyhow::Result<Self> {
        let mut matched_requests = std::collections::HashSet::new();
        let tools = plan
            .iter()
            .map(|planned| {
                let mut matches = requests.iter().filter(|request| {
                    matches!(
                        &request.details,
                        InteractionDetails::Confirm {
                            step_number: request_step_number,
                            step_id,
                            tool_index,
                            tool_name,
                            tool_input,
                            tool_call_id,
                            ..
                        } if *request_step_number == plan_step_number
                            && step_id == &planned.step_id
                            && *tool_index == planned.tool_index
                            && tool_name == &planned.tool_call.tool_name
                            && tool_input == &planned.tool_call.tool_input
                            && tool_call_id
                                == planned.tool_call.tool_call_id.as_deref().unwrap_or_default()
                    )
                });
                let request = matches.next();
                anyhow::ensure!(
                    matches.next().is_none(),
                    "confirmation batch has duplicate requests for tool index {}",
                    planned.tool_index
                );
                if let Some(request) = request {
                    anyhow::ensure!(
                        matched_requests.insert(request.id.clone()),
                        "confirmation request '{}' maps to more than one tool",
                        request.id
                    );
                }
                Ok(ConfirmationBatchTool {
                    step_id: planned.step_id.clone(),
                    tool_index: planned.tool_index,
                    tool_call_id: planned.tool_call.tool_call_id.clone(),
                    confirmation_request_id: request.map(|request| request.id.clone()),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        anyhow::ensure!(
            matched_requests.len() == requests.len(),
            "confirmation batch contains a request that does not match the ordered tool plan"
        );
        let continuation = Self {
            step_number: plan_step_number,
            tools,
        };
        continuation.validate_shape()?;
        Ok(continuation)
    }

    pub(crate) fn validate_requests(&self, requests: &[InteractionRequest]) -> anyhow::Result<()> {
        self.validate_shape()?;
        let mut linked_ids = std::collections::HashSet::new();
        for tool in &self.tools {
            let Some(request_id) = tool.confirmation_request_id.as_deref() else {
                continue;
            };
            let request = requests
                .iter()
                .find(|request| request.id == request_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "confirmation request '{}' is missing from its ordered plan",
                        request_id
                    )
                })?;
            anyhow::ensure!(
                linked_ids.insert(request_id),
                "confirmation request '{}' appears more than once in its ordered plan",
                request_id
            );
            let matches = matches!(
                &request.details,
                InteractionDetails::Confirm {
                    step_number,
                    step_id,
                    tool_index,
                    tool_call_id,
                    ..
                } if *step_number == self.step_number
                    && step_id == &tool.step_id
                    && *tool_index == tool.tool_index
                    && tool_call_id == tool.tool_call_id.as_deref().unwrap_or_default()
            );
            anyhow::ensure!(
                matches,
                "confirmation request '{}' does not match its ordered plan identity",
                request_id
            );
        }
        anyhow::ensure!(
            linked_ids.len() == requests.len(),
            "confirmation batch contains a request that is absent from its ordered plan"
        );
        Ok(())
    }

    pub(crate) fn from_confirm_requests(requests: &[InteractionRequest]) -> Self {
        let plan = ToolBatchPlan::from_confirm_requests(requests);
        Self {
            step_number: requests
                .iter()
                .find_map(|request| match &request.details {
                    InteractionDetails::Confirm { step_number, .. } => Some(*step_number),
                    _ => None,
                })
                .unwrap_or(0),
            tools: plan
                .iter()
                .zip(requests.iter())
                .map(|(planned, request)| ConfirmationBatchTool {
                    step_id: planned.step_id.clone(),
                    tool_index: planned.tool_index,
                    tool_call_id: planned.tool_call.tool_call_id.clone(),
                    confirmation_request_id: Some(request.id.clone()),
                })
                .collect(),
        }
    }

    /// Replay the latest active plan. The completion clear is appended in the
    /// same transaction as the last tool result, so a crash cannot leave a
    /// completed plan active without its interaction requests.
    pub(crate) fn replay(
        active_events: &[haven_memory::SessionEvent],
    ) -> anyhow::Result<Option<Self>> {
        let mut plan = None;
        for event in active_events {
            match event.event_type.as_str() {
                haven_memory::CONFIRMATION_BATCH_PLANNED_EVENT_TYPE => {
                    let restored: Self = serde_json::from_str(&event.payload).map_err(|error| {
                        anyhow::anyhow!(
                            "invalid confirmation batch plan at sequence {}: {error}",
                            event.sequence
                        )
                    })?;
                    restored.validate_shape().map_err(|error| {
                        anyhow::anyhow!(
                            "invalid confirmation batch plan at sequence {}: {error}",
                            event.sequence
                        )
                    })?;
                    plan = Some(restored);
                }
                haven_memory::INTERACTION_CLEARED_EVENT_TYPE => {
                    if let Some(active) = &plan {
                        let payload: serde_json::Value = serde_json::from_str(&event.payload)
                            .map_err(|error| {
                                anyhow::anyhow!(
                                    "invalid interaction clear event at sequence {}: {error}",
                                    event.sequence
                                )
                            })?;
                        let ids = payload
                            .get("ids")
                            .and_then(serde_json::Value::as_array)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "interaction clear event at sequence {} has no ids",
                                    event.sequence
                                )
                            })?;
                        let gated_ids = active
                            .tools
                            .iter()
                            .filter_map(|tool| tool.confirmation_request_id.as_deref())
                            .collect::<Vec<_>>();
                        if !gated_ids.is_empty()
                            && gated_ids.iter().all(|request_id| {
                                ids.iter().any(|id| id.as_str() == Some(*request_id))
                            })
                        {
                            plan = None;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(plan)
    }
}

/// One non-final call admitted by the turn coordinator.
#[derive(Debug, Clone)]
pub(super) struct PlannedTool {
    pub(super) tool_call: ToolCall,
    pub(super) step_id: String,
    pub(super) tool_index: u32,
}

/// Stable, ordered view of the non-final calls in one model response.
/// `tool_index` remains the zero-based position in the provider's original
/// array, even when a final-answer entry is filtered from execution.
#[derive(Debug, Clone)]
pub(super) struct ToolBatchPlan {
    tools: Vec<PlannedTool>,
}

impl ToolBatchPlan {
    pub(super) fn from_tool_calls(tool_calls: &[ToolCall]) -> Self {
        let tools = tool_calls
            .iter()
            .enumerate()
            .filter(|(_, tool_call)| !tool_call.is_final)
            .map(|(tool_index, tool_call)| PlannedTool {
                tool_call: tool_call.clone(),
                step_id: haven_common::types::new_id("step"),
                tool_index: tool_index as u32,
            })
            .collect();
        Self { tools }
    }

    /// Rebuild the plan for a confirmation resume without minting new
    /// identities. The pending confirmation record is the durable carrier of
    /// the original plan's step ids and protocol indexes; this constructor
    /// restores them into the same plan type used by a live batch.
    pub(super) fn from_confirm_requests(requests: &[InteractionRequest]) -> Self {
        let tools = requests
            .iter()
            .filter_map(|request| match &request.details {
                InteractionDetails::Confirm {
                    tool_name,
                    tool_input,
                    tool_call_id,
                    step_id,
                    tool_index,
                    ..
                } => Some(PlannedTool {
                    tool_call: ToolCall {
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                        is_final: false,
                        tool_call_id: (!tool_call_id.is_empty()).then(|| tool_call_id.clone()),
                    },
                    step_id: step_id.clone(),
                    tool_index: *tool_index,
                }),
                _ => None,
            })
            .collect();
        Self { tools }
    }

    pub(crate) fn from_confirmation_batch(
        plan: &ConfirmationBatchPlan,
        calls: &[CanonicalToolCall],
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            plan.tools.len() == calls.len(),
            "confirmation batch plan does not match its transcript tool calls"
        );
        let tools = plan
            .tools
            .iter()
            .zip(calls)
            .map(|(identity, call)| {
                anyhow::ensure!(
                    identity.tool_call_id.as_deref().unwrap_or_default() == call.id,
                    "confirmation batch tool identity does not match its transcript call"
                );
                Ok(PlannedTool {
                    tool_call: ToolCall {
                        tool_name: call.name.clone(),
                        tool_input: call.arguments.clone(),
                        is_final: false,
                        tool_call_id: Some(call.id.clone()),
                    },
                    step_id: identity.step_id.clone(),
                    tool_index: identity.tool_index,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { tools })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.tools.len()
    }

    pub(super) fn get(&self, index: usize) -> Option<&PlannedTool> {
        self.tools.get(index)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &PlannedTool> {
        self.tools.iter()
    }

    pub(super) fn indexed_tool_calls(&self) -> impl Iterator<Item = (u32, &ToolCall)> {
        self.tools
            .iter()
            .map(|planned| (planned.tool_index, &planned.tool_call))
    }

    pub(super) fn canonical_calls(&self) -> Vec<CanonicalToolCall> {
        self.tools
            .iter()
            .map(|tool| CanonicalToolCall {
                id: tool.tool_call.tool_call_id.clone().unwrap_or_default(),
                name: tool.tool_call.tool_name.clone(),
                arguments: tool.tool_call.tool_input.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn tool_call_cards(&self, suppress_streamed_thought: bool) -> Vec<ToolCallCard> {
        self.build_tool_call_cards(suppress_streamed_thought, None)
    }

    pub(super) fn tool_call_cards_with_catalog(
        &self,
        suppress_streamed_thought: bool,
        catalog: &ToolCatalogSnapshot,
    ) -> Vec<ToolCallCard> {
        self.build_tool_call_cards(suppress_streamed_thought, Some(catalog))
    }

    fn build_tool_call_cards(
        &self,
        suppress_streamed_thought: bool,
        catalog: Option<&ToolCatalogSnapshot>,
    ) -> Vec<ToolCallCard> {
        self.tools
            .iter()
            .map(|tool| ToolCallCard {
                is_high_risk: catalog.is_some_and(|catalog| {
                    catalog
                        .operation_policy(&tool.tool_call.tool_name, &tool.tool_call.tool_input)
                        .risk_level
                        != haven_common::types::RiskLevel::Safe
                }),
                silent: is_silent_tool_call(&tool.tool_call.tool_name, &tool.tool_call.tool_input),
                tool_name: tool.tool_call.tool_name.clone(),
                tool_input: tool.tool_call.tool_input.clone(),
                tool_call_id: tool.tool_call.tool_call_id.clone(),
                step_id: tool.step_id.clone(),
                tool_index: tool.tool_index,
                suppress_streamed_thought,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call(name: &str, is_final: bool) -> ToolCall {
        ToolCall {
            tool_name: name.into(),
            tool_input: serde_json::json!({"name": name}),
            is_final,
            tool_call_id: Some(format!("call-{name}")),
        }
    }

    #[test]
    fn plan_filters_final_answer_and_preserves_provider_array_position() {
        let plan = ToolBatchPlan::from_tool_calls(&[
            tool_call("read", false),
            tool_call("final_answer", true),
            tool_call("write", false),
        ]);

        assert_eq!(plan.len(), 2);
        assert_eq!(plan.get(0).unwrap().tool_index, 0);
        assert_eq!(plan.get(1).unwrap().tool_index, 2);
        assert_ne!(plan.get(0).unwrap().step_id, plan.get(1).unwrap().step_id);
        assert_eq!(plan.canonical_calls()[1].name, "write");
        let cards = plan.tool_call_cards(false);
        assert_eq!(cards[1].tool_index, 2);
        assert_eq!(cards[1].tool_call_id.as_deref(), Some("call-write"));
    }

    #[test]
    fn tool_call_cards_reuse_plan_identity() {
        let plan = ToolBatchPlan::from_tool_calls(&[tool_call("read", false)]);
        let card = &plan.tool_call_cards(true)[0];
        let planned = plan.get(0).unwrap();

        assert_eq!(card.step_id, planned.step_id);
        assert_eq!(card.tool_index, planned.tool_index);
        assert!(card.suppress_streamed_thought);
    }

    #[test]
    fn single_tool_plan_is_the_complete_identity_source() {
        let plan = ToolBatchPlan::from_tool_calls(&[tool_call("read", false)]);
        let planned = plan.get(0).unwrap();
        let call = &plan.canonical_calls()[0];
        let card = &plan.tool_call_cards(false)[0];

        assert_eq!(plan.len(), 1);
        assert_eq!(call.id, planned.tool_call.tool_call_id.clone().unwrap());
        assert_eq!(card.step_id, planned.step_id);
        assert_eq!(card.tool_index, planned.tool_index);
    }

    #[test]
    fn confirm_resume_reuses_pending_identity() {
        let pending = InteractionRequest::confirm(
            "ses-test",
            1,
            "write".into(),
            serde_json::json!({"path": "a.txt"}),
            "call-write".into(),
            "step-existing".into(),
            3,
            haven_common::types::RiskLevel::High,
            None,
        );

        let plan = ToolBatchPlan::from_confirm_requests(&[pending]);
        let planned = plan.get(0).unwrap();

        assert_eq!(planned.step_id, "step-existing");
        assert_eq!(planned.tool_index, 3);
        assert_eq!(
            planned.tool_call.tool_call_id.as_deref(),
            Some("call-write")
        );
    }

    #[test]
    fn mixed_confirmation_plan_keeps_safe_sibling_without_synthetic_request() {
        let plan =
            ToolBatchPlan::from_tool_calls(&[tool_call("read", false), tool_call("write", false)]);
        let confirmation = InteractionRequest::confirm(
            "ses-test",
            7,
            "write".into(),
            serde_json::json!({"name": "write"}),
            "call-write".into(),
            plan.get(1).unwrap().step_id.clone(),
            1,
            haven_common::types::RiskLevel::High,
            None,
        );
        let confirmation_id = confirmation.id.clone();

        let durable =
            ConfirmationBatchPlan::from_plan(7, &plan, std::slice::from_ref(&confirmation))
                .unwrap();
        durable.validate_requests(&[confirmation]).unwrap();
        let encoded = serde_json::to_string(&durable).unwrap();
        assert!(!encoded.contains("tool_input"));
        assert!(!encoded.contains("receipt"));
        let restored: ConfirmationBatchPlan = serde_json::from_str(&encoded).unwrap();
        let calls = plan.canonical_calls();
        let restored_plan = ToolBatchPlan::from_confirmation_batch(&restored, &calls).unwrap();

        assert_eq!(restored.tools.len(), 2);
        assert_eq!(restored.tools[0].confirmation_request_id, None);
        assert_eq!(
            restored.tools[1].confirmation_request_id.as_deref(),
            Some(confirmation_id.as_str())
        );
        assert_eq!(restored_plan.get(0).unwrap().tool_index, 0);
        assert_eq!(restored_plan.get(1).unwrap().tool_index, 1);
        assert_eq!(
            restored_plan.get(0).unwrap().step_id,
            plan.get(0).unwrap().step_id
        );
    }

    #[test]
    fn indexed_validation_targets_preserve_original_tool_positions_after_final_call() {
        let plan = ToolBatchPlan::from_tool_calls(&[
            tool_call("read", false),
            tool_call("final_answer", true),
            tool_call("write", false),
        ]);

        assert_eq!(
            plan.indexed_tool_calls()
                .map(|(tool_index, call)| (tool_index, call.tool_name.as_str()))
                .collect::<Vec<_>>(),
            [(0, "read"), (2, "write")]
        );
    }

    #[test]
    fn replay_drops_confirmation_plan_only_after_its_requests_are_cleared() {
        let plan = ToolBatchPlan::from_tool_calls(&[tool_call("write", false)]);
        let request = InteractionRequest::confirm(
            "ses-test",
            2,
            "write".into(),
            serde_json::json!({"name": "write"}),
            "call-write".into(),
            plan.get(0).unwrap().step_id.clone(),
            0,
            haven_common::types::RiskLevel::High,
            None,
        );
        let durable =
            ConfirmationBatchPlan::from_plan(2, &plan, std::slice::from_ref(&request)).unwrap();
        let event = |sequence, event_type: &str, payload: String| haven_memory::SessionEvent {
            session_id: "ses-test".into(),
            sequence,
            event_type: event_type.into(),
            event_version: haven_memory::CURRENT_EVENT_VERSION,
            payload,
            created_at: "2026-01-01T00:00:00Z".into(),
            run_id: None,
            step_number: None,
        };
        let planned = event(
            1,
            haven_memory::CONFIRMATION_BATCH_PLANNED_EVENT_TYPE,
            serde_json::to_string(&durable).unwrap(),
        );
        let unrelated_clear = event(
            2,
            haven_memory::INTERACTION_CLEARED_EVENT_TYPE,
            r#"{"ids":["conf-other"]}"#.into(),
        );
        let completed_clear = event(
            3,
            haven_memory::INTERACTION_CLEARED_EVENT_TYPE,
            serde_json::json!({ "ids": [request.id] }).to_string(),
        );

        assert!(
            ConfirmationBatchPlan::replay(std::slice::from_ref(&planned))
                .unwrap()
                .is_some()
        );
        assert!(
            ConfirmationBatchPlan::replay(&[planned.clone(), unrelated_clear])
                .unwrap()
                .is_some()
        );
        assert!(
            ConfirmationBatchPlan::replay(&[planned, completed_clear])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn large_provider_batch_preserves_all_64_tool_run_positions() {
        let tool_calls = (0..64)
            .map(|index| tool_call(&format!("tool-{index}"), false))
            .collect::<Vec<_>>();
        let plan = ToolBatchPlan::from_tool_calls(&tool_calls);

        assert_eq!(plan.len(), 64);
        for (index, planned) in plan.iter().enumerate() {
            assert_eq!(planned.tool_index, index as u32);
            let expected_call_id = format!("call-tool-{index}");
            assert_eq!(
                planned.tool_call.tool_call_id.as_deref(),
                Some(expected_call_id.as_str())
            );
        }
        assert_eq!(plan.tool_call_cards(false).len(), 64);
    }
}
