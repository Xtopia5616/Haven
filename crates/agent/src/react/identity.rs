//! Centralized streaming-message identity map (Phase 6 / I3).
//!
//! One `IdentityMap` is owned by [`super::ReActEngine`]. Stream chunks, thought
//! snaps, and final persistence all resolve ids through it so the live bubble
//! and the DB row share one identity without content dedup.

use std::collections::HashMap;
use std::sync::Mutex;

/// Key for a streamed thought/reasoning block within a run.
pub(super) type StreamBlockKey = (String, u32, u64, &'static str);

/// Mint / reuse / clear message ids for streamed blocks.
#[derive(Debug, Default)]
pub(super) struct IdentityMap {
    step_msg_ids: Mutex<HashMap<StreamBlockKey, String>>,
}

impl IdentityMap {
    pub(super) fn new() -> Self {
        Self {
            step_msg_ids: Mutex::new(HashMap::new()),
        }
    }

    /// Mint (or reuse) the id a streamed thought/reasoning block of
    /// `(session, step, run, kind)` accumulates into.
    ///
    /// A `thought` block is the content view of a ReAct step: its id is
    /// minted with the `step-` prefix so the message row and the thought
    /// step row share one entity. `reasoning` blocks keep `msg-` ids.
    pub(super) fn ensure_msg_id(
        &self,
        session_id: &str,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> String {
        let mut map = self.step_msg_ids.lock().unwrap();
        map.entry((session_id.to_string(), step, run, kind))
            .or_insert_with(|| {
                let prefix = if kind == "thought" { "step" } else { "msg" };
                haven_common::types::new_id(prefix)
            })
            .clone()
    }

    /// Read the minted id for a block without consuming it.
    pub(super) fn peek_msg_id(
        &self,
        session_id: &str,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> Option<String> {
        self.step_msg_ids
            .lock()
            .unwrap()
            .get(&(session_id.to_string(), step, run, kind))
            .cloned()
    }

    /// Id a streamed block is persisted under: minted id when the block
    /// streamed, else a fresh id with the same per-kind prefix.
    pub(super) fn block_msg_id(
        &self,
        session_id: &str,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> String {
        self.peek_msg_id(session_id, step, run, kind)
            .unwrap_or_else(|| {
                let prefix = if kind == "thought" { "step" } else { "msg" };
                haven_common::types::new_id(prefix)
            })
    }

    /// Drop every minted id belonging to a session (once per `run_react_loop`).
    pub(super) fn clear_for_session(&self, session_id: &str) {
        self.step_msg_ids
            .lock()
            .unwrap()
            .retain(|(sid, ..), _| sid != session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_reuses_same_id_for_block() {
        let map = IdentityMap::new();
        let a = map.ensure_msg_id("ses-1", 1, 7, "thought");
        let b = map.ensure_msg_id("ses-1", 1, 7, "thought");
        assert_eq!(a, b);
        assert!(a.starts_with("step-"));
    }

    #[test]
    fn reasoning_uses_msg_prefix() {
        let map = IdentityMap::new();
        let id = map.ensure_msg_id("ses-1", 1, 7, "reasoning");
        assert!(id.starts_with("msg-"));
    }

    #[test]
    fn clear_for_session_drops_only_that_session() {
        let map = IdentityMap::new();
        let keep = map.ensure_msg_id("ses-keep", 1, 1, "thought");
        let _drop = map.ensure_msg_id("ses-drop", 1, 1, "thought");
        map.clear_for_session("ses-drop");
        assert_eq!(
            map.peek_msg_id("ses-keep", 1, 1, "thought").as_deref(),
            Some(keep.as_str())
        );
        assert!(map.peek_msg_id("ses-drop", 1, 1, "thought").is_none());
    }

    #[test]
    fn block_msg_id_falls_back_to_fresh_prefix() {
        let map = IdentityMap::new();
        let id = map.block_msg_id("ses-1", 9, 1, "thought");
        assert!(id.starts_with("step-"));
        assert!(map.peek_msg_id("ses-1", 9, 1, "thought").is_none());
    }
}
