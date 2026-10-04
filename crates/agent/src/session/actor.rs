//! Single-owner runtime for one session.
//!
//! `SessionActor` is the only component that mutates session-local runtime
//! state.  Callers hold a cheap [`SessionActorHandle`] and send typed
//! commands; they never acquire a lock around `SessionInfo` or one of the
//! session's auxiliary queues.
//!
//! ADR 0214 的热 transcript 由 `SessionState::react_run` 持有的 actor-local
//! run future 独占；actor task 在同一 select loop 中轮询它与外部 mailbox。
//! ReActState 不跨 session 共享，stream identity 和 usage 仍由各自的 run/runtime
//! owner 管理。

use super::RunEngine;
use super::{FollowUp, SessionInfo, SessionStatus, SessionWaitingReason, StepInfo};
use crate::interaction::{InteractionKind, InteractionRequest, InteractionStatus};
use crate::react::{LoopExit, ReActEngine, ReActState, RunInput, RunReplay};
use futures_util::FutureExt;
use haven_common::types::MessageAttachment;
#[cfg(test)]
use haven_memory::Database;
use haven_memory::{
    INTERACTION_CLEARED_EVENT_TYPE, INTERACTION_REQUESTED_EVENT_TYPE,
    INTERACTION_RESOLVED_EVENT_TYPE, SessionEventInput, SessionStore,
};
use haven_messaging::inbox::{Envelope, MessageType};
use serde_json::Value;
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

const ACTOR_MAILBOX_CAPACITY: usize = 128;
const ACTOR_RELEASE_CAPACITY: usize = 1;

/// Hard limits for process-local context queues.  The ingress path persists a
/// user message before it calls these queues, so rejecting an item is an
/// explicit back-pressure result rather than a silent drop.  The same limits
/// apply to answers because an ask reply is still a user-owned follow-up.
pub(crate) const CONTEXT_QUEUE_MAX_ITEMS: usize = 64;
pub(crate) const CONTEXT_QUEUE_MAX_CHARS: usize = 64 * 1024;
pub(crate) const CONTEXT_ITEM_MAX_CHARS: usize = 8 * 1024;
pub(crate) const CONTEXT_ITEM_MAX_ATTACHMENTS: usize = 8;
pub(crate) const CONTEXT_ITEM_MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const CONTEXT_QUEUE_MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;

/// A single model turn receives a bounded slice.  Items left in the actor
/// queue are deliberately retained for a later turn.
pub(crate) const CONTEXT_BATCH_MAX_ITEMS: usize = 16;
pub(crate) const CONTEXT_BATCH_MAX_CHARS: usize = 16 * 1024;
pub(crate) const CONTEXT_BATCH_MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

const SESSION_INBOX_MAX_ITEMS: usize = 256;
const SESSION_INBOX_ITEM_MAX_CHARS: usize = 16 * 1024;
const SESSION_INBOX_ARCHIVE_MAX_ITEMS: usize = 1_024;

#[derive(Debug)]
pub(crate) struct StatusTransition {
    pub changed: bool,
    pub pending: bool,
    pub terminal: bool,
}

#[derive(Debug)]
pub(crate) struct RunClaim {
    pub accepted: bool,
}

#[derive(Debug)]
pub(crate) struct RunFinished {
    pub pending: bool,
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ContextQueueStats {
    pub steering_items: usize,
    pub follow_up_items: usize,
    pub action_result_items: usize,
}

/// A terminal background-action result waiting for transcript projection.
/// `action_result_id` is stable across broadcast re-delivery and queue retries;
/// it is never regenerated at the transcript boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionResult {
    pub(crate) action_result_id: String,
    pub(crate) text: String,
}

impl ContextQueueStats {
    pub(crate) fn total_items(self) -> usize {
        self.steering_items
            .saturating_add(self.follow_up_items)
            .saturating_add(self.action_result_items)
    }
}

#[derive(Debug)]
pub(crate) struct ConfirmDecision {
    pub request: InteractionRequest,
    pub wake_session: bool,
}

pub(crate) struct ReactRunOutput {
    pub(crate) exit: LoopExit,
    pub(crate) events: Vec<crate::types::TranscriptRecord>,
}

/// The stable command surface of a session actor.  The supervisor and the
/// ReAct loop use these commands instead of reaching into separate queue,
/// interaction, or background-result maps.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    Submit {
        text: String,
        attachments: Vec<MessageAttachment>,
        is_answer: bool,
        message_id: Option<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Steer {
        text: String,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    ResolveInteraction {
        request_id: String,
        response: Value,
        expired: bool,
        reply: oneshot::Sender<anyhow::Result<Option<ConfirmDecision>>>,
    },
    Cancel {
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    BackgroundResult {
        action_result_id: String,
        text: String,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
}

/// `'static` subscribe closure carried by [`ActorCommand`]. It is not
/// debugged; the actor runs it at most once.
pub(crate) struct InboxSubscribe(Box<dyn FnOnce() -> watch::Receiver<u64> + Send>);

impl std::fmt::Debug for InboxSubscribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InboxSubscribe(..)")
    }
}

pub(crate) enum ActorCommand {
    Session(SessionCommand),
    Run {
        engine: RunEngine,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    RunReactLoop {
        engine: Arc<ReActEngine>,
        replay: RunReplay,
        input: RunInput,
        reply: oneshot::Sender<anyhow::Result<ReactRunOutput>>,
    },
    Snapshot {
        reply: oneshot::Sender<SessionInfo>,
    },
    UpdateTitle {
        title: String,
    },
    SetWaitingReason {
        reason: Option<SessionWaitingReason>,
    },
    Transition {
        expected: Option<SessionStatus>,
        status: SessionStatus,
        persist: bool,
        reply: oneshot::Sender<anyhow::Result<StatusTransition>>,
    },
    ClaimRun {
        reply: oneshot::Sender<anyhow::Result<RunClaim>>,
    },
    BeginDirectRun {
        reply: oneshot::Sender<bool>,
    },
    FinishRun {
        reply: oneshot::Sender<RunFinished>,
    },
    IsRunning {
        reply: oneshot::Sender<bool>,
    },
    TickMessagingPoll {
        every_steps: u32,
        subscribe: InboxSubscribe,
        reply: oneshot::Sender<MessagingPollTick>,
    },
    RememberMessagingTitle {
        title: Option<String>,
    },
    ClearMessaging,
    DrainFollowUps {
        reply: oneshot::Sender<Vec<FollowUp>>,
    },
    DrainSteering {
        reply: oneshot::Sender<Vec<FollowUp>>,
    },
    DrainContext {
        reply: oneshot::Sender<(Vec<FollowUp>, Vec<FollowUp>, Vec<ActionResult>)>,
    },
    HasPendingContext {
        reply: oneshot::Sender<bool>,
    },
    ContextQueueStats {
        reply: oneshot::Sender<ContextQueueStats>,
    },
    MarkQueuesAsAnswer,
    DrainActionCompletions {
        reply: oneshot::Sender<Vec<ActionResult>>,
    },
    RequestInteraction {
        request: Box<InteractionRequest>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    RequestConfirmBatch {
        requests: Vec<InteractionRequest>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    ListInteractions {
        kind: Option<InteractionKind>,
        pending_only: bool,
        reply: oneshot::Sender<Vec<InteractionRequest>>,
    },
    ClearInteractions {
        kind: Option<InteractionKind>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    RecordStep {
        step: Box<StepInfo>,
    },
    SetHasChildren {
        value: bool,
    },
    HasChildren {
        reply: oneshot::Sender<bool>,
    },
    ClearRuntime,
    DeliverMessage {
        envelope: Box<Envelope>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    ClaimMessages {
        reply: oneshot::Sender<Vec<Envelope>>,
    },
    AckMessages {
        ids: Vec<String>,
        reply: oneshot::Sender<()>,
    },
    LastReceived {
        reply: oneshot::Sender<Option<Envelope>>,
    },
    FindMessage {
        id: String,
        reply: oneshot::Sender<Option<Envelope>>,
    },
    TakeMatchingReplies {
        in_reply_to: String,
        expected_from: String,
        reply: oneshot::Sender<Vec<Envelope>>,
    },
    History {
        limit: usize,
        reply: oneshot::Sender<Vec<Envelope>>,
    },
}

/// A cloneable mailbox endpoint for one session.
#[derive(Clone)]
pub(crate) struct SessionActorHandle {
    pub(crate) id: String,
    tx: mpsc::Sender<ActorCommand>,
    actor_lifetime: CancellationToken,
    run_cancellation: watch::Receiver<CancellationToken>,
    status: watch::Sender<SessionStatus>,
    run_state: watch::Sender<bool>,
    release_run: mpsc::Sender<()>,
}

impl SessionActorHandle {
    pub(crate) fn status(&self) -> watch::Receiver<SessionStatus> {
        self.status.subscribe()
    }

    pub(crate) fn run_state(&self) -> watch::Receiver<bool> {
        self.run_state.subscribe()
    }

    pub(crate) fn run_cancellation_token(&self) -> CancellationToken {
        self.run_cancellation.borrow().clone()
    }

    /// Cancel all current and future run tokens because this actor is being
    /// torn down. Interrupting a single run must use its child token instead.
    pub(crate) fn cancel_actor(&self) {
        self.actor_lifetime.cancel();
    }

    #[cfg(test)]
    pub(crate) fn is_same_actor(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }

    pub(crate) async fn send(&self, command: ActorCommand) -> anyhow::Result<()> {
        self.tx
            .send(command)
            .await
            .map_err(|_| anyhow::anyhow!("session actor '{}' has stopped", self.id))
    }

    pub(crate) async fn snapshot(&self) -> Option<SessionInfo> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Snapshot { reply }).await.ok()?;
        rx.await.ok()
    }

    pub(crate) async fn transition(
        &self,
        status: SessionStatus,
        persist: bool,
    ) -> anyhow::Result<StatusTransition> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Transition {
            expected: None,
            status,
            persist,
            reply,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped transition", self.id))?
    }

    pub(crate) async fn transition_if(
        &self,
        expected: SessionStatus,
        status: SessionStatus,
        persist: bool,
    ) -> anyhow::Result<StatusTransition> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Transition {
            expected: Some(expected),
            status,
            persist,
            reply,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped transition", self.id))?
    }

    pub(crate) async fn set_waiting_reason(
        &self,
        reason: Option<SessionWaitingReason>,
    ) -> anyhow::Result<()> {
        self.send(ActorCommand::SetWaitingReason { reason }).await
    }

    pub(crate) async fn claim_run(&self) -> anyhow::Result<RunClaim> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::ClaimRun { reply }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped run claim", self.id))?
    }

    pub(crate) async fn begin_direct_run(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::BeginDirectRun { reply })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub(crate) async fn finish_run(&self) -> Option<RunFinished> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::FinishRun { reply }).await.ok()?;
        rx.await.ok()
    }

