//! Runtime application planning for versioned configuration changes.
//!
//! `haven-common::config::ConfigService` owns durable snapshots. This module
//! stays in the application composition root because only the app knows which
//! live components can be rebuilt and which settings require a restart.

use haven_common::config::{ConfigChanged, ConfigDomain};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeConfigTarget {
    InputPipeline,
    Shell,
    ContextLimits,
    LlmRouter,
    SessionRuntime,
    Mcp,
    Security,
    Logging,
    Hotkey,
    Skills,
    ToolSettings,
    MemoryRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeConfigApplyPlan {
    pub(crate) version: u64,
    pub(crate) live: Vec<RuntimeConfigTarget>,
    pub(crate) restart_required: Vec<RuntimeConfigTarget>,
}

impl RuntimeConfigApplyPlan {
    pub(crate) fn from_change(change: &ConfigChanged) -> Self {
        let mut plan = Self {
            version: change.version,
            live: Vec::new(),
            restart_required: Vec::new(),
        };
        for domain in &change.domains {
            match domain {
                ConfigDomain::DefaultShell => plan.push_live(RuntimeConfigTarget::Shell),
                ConfigDomain::Llm => plan.push_live(RuntimeConfigTarget::LlmRouter),
                ConfigDomain::Hotkey => plan.push_live(RuntimeConfigTarget::Hotkey),
                ConfigDomain::Session => plan.push_live(RuntimeConfigTarget::SessionRuntime),
                ConfigDomain::ContextLimits => plan.push_live(RuntimeConfigTarget::ContextLimits),
                ConfigDomain::Security => plan.push_live(RuntimeConfigTarget::Security),
                ConfigDomain::Media => {
                    plan.push_live(RuntimeConfigTarget::InputPipeline);
                    plan.push_live(RuntimeConfigTarget::LlmRouter);
                }
                ConfigDomain::McpDiscovery | ConfigDomain::McpServers => {
                    plan.push_live(RuntimeConfigTarget::Mcp)
                }
                ConfigDomain::Log => plan.push_live(RuntimeConfigTarget::Logging),
                ConfigDomain::Skills => plan.push_live(RuntimeConfigTarget::Skills),
                ConfigDomain::SkillsExec => plan.push_restart(RuntimeConfigTarget::Skills),
                ConfigDomain::Memory => plan.push_restart(RuntimeConfigTarget::MemoryRuntime),
                ConfigDomain::Notification => {
                    // Notification settings are read from the current
                    // snapshot by the notification sink; no rebuild is needed.
                }
                ConfigDomain::Tools => plan.push_live(RuntimeConfigTarget::ToolSettings),
            }
        }
        plan
    }

    pub(crate) fn contains(&self, target: RuntimeConfigTarget) -> bool {
        self.live.contains(&target) || self.restart_required.contains(&target)
    }

    fn push_live(&mut self, target: RuntimeConfigTarget) {
        if !self.live.contains(&target) && !self.restart_required.contains(&target) {
            self.live.push(target);
        }
    }

    fn push_restart(&mut self, target: RuntimeConfigTarget) {
        self.live.retain(|existing| *existing != target);
        if !self.restart_required.contains(&target) {
            self.restart_required.push(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_deduplicates_targets_and_marks_restart_boundaries() {
        let plan = RuntimeConfigApplyPlan::from_change(&ConfigChanged {
            version: 7,
            domains: vec![
                ConfigDomain::Media,
                ConfigDomain::Llm,
                ConfigDomain::SkillsExec,
                ConfigDomain::Skills,
                ConfigDomain::Notification,
            ],
        });

        assert_eq!(plan.version, 7);
        assert_eq!(
            plan.live,
            vec![
                RuntimeConfigTarget::InputPipeline,
                RuntimeConfigTarget::LlmRouter
            ]
        );
        assert_eq!(plan.restart_required, vec![RuntimeConfigTarget::Skills]);
        assert!(plan.contains(RuntimeConfigTarget::LlmRouter));
        assert!(plan.contains(RuntimeConfigTarget::Skills));
    }
}
