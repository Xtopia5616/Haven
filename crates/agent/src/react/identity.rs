//! Centralized streaming-message identity map (Phase 6 / I3).
//!
//! One `IdentityMap` is owned by a [`super::ReActState`]. Stream chunks, thought
//! snaps, and final persistence all resolve ids through it so the live bubble
//! and the DB row share one identity without content dedup.

use std::collections::HashMap;
use std::sync::Mutex;

/// Identity of one streamed thought or reasoning block within a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StreamBlockIdentity {
    step_number: u32,
    run_id: u64,
    block_kind: StreamBlockKind,
}

impl StreamBlockIdentity {
    pub(crate) const fn thought(step_number: u32, run_id: u64) -> Self {
        Self {
            step_number,
            run_id,
            block_kind: StreamBlockKind::Thought,
        }
    }

    pub(crate) const fn reasoning(step_number: u32, run_id: u64) -> Self {
        Self {
            step_number,
            run_id,
            block_kind: StreamBlockKind::Reasoning,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StreamBlockKind {
    Thought,
    Reasoning,
}

impl StreamBlockKind {
    const fn message_id_prefix(self) -> &'static str {
        match self {
            Self::Thought => "step",
            Self::Reasoning => "msg",
        }
    }
}

/// Mint or reuse message ids for streamed blocks.
#[derive(Debug, Default)]
pub(crate) struct IdentityMap {
    stream_block_message_ids: Mutex<HashMap<StreamBlockIdentity, String>>,
}

impl IdentityMap {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            stream_block_message_ids: Mutex::new(HashMap::new()),
        }
    }

    /// Mint (or reuse) the id a streamed thought/reasoning block of
    /// this identity accumulates into. The owning state already scopes the
    /// map to one ReAct run.
    ///
    /// A `thought` block is the content view of a ReAct step: its id is
    /// minted with the `step-` prefix so the message row and the thought
    /// step row share one entity. `reasoning` blocks keep `msg-` ids.
    pub(crate) fn ensure_stream_block_message_id(&self, identity: StreamBlockIdentity) -> String {
        let mut message_ids = self.stream_block_message_ids.lock().unwrap();
        message_ids
            .entry(identity)
            .or_insert_with(|| haven_common::types::new_id(identity.block_kind.message_id_prefix()))
            .clone()
    }

    /// Read the minted id for a block without consuming it.
    pub(crate) fn peek_stream_block_message_id(
        &self,
        identity: StreamBlockIdentity,
    ) -> Option<String> {
        self.stream_block_message_ids
            .lock()
            .unwrap()
            .get(&identity)
            .cloned()
    }

    /// Id a streamed block is persisted under: minted id when the block
    /// streamed, else a fresh id with the same per-kind prefix.
    pub(crate) fn stream_block_message_id_or_new(&self, identity: StreamBlockIdentity) -> String {
        self.peek_stream_block_message_id(identity)
            .unwrap_or_else(|| haven_common::types::new_id(identity.block_kind.message_id_prefix()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_reuses_same_id_for_block() {
        let map = IdentityMap::new();
        let identity = StreamBlockIdentity::thought(1, 7);
        let a = map.ensure_stream_block_message_id(identity);
        let b = map.ensure_stream_block_message_id(identity);
        assert_eq!(a, b);
        assert!(a.starts_with("step-"));
    }

    #[test]
    fn reasoning_uses_msg_prefix() {
        let map = IdentityMap::new();
        let id = map.ensure_stream_block_message_id(StreamBlockIdentity::reasoning(1, 7));
        assert!(id.starts_with("msg-"));
    }

    #[test]
    fn ids_are_scoped_to_step_run_and_kind() {
        let map = IdentityMap::new();
        let thought_identity = StreamBlockIdentity::thought(1, 1);
        let thought = map.ensure_stream_block_message_id(thought_identity);
        assert_ne!(
            thought,
            map.ensure_stream_block_message_id(StreamBlockIdentity::thought(1, 2))
        );
        assert_ne!(
            thought,
            map.ensure_stream_block_message_id(StreamBlockIdentity::thought(2, 1))
        );
        assert_ne!(
            thought,
            map.ensure_stream_block_message_id(StreamBlockIdentity::reasoning(1, 1))
        );
        assert_eq!(
            map.peek_stream_block_message_id(thought_identity),
            Some(thought)
        );
    }

    #[test]
    fn stream_block_message_id_or_new_mints_without_storing() {
        let map = IdentityMap::new();
        let identity = StreamBlockIdentity::thought(9, 1);
        let id = map.stream_block_message_id_or_new(identity);
        assert!(id.starts_with("step-"));
        assert!(map.peek_stream_block_message_id(identity).is_none());
    }
}