    pub(crate) fn release_run_now(&self) {
        // Releasing is idempotent. A one-slot side channel coalesces repeated
        // synchronous guard drops without consuming mailbox capacity or
        // creating a waiting Tokio task when the mailbox is full.
        let _ = self.release_run.try_send(());
    }

    pub(crate) async fn is_running(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        if self.send(ActorCommand::IsRunning { reply }).await.is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Start and await a run that is polled by this actor task. While the
    /// handler is suspended on a provider, tool, or storage future, the actor
    /// continues receiving and applying external mailbox commands.
    pub(crate) async fn run(&self, engine: RunEngine) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Run { engine, reply }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped run result", self.id))?
    }

    pub(crate) async fn run_react_loop(
        &self,
        engine: Arc<ReActEngine>,
        replay: RunReplay,
        input: RunInput,
    ) -> anyhow::Result<ReactRunOutput> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::RunReactLoop {
            engine,
            replay,
            input,
            reply,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped ReAct loop result", self.id))?
    }

    pub(crate) async fn queue_follow_up(
        &self,
        text: &str,
        attachments: &[MessageAttachment],
        is_answer: bool,
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Session(SessionCommand::Submit {
            text: text.to_string(),
            attachments: attachments.to_vec(),
            is_answer,
            message_id,
            reply,
        }))
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped follow-up", self.id))?
    }

    pub(crate) async fn drain_follow_ups(&self) -> Vec<FollowUp> {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::DrainFollowUps { reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub(crate) async fn queue_steering(
        &self,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Session(SessionCommand::Steer {
            text: text.to_string(),
            attachments: attachments.to_vec(),
            message_id,
            reply,
        }))
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped steering", self.id))?
    }

    pub(crate) async fn drain_steering(&self) -> Vec<FollowUp> {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::DrainSteering { reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub(crate) async fn drain_context(&self) -> (Vec<FollowUp>, Vec<FollowUp>, Vec<ActionResult>) {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::DrainContext { reply })
            .await
            .is_err()
        {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        rx.await.unwrap_or_default()
    }

    pub(crate) async fn has_pending_context(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::HasPendingContext { reply })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub(crate) async fn context_queue_stats(&self) -> ContextQueueStats {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::ContextQueueStats { reply })
            .await
            .is_err()
        {
            return ContextQueueStats::default();
        }
        rx.await.unwrap_or_default()
    }

    pub(crate) async fn mark_queues_as_answer(&self) {
        let _ = self.send(ActorCommand::MarkQueuesAsAnswer).await;
    }

    pub(crate) async fn add_action_completion(
        &self,
        action_result_id: String,
        text: String,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Session(SessionCommand::BackgroundResult {
            action_result_id,
            text,
            reply,
        }))
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped action result", self.id))?
    }

    pub(crate) async fn drain_action_completions(&self) -> Vec<ActionResult> {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::DrainActionCompletions { reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub(crate) async fn request_interaction(
        &self,
        request: InteractionRequest,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::RequestInteraction {
            request: Box::new(request),
            reply,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped interaction", self.id))?
    }

    pub(crate) async fn request_confirm_batch(
        &self,
        requests: Vec<InteractionRequest>,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::RequestConfirmBatch { requests, reply })
            .await?;
        rx.await.map_err(|_| {
            anyhow::anyhow!("session actor '{}' dropped confirmation batch", self.id)
        })?
    }

    pub(crate) async fn interactions(
        &self,
        kind: Option<InteractionKind>,
        pending_only: bool,
    ) -> Vec<InteractionRequest> {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::ListInteractions {
                kind,
                pending_only,
                reply,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub(crate) async fn clear_interactions(
        &self,
        kind: Option<InteractionKind>,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::ClearInteractions { kind, reply })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped interaction clear", self.id))?
    }

    pub(crate) async fn resolve_interaction(
        &self,
        request_id: String,
        response: Value,
        expired: bool,
    ) -> anyhow::Result<Option<ConfirmDecision>> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Session(SessionCommand::ResolveInteraction {
            request_id,
            response,
            expired,
            reply,
        }))
        .await?;
        rx.await.map_err(|_| {
            anyhow::anyhow!("session actor '{}' dropped interaction resolution", self.id)
        })?
    }

    pub(crate) async fn cancel_session(&self) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Session(SessionCommand::Cancel { reply }))
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped cancel", self.id))?
    }

    pub(crate) async fn record_step(&self, step: StepInfo) {
        let _ = self
            .send(ActorCommand::RecordStep {
                step: Box::new(step),
            })
            .await;
    }

    pub(crate) async fn set_has_children(&self, value: bool) {
        let _ = self.send(ActorCommand::SetHasChildren { value }).await;
    }

    pub(crate) async fn has_children(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::HasChildren { reply })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub(crate) async fn clear_runtime(&self) {
        let _ = self.send(ActorCommand::ClearRuntime).await;
    }

    /// Advance this session's inbox poll cursor. The subscribe closure runs
    /// only when the actor does not already hold a watch receiver, so a second
    /// turn cannot replace another session's notification position.
    pub(crate) async fn tick_messaging_poll(
        &self,
        every_steps: u32,
        subscribe: impl FnOnce() -> watch::Receiver<u64> + Send + 'static,
    ) -> MessagingPollTick {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::TickMessagingPoll {
                every_steps,
                subscribe: InboxSubscribe(Box::new(subscribe)),
                reply,
            })
            .await
            .is_err()
        {
            return MessagingPollTick {
                title: MessagingTitle::Missing,
                due: false,
            };
        }
        rx.await.unwrap_or(MessagingPollTick {
            title: MessagingTitle::Missing,
            due: false,
        })
    }

    pub(crate) async fn remember_messaging_title(&self, title: Option<String>) {
        let _ = self
            .send(ActorCommand::RememberMessagingTitle { title })
            .await;
    }

    pub(crate) fn clear_messaging_now(&self) {
        let _ = self.tx.try_send(ActorCommand::ClearMessaging);
    }

    /// Synchronous mailbox operations are called from the service's blocking
    /// transport boundary. Tokio's blocking channel/receiver methods preserve
    /// actor serialization without exposing `SessionState`.
    pub(crate) fn deliver_message(&self, envelope: Envelope) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .blocking_send(ActorCommand::DeliverMessage {
                envelope: Box::new(envelope),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("session actor '{}' has stopped", self.id))?;
        rx.blocking_recv()
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped message delivery", self.id))?
    }

    pub(crate) fn claim_messages(&self) -> Vec<Envelope> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActorCommand::ClaimMessages { reply })
            .is_err()
        {
            return Vec::new();
        }
        rx.blocking_recv().unwrap_or_default()
    }

    pub(crate) fn ack_messages(&self, ids: Vec<String>) {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActorCommand::AckMessages { ids, reply })
            .is_ok()
        {
            let _ = rx.blocking_recv();
        }
    }

    pub(crate) fn last_received_message(&self) -> Option<Envelope> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActorCommand::LastReceived { reply })
            .is_err()
        {
            return None;
        }
        rx.blocking_recv().ok().flatten()
    }

    pub(crate) fn find_message_by_id(&self, id: String) -> Option<Envelope> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActorCommand::FindMessage { id, reply })
            .is_err()
        {
            return None;
        }
        rx.blocking_recv().ok().flatten()
    }

    pub(crate) fn take_matching_replies_blocking(
        &self,
        in_reply_to: String,
        expected_from: String,
    ) -> Vec<Envelope> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActorCommand::TakeMatchingReplies {
                in_reply_to,
                expected_from,
                reply,
            })
            .is_err()
        {
            return Vec::new();
        }
        rx.blocking_recv().unwrap_or_default()
    }

    pub(crate) fn history_blocking(&self, limit: usize) -> Vec<Envelope> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActorCommand::History { limit, reply })
            .is_err()
        {
            return Vec::new();
        }
        rx.blocking_recv().unwrap_or_default()
    }
}

