//! One-shot handler slots and lifecycle hook contracts shared across layers.

use std::sync::Arc;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// User-configurable lifecycle point exposed to an external hook command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHookEvent {
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    SessionStart,
    RunStart,
    RunEnd,
    PreCompact,
    PostCompact,
    SessionEnd,
    #[serde(rename = "toolrun_start")]
    ToolRunStart,
    #[serde(rename = "toolrun_end")]
    ToolRunEnd,
}

/// Content-free outcome visible in the Hooks settings diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHookOutcome {
    NoHooks,
    Succeeded,
    Denied,
    FailedOpen,
}

/// Why Haven started the current Agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHookRunSource {
    /// A new user/session input made the session runnable.
    NewInput,
    /// The user or an interaction explicitly continued an existing run.
    Continue,
    /// Startup restored a session that was already pending.
    Recovery,
}

/// Bounded in-memory counters for one event. No command, arguments, payload,
/// stdout, stderr, or denial text is included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleHookEventDiagnostics {
    pub event: LifecycleHookEvent,
    pub event_runs: u64,
    /// Commands that passed event filtering and reached their start attempt.
    pub command_attempts: u64,
    /// Commands whose process entered its configured containment boundary.
    pub command_starts: u64,
    pub failures: u64,
    pub denials: u64,
    pub last_outcome: Option<LifecycleHookOutcome>,
    pub last_failure_code: Option<String>,
    /// Location of the most recent command failure or explicit veto.
    pub last_problem_command: Option<LifecycleHookCommandLocation>,
}

/// Which configured Hook list contains a command, without exposing its path
/// or arguments in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHookCommandSource {
    User,
    Project,
}

/// Zero-based command position in its user-level or project-level event list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleHookCommandLocation {
    pub source: LifecycleHookCommandSource,
    pub index: usize,
}

/// Bounded, typed metadata passed to a configured hook through JSON on stdin.
/// Optional tool payloads are further capped by the Tools runner before spawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookPayload {
    pub event: LifecycleHookEvent,
    pub session_id: Option<String>,
    pub run_id: Option<u64>,
    pub start_step: Option<u32>,
    /// Present on `permission_request`; identifies the tool call's step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_number: Option<u32>,
    /// Present on `run_start`; omitted from other Hook payloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_source: Option<LifecycleHookRunSource>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_run_status: Option<crate::lifecycle::ToolRunStatus>,
    pub success: Option<bool>,
    pub tokens_before: Option<u64>,
    pub tokens_after: Option<u64>,
    /// Stable, content-free run outcome such as `completed`, `paused`, or `error`.
    pub run_outcome: Option<String>,
    pub payload_truncated: bool,
}

impl LifecycleHookPayload {
    pub fn new(event: LifecycleHookEvent) -> Self {
        Self {
            event,
            session_id: None,
            run_id: None,
            start_step: None,
            step_number: None,
            run_source: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            tool_run_id: None,
            source_step_id: None,
            tool_run_status: None,
            success: None,
            tokens_before: None,
            tokens_after: None,
            run_outcome: None,
            payload_truncated: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LifecycleHookEvent, LifecycleHookPayload};

    #[test]
    fn permission_request_serializes_step_number() {
        let mut payload = LifecycleHookPayload::new(LifecycleHookEvent::PermissionRequest);
        payload.step_number = Some(7);

        let value = serde_json::to_value(payload).unwrap();

        assert_eq!(value["event"], "permission_request");
        assert_eq!(value["step_number"], 7);
    }
}

/// A veto-only decision for PreToolUse and PermissionRequest. `Allow` never
/// skips Haven authorization or the normal user confirmation prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LifecycleHookDecision {
    #[default]
    Allow,
    Deny {
        reason: String,
    },
}

/// A slot that holds at most one hook handler. Installation is one-time by
/// design: hook handlers never change at runtime. A redundant install is
/// ignored and logged (with a `debug_assert!` so it still fails fast in debug
/// builds) instead of panicking unconditionally, so a startup wiring mistake
/// is diagnosable rather than crashing the process.
///
/// `T` is normally a `dyn Trait` (e.g. `dyn ShellHandler` / `dyn InputEventHandler`),
/// so the slot stores `Arc<T>` and hands out clones without any locking; the
/// handler itself is immutable after install.
pub struct OnceHandler<T: ?Sized> {
    inner: OnceLock<Arc<T>>,
}

impl<T: ?Sized> Default for OnceHandler<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> OnceHandler<T> {
    /// Create an empty slot.
    pub fn new() -> Self {
        Self {
            inner: OnceLock::new(),
        }
    }

    /// Install the handler. Only the first install wins; a second install is
    /// ignored and logged at `warn!` (with a `debug_assert!` so it still fails
    /// fast in debug builds) and `false` is returned, so a caller can react to
    /// a startup wiring error instead of it being silently swallowed.
    pub fn set(&self, handler: Arc<T>) -> bool {
        match self.inner.set(handler) {
            Ok(()) => true,
            Err(_) => {
                debug_assert!(false, "hook handler installed more than once");
                tracing::warn!("hook handler already installed (ignored)");
                false
            }
        }
    }

    /// Snapshot the installed handler, if any. Lock-free read of an immutable
    /// slot, so callers never hold a lock across an await.
    pub fn snap(&self) -> Option<Arc<T>> {
        self.inner.get().cloned()
    }

    /// Whether a handler has been installed.
    pub fn is_installed(&self) -> bool {
        self.inner.get().is_some()
    }
}
