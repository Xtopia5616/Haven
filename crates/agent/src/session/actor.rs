//! Single-owner runtime for one session.
//!
//! `SessionActor` is the only component that mutates session-local runtime
//! state.  Callers hold a cheap [`SessionActorHandle`] and send typed
//! commands; they never acquire a lock around `SessionInfo` or one of the
//! session's auxiliary queues.
//!
//! ADR 0214：热 transcript 的目标主人是这里的 `SessionState`。一次 run 要在
//! 本任务内执行，并且只在 yield 点拿 `&mut SessionState`。mailbox 只接收外部
//! 命令。usage、stream id 和 token estimate 是函数调用，不是命令；在 run
//! 迁入本任务之前，不要再增加这类内部命令，也不要只把缓存搬进来。

use super::{FollowUp, SessionInfo, SessionStatus, SessionWaitingReason, StepInfo};
use crate::interaction::{InteractionKind, InteractionRequest, InteractionStatus};
use crate::react::identity::IdentityMap;
use crate::react::sidecars::{CumulativeTotals, CumulativeUsage, TokenEstimateCache, UsageTracker};
use crate::types::RunBudget;
use haven_common::config::RequestKind;
use haven_common::types::{CanonicalMessage, MessageAttachment};
use haven_memory::{
    Database, INTERACTION_CLEARED_EVENT_TYPE, INTERACTION_REQUESTED_EVENT_TYPE,
    INTERACTION_RESOLVED_EVENT_TYPE, SessionStore,
};
use haven_tools::inbox::{Envelope, MessageType};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

const ACTOR_MAILBOX_CAPACITY: usize = 128;

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

/// Provider-neutral usage input accepted by the actor.  Keeping this DTO at
/// the actor boundary prevents the ReAct facade from mutating a per-session
/// cumulative map and then separately persisting the same call.
#[derive(Debug, Clone)]
pub(crate) struct UsageUpdate {
    pub request: RequestKind,
    pub model: Option<String>,
    pub step_number: i32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_miss_tokens: u32,
    pub cache_accounting: String,
    pub cache_diagnostics: Option<String>,
    pub cost_usd: f64,
    pub has_cost: bool,
    pub duration_ms: Option<u64>,
    pub context_tokens: u32,
    pub context_window: Option<u32>,
    pub cancel: Option<CancellationToken>,
}

/// `'static` subscribe closure carried by [`ActorCommand`]. It is not
/// debugged; the actor runs it at most once.
pub(crate) struct InboxSubscribe(Box<dyn FnOnce() -> watch::Receiver<u64> + Send>);

impl std::fmt::Debug for InboxSubscribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InboxSubscribe(..)")
    }
}

#[derive(Debug)]
pub(crate) enum ActorCommand {
    Session(SessionCommand),
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
    /// Release a direct-run slot from a synchronous guard drop. The follow-up
    /// `FinishRun` command still performs Pending requeue bookkeeping, but
    /// this command makes the run-finished edge observable immediately so an
    /// async rollback cannot wait on the guard that is already unwinding.
    ReleaseRun,
    IsRunning {
        reply: oneshot::Sender<bool>,
    },
    SetRunBudget {
        budget: RunBudget,
    },
    ClearRunBudget,
    EnsureStreamId {
        step: u32,
        run: u64,
        kind: &'static str,
        reply: oneshot::Sender<String>,
    },
    BlockStreamId {
        step: u32,
        run: u64,
        kind: &'static str,
        reply: oneshot::Sender<String>,
    },
    ClearStreamIds,
    RecordUsage {
        update: UsageUpdate,
        reply: oneshot::Sender<anyhow::Result<CumulativeTotals>>,
    },
    ResetUsage,
    InvalidateUsage,
    EstimateTokens {
        canonical: Vec<CanonicalMessage>,
        generation: u64,
        revision: u64,
        reply: oneshot::Sender<u32>,
    },
    AppendTokenEstimate {
        message: CanonicalMessage,
        canonical_len: usize,
        generation: u64,
        revision: u64,
    },
    ResetTokenEstimate,
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
    cancel: CancellationToken,
    status: watch::Sender<SessionStatus>,
    run_state: watch::Sender<bool>,
}