/// Complete mutable state for one session.
///
/// The supervisor never owns a field from this structure.  It only owns the
/// registry and sends commands to the actor mailbox; this invariant is what
/// makes admission and lifecycle coordination independent from turn state.
pub(crate) struct SessionState {
    info: SessionInfo,
    action_completions: Vec<ActionResult>,
    action_completion_chars: usize,
    interactions: Vec<InteractionRequest>,
    follow_up_queue: Vec<FollowUp>,
    follow_up_chars: usize,
    follow_up_attachment_bytes: usize,
    steering_queue: Vec<FollowUp>,
    steering_chars: usize,
    steering_attachment_bytes: usize,
    has_children: bool,
    running: bool,
    inbox: VecDeque<Envelope>,
    processing: Vec<Envelope>,
    archive: VecDeque<Envelope>,
    active_message_ids: HashSet<String>,
    archive_message_ids: HashSet<String>,
    /// Process-local messaging poll cursor and title cache.
    messaging: SessionMessagingState,
    /// The active ReAct run and its captured hot transcript are owned by this
    /// session state and polled only by the actor task.
    react_run: Option<ActiveReactRun>,
}

type ReactLoopFuture = Pin<Box<dyn Future<Output = anyhow::Result<ReactRunOutput>> + Send>>;

/// The active-loop slot owns its future and reply together. Its presence also
/// represents the per-run claim; after completion, `Claimed` keeps duplicate
/// starts closed until the surrounding actor run releases that claim.
enum ActiveReactRun {
    Running {
        future: ReactLoopFuture,
        reply: oneshot::Sender<anyhow::Result<ReactRunOutput>>,
        claimed: bool,
    },
    Claimed,
}

/// Replay the interaction lifecycle from the active event stream. Resolved
/// confirmation gates stay until the corresponding tool batch commits and
/// appends an interaction-clear event; ask ToolResult transcript events also
/// repair the crash window before their separate requested event was appended.
/// Materialized messages and steps are never recovery input.
pub(crate) async fn load_interactions(
    store: &SessionStore,
    session_id: &str,
) -> anyhow::Result<Vec<InteractionRequest>> {
    let active_events = store.read_active_events_async(session_id).await?;
    crate::interaction::replay_session_interactions(session_id, &active_events)
}

async fn append_interaction_event(
    store: &SessionStore,
    session_id: &str,
    event_type: &str,
    payload: String,
) -> anyhow::Result<()> {
    store
        .append_domain_event(session_id, event_type, &payload)
        .await?;
    Ok(())
}

