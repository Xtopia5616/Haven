//! One-shot handler slots and lifecycle hook contracts shared across layers.

use std::sync::Arc;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// User-configurable lifecycle point exposed to an external hook command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHookEvent {
    PreToolUse,
    PostToolUse,
    SessionStart,
    PreCompact,
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
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_output: Option<Value>,
    pub success: Option<bool>,
    pub tokens_before: Option<u64>,
    pub payload_truncated: bool,
}

impl LifecycleHookPayload {
    pub fn new(event: LifecycleHookEvent) -> Self {
        Self {
            event,
            session_id: None,
            run_id: None,
            start_step: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            success: None,
            tokens_before: None,
            payload_truncated: false,
        }
    }
}

/// PreToolUse's only policy decision. `Allow` never skips Haven authorization.
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
