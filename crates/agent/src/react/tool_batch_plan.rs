//! Immutable plan for one assistant tool batch.
//!
//! The model's tool-call array is the protocol order. Execution may complete
//! in any order, but identities, action indexes and transcript materialization
//! must all derive from that one array. Building this plan before any safety
//! gate or future is created gives the batch a stable identity map and keeps
//! the executor from minting related ids in several branches.

use super::transcript::ActionCard;
use crate::types::Action;
use haven_common::types::CanonicalToolCall;

/// One non-final call admitted by the turn coordinator.
#[derive(Debug, Clone)]
pub(super) struct PlannedTool {
    pub(super) action: Action,
    pub(super) step_id: String,
    pub(super) action_index: u32,
}

/// Stable, ordered view of the non-final calls in one model response.
#[derive(Debug, Clone)]
pub(super) struct ToolBatchPlan {
    tools: Vec<PlannedTool>,
}

impl ToolBatchPlan {
    pub(super) fn from_actions(actions: &[Action]) -> Self {
        let tools = actions
            .iter()
            .filter(|action| !action.is_final)
            .enumerate()
            .map(|(action_index, action)| PlannedTool {
                action: action.clone(),
                step_id: haven_common::types::new_id("step"),
                action_index: action_index as u32,
            })
            .collect();
        Self { tools }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.tools.len()
    }

    #[cfg(test)]
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

    pub(super) fn action_cards(&self, suppress_streamed_thought: bool) -> Vec<ActionCard> {
        self.tools
            .iter()
            .map(|tool| ActionCard {
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
    fn plan_filters_final_answer_and_assigns_protocol_order() {
        let plan = ToolBatchPlan::from_actions(&[
            action("read", false),
            action("final_answer", true),
            action("write", false),
        ]);

        assert_eq!(plan.len(), 2);
        assert_eq!(plan.get(0).unwrap().action_index, 0);
        assert_eq!(plan.get(1).unwrap().action_index, 1);
        assert_ne!(plan.get(0).unwrap().step_id, plan.get(1).unwrap().step_id);
        assert_eq!(plan.canonical_calls()[1].name, "write");
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
}
