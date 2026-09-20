//! Single-owner runtime for one session.
//!
//! `SessionActor` is the only component that mutates session-local runtime
//! state.  Callers hold a cheap [`SessionActorHandle`] and send typed
//! commands; they never acquire a lock around `SessionInfo` or one of the
//! session's auxiliary queues.

use super::{FollowUp, SessionInfo, SessionStatus, StepInfo};
use crate::interaction::{InteractionKind, InteractionRequest, InteractionStatus};
use haven_common::types::MessageAttachment;
use haven_memory::Database;
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

#[derive(Debug)]
pub(crate) enum ActorCommand {
    Snapshot {
        reply: oneshot::Sender<SessionInfo>,
    },
    UpdateTitle {
        title: String,
    },
    Transition {
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
    QueueFollowUp {
        text: String,
        attachments: Vec<MessageAttachment>,
        is_answer: bool,
        message_id: Option<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    DrainFollowUps {
        reply: oneshot::Sender<Vec<FollowUp>>,
    },
    QueueSteering {
        text: String,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
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
    AddActionCompletion {
        action_result_id: String,
        text: String,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
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
    },
    ResolveInteraction {
        request_id: String,
        response: Value,
        reply: oneshot::Sender<Option<ConfirmDecision>>,
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
            status,
            persist,
            reply,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("session actor '{}' dropped transition", self.id))?
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

    pub(crate) async fn queue_follow_up(
        &self,
        text: &str,
        attachments: &[MessageAttachment],
        is_answer: bool,
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::QueueFollowUp {
            text: text.to_string(),
            attachments: attachments.to_vec(),
            is_answer,
            message_id,
            reply,
        })
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
        self.send(ActorCommand::QueueSteering {
            text: text.to_string(),
            attachments: attachments.to_vec(),
            message_id,
            reply,
        })
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
        self.send(ActorCommand::AddActionCompletion {
            action_result_id,
            text,
            reply,
        })
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

    pub(crate) async fn clear_interactions(&self, kind: Option<InteractionKind>) {
        let _ = self.send(ActorCommand::ClearInteractions { kind }).await;
    }

    pub(crate) async fn resolve_interaction(
        &self,
        request_id: String,
        response: Value,
    ) -> Option<ConfirmDecision> {
        let (reply, rx) = oneshot::channel();
        self.send(ActorCommand::ResolveInteraction {
            request_id,
            response,
            reply,
        })
        .await
        .ok()?;
        rx.await.ok().flatten()
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

    /// Synchronous mailbox operations are called from the service's blocking
    /// transport boundary. Tokio's blocking channel/receiver methods preserve
    /// actor serialization without exposing `ActorState`.
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

struct ActorState {
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
}

pub(crate) fn spawn(db: Arc<Database>, info: SessionInfo) -> SessionActorHandle {
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
        let mut state = ActorState {
            info,
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
        };
        while let Some(command) = rx.recv().await {
            match command {
                ActorCommand::Snapshot { reply } => {
                    let _ = reply.send(state.info.clone());
                }
                ActorCommand::UpdateTitle { title } => {
                    state.info.title = Some(title);
                }
                ActorCommand::Transition {
                    status: next,
                    persist,
                    reply,
                } => {
                    let result = transition(&db, &mut state, &status, next, persist).await;
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
                ActorCommand::QueueFollowUp {
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
                ActorCommand::DrainFollowUps { reply } => {
                    state.follow_up_chars = 0;
                    state.follow_up_attachment_bytes = 0;
                    let _ = reply.send(std::mem::take(&mut state.follow_up_queue));
                }
                ActorCommand::QueueSteering {
                    text,
                    attachments,
                    message_id,
                    reply,
                } => {
                    let result = queue_steering(&mut state, text, attachments, message_id);
                    let _ = reply.send(result);
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
                ActorCommand::AddActionCompletion {
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
                ActorCommand::DrainActionCompletions { reply } => {
                    state.action_completion_chars = 0;
                    let _ = reply.send(std::mem::take(&mut state.action_completions));
                }
                ActorCommand::RequestInteraction { request, reply } => {
                    let request = *request;
                    state
                        .interactions
                        .retain(|existing| existing.id != request.id);
                    state.interactions.push(request);
                    let _ = reply.send(Ok(()));
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
                ActorCommand::ClearInteractions { kind } => {
                    state
                        .interactions
                        .retain(|request| kind.is_some_and(|wanted| request.kind != wanted));
                }
                ActorCommand::ResolveInteraction {
                    request_id,
                    response,
                    reply,
                } => {
                    let result = resolve_interaction(&mut state, &request_id, response);
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

fn message_known(state: &ActorState, id: &str) -> bool {
    state.active_message_ids.contains(id) || state.archive_message_ids.contains(id)
}

fn archive_once(state: &mut ActorState, envelope: Envelope) {
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

fn claim_messages(state: &mut ActorState) -> Vec<Envelope> {
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

fn history(state: &ActorState, limit: usize) -> Vec<Envelope> {
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
    db: &Arc<Database>,
    state: &mut ActorState,
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
    state: &mut ActorState,
    status: &watch::Sender<SessionStatus>,
    run_state: &watch::Sender<bool>,
) -> anyhow::Result<RunClaim> {
    if state.info.status != SessionStatus::Pending || state.running {
        return Ok(RunClaim { accepted: false });
    }
    super::SessionSupervisor::persist_status(db, &state.info.id, SessionStatus::Running).await?;
    state.info.status = SessionStatus::Running;
    state.info.updated_at = chrono::Utc::now().to_rfc3339();
    state.running = true;
    let _ = status.send(SessionStatus::Running);
    let _ = run_state.send(true);
    Ok(RunClaim { accepted: true })
}

fn queue_follow_up(
    state: &mut ActorState,
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
    state: &mut ActorState,
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
    state: &mut ActorState,
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

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn empty_state() -> ActorState {
        ActorState {
            info: SessionInfo {
                id: "ses-queue".into(),
                input: "queue".into(),
                summary: "queue".into(),
                title: None,
                status: SessionStatus::Pending,
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
        }
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