fn panic_reason(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

pub(crate) fn spawn(
    store: SessionStore,
    info: SessionInfo,
    interactions: Vec<InteractionRequest>,
) -> SessionActorHandle {
    let (tx, mut rx) = mpsc::channel(ACTOR_MAILBOX_CAPACITY);
    let (release_run, mut release_run_rx) = mpsc::channel(ACTOR_RELEASE_CAPACITY);
    let (status, _) = watch::channel(info.status);
    let (run_state, _) = watch::channel(false);
    let actor_lifetime = CancellationToken::new();
    let initial_run_cancellation = actor_lifetime.child_token();
    let (run_cancellation, run_cancellation_rx) = watch::channel(initial_run_cancellation.clone());
    let handle = SessionActorHandle {
        id: info.id.clone(),
        tx,
        actor_lifetime: actor_lifetime.clone(),
        run_cancellation: run_cancellation_rx,
        status: status.clone(),
        run_state: run_state.clone(),
        release_run,
    };
    tokio::spawn(async move {
        let mut current_run_cancellation = initial_run_cancellation;
        let mut state = SessionState {
            info,
            action_completions: Vec::new(),
            action_completion_chars: 0,
            interactions,
            follow_up_queue: Vec::new(),
            follow_up_chars: 0,
            follow_up_attachment_bytes: 0,
            steering_queue: Vec::new(),
            steering_chars: 0,
            steering_attachment_bytes: 0,
            has_children: false,
            running: false,
            inbox: VecDeque::new(),
            processing: Vec::new(),
            archive: VecDeque::new(),
            active_message_ids: HashSet::new(),
            archive_message_ids: HashSet::new(),
            messaging: SessionMessagingState::default(),
            react_run: None,
        };
        type ActiveRun = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
        let mut active_run: Option<ActiveRun> = None;
        let mut active_run_reply: Option<oneshot::Sender<anyhow::Result<()>>> = None;
        let mut release_run_open = true;
        loop {
            enum Wake {
                Command(Option<ActorCommand>),
                ReleaseRun(Option<()>),
                Run(anyhow::Result<()>),
                ReactLoop(anyhow::Result<ReactRunOutput>),
            }
            let wake = tokio::select! {
                command = rx.recv() => Wake::Command(command),
                released = release_run_rx.recv(), if release_run_open => Wake::ReleaseRun(released),
                result = async {
                    match active_run.as_mut() {
                        Some(run) => run.as_mut().await,
                        None => std::future::pending().await,
                    }
                } => Wake::Run(result),
                result = async {
                    match state.react_run.as_mut() {
                        Some(ActiveReactRun::Running { future, .. }) => future.as_mut().await,
                        None => std::future::pending().await,
                        Some(ActiveReactRun::Claimed) => std::future::pending().await,
                    }
                } => Wake::ReactLoop(result),
            };
            // If both channels are ready, apply a queued release before a
            // later mailbox command such as FinishRun can make the slot
            // available for another direct run.
            let release_requested = match &wake {
                Wake::ReleaseRun(Some(())) => true,
                Wake::ReleaseRun(None) => false,
                _ => release_run_rx.try_recv().is_ok(),
            };
            if release_requested {
                release_direct_run(&mut state, &run_state);
            }
            let command = match wake {
                Wake::Command(Some(command)) => command,
                Wake::Command(None) => {
                    if let Some(reply) = active_run_reply.take() {
                        let _ =
                            reply.send(Err(anyhow::anyhow!("session actor stopped during run")));
                    }
                    if let Some(ActiveReactRun::Running { reply, .. }) = state.react_run.take() {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "session actor stopped during ReAct loop"
                        )));
                    }
                    break;
                }
                Wake::ReleaseRun(Some(())) => continue,
                Wake::ReleaseRun(None) => {
                    release_run_open = false;
                    continue;
                }
                Wake::Run(result) => {
                    active_run = None;
                    if let Some(reply) = active_run_reply.take() {
                        let _ = reply.send(result);
                    }
                    continue;
                }
                Wake::ReactLoop(result) => {
                    let Some(ActiveReactRun::Running { reply, claimed, .. }) =
                        state.react_run.take()
                    else {
                        unreachable!("only a running ReAct loop can complete")
                    };
                    if claimed {
                        state.react_run = Some(ActiveReactRun::Claimed);
                    }
                    let _ = reply.send(result);
                    continue;
                }
            };
            match command {
                ActorCommand::Session(command) => match command {
                    SessionCommand::Submit {
                        text,
                        attachments,
                        is_answer,
                        message_id,
                        reply,
                    } => {
                        let result =
                            queue_follow_up(&mut state, text, attachments, is_answer, message_id);
                        let _ = reply.send(result);
                    }
                    SessionCommand::Steer {
                        text,
                        attachments,
                        message_id,
                        reply,
                    } => {
                        let result = queue_steering(&mut state, text, attachments, message_id);
                        let _ = reply.send(result);
                    }
                    SessionCommand::ResolveInteraction {
                        request_id,
                        response,
                        expired,
                        reply,
                    } => {
                        let result = resolve_interaction(&state, &request_id, response, expired);
                        let result = match result {
                            Some(decision) => {
                                let payload = match serde_json::to_string(&decision.request) {
                                    Ok(payload) => payload,
                                    Err(error) => {
                                        let _ = reply.send(Err(error.into()));
                                        continue;
                                    }
                                };
                                let persisted = append_interaction_event(
                                    &store,
                                    &state.info.id,
                                    INTERACTION_RESOLVED_EVENT_TYPE,
                                    payload,
                                )
                                .await;
                                match persisted {
                                    Ok(()) => {
                                        if let Some(request) = state
                                            .interactions
                                            .iter_mut()
                                            .find(|request| request.id == decision.request.id)
                                        {
                                            *request = decision.request.clone();
                                        }
                                        Ok(Some(decision))
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            None => Ok(None),
                        };
                        let _ = reply.send(result);
                    }
                    SessionCommand::Cancel { reply } => {
                        let ids = state
                            .interactions
                            .iter()
                            .map(|request| request.id.clone())
                            .collect::<Vec<_>>();
                        let result = if ids.is_empty() {
                            Ok(())
                        } else {
                            let payload = serde_json::json!({ "ids": ids });
                            append_interaction_event(
                                &store,
                                &state.info.id,
                                INTERACTION_CLEARED_EVENT_TYPE,
                                payload.to_string(),
                            )
                            .await
                        };
                        if result.is_ok() {
                            state.action_completions.clear();
                            state.action_completion_chars = 0;
                            state.follow_up_queue.clear();
                            state.follow_up_chars = 0;
                            state.follow_up_attachment_bytes = 0;
                            state.steering_queue.clear();
                            state.steering_chars = 0;
                            state.steering_attachment_bytes = 0;
                            state.interactions.clear();
                            current_run_cancellation.cancel();
                        }
                        let _ = reply.send(result);
                    }
                    SessionCommand::BackgroundResult {
                        action_result_id,
                        text,
                        reply,
                    } => {
                        let result = queue_action_completion(
                            &mut state.action_completions,
                            &mut state.action_completion_chars,
                            action_result_id,
                            text,
                        );
                        let _ = reply.send(result);
                    }
                },
                ActorCommand::Run { engine, reply } => {
                    if active_run.is_some() || !state.running {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "session actor '{}' cannot start a run in its current state",
                            state.info.id
                        )));
                        continue;
                    }
                    let session_id = state.info.id.clone();
                    active_run = Some(Box::pin(async move {
                        match AssertUnwindSafe(engine.run(session_id))
                            .catch_unwind()
                            .await
                        {
                            Ok(result) => result,
                            Err(payload) => {
                                let reason = payload
                                    .downcast_ref::<&str>()
                                    .map(|message| (*message).to_string())
                                    .or_else(|| payload.downcast_ref::<String>().cloned())
                                    .unwrap_or_else(|| "non-string panic payload".into());
                                Err(anyhow::anyhow!("handler panicked: {reason}"))
                            }
                        }
                    }));
                    active_run_reply = Some(reply);
                }
                ActorCommand::RunReactLoop {
                    engine,
                    replay,
                    input,
                    reply,
                } => {
                    if !state.running || state.react_run.is_some() {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "session actor '{}' cannot start another ReAct loop",
                            state.info.id
                        )));
                        continue;
                    }
                    let mut react_state =
                        ReActState::new(replay.events, replay.canonical, replay.branch_points);
                    let future: ReactLoopFuture = Box::pin(async move {
                        let result =
                            AssertUnwindSafe(engine.run_react_loop(input, &mut react_state))
                                .catch_unwind()
                                .await
                                .map_err(|payload| {
                                    anyhow::anyhow!(
                                        "ReAct loop panicked: {}",
                                        panic_reason(payload)
                                    )
                                })
                                .and_then(std::convert::identity);
                        result.map(|exit| ReactRunOutput {
                            exit,
                            events: react_state.events.clone(),
                        })
                    });
                    state.react_run = Some(ActiveReactRun::Running {
                        future,
                        reply,
                        claimed: true,
                    });
                }
                ActorCommand::Snapshot { reply } => {
                    let _ = reply.send(state.info.clone());
                }
                ActorCommand::UpdateTitle { title } => {
                    state.info.title = Some(title);
                }
                ActorCommand::SetWaitingReason { reason } => {
                    state.info.waiting_reason = (state.info.status == SessionStatus::Paused)
                        .then_some(reason)
                        .flatten();
                }
                ActorCommand::Transition {
                    expected,
                    status: next,
                    persist,
                    reply,
                } => {
                    let result = if expected.is_none_or(|expected| state.info.status == expected) {
                        transition(&store, &mut state, &status, next, persist).await
                    } else {
                        Ok(StatusTransition {
                            changed: false,
                            pending: false,
                            terminal: false,
                        })
                    };
                    let _ = reply.send(result);
                }
                ActorCommand::ClaimRun { reply } => {
                    let result = claim_run(&store, &mut state, &status, &run_state).await;
                    if result.as_ref().is_ok_and(|claim| claim.accepted) {
                        current_run_cancellation = actor_lifetime.child_token();
                        run_cancellation.send_replace(current_run_cancellation.clone());
                    }
                    let _ = reply.send(result);
                }
                ActorCommand::BeginDirectRun { reply } => {
                    let accepted = !state.running && !state.info.status.is_terminal();
                    if accepted {
                        current_run_cancellation = actor_lifetime.child_token();
                        run_cancellation.send_replace(current_run_cancellation.clone());
                        state.running = true;
                        let _ = run_state.send(true);
                    }
                    let _ = reply.send(accepted);
                }
                ActorCommand::FinishRun { reply } => {
                    state.running = false;
                    match state.react_run.as_mut() {
                        Some(ActiveReactRun::Running { claimed, .. }) => *claimed = false,
                        Some(ActiveReactRun::Claimed) => state.react_run = None,
                        None => {}
                    }
                    let _ = run_state.send(false);
                    let _ = reply.send(RunFinished {
                        pending: state.info.status == SessionStatus::Pending,
                        terminal: state.info.status.is_terminal(),
                    });
                }
                ActorCommand::IsRunning { reply } => {
                    let _ = reply.send(state.running);
                }
                ActorCommand::DrainFollowUps { reply } => {
                    state.follow_up_chars = 0;
                    state.follow_up_attachment_bytes = 0;
                    let _ = reply.send(std::mem::take(&mut state.follow_up_queue));
                }
                ActorCommand::DrainSteering { reply } => {
                    state.steering_chars = 0;
                    state.steering_attachment_bytes = 0;
                    let _ = reply.send(std::mem::take(&mut state.steering_queue));
                }
                ActorCommand::DrainContext { reply } => {
                    let mut budget = ContextBatchBudget::default();
                    let had_steering = !state.steering_queue.is_empty();
                    let steering = take_follow_ups(
                        &mut state.steering_queue,
                        &mut state.steering_chars,
                        &mut state.steering_attachment_bytes,
                        &mut budget,
                    );
                    // Steering retains its existing priority: while any
                    // steering was queued at the boundary, follow-ups stay
                    // deferred until the next drain even if this batch had
                    // enough budget to consume the entire steering queue.
                    let follow_ups = if !had_steering {
                        take_follow_ups(
                            &mut state.follow_up_queue,
                            &mut state.follow_up_chars,
                            &mut state.follow_up_attachment_bytes,
                            &mut budget,
                        )
                    } else {
                        Vec::new()
                    };
                    let action_results = take_action_results(
                        &mut state.action_completions,
                        &mut state.action_completion_chars,
                        &mut budget,
                    );
                    let _ = reply.send((steering, follow_ups, action_results));
                }
                ActorCommand::HasPendingContext { reply } => {
                    let _ = reply.send(
                        !state.follow_up_queue.is_empty()
                            || !state.steering_queue.is_empty()
                            || !state.action_completions.is_empty(),
                    );
                }
                ActorCommand::ContextQueueStats { reply } => {
                    let _ = reply.send(ContextQueueStats {
                        steering_items: state.steering_queue.len(),
                        follow_up_items: state.follow_up_queue.len(),
                        action_result_items: state.action_completions.len(),
                    });
                }
                ActorCommand::MarkQueuesAsAnswer => {
                    for item in &mut state.follow_up_queue {
                        item.is_answer = true;
                    }
                    for item in &mut state.steering_queue {
                        item.is_answer = true;
                    }
                }
                ActorCommand::DrainActionCompletions { reply } => {
                    state.action_completion_chars = 0;
                    let _ = reply.send(std::mem::take(&mut state.action_completions));
                }
                ActorCommand::RequestInteraction { request, reply } => {
                    let request = *request;
                    let result = match crate::interaction::validate_session_association(
                        request.session_id.as_deref(),
                        &state.info.id,
                    ) {
                        Err(error) => Err(error),
                        Ok(()) => match serde_json::to_string(&request) {
                            Ok(payload) => {
                                append_interaction_event(
                                    &store,
                                    &state.info.id,
                                    INTERACTION_REQUESTED_EVENT_TYPE,
                                    payload,
                                )
                                .await
                            }
                            Err(error) => Err(error.into()),
                        },
                    };
                    if result.is_ok() {
                        state
                            .interactions
                            .retain(|existing| existing.id != request.id);
                        state.interactions.push(request);
                    }
                    let _ = reply.send(result);
                }
                ActorCommand::RequestConfirmBatch { requests, reply } => {
                    let result = async {
                        let mut ids = HashSet::with_capacity(requests.len());
                        let mut events = Vec::with_capacity(requests.len());
                        for request in &requests {
                            anyhow::ensure!(
                                request.kind == InteractionKind::Confirm
                                    && request.status == InteractionStatus::Pending,
                                "confirmation batch contains a non-pending confirmation"
                            );
                            request.validate_new_pending_permission()?;
                            crate::interaction::validate_session_association(
                                request.session_id.as_deref(),
                                &state.info.id,
                            )?;
                            anyhow::ensure!(
                                ids.insert(request.id.clone()),
                                "interaction batch contains duplicate request id '{}'",
                                request.id
                            );
                            events.push(SessionEventInput::new(
                                INTERACTION_REQUESTED_EVENT_TYPE,
                                serde_json::to_string(request)?,
                            ));
                        }
                        anyhow::ensure!(
                            state.info.status.can_transition_to(SessionStatus::Paused),
                            "session cannot pause for confirmation from status '{}'",
                            state.info.status.as_str()
                        );
                        store
                            .append_domain_event_batch_with_session_status(
                                &state.info.id,
                                state.info.status,
                                SessionStatus::Paused,
                                &events,
                            )
                            .await?;
                        state.info.status = SessionStatus::Paused;
                        state.info.updated_at = chrono::Utc::now().to_rfc3339();
                        let _ = status.send(SessionStatus::Paused);
                        state
                            .interactions
                            .retain(|existing| !ids.contains(&existing.id));
                        state.interactions.extend(requests);
                        Ok(())
                    }
                    .await;
                    let _ = reply.send(result);
                }
                ActorCommand::ListInteractions {
                    kind,
                    pending_only,
                    reply,
                } => {
                    let requests = state
                        .interactions
                        .iter()
                        .filter(|request| {
                            kind.is_none_or(|wanted| request.kind == wanted)
                                && (!pending_only || request.status == InteractionStatus::Pending)
                        })
                        .cloned()
                        .collect();
                    let _ = reply.send(requests);
                }
                ActorCommand::ClearInteractions { kind, reply } => {
                    let ids = state
                        .interactions
                        .iter()
                        .filter(|request| kind.is_none_or(|wanted| request.kind == wanted))
                        .map(|request| request.id.clone())
                        .collect::<Vec<_>>();
                    let result = if ids.is_empty() {
                        Ok(())
                    } else {
                        let payload = serde_json::json!({ "ids": ids });
                        match serde_json::to_string(&payload) {
                            Ok(payload) => {
                                append_interaction_event(
                                    &store,
                                    &state.info.id,
                                    INTERACTION_CLEARED_EVENT_TYPE,
                                    payload,
                                )
                                .await
                            }
                            Err(error) => Err(error.into()),
                        }
                    };
                    if result.is_ok() {
                        state
                            .interactions
                            .retain(|request| !kind.is_none_or(|wanted| request.kind == wanted));
                    }
                    let _ = reply.send(result);
                }
                ActorCommand::RecordStep { step } => {
                    if state.running && state.info.status == SessionStatus::Running {
                        state.info.steps.push(*step);
                        state.info.updated_at = chrono::Utc::now().to_rfc3339();
                    }
                }
                ActorCommand::SetHasChildren { value } => {
                    state.has_children = value;
                }
                ActorCommand::HasChildren { reply } => {
                    let _ = reply.send(state.has_children);
                }
                ActorCommand::TickMessagingPoll {
                    every_steps,
                    subscribe,
                    reply,
                } => {
                    let _ = reply.send(tick_messaging_poll(&mut state, every_steps, subscribe));
                }
                ActorCommand::RememberMessagingTitle { title } => {
                    remember_messaging_title(&mut state, title);
                }
                ActorCommand::ClearMessaging => {
                    clear_messaging(&mut state);
                }
                ActorCommand::ClearRuntime => {
                    state.action_completions.clear();
                    state.action_completion_chars = 0;
                    state.follow_up_queue.clear();
                    state.follow_up_chars = 0;
                    state.follow_up_attachment_bytes = 0;
                    state.steering_queue.clear();
                    state.steering_chars = 0;
                    state.steering_attachment_bytes = 0;
                    state.interactions.clear();
                    state.has_children = false;
                    clear_messaging(&mut state);
                }
                ActorCommand::DeliverMessage { envelope, reply } => {
                    let result = if message_known(&state, &envelope.id) {
                        Ok(())
                    } else if state.inbox.len() >= SESSION_INBOX_MAX_ITEMS {
                        Err(anyhow::anyhow!(
                            "session inbox is full ({} items); message was retained by the sender and must be retried",
                            SESSION_INBOX_MAX_ITEMS
                        ))
                    } else if envelope.text.chars().count() > SESSION_INBOX_ITEM_MAX_CHARS {
                        Err(anyhow::anyhow!(
                            "session inbox message exceeds {} characters; message was retained by the sender and must be retried",
                            SESSION_INBOX_ITEM_MAX_CHARS
                        ))
                    } else {
                        state.active_message_ids.insert(envelope.id.clone());
                        state.inbox.push_back(*envelope);
                        Ok(())
                    };
                    let _ = reply.send(result);
                }
                ActorCommand::ClaimMessages { reply } => {
                    let _ = reply.send(claim_messages(&mut state));
                }
                ActorCommand::AckMessages { ids, reply } => {
                    state
                        .processing
                        .retain(|envelope| !ids.iter().any(|id| id == &envelope.id));
                    for id in ids {
                        state.active_message_ids.remove(&id);
                    }
                    let _ = reply.send(());
                }
                ActorCommand::LastReceived { reply } => {
                    let result = state
                        .inbox
                        .back()
                        .cloned()
                        .or_else(|| {
                            state
                                .processing
                                .iter()
                                .max_by_key(|env| &env.created_at)
                                .cloned()
                        })
                        .or_else(|| state.archive.back().cloned());
                    let _ = reply.send(result);
                }
                ActorCommand::FindMessage { id, reply } => {
                    let result = state
                        .inbox
                        .iter()
                        .find(|env| env.id == id)
                        .cloned()
                        .or_else(|| state.processing.iter().find(|env| env.id == id).cloned())
                        .or_else(|| state.archive.iter().rev().find(|env| env.id == id).cloned());
                    let _ = reply.send(result);
                }
                ActorCommand::TakeMatchingReplies {
                    in_reply_to,
                    expected_from,
                    reply,
                } => {
                    let mut matching = Vec::new();
                    let mut rest = VecDeque::new();
                    while let Some(envelope) = state.inbox.pop_front() {
                        if is_matching_reply(&envelope, &in_reply_to, &expected_from) {
                            let message_id = envelope.id.clone();
                            if haven_messaging::is_expired(&envelope) {
                                archive_once(&mut state, envelope);
                            } else {
                                archive_once(&mut state, envelope.clone());
                                matching.push(envelope);
                            }
                            state.active_message_ids.remove(&message_id);
                        } else {
                            rest.push_back(envelope);
                        }
                    }
                    state.inbox = rest;
                    let _ = reply.send(matching);
                }
                ActorCommand::History { limit, reply } => {
                    let _ = reply.send(history(&state, limit));
                }
            }
        }
        actor_lifetime.cancel();
        let _ = run_state.send(false);
    });
    handle
}

