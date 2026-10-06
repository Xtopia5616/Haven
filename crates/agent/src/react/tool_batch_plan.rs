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
