//! One-shot hook handler slots shared by the shell and input hook surfaces.

use std::sync::Arc;
use std::sync::OnceLock;

/// A slot that holds at most one hook handler. Installation is one-time by
/// design: hook handlers never change at runtime. A redundant install is
/// ignored and logged (with a `debug_assert!` so it still fails fast in debug
/// builds) instead of panicking unconditionally, so a startup wiring mistake
/// is diagnosable rather than crashing the process.
///
/// `T` is normally a `dyn Trait` (e.g. `dyn ShellHandler` / `dyn InputHandler`),
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
