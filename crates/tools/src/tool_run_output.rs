//! Bounded, process-local ToolRun output snapshots.
//!
//! `ToolRunService` owns the shared tail-size policy. Foreground tool cards and
//! background ToolRun previews borrow this port to create bounded tails, then
//! project snapshots through their existing, distinct UI event contracts.

use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

const DEFAULT_TAIL_MAX_CHARS: usize = 2000;

/// Internal owner of the tail-size policy shared by ToolRun output consumers.
/// It deliberately exposes neither serialization nor a raw string buffer.
#[derive(Clone)]
pub(crate) struct ToolRunOutputPort {
    tail_max_chars: Arc<RwLock<usize>>,
}

impl Default for ToolRunOutputPort {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRunOutputPort {
    pub(crate) fn new() -> Self {
        Self {
            tail_max_chars: Arc::new(RwLock::new(DEFAULT_TAIL_MAX_CHARS)),
        }
    }

    pub(crate) async fn set_tail_max_chars(&self, max_chars: usize) {
        *self.tail_max_chars.write().await = max_chars;
    }

    /// Snapshot the configured limit when a command starts, matching the
    /// existing behavior when live limits change while a child is running.
    pub(crate) async fn new_tail(&self) -> ToolRunOutputTail {
        ToolRunOutputTail::with_limit(*self.tail_max_chars.read().await)
    }

    pub(crate) fn tail_factory(&self) -> ToolRunTailFactory {
        ToolRunTailFactory {
            tail_max_chars: Arc::clone(&self.tail_max_chars),
        }
    }
}

/// Read-only tail constructor shared with preview consumers. Only the
/// ToolRunService-owned port can change the configured policy.
#[derive(Clone)]
pub(crate) struct ToolRunTailFactory {
    tail_max_chars: Arc<RwLock<usize>>,
}

impl ToolRunTailFactory {
    pub(crate) async fn new_tail(&self) -> ToolRunOutputTail {
        ToolRunOutputTail::with_limit(*self.tail_max_chars.read().await)
    }
}

/// A single bounded combined stdout/stderr tail. The buffer and its limit stay
/// private so callers can only append decoded output or request a snapshot.
#[derive(Clone)]
pub(crate) struct ToolRunOutputTail {
    state: Arc<Mutex<TailState>>,
}

struct TailState {
    output: String,
    max_chars: usize,
}

impl ToolRunOutputTail {
    fn with_limit(max_chars: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(TailState {
                output: String::new(),
                max_chars,
            })),
        }
    }

    pub(crate) fn append_bytes(&self, chunk: &[u8]) {
        self.append_text(&haven_common::encoding::decode_lossy(chunk));
    }

    pub(crate) fn append_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.output.push_str(text);
        let overflow = state.output.chars().count().saturating_sub(state.max_chars);
        if overflow == 0 {
            return;
        }
        let cut = state
            .output
            .char_indices()
            .nth(overflow)
            .map(|(index, _)| index)
            .unwrap_or(state.output.len());
        state.output.drain(..cut);
    }

    pub(crate) fn snapshot(&self) -> ToolRunTailSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ToolRunTailSnapshot(state.output.clone())
    }

    /// Replace a consumer's previous snapshot only when content changed.
    /// Comparing values keeps sliding windows observable after the cap fills.
    pub(crate) fn snapshot_if_changed(&self, previous: &mut ToolRunTailSnapshot) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.output == previous.0 {
            return false;
        }
        previous.0.clone_from(&state.output);
        true
    }
}

/// Bounded preview value. It intentionally has no `Debug` or `Serialize`
/// implementation so logs and persistence cannot accidentally capture it.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolRunTailSnapshot(String);

impl ToolRunTailSnapshot {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tail_keeps_exact_char_limit_at_utf8_boundaries() {
        let port = ToolRunOutputPort::new();
        port.set_tail_max_chars(3).await;
        let tail = port.new_tail().await;
        tail.append_text("a中🙂z");

        assert_eq!(tail.snapshot().as_str(), "中🙂z");
        assert_eq!(tail.snapshot().as_str().chars().count(), 3);
    }

    #[tokio::test]
    async fn large_tail_keeps_exact_limit_across_appends() {
        let max_chars = 2000usize;
        let port = ToolRunOutputPort::new();
        port.set_tail_max_chars(max_chars).await;
        let tail = port.new_tail().await;
        let oversized = "x".repeat(max_chars + 500);

        tail.append_bytes(oversized.as_bytes());
        assert_eq!(tail.snapshot().as_str().chars().count(), max_chars);

        tail.append_bytes("tail-end".as_bytes());
        let snapshot = tail.snapshot();
        let text = snapshot.as_str();
        assert_eq!(text.chars().count(), max_chars);
        assert!(
            text.ends_with("tail-end"),
            "got tail: {}",
            &text[text.len().saturating_sub(40)..]
        );
    }

    #[tokio::test]
    async fn zero_limit_drops_every_character() {
        let port = ToolRunOutputPort::new();
        port.set_tail_max_chars(0).await;
        let tail = port.new_tail().await;
        tail.append_text("secret output");

        assert!(tail.snapshot().is_empty());
    }

    #[tokio::test]
    async fn snapshots_advance_in_order_and_detect_sliding_content() {
        let port = ToolRunOutputPort::new();
        port.set_tail_max_chars(4).await;
        let tail = port.new_tail().await;
        let mut previous = ToolRunTailSnapshot::default();

        tail.append_text("ab");
        assert!(tail.snapshot_if_changed(&mut previous));
        assert_eq!(previous.as_str(), "ab");
        assert!(!tail.snapshot_if_changed(&mut previous));

        tail.append_text("cd");
        assert!(tail.snapshot_if_changed(&mut previous));
        assert_eq!(previous.as_str(), "abcd");

        tail.append_text("e");
        assert!(tail.snapshot_if_changed(&mut previous));
        assert_eq!(previous.as_str(), "bcde");
        assert!(!tail.snapshot_if_changed(&mut previous));
    }
}
