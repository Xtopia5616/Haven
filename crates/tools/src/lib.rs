mod action_completion;
mod action_lifecycle;
mod action_output;
mod action_retry_policy;
mod action_service;
mod action_terminal;
mod action_trigger_policy;
pub mod adapters;
mod asset_registry;
mod authorization_policy;
pub mod builtin;
mod catalog;
pub mod circuit;
mod coordinator;
mod document;
mod execution;
pub mod inbox;
pub mod live_output;
mod manager;
pub mod messaging_service;
#[doc(hidden)]
pub mod operation_view;
mod output;
mod process;
mod prompts;
pub(crate) mod registry;
mod runtime_capabilities;
pub(crate) mod security;
mod shell_runtime;
pub mod simulate;
pub mod skill_runner;
#[cfg(test)]
mod tests;
mod tool_builtins;
pub(crate) mod tool_contract;
mod tool_core;
mod tool_runtime;
pub mod util;

use chrono::{DateTime, Utc};
use haven_common::config::{
    ContextLimitsConfig, McpServerConfig, SecurityConfig, SkillsExecConfig, ToolConfig,
};
use haven_common::types::{MessageAttachment, RiskLevel, ShellChoice};
use haven_llm::LlmRouter;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Result of a tool catalog update requested by a runtime configuration
/// change. `Unchanged` means the requested settings did not affect any catalog
/// roots and no rebuild was needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogRebuildOutcome {
    Published,
    Unchanged,
}

/// A catalog rebuild can be rejected before its atomic registry snapshot is
/// published. Callers that own durable configuration apply can propagate this
/// failure without pretending the live catalog was updated.
#[derive(Debug, thiserror::Error)]
pub enum CatalogRebuildError {
    #[error("tool catalog rebuild was rejected: {source}")]
    RegistryRejected {
        #[source]
        source: anyhow::Error,
    },
}

/// Whether a tool is enabled per `tool_settings`. Tools without a settings
/// entry are enabled by default. Single source of truth shared by the
/// registry filter, the execution gate, and the UI listing, so the three
/// cannot drift apart.
fn tool_config_enabled(settings: &HashMap<String, ToolConfig>, name: &str) -> bool {
    settings
        .get(name)
        .or_else(|| name.split('.').next().and_then(|root| settings.get(root)))
        .map(|c| c.enabled)
        .unwrap_or(true)
}

/// The always-visible provider surface is deliberately small. These tools
/// support clarification, narrow source inspection, and activation of deeper
/// capability layers; all other enabled builtins and Skills are loaded into a
/// session only when requested by the model.
const CORE_MODEL_TOOLS: &[&str] = &[
    "ask",
    "notify",
    "tool_catalog",
    "load_skill",
    "load_mcp",
    "files.read",
    "files.outline",
    "files.search",
    "system.info",
];

fn is_core_model_tool(name: &str) -> bool {
    CORE_MODEL_TOOLS.contains(&name)
}

#[derive(Debug, Clone, Default)]
enum CatalogRebuildScope {
    #[default]
    All,
    Roots(HashSet<String>),
}

impl CatalogRebuildScope {
    fn roots(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Roots(names.into_iter().map(Into::into).collect())
    }

    fn affects(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Roots(roots) => {
                let root = name.split('.').next().unwrap_or(name);
                roots.contains(root)
                    || (name.starts_with("skill__") && roots.contains("skills"))
                    || (name.starts_with("mcp__") && roots.contains("mcp"))
            }
        }
    }
}

