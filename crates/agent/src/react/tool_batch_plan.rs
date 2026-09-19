//! Immutable plan for one assistant tool batch.
//!
//! The model's tool-call array is the protocol order. Execution may complete
//! in any order, but identities, action indexes and transcript materialization
//! must all derive from that one array. Building this plan before any safety
//! gate or future is created gives the batch a stable identity map and keeps
//! the executor from minting related ids in several branches.

use super::transcript::ActionCard;
use crate::interaction::{InteractionDetails, InteractionRequest};
use crate::types::Action;
use haven_common::types::CanonicalToolCall;
use haven_tools::{ToolCatalogSnapshot, is_silent_action};

/// One non-final call admitted by the turn coordinator.
#[derive(Debug, Clone)]
pub(super) struct PlannedTool {
    pub(super) action: Action,
    pub(super) step_id: String,
    pub(super) action_index: u32,
}

/// Stable, ordered view of the non-final calls in one model response.
/// `action_index` remains the zero-based position in the provider's original
/// array, even when a final-answer entry is filtered from execution.
#[derive(Debug, Clone)]
pub(super) struct ToolBatchPlan {
    tools: Vec<PlannedTool>,
}

impl ToolBatchPlan {
    pub(super) fn from_actions(actions: &[Action]) -> Self {
        let tools = actions
            .iter()
            .enumerate()
            .filter(|(_, action)| !action.is_final)
            .map(|(action_index, action)| PlannedTool {
                action: action.clone(),
                step_id: haven_common::types::new_id("step"),
                action_index: action_index as u32,
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
                    action_index,
                    ..
                } => Some(PlannedTool {
                    action: Action {
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                        is_final: false,
                        tool_call_id: (!tool_call_id.is_empty()).then(|| tool_call_id.clone()),
                    },
                    step_id: step_id.clone(),
                    action_index: *action_index,
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
                id: tool.action.tool_call_id.clone().unwrap_or_default(),
                name: tool.action.tool_name.clone(),
                arguments: tool.action.tool_input.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn action_cards(&self, suppress_streamed_thought: bool) -> Vec<ActionCard> {
        self.build_action_cards(suppress_streamed_thought, None)
    }

    pub(super) fn action_cards_with_catalog(
        &self,
        suppress_streamed_thought: bool,
        catalog: &ToolCatalogSnapshot,
    ) -> Vec<ActionCard> {
        self.build_action_cards(suppress_streamed_thought, Some(catalog))
    }

    fn build_action_cards(
        &self,
        suppress_streamed_thought: bool,
        catalog: Option<&ToolCatalogSnapshot>,
    ) -> Vec<ActionCard> {
        self.tools
            .iter()
            .map(|tool| ActionCard {
                is_high_risk: catalog.is_some_and(|catalog| {
                    catalog
                        .operation_policy(&tool.action.tool_name, &tool.action.tool_input)
                        .risk_level
                        != haven_common::types::RiskLevel::Safe
                }),
                silent: is_silent_action(&tool.action.tool_name, &tool.action.tool_input),
                tool_name: tool.action.tool_name.clone(),
                tool_input: tool.action.tool_input.clone(),
                tool_call_id: tool.action.tool_call_id.clone(),
                step_id: tool.step_id.clone(),
                action_index: tool.action_index,
                suppress_streamed_thought,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(name: &str, is_final: bool) -> Action {
        Action {
            tool_name: name.into(),
            tool_input: serde_json::json!({"name": name}),
            is_final,
            tool_call_id: Some(format!("call-{name}")),
        }
    }

    #[test]
    fn plan_filters_final_answer_and_preserves_provider_array_position() {
        let plan = ToolBatchPlan::from_actions(&[
            action("read", false),
            action("final_answer", true),
            action("write", false),
        ]);

        assert_eq!(plan.len(), 2);
        assert_eq!(plan.get(0).unwrap().action_index, 0);
        assert_eq!(plan.get(1).unwrap().action_index, 2);
        assert_ne!(plan.get(0).unwrap().step_id, plan.get(1).unwrap().step_id);
        assert_eq!(plan.canonical_calls()[1].name, "write");
        let cards = plan.action_cards(false);
        assert_eq!(cards[1].action_index, 2);
        assert_eq!(cards[1].tool_call_id.as_deref(), Some("call-write"));
    }

    #[test]
    fn action_cards_reuse_plan_identity() {
        let plan = ToolBatchPlan::from_actions(&[action("read", false)]);
        let card = &plan.action_cards(true)[0];
        let planned = plan.get(0).unwrap();

        assert_eq!(card.step_id, planned.step_id);
        assert_eq!(card.action_index, planned.action_index);
        assert!(card.suppress_streamed_thought);
    }

    #[test]
    fn single_tool_plan_is_the_complete_identity_source() {
        let plan = ToolBatchPlan::from_actions(&[action("read", false)]);
        let planned = plan.get(0).unwrap();
        let call = &plan.canonical_calls()[0];
        let card = &plan.action_cards(false)[0];

        assert_eq!(plan.len(), 1);
        assert_eq!(call.id, planned.action.tool_call_id.clone().unwrap());
        assert_eq!(card.step_id, planned.step_id);
        assert_eq!(card.action_index, planned.action_index);
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
        assert_eq!(planned.action_index, 3);
        assert_eq!(planned.action.tool_call_id.as_deref(), Some("call-write"));
    }
}