impl SessionActorHandle {
    pub(crate) fn status(&self) -> watch::Receiver<SessionStatus> {
        self.status.subscribe()
    }

    pub(crate) fn run_state(&self) -> watch::Receiver<bool> {
        self.run_state.subscribe()
    }

    pub(crate) fn cancel(&self) -> CancellationToken {
        self.cancel.clone()
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
        let command = ActorCommand::ReleaseRun;
        match self.tx.try_send(command) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(command)) => {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(command).await;
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    pub(crate) async fn is_running(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        if self.send(ActorCommand::IsRunning { reply }).await.is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub(crate) async fn set_run_budget(&self, budget: RunBudget) {
        let _ = self.send(ActorCommand::SetRunBudget { budget }).await;
    }

    pub(crate) fn clear_run_budget_now(&self) {
        let _ = self.tx.try_send(ActorCommand::ClearRunBudget);
    }

    pub(crate) async fn ensure_stream_id(
        &self,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::EnsureStreamId {
            step,
            run,
            kind,
            reply,
        })
        .await
        .ok()?;
        rx.await.ok()
    }

    pub(crate) async fn block_stream_id(
        &self,
        step: u32,
        run: u64,
        kind: &'static str,
    ) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::BlockStreamId {
            step,
            run,
            kind,
            reply,
        })
        .await
        .ok()?;
        rx.await.ok()
    }

    pub(crate) fn clear_stream_ids_now(&self) {
        let _ = self.tx.try_send(ActorCommand::ClearStreamIds);
    }

    pub(crate) async fn record_usage(
        &self,
        update: UsageUpdate,
    ) -> anyhow::Result<CumulativeTotals> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::RecordUsage { update, reply })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped usage update", self.id))?
    }

    pub(crate) fn reset_usage_now(&self) {
        let _ = self.tx.try_send(ActorCommand::ResetUsage);
    }

    pub(crate) fn invalidate_usage_now(&self) {
        let _ = self.tx.try_send(ActorCommand::InvalidateUsage);
    }

    pub(crate) async fn estimate_tokens(
        &self,
        canonical: Vec<CanonicalMessage>,
        generation: u64,
        revision: u64,
    ) -> u32 {
        let (reply, rx) = oneshot::channel();
        if self
            .send(ActorCommand::EstimateTokens {
                canonical,
                generation,
                revision,
                reply,
            })
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    pub(crate) async fn append_token_estimate(
        &self,
        message: CanonicalMessage,
        canonical_len: usize,
        generation: u64,
        revision: u64,
    ) {
        let _ = self
            .send(ActorCommand::AppendTokenEstimate {
                message,
                canonical_len,
                generation,
                revision,
            })
            .await;
    }

    pub(crate) fn reset_token_estimate_now(&self) {
        let _ = self.tx.try_send(ActorCommand::ResetTokenEstimate);
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
    ) -> anyhow::Result<Option<ConfirmDecision>> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::Session(SessionCommand::ResolveInteraction {
            request_id,
            response,
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
    /// Run-local identity and budget belong to the session actor, not to the
    /// process-wide ReAct facade.  The rest of the runtime state is migrated
    /// through the same command boundary below.
    runtime: SessionRuntimeState,
}

#[derive(Default)]
struct SessionRuntimeState {
    run_budget: Option<RunBudget>,
    stream_identity: IdentityMap,
    usage: UsageTracker,
    token_estimates: TokenEstimateCache,
    messaging: SessionMessagingState,
}

/// Replay only the interaction domain events needed to initialize a fresh
/// actor.  The transcript, messages and steps are intentionally absent from
/// this reducer: they are projections for UI/history and never recovery input.
pub(crate) fn load_interactions(
    store: &SessionStore,
    session_id: &str,
) -> anyhow::Result<Vec<InteractionRequest>> {
    let mut interactions: Vec<InteractionRequest> = Vec::new();
    for event in store.read_active_domain_events(session_id)? {
        match event.event_type.as_str() {
            INTERACTION_REQUESTED_EVENT_TYPE | INTERACTION_RESOLVED_EVENT_TYPE => {
                let request: InteractionRequest =
                    serde_json::from_str(&event.payload).map_err(|error| {
                        anyhow::anyhow!(
                            "invalid interaction event at sequence {}: {error}",
                            event.sequence
                        )
                    })?;
                interactions.retain(|existing| existing.id != request.id);
                if request.status == InteractionStatus::Pending {
                    interactions.push(request);
                }
            }
            INTERACTION_CLEARED_EVENT_TYPE => {
                let payload: serde_json::Value =
                    serde_json::from_str(&event.payload).map_err(|error| {
                        anyhow::anyhow!(
                            "invalid interaction clear event at sequence {}: {error}",
                            event.sequence
                        )
                    })?;
                let ids = payload
                    .get("ids")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "interaction clear event at sequence {} has no ids",
                            event.sequence
                        )
                    })?;
                interactions.retain(|request| {
                    !ids.iter()
                        .any(|id| id.as_str() == Some(request.id.as_str()))
                });
            }
            _ => {}
        }
    }
    Ok(interactions)
}