fn release_direct_run(state: &mut SessionState, run_state: &watch::Sender<bool>) {
    state.running = false;
    match state.react_run.as_mut() {
        Some(ActiveReactRun::Running { claimed, .. }) => *claimed = false,
        Some(ActiveReactRun::Claimed) => state.react_run = None,
        None => {}
    }
    let _ = run_state.send(false);
}

fn message_known(state: &SessionState, id: &str) -> bool {
    state.active_message_ids.contains(id) || state.archive_message_ids.contains(id)
}

fn archive_once(state: &mut SessionState, envelope: Envelope) {
    if !state.archive_message_ids.insert(envelope.id.clone()) {
        return;
    }
    state.archive.push_back(envelope);
    while state.archive.len() > SESSION_INBOX_ARCHIVE_MAX_ITEMS {
        if let Some(evicted) = state.archive.pop_front() {
            state.archive_message_ids.remove(&evicted.id);
        }
    }
}

fn claim_messages(state: &mut SessionState) -> Vec<Envelope> {
    let mut candidates = Vec::with_capacity(state.processing.len() + state.inbox.len());
    candidates.extend(std::mem::take(&mut state.processing));
    candidates.extend(state.inbox.drain(..));

    let mut seen = HashSet::new();
    let mut claimed = Vec::new();
    for mut envelope in candidates {
        if !seen.insert(envelope.id.clone()) {
            continue;
        }
        archive_once(state, envelope.clone());
        if haven_messaging::is_expired(&envelope) {
            continue;
        }
        envelope.delivery_attempt = envelope.delivery_attempt.saturating_add(1);
        state.processing.push(envelope.clone());
        claimed.push(envelope);
    }
    claimed
}

