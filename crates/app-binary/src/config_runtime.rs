//! Runtime application planning for versioned configuration changes.
//!
//! `haven-common::config::ConfigService` owns durable snapshots. This module
//! stays in the application composition root because only the app knows which
//! live components can be rebuilt and which settings require a restart.

use crate::app_state::AppState;
use crate::logging::log_err;
use crate::router_media_builder::build_router_media;
use haven_common::config::{
    AppConfig, ConfigChanged, ConfigDomain, ConfigService, ConfigSnapshot, ConfigVersion, LogLevel,
};
use std::future::Future;
use std::sync::Arc;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

/// One serialization boundary for configuration writes that immediately
/// rebuild and publish live runtime state.
#[derive(Default)]
pub(crate) struct RuntimeConfigCoordinator {
    apply_gate: Arc<tokio::sync::Mutex<()>>,
    settings_apply_state: Arc<std::sync::Mutex<SettingsRuntimeApplyState>>,
}

impl RuntimeConfigCoordinator {
    pub(crate) fn with_shared_gate(apply_gate: Arc<tokio::sync::Mutex<()>>) -> Self {
        Self {
            apply_gate,
            settings_apply_state: Arc::new(std::sync::Mutex::new(
                SettingsRuntimeApplyState::default(),
            )),
        }
    }

    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.apply_gate.lock().await
    }

    pub(crate) async fn lock_owned(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.apply_gate.clone().lock_owned().await
    }

    /// Commit a model configuration mutation and apply the committed router
    /// generation while holding the shared settings/model serialization gate.
    /// The mutation stays a closure so this app-level coordinator does not
    /// depend on command selectors or model-field details.
    pub(crate) async fn edit_model_and_apply(
        &self,
        state: &AppState,
        ctx: &str,
        edit: impl FnOnce(&mut AppConfig) -> anyhow::Result<()>,
    ) -> Result<(), String> {
        self.edit_model_and_apply_with(
            &state.runtime.config_service,
            ctx,
            edit,
            |snapshot| async move { self.apply_router_runtime(state, &snapshot, ctx).await },
        )
        .await
    }

    /// Injectable apply edge used by the production helper and its focused
    /// concurrency/no-op tests. The apply callback is invoked only for a
    /// committed change whose plan includes the LLM router.
    async fn edit_model_and_apply_with<T, Edit, Apply, ApplyFuture>(
        &self,
        config_service: &ConfigService,
        ctx: &str,
        edit: Edit,
        apply_router: Apply,
    ) -> Result<T, String>
    where
        Edit: FnOnce(&mut AppConfig) -> anyhow::Result<T>,
        Apply: FnOnce(ConfigSnapshot) -> ApplyFuture,
        ApplyFuture: Future<Output = Result<(), String>>,
    {
        let _apply_guard = self.lock().await;
        let update = config_service
            .edit(edit)
            .map_err(|error| log_err(ctx, error))?;
        let should_apply_router = update.change.as_ref().is_some_and(|change| {
            RuntimeConfigApplyPlan::from_change(change).contains(RuntimeConfigTarget::LlmRouter)
        });
        if should_apply_router {
            let snapshot = update.snapshot;
            let change = update
                .change
                .expect("router apply requires a config change");
            let plan = SettingsRuntimeApplyPlan::from_change(&change, &snapshot, "");
            self.begin_settings_apply(&plan, "");
            match apply_router(snapshot).await {
                Ok(()) => self.finish_settings_apply(&plan, plan.phases(), None),
                Err(error) => {
                    let reason = partial_router_config_apply_error(&error);
                    self.finish_settings_apply(
                        &plan,
                        &[],
                        Some((SettingsRuntimeApplyPhase::RouterPrepare, reason.clone())),
                    );
                    return Err(log_err(ctx, reason));
                }
            }
        }
        Ok(update.value)
    }

    pub(crate) fn begin_settings_apply(&self, plan: &SettingsRuntimeApplyPlan, old_hotkey: &str) {
        self.settings_apply_state
            .lock()
            .expect("settings apply state mutex poisoned")
            .begin(plan, old_hotkey);
    }

    pub(crate) fn finish_settings_apply(
        &self,
        plan: &SettingsRuntimeApplyPlan,
        completed_phases: &[SettingsRuntimeApplyPhase],
        failure: Option<(SettingsRuntimeApplyPhase, String)>,
    ) {
        self.settings_apply_state
            .lock()
            .expect("settings apply state mutex poisoned")
            .finish(plan, completed_phases, failure);
    }

    pub(crate) fn settings_apply_statuses(&self) -> Vec<SettingsRuntimeTargetStatus> {
        self.settings_apply_state
            .lock()
            .expect("settings apply state mutex poisoned")
            .statuses()
    }

    /// Build a retry plan from the latest durable snapshot. Pending phase sets
    /// were captured from failed saves so retries do not repeat targets that
    /// already completed.
    pub(crate) fn prepare_settings_reapply(
        &self,
        snapshot: &ConfigSnapshot,
    ) -> Option<(SettingsRuntimeApplyPlan, String)> {
        self.settings_apply_state
            .lock()
            .expect("settings apply state mutex poisoned")
            .prepare_reapply(snapshot)
    }

    /// Prepare router and media clients from the exact committed snapshot.
    /// Callers hold this coordinator's gate across config edit and apply.
    pub(crate) fn prepare_router_runtime(
        &self,
        state: &AppState,
        snapshot: &ConfigSnapshot,
        ctx: &str,
    ) -> Result<PreparedRouterRuntime, String> {
        let mcp_caller: Arc<dyn haven_llm::McpToolCaller> =
            Arc::clone(&state.runtime.services.mcp_tool_caller);
        Self::prepare_router_from_snapshot(
            snapshot,
            Some(mcp_caller),
            Some(state.runtime.chatgpt_plan_auth.clone()),
            ctx,
        )
    }

    /// Publish one fully prepared router/media generation through the shared
    /// handle, then rebuild the derived Tools catalog. In-flight consumers
    /// keep their captured generation if the pointer advances.
    pub(crate) async fn publish_router_runtime(
        &self,
        state: &AppState,
        prepared: PreparedRouterRuntime,
    ) -> Result<(), RouterRuntimePublishError> {
        tracing::debug!(
            config_version = prepared.config_version,
            "publishing prepared router runtime"
        );
        state
            .runtime
            .model_media_publisher
            .publish(prepared.snapshot.clone())
            .map_err(RouterRuntimePublishError::ModelMedia)?;
        state.runtime.agent.prewarm_current_router();
        state
            .runtime
            .tools
            .rebuild_model_media_catalog()
            .await
            .map_err(|source| RouterRuntimePublishError::ToolCatalog {
                config_version: prepared.config_version,
                source,
            })?;
        Ok(())
    }

    /// Model updates have no intervening live side effects, so use the complete
    /// prepare/publish operation. Settings use the two stages separately to
    /// preserve their existing security, MCP, and context update order.
    pub(crate) async fn apply_router_runtime(
        &self,
        state: &AppState,
        snapshot: &ConfigSnapshot,
        ctx: &str,
    ) -> Result<(), String> {
        let prepared = self
            .prepare_router_runtime(state, snapshot, ctx)
            .map_err(|error| log_err(ctx, error))?;
        self.publish_router_runtime(state, prepared)
            .await
            .map_err(|error| log_err(ctx, error))
    }

    /// Keep preparation and publication as one fallible boundary: `apply` is
    /// never invoked when constructing the replacement fails.
    #[cfg(test)]
    async fn prepare_then_publish<T, E, Prepare, Apply, ApplyFuture>(
        prepare: Prepare,
        apply: Apply,
    ) -> Result<(), E>
    where
        Prepare: FnOnce() -> Result<T, E>,
        Apply: FnOnce(T) -> ApplyFuture,
        ApplyFuture: Future<Output = Result<(), E>>,
    {
        let prepared = prepare()?;
        apply(prepared).await
    }

    fn prepare_router_from_snapshot(
        snapshot: &ConfigSnapshot,
        mcp_caller: Option<Arc<dyn haven_llm::McpToolCaller>>,
        chatgpt_plan_auth: Option<Arc<dyn haven_llm::ChatGptPlanAuthPort>>,
        ctx: &str,
    ) -> Result<PreparedRouterRuntime, String> {
        let built = build_router_media(&snapshot.config, mcp_caller, chatgpt_plan_auth);
        let stt_client = built
            .stt_client
            .map_err(|error| log_err(&format!("{ctx} STT"), error))?;
        let ocr_client = built
            .ocr_client
            .map_err(|error| log_err(&format!("{ctx} OCR"), error))?;
        let tts_client = built
            .tts_client
            .map_err(|error| log_err(&format!("{ctx} TTS"), error))?;
        let image_gen_client = built
            .image_gen_client
            .map_err(|error| log_err(&format!("{ctx} image generation"), error))?;

        let model_media =
            haven_llm::ModelMediaSnapshot::new(snapshot.version, built.router, built.media)
                .with_clients(stt_client, ocr_client, image_gen_client, tts_client);
        Ok(PreparedRouterRuntime {
            config_version: snapshot.version,
            snapshot: model_media,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RouterRuntimePublishError {
    #[error("model/media generation publication failed: {0}")]
    ModelMedia(#[source] anyhow::Error),
    #[error(
        "model/media generation config_version={config_version} was published atomically, but the derived tool catalog rebuild failed: {source}"
    )]
    ToolCatalog {
        config_version: ConfigVersion,
        #[source]
        source: haven_tools::CatalogRebuildError,
    },
}

pub(crate) fn partial_config_apply_error(
    error: impl std::fmt::Display,
    config_version: ConfigVersion,
    phase: &str,
    security_runtime: &str,
    security_base_version: ConfigVersion,
) -> String {
    format!(
        "部分 apply 失败：配置已写入（config_version={config_version}; phase={phase}; security_runtime={security_runtime}; security_base_version={security_base_version}）；可在设置页重试未应用目标；重启应用后会从配置重新初始化。{error}"
    )
}

fn partial_router_config_apply_error(error: impl std::fmt::Display) -> String {
    format!(
        "部分 apply 失败：配置已写入；可在设置页重试未应用目标；重启应用后会从配置重新初始化。{error}"
    )
}

/// Complete router and media runtime derived from one immutable snapshot.
/// Construction may fail; publication only accepts this prepared value.
pub(crate) struct PreparedRouterRuntime {
    config_version: ConfigVersion,
    snapshot: haven_llm::ModelMediaSnapshot,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigTarget {
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
    MemoryInference,
    SkillsRestart,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsRuntimeTargetStatus {
    pub target: RuntimeConfigTarget,
    pub expected_config_version: u64,
    /// Last fully applied process-local config version; ConfigService starts at 0.
    pub actual_config_version: u64,
    pub result: SettingsRuntimeApplyResult,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsRuntimeApplyResult {
    Applied,
    Pending,
    Retryable,
    PendingRestart,
}

#[derive(Default)]
struct SettingsRuntimeApplyState {
    statuses: std::collections::HashMap<RuntimeConfigTarget, SettingsRuntimeTargetStatus>,
    pending: std::collections::HashMap<RuntimeConfigTarget, PendingSettingsTarget>,
}

#[derive(Clone)]
struct PendingSettingsTarget {
    phases: Vec<SettingsRuntimeApplyPhase>,
    old_hotkey: String,
}

const RUNTIME_CONFIG_TARGET_ORDER: [RuntimeConfigTarget; 13] = [
    RuntimeConfigTarget::InputPipeline,
    RuntimeConfigTarget::Shell,
    RuntimeConfigTarget::ContextLimits,
    RuntimeConfigTarget::LlmRouter,
    RuntimeConfigTarget::SessionRuntime,
    RuntimeConfigTarget::Mcp,
    RuntimeConfigTarget::Security,
    RuntimeConfigTarget::Logging,
    RuntimeConfigTarget::Hotkey,
    RuntimeConfigTarget::Skills,
    RuntimeConfigTarget::ToolSettings,
    RuntimeConfigTarget::MemoryInference,
    RuntimeConfigTarget::SkillsRestart,
];

impl SettingsRuntimeApplyState {
    fn begin(&mut self, plan: &SettingsRuntimeApplyPlan, old_hotkey: &str) {
        for target in &plan.live_targets {
            let phases = plan
                .phases()
                .iter()
                .copied()
                .filter(|phase| phase.target() == *target)
                .collect::<Vec<_>>();
            let actual = self.actual_config_version(*target);
            self.set_status(
                *target,
                plan.config_version,
                SettingsRuntimeApplyResult::Pending,
                actual,
                None,
            );
            if phases.is_empty() {
                self.pending.remove(target);
                self.set_status(
                    *target,
                    plan.config_version,
                    SettingsRuntimeApplyResult::Applied,
                    plan.config_version,
                    None,
                );
            } else {
                self.pending.insert(
                    *target,
                    PendingSettingsTarget {
                        phases,
                        old_hotkey: old_hotkey.to_owned(),
                    },
                );
            }
        }
        for target in &plan.restart_required_targets {
            self.pending.remove(target);
            let actual = self.actual_config_version(*target);
            self.set_status(
                *target,
                plan.config_version,
                SettingsRuntimeApplyResult::PendingRestart,
                actual,
                None,
            );
        }
    }

    fn finish(
        &mut self,
        plan: &SettingsRuntimeApplyPlan,
        completed_phases: &[SettingsRuntimeApplyPhase],
        failure: Option<(SettingsRuntimeApplyPhase, String)>,
    ) {
        let failed_target = failure.as_ref().map(|(phase, _)| phase.target());
        for target in &plan.live_targets {
            let Some(pending) = self.pending.get_mut(target) else {
                continue;
            };
            pending
                .phases
                .retain(|phase| !completed_phases.contains(phase));
            let remaining = !pending.phases.is_empty();
            if !remaining {
                self.pending.remove(target);
                self.set_status(
                    *target,
                    plan.config_version,
                    SettingsRuntimeApplyResult::Applied,
                    plan.config_version,
                    None,
                );
            } else if failed_target == Some(*target) {
                let reason = failure
                    .as_ref()
                    .map(|(_, reason)| haven_common::error::sanitize_error_text(reason));
                let actual = self.actual_config_version(*target);
                self.set_status(
                    *target,
                    plan.config_version,
                    SettingsRuntimeApplyResult::Retryable,
                    actual,
                    reason,
                );
            } else {
                let actual = self.actual_config_version(*target);
                self.set_status(
                    *target,
                    plan.config_version,
                    SettingsRuntimeApplyResult::Pending,
                    actual,
                    None,
                );
            }
        }
    }

    fn prepare_reapply(
        &mut self,
        snapshot: &ConfigSnapshot,
    ) -> Option<(SettingsRuntimeApplyPlan, String)> {
        if self.pending.is_empty() {
            return None;
        }
        let targets = RUNTIME_CONFIG_TARGET_ORDER
            .into_iter()
            .filter(|target| self.pending.contains_key(target))
            .collect::<Vec<_>>();
        for target in &targets {
            let actual = self.actual_config_version(*target);
            self.set_status(
                *target,
                snapshot.version,
                SettingsRuntimeApplyResult::Pending,
                actual,
                None,
            );
        }

        let mut phases = Vec::new();
        let mut old_hotkey = None;
        for target in &targets {
            if let Some(pending) = self.pending.get(target) {
                phases.extend(pending.phases.iter().copied());
                if *target == RuntimeConfigTarget::Hotkey {
                    old_hotkey = Some(pending.old_hotkey.clone());
                }
            }
        }
        if targets.contains(&RuntimeConfigTarget::LlmRouter)
            && phases.contains(&SettingsRuntimeApplyPhase::RouterPublish)
            && !phases.contains(&SettingsRuntimeApplyPhase::RouterPrepare)
        {
            phases.push(SettingsRuntimeApplyPhase::RouterPrepare);
        }
        phases.sort_by_key(|phase| {
            SETTINGS_RUNTIME_APPLY_PHASE_ORDER
                .iter()
                .position(|candidate| candidate == phase)
                .unwrap_or(usize::MAX)
        });
        phases.dedup();

        Some((
            SettingsRuntimeApplyPlan {
                config_version: snapshot.version,
                live_targets: targets,
                restart_required_targets: Vec::new(),
                phases,
            },
            old_hotkey.unwrap_or_else(|| snapshot.config.hotkey.key_binding.clone()),
        ))
    }

    fn statuses(&self) -> Vec<SettingsRuntimeTargetStatus> {
        RUNTIME_CONFIG_TARGET_ORDER
            .iter()
            .filter_map(|target| self.statuses.get(target).cloned())
            .collect()
    }

    fn set_status(
        &mut self,
        target: RuntimeConfigTarget,
        expected_config_version: u64,
        result: SettingsRuntimeApplyResult,
        actual_config_version: u64,
        reason: Option<String>,
    ) {
        self.statuses.insert(
            target,
            SettingsRuntimeTargetStatus {
                target,
                expected_config_version,
                actual_config_version,
                result,
                reason,
            },
        );
    }

    fn actual_config_version(&self, target: RuntimeConfigTarget) -> u64 {
        self.statuses
            .get(&target)
            .map(|status| status.actual_config_version)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeConfigApplyPlan {
    pub(crate) config_version: ConfigVersion,
    pub(crate) live: Vec<RuntimeConfigTarget>,
    pub(crate) restart_required: Vec<RuntimeConfigTarget>,
}

impl RuntimeConfigApplyPlan {
    pub(crate) fn from_change(change: &ConfigChanged) -> Self {
        let mut plan = Self {
            config_version: change.version,
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
                ConfigDomain::SkillsExec => plan.push_restart(RuntimeConfigTarget::SkillsRestart),
                ConfigDomain::Memory => plan.push_live(RuntimeConfigTarget::MemoryInference),
                ConfigDomain::Notification => {
                    // Notification settings are read from the current
                    // snapshot by the notification sink; no rebuild is needed.
                }
                ConfigDomain::Hooks => {
                    // LifecycleHookService reads the current config snapshot per event.
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
        if !self.live.contains(&target) {
            self.live.push(target);
        }
    }

    fn push_restart(&mut self, target: RuntimeConfigTarget) {
        if !self.restart_required.contains(&target) {
            self.restart_required.push(target);
        }
    }
}

/// A named point in the settings runtime-apply sequence. These phases describe
/// ordering and observability only; they do not imply rollback boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SettingsRuntimeApplyPhase {
    RouterPrepare,
    InputPipeline,
    Shell,
    Security,
    McpConfig,
    McpMonitors,
    RouterPublish,
    ContextLimits,
    FactInference,
    SessionRuntime,
    ToolSettings,
    Skills,
    Logging,
    HotkeyMode,
    HotkeyUnregister,
    HotkeyRegister,
    HotkeyRebindEvent,
}

impl SettingsRuntimeApplyPhase {
    fn target(self) -> RuntimeConfigTarget {
        match self {
            Self::RouterPrepare | Self::RouterPublish => RuntimeConfigTarget::LlmRouter,
            Self::InputPipeline => RuntimeConfigTarget::InputPipeline,
            Self::Shell => RuntimeConfigTarget::Shell,
            Self::Security => RuntimeConfigTarget::Security,
            Self::McpConfig | Self::McpMonitors => RuntimeConfigTarget::Mcp,
            Self::ContextLimits => RuntimeConfigTarget::ContextLimits,
            Self::FactInference => RuntimeConfigTarget::MemoryInference,
            Self::SessionRuntime => RuntimeConfigTarget::SessionRuntime,
            Self::ToolSettings => RuntimeConfigTarget::ToolSettings,
            Self::Skills => RuntimeConfigTarget::Skills,
            Self::Logging => RuntimeConfigTarget::Logging,
            Self::HotkeyMode
            | Self::HotkeyUnregister
            | Self::HotkeyRegister
            | Self::HotkeyRebindEvent => RuntimeConfigTarget::Hotkey,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::RouterPrepare => "router_prepare",
            Self::InputPipeline => "input_pipeline",
            Self::Shell => "shell",
            Self::Security => "security",
            Self::McpConfig => "mcp_config",
            Self::McpMonitors => "mcp_monitors",
            Self::RouterPublish => "router_publish",
            Self::ContextLimits => "context_limits",
            Self::FactInference => "fact_inference",
            Self::SessionRuntime => "session_runtime",
            Self::ToolSettings => "tool_settings",
            Self::Skills => "skills",
            Self::Logging => "logging",
            Self::HotkeyMode => "hotkey_mode",
            Self::HotkeyUnregister => "hotkey_unregister",
            Self::HotkeyRegister => "hotkey_register",
            Self::HotkeyRebindEvent => "hotkey_rebind_event",
        }
    }

    fn requires_hotkey_binding_change(self) -> bool {
        matches!(
            self,
            Self::HotkeyUnregister | Self::HotkeyRegister | Self::HotkeyRebindEvent
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsRuntimeApplyFailureKind {
    RouterPrepare,
    RuntimeOwner,
    ToolCatalogRebuild,
}

impl SettingsRuntimeApplyFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RouterPrepare => "router_prepare",
            Self::RuntimeOwner => "runtime_owner",
            Self::ToolCatalogRebuild => "tool_catalog_rebuild",
        }
    }
}

const SETTINGS_RUNTIME_APPLY_PHASE_ORDER: [SettingsRuntimeApplyPhase; 17] = [
    SettingsRuntimeApplyPhase::RouterPrepare,
    SettingsRuntimeApplyPhase::InputPipeline,
    SettingsRuntimeApplyPhase::Shell,
    SettingsRuntimeApplyPhase::Security,
    SettingsRuntimeApplyPhase::McpConfig,
    SettingsRuntimeApplyPhase::McpMonitors,
    SettingsRuntimeApplyPhase::RouterPublish,
    SettingsRuntimeApplyPhase::ContextLimits,
    SettingsRuntimeApplyPhase::FactInference,
    SettingsRuntimeApplyPhase::SessionRuntime,
    SettingsRuntimeApplyPhase::ToolSettings,
    SettingsRuntimeApplyPhase::Skills,
    SettingsRuntimeApplyPhase::Logging,
    SettingsRuntimeApplyPhase::HotkeyMode,
    SettingsRuntimeApplyPhase::HotkeyUnregister,
    SettingsRuntimeApplyPhase::HotkeyRegister,
    SettingsRuntimeApplyPhase::HotkeyRebindEvent,
];

/// Typed settings targets and ordered stages derived from the shared runtime
/// target map. The snapshot version is authoritative for apply diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsRuntimeApplyPlan {
    pub(crate) config_version: ConfigVersion,
    pub(crate) live_targets: Vec<RuntimeConfigTarget>,
    pub(crate) restart_required_targets: Vec<RuntimeConfigTarget>,
    phases: Vec<SettingsRuntimeApplyPhase>,
}

impl SettingsRuntimeApplyPlan {
    pub(crate) fn from_change(
        change: &ConfigChanged,
        snapshot: &ConfigSnapshot,
        old_hotkey: &str,
    ) -> Self {
        let runtime_plan = RuntimeConfigApplyPlan::from_change(change);
        debug_assert_eq!(runtime_plan.config_version, snapshot.version);
        let hotkey_binding_changed = old_hotkey != snapshot.config.hotkey.key_binding;
        let phases = SETTINGS_RUNTIME_APPLY_PHASE_ORDER
            .into_iter()
            .filter(|phase| {
                runtime_plan.live.contains(&phase.target())
                    && (!phase.requires_hotkey_binding_change() || hotkey_binding_changed)
            })
            .collect();

        Self {
            config_version: snapshot.version,
            live_targets: runtime_plan.live,
            restart_required_targets: runtime_plan.restart_required,
            phases,
        }
    }

    pub(crate) fn phases(&self) -> &[SettingsRuntimeApplyPhase] {
        &self.phases
    }
}

/// Safe metadata captured for the active settings runtime-apply phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsRuntimeApplyObservation {
    pub(crate) config_version: ConfigVersion,
    pub(crate) phase: SettingsRuntimeApplyPhase,
    pub(crate) failure_kind: Option<SettingsRuntimeApplyFailureKind>,
    pub(crate) router_published: bool,
    pub(crate) restart_required_targets: Vec<RuntimeConfigTarget>,
}

impl SettingsRuntimeApplyObservation {
    fn record(
        &self,
        command: &str,
        error: &dyn std::fmt::Display,
        failure_kind: Option<SettingsRuntimeApplyFailureKind>,
    ) {
        let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
        if let Some(failure_kind) = failure_kind {
            tracing::error!(
                command,
                config_version = self.config_version,
                phase = self.phase.as_str(),
                failure_kind = failure_kind.as_str(),
                router_published = self.router_published,
                restart_required = !self.restart_required_targets.is_empty(),
                restart_required_targets = ?self.restart_required_targets,
                error = %safe_error,
                "settings runtime apply failed"
            );
        } else {
            tracing::warn!(
                command,
                config_version = self.config_version,
                phase = self.phase.as_str(),
                router_published = self.router_published,
                restart_required = !self.restart_required_targets.is_empty(),
                restart_required_targets = ?self.restart_required_targets,
                error = %safe_error,
                "settings runtime apply warning"
            );
        }
    }
}

/// Result of one phase callback. The coordinator owns error rendering and
/// structured failure/warning logging; a pre-rendered error is used only for
/// Router preparation, whose builder already crosses `log_err` boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsRuntimeApplyOutcome {
    Applied,
    Failed {
        command: &'static str,
        error: String,
        already_rendered: bool,
        failure_kind: SettingsRuntimeApplyFailureKind,
        router_published: bool,
    },
    Warning {
        command: &'static str,
        error: String,
    },
}

impl SettingsRuntimeApplyOutcome {
    pub(crate) fn applied() -> Self {
        Self::Applied
    }

    pub(crate) fn failed(command: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Failed {
            command,
            error: error.to_string(),
            already_rendered: false,
            failure_kind: SettingsRuntimeApplyFailureKind::RuntimeOwner,
            router_published: false,
        }
    }

    pub(crate) fn failed_catalog_rebuild(
        command: &'static str,
        error: impl std::fmt::Display,
    ) -> Self {
        Self::Failed {
            command,
            error: error.to_string(),
            already_rendered: false,
            failure_kind: SettingsRuntimeApplyFailureKind::ToolCatalogRebuild,
            router_published: false,
        }
    }

    pub(crate) fn failed_catalog_rebuild_after_router_publish(
        command: &'static str,
        error: impl std::fmt::Display,
    ) -> Self {
        Self::Failed {
            command,
            error: error.to_string(),
            already_rendered: false,
            failure_kind: SettingsRuntimeApplyFailureKind::ToolCatalogRebuild,
            router_published: true,
        }
    }

    pub(crate) fn failed_already_rendered(command: &'static str, error: String) -> Self {
        Self::Failed {
            command,
            error,
            already_rendered: true,
            failure_kind: SettingsRuntimeApplyFailureKind::RouterPrepare,
            router_published: false,
        }
    }

    pub(crate) fn warning(command: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Warning {
            command,
            error: error.to_string(),
        }
    }
}

/// Owns the settings phase sequence and its failure observations. Side effects
/// remain in their existing runtime owners and are supplied as phase callbacks.
pub(crate) struct SettingsRuntimeApplyCoordinator {
    plan: SettingsRuntimeApplyPlan,
    phase: Option<SettingsRuntimeApplyPhase>,
    failure_kind: Option<SettingsRuntimeApplyFailureKind>,
    router_published: bool,
    completed_phases: Vec<SettingsRuntimeApplyPhase>,
}

impl SettingsRuntimeApplyCoordinator {
    #[cfg(test)]
    pub(crate) fn new(change: &ConfigChanged, snapshot: &ConfigSnapshot, old_hotkey: &str) -> Self {
        Self {
            plan: SettingsRuntimeApplyPlan::from_change(change, snapshot, old_hotkey),
            phase: None,
            failure_kind: None,
            router_published: false,
            completed_phases: Vec::new(),
        }
    }

    pub(crate) fn from_plan(plan: SettingsRuntimeApplyPlan) -> Self {
        Self {
            plan,
            phase: None,
            failure_kind: None,
            router_published: false,
            completed_phases: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn plan(&self) -> &SettingsRuntimeApplyPlan {
        &self.plan
    }

    pub(crate) fn failed_phase_name(&self) -> Option<&'static str> {
        self.phase.map(SettingsRuntimeApplyPhase::as_str)
    }

    pub(crate) fn failed_phase(&self) -> Option<SettingsRuntimeApplyPhase> {
        self.phase
            .filter(|phase| !self.completed_phases.contains(phase))
    }

    pub(crate) fn completed_phases(&self) -> &[SettingsRuntimeApplyPhase] {
        &self.completed_phases
    }

    /// State of the security consumer after the most recent failed phase.
    /// A failure in the Security phase happens after static config publication
    /// but before all durable session grants have been restored, so it is
    /// reported separately from a failure before or after that phase.
    pub(crate) fn security_runtime_disposition(&self) -> &'static str {
        let Some(failed_phase) = self.phase else {
            return "unknown";
        };
        if failed_phase == SettingsRuntimeApplyPhase::Security {
            return "incomplete_fail_closed";
        }
        let phases = self.plan.phases();
        let Some(security_index) = phases
            .iter()
            .position(|phase| *phase == SettingsRuntimeApplyPhase::Security)
        else {
            return "unchanged";
        };
        let Some(failed_index) = phases.iter().position(|phase| *phase == failed_phase) else {
            return "unknown";
        };
        if failed_index < security_index {
            "unchanged"
        } else {
            "applied"
        }
    }

    /// Execute callbacks in the typed plan order. A failure stops later stages,
    /// preserving the existing partial-apply behavior.
    pub(crate) async fn apply<F, Fut>(&mut self, mut execute: F) -> Result<(), String>
    where
        F: FnMut(SettingsRuntimeApplyPhase) -> Fut,
        Fut: Future<Output = SettingsRuntimeApplyOutcome>,
    {
        for phase in self.plan.phases().iter().copied() {
            self.phase = Some(phase);
            self.failure_kind = None;
            match execute(phase).await {
                SettingsRuntimeApplyOutcome::Applied => {
                    self.completed_phases.push(phase);
                    if phase == SettingsRuntimeApplyPhase::RouterPublish {
                        self.router_published = true;
                    }
                }
                SettingsRuntimeApplyOutcome::Failed {
                    command,
                    error,
                    already_rendered,
                    failure_kind,
                    router_published,
                } => {
                    self.router_published |= router_published;
                    self.failure_kind = Some(failure_kind);
                    let rendered = if already_rendered {
                        error
                    } else {
                        log_err(command, error)
                    };
                    self.observation(phase)
                        .record(command, &rendered, Some(failure_kind));
                    return Err(rendered);
                }
                SettingsRuntimeApplyOutcome::Warning { command, error } => {
                    self.observation(phase).record(command, &error, None);
                    self.completed_phases.push(phase);
                }
            }
        }
        Ok(())
    }

    fn observation(&self, phase: SettingsRuntimeApplyPhase) -> SettingsRuntimeApplyObservation {
        debug_assert_eq!(self.phase, Some(phase));
        SettingsRuntimeApplyObservation {
            config_version: self.plan.config_version,
            phase,
            failure_kind: self.failure_kind,
            router_published: self.router_published,
            restart_required_targets: self.plan.restart_required_targets.clone(),
        }
    }

    #[cfg(test)]
    fn current_phase(&self) -> Option<SettingsRuntimeApplyPhase> {
        self.phase
    }

    #[cfg(test)]
    fn router_published(&self) -> bool {
        self.router_published
    }

    #[cfg(test)]
    fn current_observation(&self) -> Option<SettingsRuntimeApplyObservation> {
        self.phase.map(|phase| self.observation(phase))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ConfigLoader, InMemoryCredentialStore, ModelConfig};
    use tracing_subscriber::reload;

    fn test_config_service(loader: ConfigLoader) -> ConfigService {
        ConfigService::new_with_credential_store(
            loader,
            std::sync::Arc::new(InMemoryCredentialStore::default()),
        )
        .unwrap()
    }

    #[test]
    fn partial_apply_error_explains_config_write_and_restart_recovery() {
        let error = partial_config_apply_error("skills refresh failed", 9, "skills", "applied", 9);
        assert!(error.starts_with("部分 apply 失败：配置已写入"));
        assert!(error.contains("config_version=9; phase=skills; security_runtime=applied"));
        assert!(error.contains("可在设置页重试未应用目标"));
        assert!(error.contains("重启应用后会从配置重新初始化"));
        assert!(error.ends_with("skills refresh failed"));
    }

    #[tokio::test]
    async fn security_runtime_disposition_identifies_the_effective_state_after_failure() {
        let snapshot = ConfigSnapshot {
            version: 9,
            config: AppConfig::default(),
        };

        let before_security = ConfigChanged {
            version: 9,
            domains: vec![ConfigDomain::Media, ConfigDomain::Security],
        };
        let mut coordinator = SettingsRuntimeApplyCoordinator::new(&before_security, &snapshot, "");
        let _ = coordinator
            .apply(|phase| async move {
                if phase == SettingsRuntimeApplyPhase::InputPipeline {
                    SettingsRuntimeApplyOutcome::failed("settings_test", "input pipeline failed")
                } else {
                    SettingsRuntimeApplyOutcome::applied()
                }
            })
            .await;
        assert_eq!(coordinator.security_runtime_disposition(), "unchanged");

        let security_failure = ConfigChanged {
            version: 9,
            domains: vec![ConfigDomain::Security],
        };
        let mut coordinator =
            SettingsRuntimeApplyCoordinator::new(&security_failure, &snapshot, "");
        let _ = coordinator
            .apply(|phase| async move {
                if phase == SettingsRuntimeApplyPhase::Security {
                    SettingsRuntimeApplyOutcome::failed("settings_test", "grant restore failed")
                } else {
                    SettingsRuntimeApplyOutcome::applied()
                }
            })
            .await;
        assert_eq!(
            coordinator.security_runtime_disposition(),
            "incomplete_fail_closed"
        );

        let after_security = ConfigChanged {
            version: 9,
            domains: vec![ConfigDomain::Security, ConfigDomain::Tools],
        };
        let mut coordinator = SettingsRuntimeApplyCoordinator::new(&after_security, &snapshot, "");
        let _ = coordinator
            .apply(|phase| async move {
                if phase == SettingsRuntimeApplyPhase::ToolSettings {
                    SettingsRuntimeApplyOutcome::failed("settings_test", "tool settings failed")
                } else {
                    SettingsRuntimeApplyOutcome::applied()
                }
            })
            .await;
        assert_eq!(coordinator.security_runtime_disposition(), "applied");
    }

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

        assert_eq!(plan.config_version, 7);
        assert_eq!(
            plan.live,
            vec![
                RuntimeConfigTarget::InputPipeline,
                RuntimeConfigTarget::LlmRouter,
                RuntimeConfigTarget::ContextLimits,
                RuntimeConfigTarget::Skills,
            ]
        );
        assert_eq!(
            plan.restart_required,
            vec![RuntimeConfigTarget::SkillsRestart]
        );
        assert!(plan.contains(RuntimeConfigTarget::LlmRouter));
        assert!(plan.contains(RuntimeConfigTarget::Skills));
    }

    #[test]
    fn context_limits_alone_refresh_context_consumers_and_router_live() {
        let plan = RuntimeConfigApplyPlan::from_change(&ConfigChanged {
            version: 8,
            domains: vec![ConfigDomain::ContextLimits],
        });

        assert_eq!(plan.config_version, 8);
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
    fn reapply_status_tracks_target_versions_and_skips_completed_phases() {
        let first_snapshot = ConfigSnapshot {
            version: 9,
            config: AppConfig::default(),
        };
        let change = ConfigChanged {
            version: first_snapshot.version,
            domains: vec![
                ConfigDomain::Media,
                ConfigDomain::DefaultShell,
                ConfigDomain::Security,
                ConfigDomain::Llm,
            ],
        };
        let plan = SettingsRuntimeApplyPlan::from_change(&change, &first_snapshot, "");
        let mut state = SettingsRuntimeApplyState::default();
        state.begin(&plan, "");
        state.finish(
            &plan,
            &[
                SettingsRuntimeApplyPhase::RouterPrepare,
                SettingsRuntimeApplyPhase::InputPipeline,
            ],
            Some((
                SettingsRuntimeApplyPhase::Shell,
                "terminal runtime unavailable".to_owned(),
            )),
        );

        let input_status = state
            .statuses()
            .into_iter()
            .find(|status| status.target == RuntimeConfigTarget::InputPipeline)
            .unwrap();
        assert_eq!(input_status.result, SettingsRuntimeApplyResult::Applied);
        assert_eq!(input_status.expected_config_version, 9);
        assert_eq!(input_status.actual_config_version, 9);

        let latest_snapshot = ConfigSnapshot {
            version: 10,
            config: AppConfig::default(),
        };
        let (retry_plan, _) = state.prepare_reapply(&latest_snapshot).unwrap();
        assert_eq!(retry_plan.config_version, 10);
        assert_eq!(
            retry_plan.phases(),
            &[
                SettingsRuntimeApplyPhase::RouterPrepare,
                SettingsRuntimeApplyPhase::Shell,
                SettingsRuntimeApplyPhase::Security,
                SettingsRuntimeApplyPhase::RouterPublish,
            ]
        );
        assert!(
            !retry_plan
                .phases()
                .contains(&SettingsRuntimeApplyPhase::InputPipeline)
        );
        let shell_status = state
            .statuses()
            .into_iter()
            .find(|status| status.target == RuntimeConfigTarget::Shell)
            .unwrap();
        assert_eq!(shell_status.result, SettingsRuntimeApplyResult::Pending);
        assert_eq!(shell_status.expected_config_version, 10);
        assert_eq!(shell_status.actual_config_version, 0);
        state.finish(&retry_plan, retry_plan.phases(), None);
        let statuses = state.statuses();
        assert!(
            statuses
                .iter()
                .filter(|status| {
                    [
                        RuntimeConfigTarget::LlmRouter,
                        RuntimeConfigTarget::Shell,
                        RuntimeConfigTarget::Security,
                    ]
                    .contains(&status.target)
                })
                .all(|status| {
                    status.result == SettingsRuntimeApplyResult::Applied
                        && status.actual_config_version == 10
                })
        );
        assert_eq!(
            statuses
                .iter()
                .find(|status| status.target == RuntimeConfigTarget::InputPipeline)
                .unwrap()
                .actual_config_version,
            9
        );
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

    #[tokio::test]
    async fn settings_and_model_apply_operations_do_not_overlap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));

        let operation = |name: &'static str| {
            let coordinator = coordinator.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let order = order.clone();
            async move {
                let _guard = coordinator.lock().await;
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now_active, Ordering::SeqCst);
                order.lock().unwrap().push(format!("{name}:start"));
                tokio::task::yield_now().await;
                order.lock().unwrap().push(format!("{name}:finish"));
                active.fetch_sub(1, Ordering::SeqCst);
            }
        };

        tokio::join!(operation("settings"), operation("model"));

        let order = order.lock().unwrap();
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(order.len(), 4);
        assert_eq!(
            &order[0][..order[0].find(':').unwrap()],
            &order[1][..order[1].find(':').unwrap()]
        );
        assert_eq!(
            &order[2][..order[2].find(':').unwrap()],
            &order[3][..order[3].find(':').unwrap()]
        );
        assert_ne!(
            &order[0][..order[0].find(':').unwrap()],
            &order[2][..order[2].find(':').unwrap()]
        );
        assert!(order[0].ends_with(":start"));
        assert!(order[1].ends_with(":finish"));
        assert!(order[2].ends_with(":start"));
        assert!(order[3].ends_with(":finish"));
    }

    fn settings_change(version: ConfigVersion) -> ConfigChanged {
        ConfigChanged {
            version,
            domains: vec![
                ConfigDomain::Media,
                ConfigDomain::DefaultShell,
                ConfigDomain::Security,
                ConfigDomain::McpServers,
                ConfigDomain::McpDiscovery,
                ConfigDomain::Llm,
                ConfigDomain::ContextLimits,
                ConfigDomain::Session,
                ConfigDomain::Tools,
                ConfigDomain::Skills,
                ConfigDomain::Log,
                ConfigDomain::Hotkey,
                ConfigDomain::SkillsExec,
                ConfigDomain::Memory,
            ],
        }
    }

    fn settings_snapshot(version: ConfigVersion, hotkey: &str) -> ConfigSnapshot {
        let mut config = AppConfig::default();
        config.hotkey.key_binding = hotkey.into();
        ConfigSnapshot { version, config }
    }

    fn settings_coordinator(version: ConfigVersion) -> SettingsRuntimeApplyCoordinator {
        let change = settings_change(version);
        let snapshot = settings_snapshot(version, "Ctrl+Alt+N");
        SettingsRuntimeApplyCoordinator::new(&change, &snapshot, "Ctrl+Alt+O")
    }

    #[test]
    fn settings_plan_uses_shared_targets_and_declares_the_existing_phase_order() {
        let change = settings_change(18);
        let snapshot = settings_snapshot(18, "Ctrl+Alt+N");
        let plan = SettingsRuntimeApplyPlan::from_change(&change, &snapshot, "Ctrl+Alt+O");

        assert_eq!(plan.config_version, snapshot.version);
        assert_eq!(
            plan.phases(),
            &[
                SettingsRuntimeApplyPhase::RouterPrepare,
                SettingsRuntimeApplyPhase::InputPipeline,
                SettingsRuntimeApplyPhase::Shell,
                SettingsRuntimeApplyPhase::Security,
                SettingsRuntimeApplyPhase::McpConfig,
                SettingsRuntimeApplyPhase::McpMonitors,
                SettingsRuntimeApplyPhase::RouterPublish,
                SettingsRuntimeApplyPhase::ContextLimits,
                SettingsRuntimeApplyPhase::FactInference,
                SettingsRuntimeApplyPhase::SessionRuntime,
                SettingsRuntimeApplyPhase::ToolSettings,
                SettingsRuntimeApplyPhase::Skills,
                SettingsRuntimeApplyPhase::Logging,
                SettingsRuntimeApplyPhase::HotkeyMode,
                SettingsRuntimeApplyPhase::HotkeyUnregister,
                SettingsRuntimeApplyPhase::HotkeyRegister,
                SettingsRuntimeApplyPhase::HotkeyRebindEvent,
            ]
        );
        assert_eq!(
            plan.restart_required_targets,
            vec![RuntimeConfigTarget::SkillsRestart]
        );

        let unchanged_hotkey = settings_snapshot(18, "Ctrl+Alt+O");
        let plan = SettingsRuntimeApplyPlan::from_change(&change, &unchanged_hotkey, "Ctrl+Alt+O");
        assert!(
            plan.phases()
                .contains(&SettingsRuntimeApplyPhase::HotkeyMode)
        );
        assert!(
            !plan
                .phases()
                .contains(&SettingsRuntimeApplyPhase::HotkeyUnregister)
        );
        assert!(
            !plan
                .phases()
                .contains(&SettingsRuntimeApplyPhase::HotkeyRegister)
        );
        assert!(
            !plan
                .phases()
                .contains(&SettingsRuntimeApplyPhase::HotkeyRebindEvent)
        );
    }

    #[test]
    fn skills_exec_requires_restart_without_running_live_skills_phase() {
        let change = ConfigChanged {
            version: 21,
            domains: vec![ConfigDomain::SkillsExec],
        };
        let snapshot = settings_snapshot(21, "Ctrl+Alt+O");
        let plan = SettingsRuntimeApplyPlan::from_change(&change, &snapshot, "Ctrl+Alt+O");

        assert!(plan.live_targets.is_empty());
        assert_eq!(
            plan.restart_required_targets,
            vec![RuntimeConfigTarget::SkillsRestart]
        );
        assert!(plan.phases().is_empty());
    }

    #[test]
    fn mixed_skills_and_skills_exec_runs_live_skills_and_marks_restart() {
        let change = ConfigChanged {
            version: 22,
            domains: vec![ConfigDomain::SkillsExec, ConfigDomain::Skills],
        };
        let snapshot = settings_snapshot(22, "Ctrl+Alt+O");
        let plan = SettingsRuntimeApplyPlan::from_change(&change, &snapshot, "Ctrl+Alt+O");

        assert_eq!(plan.live_targets, vec![RuntimeConfigTarget::Skills]);
        assert_eq!(
            plan.restart_required_targets,
            vec![RuntimeConfigTarget::SkillsRestart]
        );
        assert_eq!(plan.phases(), &[SettingsRuntimeApplyPhase::Skills]);
    }

    #[tokio::test]
    async fn settings_coordinator_runs_in_order_and_tracks_success_and_failure_for_every_phase() {
        use std::sync::{Arc, Mutex};

        let mut successful = settings_coordinator(19);
        let expected_phases = successful.plan().phases().to_vec();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_callback = seen.clone();
        successful
            .apply(move |phase| {
                let seen = seen_by_callback.clone();
                async move {
                    seen.lock().unwrap().push(phase);
                    SettingsRuntimeApplyOutcome::applied()
                }
            })
            .await
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), expected_phases);
        assert_eq!(successful.current_phase(), expected_phases.last().copied());
        assert!(successful.router_published());
        let successful_metadata = successful.current_observation().unwrap();
        assert_eq!(successful_metadata.config_version, 19);
        assert_eq!(
            successful_metadata.phase,
            SettingsRuntimeApplyPhase::HotkeyRebindEvent
        );
        assert!(successful_metadata.router_published);
        assert_eq!(
            successful_metadata.restart_required_targets,
            vec![RuntimeConfigTarget::SkillsRestart]
        );

        for (failed_index, failed_phase) in expected_phases.iter().copied().enumerate() {
            if failed_phase == SettingsRuntimeApplyPhase::HotkeyRebindEvent {
                // This callback is warning-only in production; its warning
                // behavior has a separate regression test below.
                continue;
            }
            let mut coordinator = settings_coordinator(19);
            let seen = Arc::new(Mutex::new(Vec::new()));
            let seen_by_callback = seen.clone();
            let result = coordinator
                .apply(move |phase| {
                    let seen = seen_by_callback.clone();
                    async move {
                        seen.lock().unwrap().push(phase);
                        if phase == failed_phase {
                            if phase == SettingsRuntimeApplyPhase::RouterPrepare {
                                SettingsRuntimeApplyOutcome::failed_already_rendered(
                                    "settings_phase_test",
                                    "phase failed".into(),
                                )
                            } else {
                                SettingsRuntimeApplyOutcome::failed(
                                    "settings_phase_test",
                                    "phase failed",
                                )
                            }
                        } else {
                            SettingsRuntimeApplyOutcome::applied()
                        }
                    }
                })
                .await;

            assert!(result.is_err(), "{failed_phase:?} should fail the apply");
            assert_eq!(
                *seen.lock().unwrap(),
                expected_phases[..=failed_index].to_vec(),
                "later phases must not run after {failed_phase:?} fails"
            );
            let failure = coordinator.current_observation().unwrap();
            assert_eq!(failure.config_version, 19);
            assert_eq!(failure.phase, failed_phase);
            assert_eq!(
                failure.failure_kind,
                Some(
                    if failed_phase == SettingsRuntimeApplyPhase::RouterPrepare {
                        SettingsRuntimeApplyFailureKind::RouterPrepare
                    } else {
                        SettingsRuntimeApplyFailureKind::RuntimeOwner
                    }
                )
            );
            assert_eq!(
                failure.router_published,
                expected_phases[..failed_index].contains(&SettingsRuntimeApplyPhase::RouterPublish)
            );
            assert_eq!(
                failure.restart_required_targets,
                vec![RuntimeConfigTarget::SkillsRestart]
            );
        }
    }

    #[tokio::test]
    async fn router_publish_catalog_failure_reports_partial_publish_and_stops_later_phases() {
        let change = ConfigChanged {
            version: 25,
            domains: vec![ConfigDomain::Llm, ConfigDomain::ContextLimits],
        };
        let snapshot = settings_snapshot(25, "Ctrl+Alt+O");
        let mut coordinator =
            SettingsRuntimeApplyCoordinator::new(&change, &snapshot, "Ctrl+Alt+O");
        let mut seen = Vec::new();

        let result = coordinator
            .apply(|phase| {
                seen.push(phase);
                let outcome = if phase == SettingsRuntimeApplyPhase::RouterPublish {
                    SettingsRuntimeApplyOutcome::failed_catalog_rebuild_after_router_publish(
                        "update_settings router publish",
                        "catalog conflict",
                    )
                } else {
                    SettingsRuntimeApplyOutcome::applied()
                };
                async move { outcome }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(
            seen,
            vec![
                SettingsRuntimeApplyPhase::RouterPrepare,
                SettingsRuntimeApplyPhase::RouterPublish,
            ]
        );
        let failure = coordinator.current_observation().unwrap();
        assert_eq!(failure.phase, SettingsRuntimeApplyPhase::RouterPublish);
        assert_eq!(
            failure.failure_kind,
            Some(SettingsRuntimeApplyFailureKind::ToolCatalogRebuild)
        );
        assert!(failure.router_published);
    }

    #[tokio::test]
    async fn router_prepare_failure_stops_before_publish_and_keeps_snapshot_metadata() {
        let change = ConfigChanged {
            version: 20,
            domains: vec![
                ConfigDomain::Llm,
                ConfigDomain::SkillsExec,
                ConfigDomain::Memory,
            ],
        };
        let snapshot = settings_snapshot(20, "Ctrl+Alt+N");
        let mut coordinator =
            SettingsRuntimeApplyCoordinator::new(&change, &snapshot, "Ctrl+Alt+O");
        let mut router_published = false;

        let result = coordinator
            .apply(|phase| {
                let result = match phase {
                    SettingsRuntimeApplyPhase::RouterPrepare => {
                        SettingsRuntimeApplyOutcome::failed_already_rendered(
                            "update_settings",
                            "client preparation failed".into(),
                        )
                    }
                    SettingsRuntimeApplyPhase::RouterPublish => {
                        router_published = true;
                        SettingsRuntimeApplyOutcome::applied()
                    }
                    _ => SettingsRuntimeApplyOutcome::applied(),
                };
                async move { result }
            })
            .await;

        assert_eq!(result, Err("client preparation failed".into()));
        assert!(!router_published);
        let failure = coordinator.current_observation().unwrap();
        assert_eq!(failure.config_version, snapshot.version);
        assert_eq!(failure.phase, SettingsRuntimeApplyPhase::RouterPrepare);
        assert!(!failure.router_published);
        assert_eq!(
            failure.restart_required_targets,
            vec![RuntimeConfigTarget::SkillsRestart]
        );
    }

    #[tokio::test]
    async fn hotkey_rebind_event_warning_keeps_settings_apply_successful() {
        let change = ConfigChanged {
            version: 21,
            domains: vec![ConfigDomain::Hotkey],
        };
        let snapshot = settings_snapshot(21, "Ctrl+Alt+N");
        let mut coordinator =
            SettingsRuntimeApplyCoordinator::new(&change, &snapshot, "Ctrl+Alt+O");

        coordinator
            .apply(|phase| async move {
                if phase == SettingsRuntimeApplyPhase::HotkeyRebindEvent {
                    SettingsRuntimeApplyOutcome::warning(
                        "update_settings hotkey rebind event",
                        "event failed",
                    )
                } else {
                    SettingsRuntimeApplyOutcome::applied()
                }
            })
            .await
            .unwrap();

        let metadata = coordinator.current_observation().unwrap();
        assert_eq!(metadata.phase, SettingsRuntimeApplyPhase::HotkeyRebindEvent);
        assert_eq!(metadata.config_version, snapshot.version);
        assert!(!metadata.router_published);
        assert!(metadata.restart_required_targets.is_empty());
    }

    #[test]
    fn failure_after_router_publish_logs_phase_metadata_and_sanitizes_secrets() {
        use std::io::{self, Write};
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct BufferWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for BufferWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for BufferWriter {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let change = ConfigChanged {
            version: 23,
            domains: vec![
                ConfigDomain::Llm,
                ConfigDomain::SkillsExec,
                ConfigDomain::Skills,
                ConfigDomain::Memory,
            ],
        };
        let snapshot = settings_snapshot(23, "Ctrl+Alt+N");
        let mut coordinator =
            SettingsRuntimeApplyCoordinator::new(&change, &snapshot, "Ctrl+Alt+O");
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(BufferWriter(output.clone()))
            .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let rendered = tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(coordinator.apply(|phase| async move {
                if phase == SettingsRuntimeApplyPhase::Skills {
                    SettingsRuntimeApplyOutcome::failed(
                        "update_settings skills",
                        "request failed with api_key=never-log-this-secret",
                    )
                } else {
                    SettingsRuntimeApplyOutcome::applied()
                }
            }))
        })
        .unwrap_err();
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();

        assert!(rendered.contains("request failed"));
        assert!(!rendered.contains("never-log-this-secret"));
        assert!(logs.contains("config_version=23"), "captured logs: {logs}");
        assert!(logs.contains("phase=\"skills\""));
        assert!(logs.contains("failure_kind=\"runtime_owner\""));
        assert!(logs.contains("router_published=true"));
        assert!(logs.contains("restart_required=true"));
        assert!(logs.contains("restart_required_targets=[SkillsRestart]"));
        assert!(!logs.contains("never-log-this-secret"));
    }

    #[test]
    fn memory_inference_setting_is_applied_live_without_restart() {
        let change = ConfigChanged {
            version: 24,
            domains: vec![ConfigDomain::SkillsExec, ConfigDomain::Memory],
        };
        let snapshot = settings_snapshot(24, "Ctrl+Alt+O");
        let plan = SettingsRuntimeApplyPlan::from_change(&change, &snapshot, "Ctrl+Alt+O");

        assert_eq!(plan.config_version, snapshot.version);
        assert_eq!(
            plan.restart_required_targets,
            vec![RuntimeConfigTarget::SkillsRestart]
        );
        assert_eq!(
            plan.live_targets,
            vec![RuntimeConfigTarget::MemoryInference]
        );
        assert_eq!(plan.phases(), &[SettingsRuntimeApplyPhase::FactInference]);
    }
    #[tokio::test]
    async fn model_edit_no_op_skips_router_rebuild() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(test_config_service(loader));
        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let rebuilds = Arc::new(AtomicUsize::new(0));
        let rebuilds_in_apply = rebuilds.clone();

        let value = coordinator
            .edit_model_and_apply_with(
                &service,
                "model_test",
                |_| Ok(7),
                move |_| async move {
                    rebuilds_in_apply.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert_eq!(value, 7);
        assert_eq!(rebuilds.load(Ordering::SeqCst), 0);
        assert_eq!(service.snapshot().unwrap().version, 0);
    }

    #[tokio::test]
    async fn failed_model_router_apply_keeps_durable_edit_and_same_edit_skips_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let loader = ConfigLoader::load_from(&path).unwrap();
        let service = test_config_service(loader);
        let coordinator = RuntimeConfigCoordinator::default();
        let apply_attempts = Arc::new(AtomicUsize::new(0));
        let first_attempts = apply_attempts.clone();

        let result = coordinator
            .edit_model_and_apply_with(
                &service,
                "model_test",
                |config| {
                    if !config
                        .llm
                        .models
                        .iter()
                        .any(|model| model.id == "model_test")
                    {
                        config.llm.models.push(ModelConfig {
                            id: "model_test".into(),
                            ..Default::default()
                        });
                    }
                    Ok(())
                },
                move |_| async move {
                    first_attempts.fetch_add(1, Ordering::SeqCst);
                    Err("router preparation failed".to_string())
                },
            )
            .await;

        let error = result.unwrap_err();
        assert!(error.starts_with("部分 apply 失败：配置已写入"));
        assert!(error.contains("重启应用后会从配置重新初始化"));
        assert!(error.ends_with("router preparation failed"));
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.version, 1);
        assert!(
            snapshot
                .config
                .llm
                .models
                .iter()
                .any(|model| model.id == "model_test")
        );
        let persisted = ConfigLoader::load_from(&path).unwrap();
        assert!(
            persisted
                .config()
                .llm
                .models
                .iter()
                .any(|model| model.id == "model_test")
        );

        let retry_attempts = apply_attempts.clone();
        let retry_result = coordinator
            .edit_model_and_apply_with(
                &service,
                "model_test",
                |config| {
                    if !config
                        .llm
                        .models
                        .iter()
                        .any(|model| model.id == "model_test")
                    {
                        config.llm.models.push(ModelConfig {
                            id: "model_test".into(),
                            ..Default::default()
                        });
                    }
                    Ok(())
                },
                move |_| async move {
                    retry_attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;

        assert_eq!(retry_result, Ok(()));
        assert_eq!(apply_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(service.snapshot().unwrap().version, 1);
        let latest_snapshot = service.snapshot().unwrap();
        let (retry_plan, _) = coordinator
            .prepare_settings_reapply(&latest_snapshot)
            .expect("failed model apply remains explicitly retryable");
        assert_eq!(
            retry_plan.phases(),
            &[
                SettingsRuntimeApplyPhase::RouterPrepare,
                SettingsRuntimeApplyPhase::RouterPublish,
            ]
        );
        coordinator.finish_settings_apply(&retry_plan, retry_plan.phases(), None);
        let router_status = coordinator
            .settings_apply_statuses()
            .into_iter()
            .find(|status| status.target == RuntimeConfigTarget::LlmRouter)
            .unwrap();
        assert_eq!(router_status.result, SettingsRuntimeApplyResult::Applied);
        assert_eq!(router_status.expected_config_version, 1);
        assert_eq!(router_status.actual_config_version, 1);
    }

    #[tokio::test]
    async fn explicit_reapply_serializes_with_newer_model_edits() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(test_config_service(loader));
        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let failed = coordinator
            .edit_model_and_apply_with(
                &service,
                "model_test",
                |config| {
                    config.llm.models.push(ModelConfig {
                        id: "saved-model".into(),
                        ..Default::default()
                    });
                    Ok(())
                },
                |_| async { Err("router preparation failed".to_owned()) },
            )
            .await;
        assert!(failed.is_err());

        let retry_coordinator = coordinator.clone();
        let retry_service = service.clone();
        let (retry_started_tx, retry_started_rx) = tokio::sync::oneshot::channel();
        let (release_retry_tx, release_retry_rx) = tokio::sync::oneshot::channel();
        let retry = tokio::spawn(async move {
            let _guard = retry_coordinator.lock().await;
            let snapshot = retry_service.snapshot().unwrap();
            let (plan, _) = retry_coordinator
                .prepare_settings_reapply(&snapshot)
                .expect("failed router target remains retryable");
            retry_started_tx.send(snapshot.version).unwrap();
            release_retry_rx.await.unwrap();
            retry_coordinator.finish_settings_apply(&plan, plan.phases(), None);
        });
        assert_eq!(retry_started_rx.await.unwrap(), 1);

        let edit_coordinator = coordinator.clone();
        let edit_service = service.clone();
        let (edit_started_tx, mut edit_started_rx) = tokio::sync::oneshot::channel();
        let edit = tokio::spawn(async move {
            edit_coordinator
                .edit_model_and_apply_with(
                    &edit_service,
                    "model_test",
                    move |config| {
                        edit_started_tx.send(()).unwrap();
                        config.llm.models.push(ModelConfig {
                            id: "newer-model".into(),
                            ..Default::default()
                        });
                        Ok(())
                    },
                    |snapshot| async move {
                        assert_eq!(snapshot.version, 2);
                        Ok(())
                    },
                )
                .await
                .unwrap();
        });
        tokio::task::yield_now().await;
        assert!(edit_started_rx.try_recv().is_err());

        release_retry_tx.send(()).unwrap();
        retry.await.unwrap();
        edit.await.unwrap();
        let status = coordinator
            .settings_apply_statuses()
            .into_iter()
            .find(|status| status.target == RuntimeConfigTarget::LlmRouter)
            .unwrap();
        assert_eq!(status.expected_config_version, 2);
        assert_eq!(status.actual_config_version, 2);
        assert_eq!(status.result, SettingsRuntimeApplyResult::Applied);
    }

    struct ActiveModelOperation(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for ActiveModelOperation {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn two_model_operations_do_not_overlap_edit_and_apply() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(test_config_service(loader));
        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let rebuilds = Arc::new(AtomicUsize::new(0));

        let run = |model_id: &'static str| {
            let coordinator = coordinator.clone();
            let service = service.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let rebuilds = rebuilds.clone();
            async move {
                coordinator
                    .edit_model_and_apply_with(
                        &service,
                        "model_test",
                        move |config| {
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            max_active.fetch_max(current, Ordering::SeqCst);
                            config.llm.models.push(ModelConfig {
                                id: model_id.into(),
                                ..Default::default()
                            });
                            Ok(ActiveModelOperation(active))
                        },
                        move |_| async move {
                            rebuilds.fetch_add(1, Ordering::SeqCst);
                            tokio::task::yield_now().await;
                            Ok(())
                        },
                    )
                    .await
                    .unwrap();
            }
        };

        tokio::join!(run("model-one"), run("model-two"));

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(rebuilds.load(Ordering::SeqCst), 2);
        assert_eq!(service.snapshot().unwrap().version, 2);
    }

    #[tokio::test]
    async fn coordinator_publishes_a_successfully_prepared_runtime() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let published = Arc::new(AtomicUsize::new(0));
        let published_in_apply = published.clone();
        let result = RuntimeConfigCoordinator::prepare_then_publish(
            || Ok::<_, &str>(23_u64),
            |version| async move {
                published_in_apply.store(version as usize, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;

        assert_eq!(result, Ok(()));
        assert_eq!(published.load(Ordering::SeqCst), 23);
    }

    #[test]
    fn router_and_media_clients_are_prepared_from_the_supplied_snapshot() {
        let mut config = haven_common::config::AppConfig::default();
        config.context_limits.default_context_window = 173_000;
        config.media.audio.max_duration_secs = 37;
        let snapshot = ConfigSnapshot {
            version: 23,
            config,
        };

        let prepared =
            RuntimeConfigCoordinator::prepare_router_from_snapshot(&snapshot, None, None, "test")
                .expect("default router and media clients should prepare");

        assert_eq!(prepared.config_version, snapshot.version);
        assert_eq!(prepared.snapshot.media_config(), &snapshot.config.media);
        assert!(Arc::strong_count(prepared.snapshot.router()) >= 1);
    }

    #[test]
    fn media_preparation_errors_and_logs_do_not_expose_config_secrets() {
        use std::io::{self, Write};
        use std::sync::Mutex;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct BufferWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for BufferWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for BufferWriter {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let secret = "media-key-never-log-this";
        let mut config = haven_common::config::AppConfig::default();
        config.media.ocr.provider = "invalid-provider".into();
        config.media.ocr.api_key = secret.into();
        let snapshot = ConfigSnapshot {
            version: 24,
            config,
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(BufferWriter(output.clone()))
            .finish();

        let result = tracing::subscriber::with_default(subscriber, || {
            RuntimeConfigCoordinator::prepare_router_from_snapshot(&snapshot, None, None, "test")
        });
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();

        let error = result.err().expect("invalid OCR provider should fail");
        assert!(!logs.contains(secret));
        assert!(!error.contains(secret));
    }
}
