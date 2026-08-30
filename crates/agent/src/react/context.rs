//! Pending-context source aggregation for the ReAct loop.
//!
//! This module owns queue and inbox reads only. It returns owned context
//! values; projection into events, messages and the canonical transcript stays
//! in `inject.rs` through `apply_transcript`.

use std::sync::Arc;

use crate::react::sidecars::MessagingPoller;
use crate::session::{ReactContextBatch, SessionExecutor};
use haven_common::types::{InjectSource, MessageAttachment};
use haven_memory::Database;

/// Fallback interval (in ReAct steps) for the automatic cross-session inbox
/// check. Delivery notifications drive the check in-process (immediate), and
/// this cadence only catches missed notifications (e.g. another process
/// wrote to the mailbox).
const MESSAGING_POLL_EVERY_STEPS: u32 = 3;

/// Per-message text cap when injecting cross-session messages into the model
/// context (defensive: a full message is at most 16 KiB, but a burst must not
/// flood the observation budget).
pub(super) const MESSAGING_INJECT_CHARS: usize = 400;

/// One context item collected from a queue or inbox and ready for projection.
#[derive(Debug, Clone)]
pub(super) struct PendingContext {
    pub(super) source: InjectSource,
    pub(super) text: String,
    pub(super) attachments: Vec<MessageAttachment>,
    pub(super) message_id: Option<String>,
}

/// All queue-owned context collected at one step boundary.
#[derive(Debug, Default)]
pub(super) struct PendingContextBatch {
    pub(super) items: Vec<PendingContext>,
    pub(super) clears_ask: bool,
}

/// Reads pending session context and cross-session messages.
pub(super) struct ContextSource {
    executor: Arc<SessionExecutor>,
    db: Arc<Database>,
    messaging: MessagingPoller,
}

impl ContextSource {
    pub(super) fn new(executor: Arc<SessionExecutor>, db: Arc<Database>) -> Self {
        Self {
            executor,
            db,
            messaging: MessagingPoller::new(),
        }
    }

    /// Drain the next model context. Steering is delivered first; follow-ups
    /// are held back until steering is empty. Only source queues are touched
    /// here; the caller decides how to project each item.
    pub(super) async fn drain_pending_context(&self, session_id: &str) -> PendingContextBatch {
        let ReactContextBatch {
            steering,
            follow_ups,
            action_results,
        } = self.executor.drain_react_context(session_id).await;
        let mut batch = PendingContextBatch::default();

        for steering_item in steering {
            let source = if steering_item.is_answer {
                batch.clears_ask = true;
                InjectSource::Answer
            } else {
                InjectSource::Steering
            };
            batch.items.push(PendingContext {
                source,
                text: steering_item.text,
                attachments: steering_item.attachments,
                message_id: steering_item.message_id,
            });
        }

        for follow_up in follow_ups {
            let source = if follow_up.is_answer {
                batch.clears_ask = true;
                InjectSource::Answer
            } else {
                InjectSource::FollowUp
            };
            batch.items.push(PendingContext {
                source,
                text: follow_up.text,
                attachments: follow_up.attachments,
                message_id: follow_up.message_id,
            });
        }

        for text in action_results {
            batch.items.push(PendingContext {
                source: InjectSource::ActionResult,
                text,
                attachments: Vec::new(),
                message_id: None,
            });
        }

        batch
    }

    /// Poll the cross-session inbox when a delivery notification arrives or
    /// the fallback cadence is due. Heartbeats remain best effort and are
    /// coalesced by [`MessagingPoller`].
    pub(super) async fn poll_inbox(&self, session_id: &str) -> Option<PendingContext> {
        let cached_title = {
            let state = self.messaging.lock();
            state.title_cache.get(session_id).cloned()
        };
        let title = match cached_title {
            Some(title) => title,
            None => {
                let title = self
                    .db
                    .run_blocking({
                        let sid = session_id.to_string();
                        move |db| {
                            let title = db.get_session(&sid).ok().flatten().and_then(|s| s.title);
                            Ok::<Option<String>, anyhow::Error>(title)
                        }
                    })
                    .await
                    .unwrap_or(None);
                self.messaging
                    .lock()
                    .title_cache
                    .insert(session_id.to_string(), title.clone());
                title
            }
        };

        let (bus, due) = {
            let mut state = self.messaging.lock();
            let bus = state.bus.clone();
            state.steps_since_poll += 1;
            let notified = state.rx.has_changed().unwrap_or(false);
            if notified {
                let _ = state.rx.borrow_and_update();
            }
            let due = notified || state.steps_since_poll >= MESSAGING_POLL_EVERY_STEPS;
            if due {
                state.steps_since_poll = 0;
            }
            (bus, due)
        };

        let session_id_owned = session_id.to_string();
        if let Some(inflight) = self.messaging.try_begin_heartbeat(session_id) {
            let heartbeat_session_id = session_id_owned.clone();
            let heartbeat_title = title.clone();
            let heartbeat_bus = bus.clone();
            tokio::task::spawn_blocking(move || {
                let _ = heartbeat_bus.register_with_title(
                    &heartbeat_session_id,
                    &[],
                    heartbeat_title.as_deref(),
                );
                inflight.lock().unwrap().remove(&heartbeat_session_id);
            });
        }

        if !due {
            return None;
        }

        let poll_session_id = session_id_owned.clone();
        let messages = match tokio::task::spawn_blocking(move || {
            let read = bus.read_and_archive(&poll_session_id)?;
            let _receipts = bus.send_receipts(&poll_session_id, &read);
            Ok::<_, anyhow::Error>(read)
        })
        .await
        {
            Ok(Ok(messages)) => messages,
            Ok(Err(error)) => {
                tracing::debug!("messaging inbox poll failed for {session_id}: {error}");
                return None;
            }
            Err(error) => {
                tracing::debug!("messaging inbox poll join failed: {error}");
                return None;
            }
        };
        if messages.is_empty() {
            return None;
        }

        let mut text = String::new();
        for envelope in &messages {
            text.push_str(&super::inject::format_cross_session_inject(envelope));
            text.push('\n');
        }
        Some(PendingContext {
            source: InjectSource::CrossSession,
            text: text.trim_end().to_string(),
            attachments: Vec::new(),
            message_id: None,
        })
    }

    /// Drop per-session inbox caches when a session leaves the working set.
    pub(super) fn clear_session(&self, session_id: &str) {
        self.messaging.clear_session(session_id);
    }
}