pub(crate) use action_lifecycle::{ActionLifecycle, EventSinkState};
pub use action_service::{
    ActionCompletion, ActionCompletionReceiver, BackgroundActionCompletion, EventSink,
    ScheduledActionFired,
};
pub use action_service::{
    ActionListView, ActionService, ActionStateView, ActionStatusView, ActionView, ActionViewKind,
    ScheduledActionView,
};
pub use adapters::{McpToolAdapter, SkillToolAdapter};
pub use asset_registry::{ManagedAsset, ManagedAssetRegistry};
pub use builtin::{
    AdminCapability, AdminContext, AdminOperationError, AdminRequest, AdminSurfaces, AgentTool,
    ConfigAdminContext, ConfigAdminOperation, ConfigAdminTool, ConfigOperationArgs,
    ConfigOperationError, ConfigOperationOutput, ConfigViewOutput, DiagnosticsOperationArgs,
    LogLevelOutput, McpOperationArgs, McpRefreshAction, McpRefreshPlan, McpRefreshTarget,
    MediaTranscriptionResult, MediaTranscriptionStatus, NativeMcpOperationArgs, ScheduleMode,
    SkillsOperationArgs, ToolsOperationArgs,
};
pub use circuit::ToolCircuitRegistry;
pub use haven_common::types::CapabilityScope;
pub use haven_mcp::{
    McpClient, McpClientStatus, McpManager, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
pub use haven_skills::{Language, Skill, SkillInfo, SkillManifest, SkillsEngine, VenvManager};
pub use live_output::LiveOutputHub;
pub use messaging_service::{
    AgentControlOperation, AgentControlRequest, AgentControlResult, AgentSpawnRequest,
    AgentSpawnResult, MessageClaim, MessageTransport, MessagingRuntime, MessagingService,
    SentMessage, SessionMailbox, is_expired,
};
pub use output::{
    OutputBudget, ToolOutput, append_windows_diagnostics, is_progress_clixml,
    sanitize_shell_output, summarize_error,
};
pub(crate) use process::read_stream_capped;
pub use registry::{
    DeferredToolCatalog, OperationRegistry, RegistryProbe, SessionCatalog, ToolCatalogSnapshot,
    ToolRegistry,
};
pub use security::{
    AuthorizationDecision, AuthorizationEngine, AuthorizationReasonCode, AuthorizationRequest,
    ConfirmationReceipt, LOCAL_TOOL_SECURITY_MATRIX, LocalToolSecurityCase, is_safe_local_path,
    permission_prompt_summary,
};
#[cfg(windows)]
pub use shell_runtime::CREATE_NO_WINDOW;
pub use shell_runtime::{
    build_shell_command, build_shell_command_silent, collect_byte_cap, output_log_dir,
    proxy_env_vars, write_output_log,
};
pub use skill_runner::SkillRunner;
pub use tool_contract::{
    ConfirmationRequirement, DataSensitivity, NetworkAccess, OperationEffect, OperationIdempotency,
    OperationPolicy, StructuredToolError, Tool, ToolAvailability, ToolBox, ToolCancellationPolicy,
    ToolConcurrency, ToolDef, ToolErrorClass, ToolErrorMetadata, ToolExecutionOutcome,
    ToolIdentity, ToolLlmUsage, ToolManifest, ToolModel, ToolOperationMetadata, ToolOperationScope,
    ToolPolicy, ToolPresentation, ToolRegistration, ToolResult, ToolResultEnvelope,
    ToolRetryability, ToolRootPresentation, ToolSignals, ToolSource, TypedToolAdapter,
    TypedToolOperation, extract_ask_signal, extract_notify_signal, is_silent_action,
    parse_tool_input,
};
pub use tool_runtime::{
    LogLevelPort, MemoryRecallPort, MemoryRecallSlot, RuntimeCapabilities, StartupWiring,
    ToolControlPort, WebSearchAvailability,
};

/// Convert an internal qualified tool name (`mcp::server::tool`, `skill::name`)
/// into a provider-safe form (`mcp__server__tool`, `skill__name`) accepted by
/// tool-calling LLM APIs. OpenAI-compatible providers
/// restrict tool names to `^[a-zA-Z0-9_-]+$` (DeepSeek rejects the `::`
/// namespace separator with a 400, which permanently errors the session after a
/// successful `load_mcp`); Anthropic additionally caps the length at 64.
/// The transform is deterministic so the name advertised to the model in the
/// tool definitions always equals the per-session registration key used for
/// execution lookup — no reverse mapping is needed. A digest suffix is kept
/// when truncating, otherwise two long MCP names with the same prefix could
/// silently overwrite one another in the session catalog.
pub fn llm_tool_name(qualified: &str) -> String {
    let mut out: String = qualified
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.len() > 64 {
        let digest = Sha256::digest(qualified.as_bytes());
        let suffix: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let prefix_len = 64 - 1 - suffix.len();
        let idx = out.floor_char_boundary(prefix_len);
        out.truncate(idx);
        out.push('_');
        out.push_str(&suffix);
    }
    out
}

/// Lightweight sanitizer for strings interpolated into the system prompt:
/// replaces control characters (newlines, tabs) that could inject prompt
/// text, and caps the length. Shared implementation lives in
/// `haven_common::text` so the policy cannot drift from the agent prompt /
/// fact sanitizers.
fn sanitize_index_field(s: &str) -> String {
    haven_common::text::sanitize_prompt_field(s, 256)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolBudgetPriority {
    /// Builtin operation views are the stable core surface. They must be
    /// considered before any user-extensible source.
    Builtin = 0,
    /// A tool explicitly loaded into this session (normally an MCP tool) is
    /// preferred over globally enabled optional tools. This preserves the
    /// meaning of a successful explicit load without changing its admission
    /// check in `register_mcp_for_session`.
    Session = 1,
    /// Globally enabled skills are useful, but must not displace the core
    /// operation views when the provider budget is tight.
    Optional = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolBudgetOrigin {
    Global,
    Session,
}

#[derive(Debug)]
struct ToolBudgetCandidate {
    def: ToolDef,
    origin: ToolBudgetOrigin,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ToolBudgetSelection {
    selected: Vec<ToolDef>,
    omitted: Vec<String>,
    omitted_core: usize,
}

impl ToolBudgetCandidate {
    fn source(&self) -> Option<ToolSource> {
        self.def
            .manifest
            .as_ref()
            .map(|manifest| manifest.identity.source)
    }

    fn priority(&self) -> ToolBudgetPriority {
        if self.origin == ToolBudgetOrigin::Session {
            // Registration location is authoritative here. A custom tool
            // created through the default Tool implementation may carry a
            // Builtin source in its inferred manifest, but it is still an
            // explicit session overlay rather than part of the core catalog.
            return ToolBudgetPriority::Session;
        }
        match self.source() {
            Some(ToolSource::Builtin) => ToolBudgetPriority::Builtin,
            Some(ToolSource::Skill | ToolSource::Mcp) | None => ToolBudgetPriority::Optional,
        }
    }
}

/// Select a stable provider-facing tool surface under the per-request budget.
///
/// The registry order is an implementation detail and must not decide which
/// capabilities survive a tight provider limit. Builtins are always selected
/// before optional sources; explicitly registered session tools (including
/// MCP tools admitted by `register_mcp_for_session`) come next; globally
/// enabled skills are last. Every bucket is sorted by stable tool name so the
/// result does not depend on HashMap iteration or skill discovery order.
///
/// The returned omitted names are intentionally kept separate from the
/// provider definitions. The current public API returns only `Vec<ToolDef>`,
/// so `list_defs_for_session` logs this explanation without changing the
/// cross-crate contract.
fn select_tool_defs_for_budget(
    global: Vec<ToolDef>,
    session: Vec<ToolDef>,
    max: usize,
) -> ToolBudgetSelection {
    let mut candidates: Vec<_> = global
        .into_iter()
        .map(|def| ToolBudgetCandidate {
            def,
            origin: ToolBudgetOrigin::Global,
        })
        .chain(session.into_iter().map(|def| ToolBudgetCandidate {
            def,
            origin: ToolBudgetOrigin::Session,
        }))
        .collect();

    candidates.sort_by(|a, b| {
        a.priority()
            .cmp(&b.priority())
            .then_with(|| a.def.name.cmp(&b.def.name))
    });

    let selected_len = max.max(1).min(candidates.len());
    let omitted_core = candidates
        .iter()
        .skip(selected_len)
        .filter(|candidate| candidate.priority() == ToolBudgetPriority::Builtin)
        .count();
    let omitted = candidates
        .iter()
        .skip(selected_len)
        .map(|candidate| candidate.def.name.clone())
        .collect();
    let selected = candidates
        .into_iter()
        .take(selected_len)
        .map(|candidate| candidate.def)
        .collect();

    ToolBudgetSelection {
        selected,
        omitted,
        omitted_core,
    }
}

/// Put the concrete retry policy beside a terminal failure so the model can
/// choose an equivalent read-only path without escalating to a user-facing
/// confirmation. This is result metadata, not authorization: unsafe and
/// unknown operations remain non-replayable by the executor.
fn annotate_retry_safety(result: &mut ToolResult, idempotency: OperationIdempotency) {
    if result.success {
        return;
    }
    let retry_safety = Value::String(idempotency.as_str().into());
    match &mut result.output {
        Value::Object(object) => {
            object.insert("retry_safety".into(), retry_safety);
            if let Some(error_class) = result.error_class {
                object.insert(
                    "error_class".into(),
                    Value::String(error_class.as_str().into()),
                );
            }
            object.insert(
                "retryability".into(),
                Value::String(result.retryability.as_str().into()),
            );
        }
        output => {
            let previous = std::mem::replace(output, Value::Null);
            *output = serde_json::json!({
                "retry_safety": retry_safety,
                "retryability": result.retryability.as_str(),
                "output": previous,
            });
        }
    }
}

const MAX_TOOL_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Exponential delay with a small deterministic jitter and a hard ceiling.
/// Deterministic jitter avoids synchronizing repeated calls from the same
/// process without introducing a new random source into the tool crate.
fn tool_retry_delay(tool_name: &str, base_secs: u64, attempt: u32) -> Duration {
    if base_secs == 0 {
        return Duration::ZERO;
    }
    let exponential_secs = base_secs.saturating_mul(2u64.saturating_pow(attempt - 1));
    let base = Duration::from_secs(exponential_secs).min(MAX_TOOL_RETRY_DELAY);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    tool_name.hash(&mut hasher);
    attempt.hash(&mut hasher);
    let jitter_ms = hasher.finish() % 250;
    (base + Duration::from_millis(jitter_ms)).min(MAX_TOOL_RETRY_DELAY)
}

/// Retry only failures that are known to be transient. Unknown/cancelled
/// outcomes are never replayed, because the operation may still be running.
fn retryable_result(result: &ToolResult) -> bool {
    if matches!(
        result.outcome,
        ToolExecutionOutcome::Cancelled | ToolExecutionOutcome::TimedOutUnknown
    ) {
        return false;
    }
    result.retryability == crate::ToolRetryability::Retryable
}

pub use manager::{ToolServices, ToolsManager};
