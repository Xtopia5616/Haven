//! Compatibility name for the memory worker.
//!
//! Fact inference is background memory work.  Keep this alias for existing
//! callers while the concrete implementation lives in `memory_worker.rs`.

pub use crate::memory_worker::MemoryWorker;

/// Historical name retained for the agent-facing API during the memory
/// boundary migration. New code should depend on [`MemoryWorker`].
pub type InferenceEngine = MemoryWorker;
