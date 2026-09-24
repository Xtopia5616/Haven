//! Centralized streaming-message identity map (Phase 6 / I3).
//!
//! One `IdentityMap` is owned by a [`super::ReActState`]. Stream chunks, thought
//! snaps, and final persistence all resolve ids through it so the live bubble
//! and the DB row share one identity without content dedup.

use std::collections::HashMap;
use std::sync::Mutex;

/// Key for a streamed thought/reasoning block within a run.
pub(crate) type StreamBlockKey = (u32, u64, &'static str);

/// Mint or reuse message ids for streamed blocks.
#[derive(Debug, Default)]
pub(crate) struct IdentityMap {
    step_msg_ids: Mutex<HashMap<StreamBlockKey, String>>,
}

impl IdentityMap {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            step_msg_ids: Mutex::new(HashMap::new()),
        }
    }

    /// Mint (or reuse) the id a streamed thought/reasoning block of
    /// `(step, run, kind)` accumulates into. The owning state already scopes
    /// this key to one ReAct run.
    ///
    /// A `thought` block is the content view of a ReAct step: its id is
    /// minted with the `step-` prefix so the message row and the thought
    /// step row share one entity. `reasoning` blocks keep `msg-` ids.
    pub(crate) fn ensure_msg_id(&self, step: u32, run: u64, kind: &'static str) -> String {
        let mut map = self.step_msg_ids.lock().unwrap();
        map.entry((step, run, kind))
            .or_insert_with(|| {
                let prefix = if kind == "thought" { "step" } else { "msg" };
                haven_common::types::new_id(prefix)
            })
            .clone()
    }

    /// Read the minted id for a block without consuming it.
    pub(crate) fn peek_msg_id(&self, step: u32, run: u64, kind: &'static str) -> Option<String> {
        self.step_msg_ids
            .lock()
            .unwrap()
            .get(&(step, run, kind))
            .cloned()
    }

    /// Id a streamed block is persisted under: minted id when the block
    /// streamed, else a fresh id with the same per-kind prefix.
    pub(crate) fn block_msg_id(&self, step: u32, run: u64, kind: &'static str) -> String {
        self.peek_msg_id(step, run, kind).unwrap_or_else(|| {
            let prefix = if kind == "thought" { "step" } else { "msg" };
            haven_common::types::new_id(prefix)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_reuses_same_id_for_block() {
        let map = IdentityMap::new();
        let a = map.ensure_msg_id(1, 7, "thought");
        let b = map.ensure_msg_id(1, 7, "thought");
        assert_eq!(a, b);
        assert!(a.starts_with("step-"));
    }

    #[test]
    fn reasoning_uses_msg_prefix() {
        let map = IdentityMap::new();
        let id = map.ensure_msg_id(1, 7, "reasoning");
        assert!(id.starts_with("msg-"));
    }

    #[test]
    fn ids_are_scoped_to_step_run_and_kind() {
        let map = IdentityMap::new();
        let thought = map.ensure_msg_id(1, 1, "thought");
        assert_ne!(thought, map.ensure_msg_id(1, 2, "thought"));
        assert_ne!(thought, map.ensure_msg_id(2, 1, "thought"));
        assert_ne!(thought, map.ensure_msg_id(1, 1, "reasoning"));
        assert_eq!(map.peek_msg_id(1, 1, "thought"), Some(thought));
    }

    #[test]
    fn block_msg_id_falls_back_to_fresh_prefix() {
        let map = IdentityMap::new();
        let id = map.block_msg_id(9, 1, "thought");
        assert!(id.starts_with("step-"));
        assert!(map.peek_msg_id(9, 1, "thought").is_none());
    }
}