fn is_matching_reply(envelope: &Envelope, in_reply_to: &str, expected_from: &str) -> bool {
    envelope.from == expected_from
        && envelope.in_reply_to.as_deref() == Some(in_reply_to)
        && matches!(envelope.r#type, MessageType::Reply | MessageType::Message)
}

fn history(state: &SessionState, limit: usize) -> Vec<Envelope> {
    let mut by_id = HashMap::new();
    for envelope in &state.archive {
        by_id.insert(envelope.id.clone(), envelope.clone());
    }
    for envelope in &state.processing {
        by_id.insert(envelope.id.clone(), envelope.clone());
    }
    for envelope in &state.inbox {
        by_id.insert(envelope.id.clone(), envelope.clone());
    }
    let mut entries: Vec<Envelope> = by_id.into_values().collect();
    entries.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    entries.reverse();
    entries.truncate(limit);
    entries
}

async fn transition(
    store: &SessionStore,
    state: &mut SessionState,
    status: &watch::Sender<SessionStatus>,
    next: SessionStatus,
    persist: bool,
) -> anyhow::Result<StatusTransition> {
    let old = state.info.status;
    if old == next {
        return Ok(StatusTransition {
            changed: false,
            pending: next == SessionStatus::Pending,
            terminal: false,
        });
    }
    if !old.can_transition_to(next) {
        tracing::warn!(
            session_id = %state.info.id,
            from = old.as_str(),
            to = next.as_str(),
            "rejected illegal session transition"
        );
        anyhow::bail!(
            "illegal session transition {} -> {}",
            old.as_str(),
            next.as_str()
        );
    }
    if persist {
        super::SessionSupervisor::persist_status(store, &state.info.id, next).await?;
    }
    state.info.status = next;
    if next != SessionStatus::Paused {
        state.info.waiting_reason = None;
    }
    state.info.updated_at = chrono::Utc::now().to_rfc3339();
    let _ = status.send(next);
    Ok(StatusTransition {
        changed: true,
        pending: next == SessionStatus::Pending,
        terminal: next.is_terminal(),
    })
}

async fn claim_run(
    store: &SessionStore,
    state: &mut SessionState,
    status: &watch::Sender<SessionStatus>,
    run_state: &watch::Sender<bool>,
) -> anyhow::Result<RunClaim> {
    if state.info.status != SessionStatus::Pending || state.running {
        return Ok(RunClaim { accepted: false });
    }
    super::SessionSupervisor::persist_status(store, &state.info.id, SessionStatus::Running).await?;
    state.info.status = SessionStatus::Running;
    state.info.waiting_reason = None;
    state.info.updated_at = chrono::Utc::now().to_rfc3339();
    state.running = true;
    let _ = status.send(SessionStatus::Running);
    let _ = run_state.send(true);
    Ok(RunClaim { accepted: true })
}

fn queue_follow_up(
    state: &mut SessionState,
    text: String,
    attachments: Vec<MessageAttachment>,
    is_answer: bool,
    message_id: Option<String>,
) -> anyhow::Result<()> {
    validate_context_item(&text, &attachments)?;
    if let Some(mid) = message_id.as_deref()
        && state
            .follow_up_queue
            .iter()
            .any(|item| item.message_id.as_deref() == Some(mid))
    {
        return Ok(());
    }
    let chars = text.chars().count();
    let attachment_bytes = attachment_bytes(&attachments);
    ensure_queue_capacity(
        state.follow_up_queue.len(),
        state.follow_up_chars,
        state.follow_up_attachment_bytes,
        chars,
        attachment_bytes,
        "follow-up",
    )?;
    state.follow_up_queue.push(if is_answer {
        FollowUp::answer_with_message_id(&text, attachments, message_id)
    } else {
        FollowUp::new_with_message_id(&text, attachments, message_id)
    });
    state.follow_up_chars += chars;
    state.follow_up_attachment_bytes += attachment_bytes;
    Ok(())
}

fn queue_steering(
    state: &mut SessionState,
    text: String,
    attachments: Vec<MessageAttachment>,
    message_id: Option<String>,
) -> anyhow::Result<()> {
    validate_context_item(&text, &attachments)?;
    if let Some(mid) = message_id.as_deref()
        && state
            .steering_queue
            .iter()
            .any(|item| item.message_id.as_deref() == Some(mid))
    {
        return Ok(());
    }
    let chars = text.chars().count();
    let attachment_bytes = attachment_bytes(&attachments);
    ensure_queue_capacity(
        state.steering_queue.len(),
        state.steering_chars,
        state.steering_attachment_bytes,
        chars,
        attachment_bytes,
        "steering",
    )?;
    state
        .steering_queue
        .push(FollowUp::with_message_id(&text, attachments, message_id));
    state.steering_chars += chars;
    state.steering_attachment_bytes += attachment_bytes;
    Ok(())
}

fn queue_action_completion(
    queue: &mut Vec<ActionResult>,
    queue_chars: &mut usize,
    action_result_id: String,
    text: String,
) -> anyhow::Result<()> {
    validate_context_item(&text, &[])?;
    if queue
        .iter()
        .any(|item| item.action_result_id == action_result_id)
    {
        return Ok(());
    }
    let chars = text.chars().count();
    ensure_queue_capacity(queue.len(), *queue_chars, 0, chars, 0, "action result")?;
    queue.push(ActionResult {
        action_result_id,
        text,
    });
    *queue_chars += chars;
    Ok(())
}

fn attachment_bytes(attachments: &[MessageAttachment]) -> usize {
    attachments
        .iter()
        .map(|attachment| attachment.data.len())
        .sum()
}

fn validate_context_item(text: &str, attachments: &[MessageAttachment]) -> anyhow::Result<()> {
    let chars = text.chars().count();
    if chars > CONTEXT_ITEM_MAX_CHARS {
        anyhow::bail!(
            "context item exceeds {} characters; input was retained by durable ingress and must be retried",
            CONTEXT_ITEM_MAX_CHARS
        );
    }
    if attachments.len() > CONTEXT_ITEM_MAX_ATTACHMENTS {
        anyhow::bail!(
            "context item exceeds {} attachments; input was retained by durable ingress and must be retried",
            CONTEXT_ITEM_MAX_ATTACHMENTS
        );
    }
    if let Some(index) = attachments
        .iter()
        .position(|attachment| attachment.data.len() > CONTEXT_ITEM_MAX_ATTACHMENT_BYTES)
    {
        anyhow::bail!(
            "context attachment {} exceeds {} bytes; input was retained by durable ingress and must be retried",
            index,
            CONTEXT_ITEM_MAX_ATTACHMENT_BYTES
        );
    }
    let bytes = attachment_bytes(attachments);
    if bytes > CONTEXT_QUEUE_MAX_ATTACHMENT_BYTES {
        anyhow::bail!(
            "context item attachments exceed {} bytes; input was retained by durable ingress and must be retried",
            CONTEXT_QUEUE_MAX_ATTACHMENT_BYTES
        );
    }
    Ok(())
}

fn ensure_queue_capacity(
    item_count: usize,
    chars: usize,
    attachment_bytes: usize,
    new_chars: usize,
    new_attachment_bytes: usize,
    kind: &str,
) -> anyhow::Result<()> {
    if item_count >= CONTEXT_QUEUE_MAX_ITEMS {
        anyhow::bail!(
            "{kind} queue is full ({} items); input was retained by durable ingress and is deferred",
            CONTEXT_QUEUE_MAX_ITEMS
        );
    }
    if chars.saturating_add(new_chars) > CONTEXT_QUEUE_MAX_CHARS {
        anyhow::bail!(
            "{kind} queue exceeds {} characters; input was retained by durable ingress and is deferred",
            CONTEXT_QUEUE_MAX_CHARS
        );
    }
    if attachment_bytes.saturating_add(new_attachment_bytes) > CONTEXT_QUEUE_MAX_ATTACHMENT_BYTES {
        anyhow::bail!(
            "{kind} queue exceeds {} attachment bytes; input was retained by durable ingress and is deferred",
            CONTEXT_QUEUE_MAX_ATTACHMENT_BYTES
        );
    }
    Ok(())
}

#[derive(Default)]
struct ContextBatchBudget {
    items: usize,
    chars: usize,
    attachment_bytes: usize,
}

fn fits_budget(budget: &ContextBatchBudget, chars: usize, attachment_bytes: usize) -> bool {
    budget.items < CONTEXT_BATCH_MAX_ITEMS
        && budget.chars.saturating_add(chars) <= CONTEXT_BATCH_MAX_CHARS
        && budget.attachment_bytes.saturating_add(attachment_bytes)
            <= CONTEXT_BATCH_MAX_ATTACHMENT_BYTES
}

fn take_follow_ups(
    queue: &mut Vec<FollowUp>,
    queue_chars: &mut usize,
    queue_attachment_bytes: &mut usize,
    budget: &mut ContextBatchBudget,
) -> Vec<FollowUp> {
    let mut taken = Vec::new();
    let mut count = 0;
    while count < queue.len() {
        let item = &queue[count];
        let chars = item.text.chars().count();
        let attachments = attachment_bytes(&item.attachments);
        if !fits_budget(budget, chars, attachments) {
            break;
        }
        budget.items += 1;
        budget.chars += chars;
        budget.attachment_bytes += attachments;
        count += 1;
    }
    if count > 0 {
        taken.extend(queue.drain(..count));
        *queue_chars =
            queue_chars.saturating_sub(taken.iter().map(|item| item.text.chars().count()).sum());
        *queue_attachment_bytes = queue_attachment_bytes.saturating_sub(
            taken
                .iter()
                .map(|item| attachment_bytes(&item.attachments))
                .sum(),
        );
    }
    taken
}

fn take_action_results(
    queue: &mut Vec<ActionResult>,
    queue_chars: &mut usize,
    budget: &mut ContextBatchBudget,
) -> Vec<ActionResult> {
    let mut count = 0;
    while count < queue.len() && fits_budget(budget, queue[count].text.chars().count(), 0) {
        budget.items += 1;
        budget.chars += queue[count].text.chars().count();
        count += 1;
    }
    let taken: Vec<_> = queue.drain(..count).collect();
    *queue_chars =
        queue_chars.saturating_sub(taken.iter().map(|item| item.text.chars().count()).sum());
    taken
}

fn resolve_interaction(
    state: &SessionState,
    request_id: &str,
    response: Value,
    expired: bool,
) -> Option<ConfirmDecision> {
    let request = state
        .interactions
        .iter()
        .find(|request| request.id == request_id)?;
    let mut resolved = request.clone();
    if resolved.status != InteractionStatus::Pending || resolved.kind != InteractionKind::Confirm {
        return None;
    }
    let resolved_ok = if expired {
        resolved.expire()
    } else {
        response.is_boolean() && resolved.resolve(response)
    };
    if !resolved_ok {
        return None;
    }
    let wake_session = state
        .interactions
        .iter()
        .filter(|entry| entry.kind == InteractionKind::Confirm)
        .all(|entry| entry.id == resolved.id || entry.status != InteractionStatus::Pending);
    Some(ConfirmDecision {
        request: resolved,
        wake_session,
    })
}

#[derive(Default)]
struct SessionMessagingState {
    inbox_watch: Option<watch::Receiver<u64>>,
    steps_since_poll: u32,
    /// `None` has not been loaded. `Some(None)` is a session with no title.
    title: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessagingTitle {
    Cached(Option<String>),
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessagingPollTick {
    pub(crate) title: MessagingTitle,
    pub(crate) due: bool,
}

fn tick_messaging_poll(
    state: &mut SessionState,
    every_steps: u32,
    subscribe: InboxSubscribe,
) -> MessagingPollTick {
    let messaging = &mut state.messaging;
    messaging.steps_since_poll = messaging.steps_since_poll.saturating_add(1);
    if messaging.inbox_watch.is_none() {
        messaging.inbox_watch = Some((subscribe.0)());
    }
    let notified = messaging
        .inbox_watch
        .as_mut()
        .map(|receiver| match receiver.has_changed() {
            Ok(changed) => {
                if changed {
                    let _ = receiver.borrow_and_update();
                }
                changed
            }
            Err(_) => false,
        })
        .unwrap_or(false);
    let due = notified || messaging.steps_since_poll >= every_steps.max(1);
    if due {
        messaging.steps_since_poll = 0;
    }
    let title = match &messaging.title {
        Some(title) => MessagingTitle::Cached(title.clone()),
        None => MessagingTitle::Missing,
    };
    MessagingPollTick { title, due }
}

fn remember_messaging_title(state: &mut SessionState, title: Option<String>) {
    state.messaging.title = Some(title);
}

fn clear_messaging(state: &mut SessionState) {
    state.messaging = SessionMessagingState::default();
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn empty_state() -> SessionState {
        SessionState {
            info: SessionInfo {
                id: "ses-queue".into(),
                input: "queue".into(),
                summary: "queue".into(),
                title: None,
                status: SessionStatus::Pending,
                waiting_reason: None,
                steps: Vec::new(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            action_completions: Vec::new(),
            action_completion_chars: 0,
            interactions: Vec::new(),
            follow_up_queue: Vec::new(),
            follow_up_chars: 0,
            follow_up_attachment_bytes: 0,
            steering_queue: Vec::new(),
            steering_chars: 0,
            steering_attachment_bytes: 0,
            has_children: false,
            running: false,
            inbox: VecDeque::new(),
            processing: Vec::new(),
            archive: VecDeque::new(),
            active_message_ids: HashSet::new(),
            archive_message_ids: HashSet::new(),
            messaging: SessionMessagingState::default(),
            react_run: None,
        }
    }

    #[tokio::test]
    async fn actor_persists_only_interactions_for_its_session() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("actor-interaction-owner.db"))
                .expect("temporary database"),
        );
        let session = db.create_session("interaction owner").expect("session");
        let mut info = empty_state().info;
        info.id = session.id.clone();
        let actor = spawn(SessionStore::new(db.clone()), info, Vec::new());

        actor
            .request_interaction(crate::interaction::InteractionRequest::ask(
                &session.id,
                vec!["A".into()],
                vec!["step-valid".into()],
            ))
            .await
            .expect("matching session interaction should be persisted");
        assert!(
            actor
                .request_interaction(crate::interaction::InteractionRequest::ask(
                    "ses-other",
                    Vec::new(),
                    vec!["step-invalid".into()],
                ))
                .await
                .is_err()
        );

        let events = SessionStore::new(db)
            .read_active_events_async(&session.id)
            .await
            .expect("read active events");
        assert_eq!(events.len(), 1, "invalid request must not be appended");
        let persisted: crate::interaction::InteractionRequest =
            serde_json::from_str(&events[0].payload).expect("decode durable interaction");
        assert_eq!(persisted.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(persisted.id, "step-valid");
    }

    #[tokio::test]
    async fn actor_services_external_commands_while_run_handler_is_awaiting_provider() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let db_path = directory.path().join("actor.db");
        let db = Arc::new(Database::open(&db_path).expect("temporary database"));
        let store = SessionStore::new(db);
        let info = empty_state().info;
        let actor = spawn(store, info, Vec::new());
        assert!(actor.begin_direct_run().await);

        let (started_tx, mut started_rx) = watch::channel(false);
        let cancellation = actor.run_cancellation_token();
        let handler: crate::session::RunHandler = Arc::new(move |_session_id| {
            let cancellation = cancellation.clone();
            let started_tx = started_tx.clone();
            Box::pin(async move {
                started_tx.send_replace(true);
                cancellation.cancelled().await;
                Ok(())
            })
        });
        let run_actor = actor.clone();
        let run = tokio::spawn(async move { run_actor.run(RunEngine::new(handler)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !*started_rx.borrow() {
                started_rx.changed().await.expect("started signal sender");
            }
        })
        .await
        .expect("run handler should start");

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            actor.queue_follow_up("submit while waiting", &[], false, None),
        )
        .await
        .expect("submit should be serviced during the provider wait")
        .expect("submit should be accepted");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            actor.queue_steering("steer while waiting", &[], None),
        )
        .await
        .expect("steer should be serviced during the provider wait")
        .expect("steer should be accepted");

        actor
            .cancel_session()
            .await
            .expect("cancel should be serviced during the provider wait");
        tokio::time::timeout(std::time::Duration::from_secs(1), run)
            .await
            .expect("run should finish after cancellation")
            .expect("run task should join")
            .expect("run handler should complete");
        drop(actor);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn actor_makes_run_sender_and_cancel_progress_under_saturated_mailbox() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;

        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("actor-saturated.db"))
                .expect("temporary database"),
        );
        let actor = spawn(SessionStore::new(db), empty_state().info, Vec::new());
        assert!(actor.begin_direct_run().await);

        for index in 0..ACTOR_MAILBOX_CAPACITY {
            actor
                .tx
                .try_send(ActorCommand::UpdateTitle {
                    title: format!("queued-{index}"),
                })
                .expect("fill actor mailbox");
        }
        assert_eq!(actor.tx.capacity(), 0, "mailbox should start saturated");

        let keep_flooding = Arc::new(AtomicBool::new(true));
        let full_observations = Arc::new(AtomicUsize::new(0));
        let flood_actor = actor.clone();
        let flood_keep_flooding = keep_flooding.clone();
        let flood_full_observations = full_observations.clone();
        let flood = tokio::spawn(async move {
            let mut index = 0usize;
            while flood_keep_flooding.load(Ordering::Acquire) {
                match flood_actor.tx.try_send(ActorCommand::UpdateTitle {
                    title: format!("flood-{index}"),
                }) {
                    Ok(()) => index = index.wrapping_add(1),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        flood_full_observations.fetch_add(1, Ordering::Relaxed);
                        tokio::task::yield_now().await;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        });

        let run_polls = Arc::new(AtomicUsize::new(0));
        let run_cancellation = actor.run_cancellation_token();
        let handler_polls = run_polls.clone();
        let handler: crate::session::RunHandler = Arc::new(move |_session_id| {
            let cancellation = run_cancellation.clone();
            let polls = handler_polls.clone();
            Box::pin(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(2));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = cancellation.cancelled() => return Ok(()),
                        _ = tick.tick() => {
                            polls.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })
        });
        let run_actor = actor.clone();
        let run = tokio::spawn(async move { run_actor.run(RunEngine::new(handler)).await });

        tokio::time::timeout(Duration::from_secs(2), async {
            while full_observations.load(Ordering::Relaxed) == 0
                || run_polls.load(Ordering::Relaxed) < 4
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("mailbox saturation and active-run progress should both be observed");

        tokio::time::timeout(Duration::from_secs(2), actor.snapshot())
            .await
            .expect("a sender waiting on the saturated mailbox should make progress")
            .expect("snapshot should be served");

        tokio::time::timeout(Duration::from_secs(2), actor.cancel_session())
            .await
            .expect("cancel should be serviced under continuous mailbox traffic")
            .expect("cancel should succeed");
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("cancelled run should exit under continuous mailbox traffic")
            .expect("run task should join")
            .expect("run handler should complete");

        keep_flooding.store(false, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), flood)
            .await
            .expect("mailbox flooder should stop")
            .expect("flood task should join");
    }

    #[tokio::test]
    async fn direct_run_release_is_coalesced_when_actor_mailbox_is_full() {
        use std::time::Duration;

        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("actor-release-overload.db"))
                .expect("temporary database"),
        );
        let actor = spawn(SessionStore::new(db), empty_state().info, Vec::new());
        let mut run_state = actor.run_state();
        assert!(actor.begin_direct_run().await);
        assert!(*run_state.borrow());

        for index in 0..ACTOR_MAILBOX_CAPACITY {
            actor
                .tx
                .try_send(ActorCommand::UpdateTitle {
                    title: format!("queued-{index}"),
                })
                .expect("fill actor mailbox");
        }
        assert_eq!(actor.tx.capacity(), 0, "mailbox should be saturated");

        for _ in 0..10_000 {
            actor.release_run_now();
        }
        assert_eq!(
            actor.release_run.capacity(),
            0,
            "repeated releases should occupy only the one-slot signal"
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            while *run_state.borrow() {
                run_state
                    .changed()
                    .await
                    .expect("actor should retain the run-state sender");
            }
        })
        .await
        .expect("release should be observed while the main mailbox is saturated");
    }

    #[tokio::test]
    #[ignore = "manual performance profile; run with --ignored --nocapture"]
    async fn actor_mailbox_latency_profile_by_prefilled_depth() {
        use std::time::Instant;

        fn percentile(samples: &mut [u128], numerator: usize) -> u128 {
            samples.sort_unstable();
            let rank = samples.len().saturating_mul(numerator).div_ceil(100);
            samples[rank.saturating_sub(1)]
        }

        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("actor-mailbox-profile.db"))
                .expect("temporary database"),
        );
        let actor = spawn(SessionStore::new(db), empty_state().info, Vec::new());
        const SAMPLE_COUNT: usize = 129;
        const WARMUP_COUNT: usize = 8;

        for depth in [0, 32, 64, ACTOR_MAILBOX_CAPACITY] {
            for sample in 0..WARMUP_COUNT {
                for index in 0..depth {
                    actor
                        .tx
                        .try_send(ActorCommand::UpdateTitle {
                            title: format!("profile-{depth}-{sample}-{index}"),
                        })
                        .expect("prefill mailbox before actor task is polled");
                }
                let observed_depth = ACTOR_MAILBOX_CAPACITY - actor.tx.capacity();
                assert_eq!(observed_depth, depth, "prefilled queue depth");
                actor.snapshot().await.expect("snapshot should roundtrip");
            }

            let mut samples_ns = Vec::with_capacity(SAMPLE_COUNT);
            for sample in 0..SAMPLE_COUNT {
                for index in 0..depth {
                    actor
                        .tx
                        .try_send(ActorCommand::UpdateTitle {
                            title: format!("measure-{depth}-{sample}-{index}"),
                        })
                        .expect("prefill mailbox before measuring roundtrip");
                }
                let observed_depth = ACTOR_MAILBOX_CAPACITY - actor.tx.capacity();
                assert_eq!(observed_depth, depth, "measured queue depth");
                let started = Instant::now();
                actor.snapshot().await.expect("snapshot should roundtrip");
                samples_ns.push(started.elapsed().as_nanos());
            }

            let total_ns: u128 = samples_ns.iter().sum();
            let throughput = SAMPLE_COUNT as f64 * 1_000_000_000.0 / total_ns as f64;
            let p50_us = percentile(&mut samples_ns.clone(), 50) as f64 / 1_000.0;
            let p95_us = percentile(&mut samples_ns, 95) as f64 / 1_000.0;
            println!(
                "profile actor_mailbox depth={depth} high_water={depth} capacity={ACTOR_MAILBOX_CAPACITY} samples={SAMPLE_COUNT} command=snapshot_roundtrip p50_us={p50_us:.2} p95_us={p95_us:.2} throughput_per_s={throughput:.1}"
            );
        }

        drop(actor);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn completed_react_loop_returns_events_and_next_run_can_start() {
        struct NoopEmitter;

        #[async_trait::async_trait]
        impl crate::event::AgentEventEmitter for NoopEmitter {
            async fn emit(&self, _event: crate::event::AgentEvent) {}
        }

        let directory = tempfile::tempdir().expect("temporary database directory");
        let db = Arc::new(
            Database::open(&directory.path().join("actor-react-loop.db"))
                .expect("temporary database"),
        );
        let session = db.create_session("actor ReAct loop").expect("session");
        db.update_session_status(&session.id, SessionStatus::Paused)
            .expect("paused session status");

        let executor = Arc::new(crate::session::SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(haven_tools::ToolsManager::new()),
            1,
        ));
        executor
            .ensure_session_loaded(&session.id)
            .await
            .expect("load actor");
        let actor = executor
            .actor_for(&session.id)
            .await
            .expect("session actor");
        let engine = Arc::new(crate::react::ReActEngine::new(
            Arc::new(haven_llm::LlmRouter::new(
                haven_common::config::RouterConfig::default(),
            )),
            crate::react::test_tool_catalog_port(&executor),
            executor,
            haven_memory::MemoryStore::new(db.clone()),
            1,
            haven_common::config::ContextLimitsConfig::default(),
        ));
        let events = vec![crate::types::TranscriptRecord::Thought {
            step_number: 1,
            text: "replayed thought".into(),
            message_id: "step-actor-test".into(),
        }];
        engine
            .seed_transcript_events(&session.id, &events, 1)
            .await
            .expect("seed durable transcript");

        let session_id = session.id.clone();
        let run_loop = |run_id| {
            let actor = actor.clone();
            let engine = engine.clone();
            let events = events.clone();
            let session_id = session_id.clone();
            async move {
                actor
                    .run_react_loop(
                        engine,
                        RunReplay {
                            events,
                            canonical: Vec::new(),
                            branch_points: HashMap::new(),
                        },
                        RunInput {
                            session_id,
                            start_step: 1,
                            emitter: Arc::new(NoopEmitter),
                            run_id,
                        },
                    )
                    .await
            }
        };

        assert!(actor.begin_direct_run().await);
        let first = run_loop(1).await.expect("first ReAct loop");
        assert_eq!(
            first.exit,
            LoopExit::Paused {
                reason: crate::react::PauseReason::External,
            }
        );
        assert!(matches!(
            first.events.as_slice(),
            [crate::types::TranscriptRecord::Thought { text, .. }]
                if text == "replayed thought"
        ));

        let duplicate = run_loop(1).await.err().expect("same run remains claimed");
        assert!(
            duplicate
                .to_string()
                .contains("cannot start another ReAct loop")
        );

        actor.finish_run().await.expect("finish first run");
        assert!(actor.begin_direct_run().await);
        let second = run_loop(2).await.expect("next ReAct loop");
        assert_eq!(
            second.exit,
            LoopExit::Paused {
                reason: crate::react::PauseReason::External,
            }
        );
        assert!(matches!(
            second.events.as_slice(),
            [crate::types::TranscriptRecord::Thought { text, .. }]
                if text == "replayed thought"
        ));
        actor.finish_run().await.expect("finish second run");
    }

    #[test]
    fn messaging_poll_state_stays_on_the_session_and_clears() {
        let mut state = empty_state();
        let (tx, _rx) = watch::channel(0_u64);
        // Production subscriptions come from Sender::subscribe, which marks
        // the current value as seen. Cloning a receiver that never observed
        // the latest send would look like a fresh notification.
        let subscribe = |tx: &watch::Sender<u64>| {
            let tx = tx.clone();
            InboxSubscribe(Box::new(move || tx.subscribe()))
        };
        let first = tick_messaging_poll(&mut state, 3, subscribe(&tx));
        assert_eq!(first.title, MessagingTitle::Missing);
        assert!(!first.due);

        remember_messaging_title(&mut state, Some("hello".into()));
        let second = tick_messaging_poll(&mut state, 3, subscribe(&tx));
        assert_eq!(second.title, MessagingTitle::Cached(Some("hello".into())));
        assert!(!second.due);

        let third = tick_messaging_poll(&mut state, 3, subscribe(&tx));
        assert!(third.due);

        tx.send_replace(1);
        let notified = tick_messaging_poll(&mut state, 3, subscribe(&tx));
        assert!(notified.due);
        assert_eq!(notified.title, MessagingTitle::Cached(Some("hello".into())));

        clear_messaging(&mut state);
        let cleared = tick_messaging_poll(&mut state, 3, subscribe(&tx));
        assert_eq!(cleared.title, MessagingTitle::Missing);
        assert!(!cleared.due);
    }

    #[test]
    fn oversized_items_are_rejected_without_truncation() {
        let mut state = empty_state();
        let error = queue_follow_up(
            &mut state,
            "x".repeat(CONTEXT_ITEM_MAX_CHARS + 1),
            Vec::new(),
            false,
            None,
        )
        .expect_err("oversized input must apply back-pressure");
        assert!(error.to_string().contains("retained"));
        assert!(state.follow_up_queue.is_empty());
    }

    #[test]
    fn inbox_archive_is_bounded_and_message_dedupe_is_indexed() {
        let mut state = empty_state();
        for index in 0..(SESSION_INBOX_ARCHIVE_MAX_ITEMS + 8) {
            let mut envelope = Envelope::new("sender", "receiver", "message");
            envelope.id = format!("msg-{index}");
            archive_once(&mut state, envelope);
        }

        assert_eq!(state.archive.len(), SESSION_INBOX_ARCHIVE_MAX_ITEMS);
        assert!(!message_known(&state, "msg-0"));
        assert!(!message_known(&state, "msg-7"));
        assert!(message_known(&state, "msg-8"));
        assert!(message_known(
            &state,
            &format!("msg-{}", SESSION_INBOX_ARCHIVE_MAX_ITEMS + 7)
        ));
        assert_eq!(state.archive_message_ids.len(), state.archive.len());
    }

    #[test]
    fn bounded_drain_retains_tail_and_preserves_steering_priority() {
        let mut steering = (0..=CONTEXT_BATCH_MAX_ITEMS)
            .map(|index| FollowUp::new(format!("steer-{index}"), Vec::new()))
            .collect::<Vec<_>>();
        let mut follow_ups = vec![FollowUp::new("follow-up", Vec::new())];
        let mut steering_chars = steering.iter().map(|item| item.text.len()).sum();
        let mut follow_up_chars = follow_ups.iter().map(|item| item.text.len()).sum();
        let mut steering_attachment_bytes = 0;
        let mut follow_up_attachment_bytes = 0;
        let mut budget = ContextBatchBudget::default();

        let taken = take_follow_ups(
            &mut steering,
            &mut steering_chars,
            &mut steering_attachment_bytes,
            &mut budget,
        );
        assert_eq!(taken.len(), CONTEXT_BATCH_MAX_ITEMS);
        assert_eq!(steering.len(), 1, "the tail is explicitly deferred");
        assert!(follow_ups.len() == 1, "follow-ups remain behind steering");
        assert!(steering_chars > 0);

        let follow_taken = if steering.is_empty() {
            take_follow_ups(
                &mut follow_ups,
                &mut follow_up_chars,
                &mut follow_up_attachment_bytes,
                &mut budget,
            )
        } else {
            Vec::new()
        };
        assert!(follow_taken.is_empty());
    }
}
