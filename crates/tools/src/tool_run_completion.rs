//! Completion transport for detached background ToolRuns.

use std::time::Duration;

use haven_common::ToolRunCompletionPayload;
use haven_common::lifecycle::ToolRunStatus;
use tokio::sync::broadcast;

use crate::tool_run_service::ToolRunService;

const TOOL_RUN_COMPLETION_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const TOOL_RUN_COMPLETION_CHANNEL_CAPACITY: usize = 256;

/// A background ToolRun that has reached a terminal state, surfaced to a
/// consumer so the owning session receives it without status polling.
#[derive(Clone, Debug)]
pub struct BackgroundToolRunCompletion {
    pub tool_run_id: String,
    /// Stable identity of the terminal result across retries and recovery.
    pub tool_run_result_id: String,
    pub session_id: Option<String>,
    pub status: ToolRunStatus,
    pub payload: ToolRunCompletionPayload,
}

/// Completion stream for detached background ToolRuns.
#[derive(Clone, Debug)]
pub enum ToolRunCompletion {
    Background(BackgroundToolRunCompletion),
}

/// Receiver for the background completion stream.
pub struct ToolRunCompletionReceiver {
    rx: broadcast::Receiver<ToolRunCompletion>,
}

/// Shared owner of the transient background completion broadcast.
pub(crate) struct ToolRunCompletionBus {
    tx: broadcast::Sender<ToolRunCompletion>,
}

impl ToolRunCompletionBus {
    pub(crate) fn new() -> Self {
        let (tx, _) = broadcast::channel(TOOL_RUN_COMPLETION_CHANNEL_CAPACITY);
        Self { tx }
    }

    pub(crate) fn subscribe(&self) -> ToolRunCompletionReceiver {
        ToolRunCompletionReceiver {
            rx: self.tx.subscribe(),
        }
    }

    pub(crate) fn send(
        &self,
        completion: ToolRunCompletion,
    ) -> Result<usize, Box<broadcast::error::SendError<ToolRunCompletion>>> {
        self.tx.send(completion).map_err(Box::new)
    }
}

impl ToolRunCompletionReceiver {
    /// Receive a background ToolRun result from the transient broadcast or
    /// durable completion outbox. The outbox remains the recovery source if a
    /// process exits after terminal persistence but before delivery.
    pub async fn recv_tool_run_result_with_recovery(
        &mut self,
        service: &ToolRunService,
    ) -> Option<ToolRunCompletion> {
        let mut reconcile = tokio::time::interval(TOOL_RUN_COMPLETION_RECONCILE_INTERVAL);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        reconcile.tick().await;
        loop {
            if let Some(completion) = service.claim_pending_tool_run_result().await {
                return Some(completion);
            }
            match tokio::select! {
                result = self.rx.recv() => result,
                _ = reconcile.tick() => continue,
            } {
                Ok(completion) => return Some(completion),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "ToolRun result receiver lagged; reconciling durable outbox"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return service.claim_pending_tool_run_result().await;
                }
            }
        }
    }

    #[cfg(test)]
    pub async fn recv(&mut self) -> Option<ToolRunCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(completion) => return Some(completion),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ToolRun completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    #[cfg(test)]
    pub async fn recv_background(&mut self) -> Option<ToolRunCompletion> {
        self.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(tool_run_id: String) -> ToolRunCompletion {
        ToolRunCompletion::Background(BackgroundToolRunCompletion {
            tool_run_result_id: tool_run_id.clone(),
            payload: ToolRunCompletionPayload {
                tool_run_id: tool_run_id.clone(),
                tool_name: "shell".into(),
                status: ToolRunStatus::Completed,
                output: None,
                error: None,
                error_reason: None,
                log_path: None,
                started_at: None,
                finished_at: None,
                source_step_id: None,
                truncated: false,
            },
            tool_run_id,
            session_id: None,
            status: ToolRunStatus::Completed,
        })
    }

    #[tokio::test]
    async fn background_completion_passes_through() {
        let bus = ToolRunCompletionBus::new();
        let mut receiver = bus.subscribe();
        bus.send(completion(
            "toolrun-0000000000000000000000000000001b".into(),
        ))
        .unwrap();
        let Some(ToolRunCompletion::Background(received)) = receiver.recv().await else {
            panic!("background completion must pass through");
        };
        assert_eq!(
            received.tool_run_result_id,
            "toolrun-0000000000000000000000000000001b"
        );
    }

    #[tokio::test]
    async fn lagged_receiver_continues_and_closed_receiver_returns_none() {
        let bus = ToolRunCompletionBus::new();
        let mut receiver = bus.subscribe();
        for index in 0..=TOOL_RUN_COMPLETION_CHANNEL_CAPACITY {
            bus.send(completion(format!("toolrun-{index:032x}")))
                .unwrap();
        }
        let Some(ToolRunCompletion::Background(received)) = receiver.recv().await else {
            panic!("lag recovery should return a retained event");
        };
        assert_eq!(
            received.tool_run_id,
            "toolrun-00000000000000000000000000000001"
        );
        drop(bus);
        // Buffered messages remain readable after the last sender closes.
        while receiver.recv().await.is_some() {}
    }
}