async fn append_interaction_event(
    db: &Arc<Database>,
    store: &SessionStore,
    session_id: &str,
    event_type: &str,
    payload: String,
) -> anyhow::Result<()> {
    let store = store.clone();
    let session_id = session_id.to_string();
    let event_type = event_type.to_string();
    db.run_blocking(move |_| {
        store.append(&session_id, &event_type, &payload, None, None)?;
        Ok(())
    })
    .await
}

fn restore_interaction(state: &mut SessionState, resolved: &InteractionRequest) {
    let mut pending = resolved.clone();
    pending.status = InteractionStatus::Pending;
    pending.response = None;
    state
        .interactions
        .retain(|request| request.id != pending.id);
    state.interactions.push(pending);
}

pub(crate) fn spawn(
    db: Arc<Database>,
    store: SessionStore,
    info: SessionInfo,
    interactions: Vec<InteractionRequest>,
) -> SessionActorHandle {
    let (tx, mut rx) = mpsc::channel(ACTOR_MAILBOX_CAPACITY);
    let (status, _) = watch::channel(info.status);
    let (run_state, _) = watch::channel(false);
    let cancel = CancellationToken::new();
    let handle = SessionActorHandle {
        id: info.id.clone(),
        tx,
        cancel: cancel.clone(),
        status: status.clone(),
        run_state: run_state.clone(),
    };
    tokio::spawn(async move {
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
            runtime: SessionRuntimeState::default(),
        };
        while let Some(command) = rx.recv().await {
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
                        reply,
                    } => {
                        let result = resolve_interaction(&mut state, &request_id, response);
                        let result = match result {
                            Some(decision) => {
                                let payload = match serde_json::to_string(&decision.request) {
                                    Ok(payload) => payload,
                                    Err(error) => {
                                        restore_interaction(&mut state, &decision.request);
                                        let _ = reply.send(Err(error.into()));
                                        continue;
                                    }
                                };
                                let persisted = append_interaction_event(
                                    &db,
                                    &store,
                                    &state.info.id,
                                    INTERACTION_RESOLVED_EVENT_TYPE,
                                    payload,
                                )
                                .await;
                                match persisted {
                                    Ok(()) => Ok(Some(decision)),
                                    Err(error) => {
                                        restore_interaction(&mut state, &decision.request);
                                        Err(error)
                                    }
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
                                &db,
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
                            cancel.cancel();
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
                        transition(&db, &mut state, &status, next, persist).await
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
                    let result = claim_run(&db, &mut state, &status, &run_state).await;
                    let _ = reply.send(result);
                }
                ActorCommand::BeginDirectRun { reply } => {
                    let accepted = !state.running && !state.info.status.is_terminal();
                    if accepted {
                        state.running = true;
                        let _ = run_state.send(true);
                    }
                    let _ = reply.send(accepted);
                }
                ActorCommand::FinishRun { reply } => {
                    state.running = false;
                    let _ = run_state.send(false);
                    let _ = reply.send(RunFinished {
                        pending: state.info.status == SessionStatus::Pending,
                        terminal: state.info.status.is_terminal(),
                    });
                }
                ActorCommand::ReleaseRun => {
                    state.running = false;
                    let _ = run_state.send(false);
                }
                ActorCommand::IsRunning { reply } => {
                    let _ = reply.send(state.running);
                }
                ActorCommand::SetRunBudget { budget } => {
                    state.runtime.run_budget = Some(budget);
                }
                ActorCommand::ClearRunBudget => {
                    state.runtime.run_budget = None;
                }
                ActorCommand::EnsureStreamId {
                    step,
                    run,
                    kind,
                    reply,
                } => {
                    let _ = reply.send(state.runtime.stream_identity.ensure_msg_id(
                        &state.info.id,
                        step,
                        run,
                        kind,
                    ));
                }
                ActorCommand::BlockStreamId {
                    step,
                    run,
                    kind,
                    reply,
                } => {
                    let _ = reply.send(state.runtime.stream_identity.block_msg_id(
                        &state.info.id,
                        step,
                        run,
                        kind,
                    ));
                }
                ActorCommand::ClearStreamIds => {
                    state
                        .runtime
                        .stream_identity
                        .clear_for_session(&state.info.id);
                }
                ActorCommand::RecordUsage { update, reply } => {
                    let result = record_usage(&db, &store, &mut state, update).await;
                    let _ = reply.send(result);
                }
                ActorCommand::ResetUsage => {
                    state.runtime.usage.reset(&state.info.id);
                }
                ActorCommand::InvalidateUsage => {
                    state
                        .runtime
                        .usage
                        .invalidate_after_truncate(&state.info.id);
                }
                ActorCommand::EstimateTokens {
                    canonical,
                    generation,
                    revision,
                    reply,
                } => {
                    let tokens = state.runtime.token_estimates.estimate(
                        &state.info.id,
                        &canonical,
                        generation,
                        revision,
                    );
                    let _ = reply.send(tokens);
                }
                ActorCommand::AppendTokenEstimate {
                    message,
                    canonical_len,
                    generation,
                    revision,
                } => {
                    state.runtime.token_estimates.append_message(
                        &state.info.id,
                        &message,
                        canonical_len,
                        generation,
                        revision,
                    );
                }
                ActorCommand::ResetTokenEstimate => {
                    state.runtime.token_estimates.remove(&state.info.id);
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
                    let result = match serde_json::to_string(&request) {
                        Ok(payload) => {
                            append_interaction_event(
                                &db,
                                &store,
                                &state.info.id,
                                INTERACTION_REQUESTED_EVENT_TYPE,
                                payload,
                            )
                            .await
                        }
                        Err(error) => Err(error.into()),
                    };
                    if result.is_ok() {
                        state
                            .interactions
                            .retain(|existing| existing.id != request.id);
                        state.interactions.push(request);
                    }
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
                                    &db,
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
                            if haven_tools::is_expired(&envelope) {
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
        cancel.cancel();
        let _ = run_state.send(false);
    });
    handle
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
        if haven_tools::is_expired(&envelope) {
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

async fn record_usage(
    db: &Arc<Database>,
    store: &SessionStore,
    state: &mut SessionState,
    update: UsageUpdate,
) -> anyhow::Result<CumulativeTotals> {
    let session_id = state.info.id.clone();
    let UsageUpdate {
        request,
        model,
        step_number,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        cache_creation_tokens,
        cache_miss_tokens,
        cache_accounting,
        cache_diagnostics,
        cost_usd,
        has_cost,
        duration_ms,
        context_tokens,
        context_window,
        cancel,
    } = update;
    let seed = if state.runtime.usage.needs_seed(&session_id) {
        let db = db.clone();
        let sid = session_id.clone();
        let read = move |db: &Database| -> anyhow::Result<CumulativeUsage> {
            Ok(db
                .get_session_usage(&sid)?
                .map(CumulativeUsage::from)
                .unwrap_or_default())
        };
        match cancel.clone() {
            Some(cancel) => db.run_blocking_cancellable(cancel, read).await?,
            None => db.run_blocking(read).await?,
        }
    } else {
        CumulativeUsage::default()
    };

    let totals = state.runtime.usage.record_with_seed(
        &session_id,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        cache_creation_tokens,
        cache_miss_tokens,
        if has_cost { Some(cost_usd) } else { None },
        || seed,
    );

    let persist_epoch = state.runtime.usage.epoch(&session_id);
    let epochs = state.runtime.usage.epochs_handle();
    let persist_session_id = session_id.clone();
    let usage_input = haven_memory::LlmCallUsageInput {
        step_number: Some(step_number),
        request_kind: request,
        call_kind: "agent".into(),
        model,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        cache_creation_tokens,
        cache_miss_tokens,
        cache_accounting,
        cache_diagnostics,
        cost_usd,
        has_cost,
        duration_ms,
        context_tokens,
        context_window,
    };
    let store = store.clone();
    let persist = move |_db: &Database| -> anyhow::Result<()> {
        let epoch_now = || {
            epochs
                .lock()
                .unwrap()
                .get(&persist_session_id)
                .copied()
                .unwrap_or(0)
        };
        if epoch_now() != persist_epoch {
            return Ok(());
        }
        let record = store.append_usage(&persist_session_id, &usage_input)?;
        if epoch_now() != persist_epoch {
            store.discard_usage(&persist_session_id, &record.id)?;
        }
        Ok(())
    };
    match cancel {
        Some(cancel) => db.run_blocking_cancellable(cancel, persist).await?,
        None => db.run_blocking(persist).await?,
    }
    Ok(totals)
}

async fn transition(
    db: &Arc<Database>,
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
        super::SessionSupervisor::persist_status(db, &state.info.id, next).await?;
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
    db: &Arc<Database>,
    state: &mut SessionState,
    status: &watch::Sender<SessionStatus>,
    run_state: &watch::Sender<bool>,
) -> anyhow::Result<RunClaim> {
    if state.info.status != SessionStatus::Pending || state.running {
        return Ok(RunClaim { accepted: false });
    }
    super::SessionSupervisor::persist_status(db, &state.info.id, SessionStatus::Running).await?;
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
    state: &mut SessionState,
    request_id: &str,
    response: Value,
) -> Option<ConfirmDecision> {
    let index = state
        .interactions
        .iter()
        .position(|request| request.id == request_id)?;
    let resolved = {
        let request = &mut state.interactions[index];
        if request.status != InteractionStatus::Pending
            || request.kind != InteractionKind::Confirm
            || !response.is_boolean()
            || !request.resolve(response)
        {
            return None;
        }
        request.clone()
    };
    let wake_session = state
        .interactions
        .iter()
        .filter(|entry| entry.kind == InteractionKind::Confirm)
        .all(|entry| entry.status != InteractionStatus::Pending);
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
    let messaging = &mut state.runtime.messaging;
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
    state.runtime.messaging.title = Some(title);
}

fn clear_messaging(state: &mut SessionState) {
    state.runtime.messaging = SessionMessagingState::default();
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
            runtime: SessionRuntimeState::default(),
        }
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
