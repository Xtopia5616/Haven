//! Runtime application planning for versioned configuration changes.
//!
//! `haven-common::config::ConfigService` owns durable snapshots. This module
//! stays in the application composition root because only the app knows which
//! live components can be rebuilt and which settings require a restart.

use haven_common::config::{ConfigChanged, ConfigDomain, LogLevel};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

/// Apply a configured log level to every reloadable application filter.
///
/// The first reload failure is returned to the caller. Callers that own a
/// best-effort boundary can invoke this with one handle at a time and decide
/// how to report each failure.
pub(crate) fn apply_log_level_to_handles(
    handles: &[reload::Handle<EnvFilter, Registry>],
    level: &LogLevel,
) -> anyhow::Result<()> {
    for handle in handles {
        handle.modify(|current| {
            *current = EnvFilter::new(format!("haven={}", level.as_str()));
        })?;
    }
    Ok(())
}

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
                ConfigDomain::ContextLimits => {
                    plan.push_live(RuntimeConfigTarget::ContextLimits);
                    plan.push_live(RuntimeConfigTarget::LlmRouter);
                }
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
    use tracing_subscriber::reload;

    #[test]
    fn plan_deduplicates_targets_and_marks_restart_boundaries() {
        let plan = RuntimeConfigApplyPlan::from_change(&ConfigChanged {
            version: 7,
            domains: vec![
                ConfigDomain::Media,
                ConfigDomain::Llm,
                ConfigDomain::ContextLimits,
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
                RuntimeConfigTarget::LlmRouter,
                RuntimeConfigTarget::ContextLimits
            ]
        );
        assert_eq!(plan.restart_required, vec![RuntimeConfigTarget::Skills]);
        assert!(plan.contains(RuntimeConfigTarget::LlmRouter));
        assert!(plan.contains(RuntimeConfigTarget::Skills));
    }

    #[test]
    fn context_limits_alone_refresh_context_consumers_and_router_live() {
        let plan = RuntimeConfigApplyPlan::from_change(&ConfigChanged {
            version: 8,
            domains: vec![ConfigDomain::ContextLimits],
        });

        assert_eq!(plan.version, 8);
        assert_eq!(
            plan.live,
            vec![
                RuntimeConfigTarget::ContextLimits,
                RuntimeConfigTarget::LlmRouter,
            ]
        );
        assert!(plan.restart_required.is_empty());
    }

    #[test]
    fn applies_log_level_to_every_reload_handle() {
        let (_layer_one, handle_one): (
            reload::Layer<EnvFilter, Registry>,
            reload::Handle<EnvFilter, Registry>,
        ) = reload::Layer::new(EnvFilter::new("haven=off"));
        let (_layer_two, handle_two): (
            reload::Layer<EnvFilter, Registry>,
            reload::Handle<EnvFilter, Registry>,
        ) = reload::Layer::new(EnvFilter::new("haven=error"));
        let handles = vec![handle_one, handle_two];
        let level = LogLevel::Debug;

        apply_log_level_to_handles(&handles, &level).unwrap();

        let expected = EnvFilter::new(format!("haven={}", level.as_str())).to_string();
        for handle in &handles {
            assert_eq!(
                handle.with_current(|filter| filter.to_string()).unwrap(),
                expected
            );
        }
    }
}
