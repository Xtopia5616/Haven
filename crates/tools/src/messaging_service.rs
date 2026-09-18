//! Domain service for cross-session messaging.
//!
//! [`InboxBus`] is deliberately kept behind this boundary. It remains the
//! file transport adapter for cross-process interoperability, while callers
//! use one lifecycle: send, claim, process, and complete (or drop to retry).
//! This keeps transport recovery details out of tools and the ReAct loop.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::inbox::{AgentInfo, Envelope, InboxBus, MessageType, SendOutcome, validate_agent_name};

/// Transport port used by [`MessagingService`].
///
/// `InboxBus` implements this port for the cross-process JSONL adapter.
/// In-process `SessionActor` delivery is exposed through [`SessionMailbox`];
/// the claim/complete contract remains above both adapters.
pub trait MessageTransport: std::fmt::Debug + Send + Sync {
    fn subscribe(&self) -> watch::Receiver<u64>;
    fn register(&self, name: &str, capabilities: &[String]) -> anyhow::Result<()>;
    fn register_with_title(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()>;
    fn register_with_profile(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
        role: Option<&str>,
        parent: Option<&str>,
    ) -> anyhow::Result<()>;
    fn unregister(&self, name: &str) -> anyhow::Result<()>;
    fn mark_offline(&self, name: &str) -> anyhow::Result<()>;
    fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>>;
    fn list_children(&self, parent: &str) -> anyhow::Result<Vec<AgentInfo>>;
    fn list_descendants(&self, parent: &str) -> anyhow::Result<Vec<String>>;
    fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<SendOutcome>;
    fn claim(&self, recipient: &str) -> anyhow::Result<Vec<Envelope>>;
    /// Best-effort claim for background polling. `None` means the transport
    /// is busy and the caller should retry later; the normal `claim` path may
    /// wait for the transport lock.
    fn try_claim(&self, recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>> {
        self.claim(recipient).map(Some)
    }
    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<()>;
    fn last_received(&self, name: &str) -> anyhow::Result<Option<Envelope>>;
    fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Envelope>>;
    fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Vec<Envelope>>;
    fn history(&self, name: &str, limit: usize) -> anyhow::Result<Vec<Envelope>>;
}

impl MessageTransport for InboxBus {
    fn subscribe(&self) -> watch::Receiver<u64> {
        InboxBus::subscribe(self)
    }

    fn register(&self, name: &str, capabilities: &[String]) -> anyhow::Result<()> {
        InboxBus::register(self, name, capabilities)
    }

    fn register_with_title(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        InboxBus::register_with_title(self, name, capabilities, title)
    }

    fn register_with_profile(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
        role: Option<&str>,
        parent: Option<&str>,
    ) -> anyhow::Result<()> {
        InboxBus::register_with_profile(self, name, capabilities, title, role, parent)
    }

    fn unregister(&self, name: &str) -> anyhow::Result<()> {
        InboxBus::unregister(self, name)
    }

    fn mark_offline(&self, name: &str) -> anyhow::Result<()> {
        InboxBus::mark_offline(self, name)
    }

    fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        InboxBus::list_agents(self)
    }

    fn list_children(&self, parent: &str) -> anyhow::Result<Vec<AgentInfo>> {
        InboxBus::list_children(self, parent)
    }

    fn list_descendants(&self, parent: &str) -> anyhow::Result<Vec<String>> {
        InboxBus::list_descendants(self, parent)
    }

    fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<SendOutcome> {
        InboxBus::deliver(self, to, envelope)
    }

    fn claim(&self, recipient: &str) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::claim_and_archive(self, recipient)
    }

    fn try_claim(&self, recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>> {
        InboxBus::try_claim_and_archive(self, recipient)
    }

    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<()> {
        InboxBus::ack_claimed(self, recipient, ids)
    }

    fn last_received(&self, name: &str) -> anyhow::Result<Option<Envelope>> {
        InboxBus::last_received(self, name)
    }

    fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Envelope>> {
        InboxBus::find_message(self, name, id)
    }

    fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::take_matching_replies(self, name, in_reply_to, expected_from)
    }

    fn history(&self, name: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::history(self, name, limit)
    }
}

/// In-process mailbox for sessions owned by the current Haven process.
///
/// The trait deliberately uses synchronous methods because the file transport
/// is synchronous and the application-facing tool boundary already runs those
/// operations on the blocking pool. Implementations backed by an async actor
/// may use bounded mailbox sends plus `blocking_recv`; they must not expose the
/// actor's mutable state to callers.
pub trait SessionMailbox: Send + Sync {
    /// A mailbox-local wake signal. It is a hint only; callers still claim.
    fn subscribe(&self) -> watch::Receiver<u64>;
    /// Return `Some` when `to` belongs to this process, otherwise `None` so the
    /// service can fall back to the cross-process transport.
    fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<Option<SendOutcome>>;
    fn claim(&self, recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>>;
    /// Session mailboxes are in-process and non-blocking by contract. The
    /// default keeps custom mailbox implementations source-compatible.
    fn try_claim(&self, recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>> {
        self.claim(recipient)
    }
    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<Option<()>>;
    fn last_received(&self, name: &str) -> anyhow::Result<Option<Option<Envelope>>>;
    fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Option<Envelope>>>;
    fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Option<Vec<Envelope>>>;
    fn history(&self, name: &str, limit: usize) -> anyhow::Result<Option<Vec<Envelope>>>;
}

/// Runtime port for peer-session lifecycle operations.
///
/// This replaces the old mutable `Arc<dyn Fn>` spawn slot. The service owns a
/// single typed port and obtains the mailbox from the same runtime, so peer
/// creation, lifecycle control, and in-process delivery cannot silently point
/// at different session registries.
#[async_trait::async_trait]
pub trait MessagingRuntime: Send + Sync {
    fn mailbox(&self) -> Arc<dyn SessionMailbox>;
    async fn spawn_peer_session(
        &self,
        request: AgentSpawnRequest,
    ) -> anyhow::Result<AgentSpawnResult>;
    async fn control_peer_session(
        &self,
        request: AgentControlRequest,
    ) -> anyhow::Result<AgentControlResult>;
}

/// Request to create a peer session.
#[derive(Debug, Clone)]
pub struct AgentSpawnRequest {
    pub parent_session_id: String,
    pub task: String,
    pub title: Option<String>,
    pub role: Option<String>,
    pub capabilities: Vec<String>,
}

/// Result of creating a peer session.
#[derive(Debug, Clone)]
pub struct AgentSpawnResult {
    pub session_id: String,
    pub title: Option<String>,
    pub role: Option<String>,
    pub queued: bool,
    pub running_sessions: usize,
    pub max_concurrent: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentControlOperation {
    Status,
    Wait,
    Stop,
}

#[derive(Debug, Clone)]
pub struct AgentControlRequest {
    pub requester_session_id: String,
    pub target_session_id: String,
    pub operation: AgentControlOperation,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentControlResult {
    pub session_id: String,
    pub status: String,
    pub terminal: bool,
    pub timed_out: bool,
    pub title: Option<String>,
}

/// Cross-session messaging service.
///
/// The service is the application-facing port. The current implementation is
/// Backed by the JSONL adapter for independent processes and, when installed,
/// an in-process session mailbox for local delivery. The service is the only
/// place that decides which transport owns a message lifecycle.
#[derive(Clone)]
pub struct MessagingService {
    transport: Arc<dyn MessageTransport>,
    mailbox: Arc<OnceLock<Arc<dyn SessionMailbox>>>,
    runtime: Arc<OnceLock<Arc<dyn MessagingRuntime>>>,
}

impl std::fmt::Debug for MessagingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagingService")
            .field("transport", &self.transport)
            .field("has_mailbox", &self.mailbox.get().is_some())
            .field("has_runtime", &self.runtime.get().is_some())
            .finish()
    }
}

impl MessagingService {
    /// Build a service over a transport adapter.
    pub fn new(transport: Arc<dyn MessageTransport>) -> Self {
        Self {
            transport,
            mailbox: Arc::new(OnceLock::new()),
            runtime: Arc::new(OnceLock::new()),
        }
    }

    /// Build the desktop service over Haven's shared cross-process inbox.
    pub fn default_root() -> Self {
        Self::new(Arc::new(InboxBus::default_root()))
    }

    /// Build a service that routes sessions owned by `mailbox` in-process and
    /// uses the shared JSONL adapter for all other recipients.
    pub fn with_session_mailbox(mailbox: Arc<dyn SessionMailbox>) -> Self {
        let service = Self::default_root();
        assert!(
            service.mailbox.set(mailbox).is_ok(),
            "new messaging mailbox binding"
        );
        service
    }

    /// Bind the runtime once at the composition boundary. Existing service
    /// clones observe the same immutable capability port.
    pub fn bind_runtime(&self, runtime: Arc<dyn MessagingRuntime>) -> anyhow::Result<()> {
        let mailbox = runtime.mailbox();
        self.runtime
            .set(runtime)
            .map_err(|_| anyhow::anyhow!("messaging runtime is already bound"))?;
        self.mailbox
            .set(mailbox)
            .map_err(|_| anyhow::anyhow!("messaging mailbox is already bound"))
    }

    fn mailbox(&self) -> Option<Arc<dyn SessionMailbox>> {
        self.mailbox.get().cloned()
    }

    fn runtime(&self) -> Option<Arc<dyn MessagingRuntime>> {
        self.runtime.get().cloned()
    }

    /// Subscribe to delivery notifications. The signal is only a wake-up
    /// hint; callers must still claim from the service before processing.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.mailbox()
            .map(|mailbox| mailbox.subscribe())
            .unwrap_or_else(|| self.transport.subscribe())
    }

    pub fn register(&self, name: &str, capabilities: &[String]) -> anyhow::Result<()> {
        self.transport.register(name, capabilities)
    }

    pub fn register_with_title(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        self.transport
            .register_with_title(name, capabilities, title)
    }

    pub fn register_with_profile(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
        role: Option<&str>,
        parent: Option<&str>,
    ) -> anyhow::Result<()> {
        self.transport
            .register_with_profile(name, capabilities, title, role, parent)
    }

    pub fn unregister(&self, name: &str) -> anyhow::Result<()> {
        self.transport.unregister(name)
    }

    /// Retain a peer entry and make its liveness explicitly stale. Keeping
    /// the parent link after a child finishes lets a parent perform a final
    /// lifecycle lookup without racing asynchronous registry cleanup.
    pub fn mark_offline(&self, name: &str) -> anyhow::Result<()> {
        self.transport.mark_offline(name)
    }

    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        self.transport.list_agents()
    }

    pub fn list_children(&self, parent: &str) -> anyhow::Result<Vec<AgentInfo>> {
        self.transport.list_children(parent)
    }

    pub fn list_descendants(&self, parent: &str) -> anyhow::Result<Vec<String>> {
        self.transport.list_descendants(parent)
    }

    /// Deliver one already-constructed envelope through the selected
    /// transport. Identity and routing are checked here, before the adapter
    /// writes anything, so all producers share the same message contract.
    pub fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<SendOutcome> {
        validate_message_envelope(to, envelope)?;
        if let Some(mailbox) = self.mailbox()
            && let Some(outcome) = mailbox.deliver(to, envelope)?
        {
            return Ok(outcome);
        }
        self.transport.deliver(to, envelope)
    }

    /// Send one envelope and return both its stable identity and delivery
    /// outcome. All message kinds, including requests, replies, and receipts,
    /// use this one entry point after construction.
    pub fn send(&self, envelope: Envelope) -> anyhow::Result<SentMessage> {
        let to = envelope.to.clone();
        let outcome = self.deliver(&to, &envelope)?;
        Ok(SentMessage { envelope, outcome })
    }

    pub fn deliver_system_notice(&self, from: &str, to: &str, text: &str) -> anyhow::Result<()> {
        // Validate the externally supplied addresses before constructing the
        // system envelope.
        validate_agent_name(from)?;
        validate_agent_name(to)?;
        let mut envelope = Envelope::new(from, to, text);
        envelope.r#type = MessageType::System;
        match self.send(envelope) {
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::debug!(to, %error, "system notice skipped");
                Ok(())
            }
        }
    }

    /// Claim messages for durable processing. The returned claim owns the
    /// delivery lease: dropping it leaves the processing file intact and the
    /// next claim redelivers the same stable envelope ids.
    pub fn claim(&self, recipient: &str) -> anyhow::Result<MessageClaim> {
        validate_agent_name(recipient)?;
        let envelopes = if let Some(mailbox) = self.mailbox() {
            match mailbox.claim(recipient)? {
                Some(envelopes) => envelopes,
                None => self.transport.claim(recipient)?,
            }
        } else {
            self.transport.claim(recipient)?
        };
        Ok(MessageClaim {
            service: self.clone(),
            recipient: recipient.to_string(),
            envelopes,
        })
    }

    /// Best-effort claim used by automatic heartbeat polling. Unlike
    /// [`Self::claim`], it never waits on the file transport's global lock.
    /// A `None` result is an ordinary busy/no-op outcome, not a delivery
    /// failure.
    pub fn try_claim(&self, recipient: &str) -> anyhow::Result<Option<MessageClaim>> {
        validate_agent_name(recipient)?;
        let envelopes = if let Some(mailbox) = self.mailbox() {
            match mailbox.try_claim(recipient)? {
                Some(envelopes) => Some(envelopes),
                None => self.transport.try_claim(recipient)?,
            }
        } else {
            self.transport.try_claim(recipient)?
        };
        Ok(envelopes.map(|envelopes| MessageClaim {
            service: self.clone(),
            recipient: recipient.to_string(),
            envelopes,
        }))
    }

    pub fn last_received(&self, name: &str) -> anyhow::Result<Option<Envelope>> {
        if let Some(mailbox) = self.mailbox()
            && let Some(result) = mailbox.last_received(name)?
        {
            return Ok(result);
        }
        self.transport.last_received(name)
    }

    pub fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Envelope>> {
        if let Some(mailbox) = self.mailbox()
            && let Some(result) = mailbox.find_message(name, id)?
        {
            return Ok(result);
        }
        self.transport.find_message(name, id)
    }

    /// Selectively consume a reply for an outstanding request. This is a
    /// request/reply operation, not a second general inbox-consumption model:
    /// all bulk inbox delivery goes through [`Self::claim`].
    pub fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Vec<Envelope>> {
        let replies = if let Some(mailbox) = self.mailbox()
            && let Some(result) = mailbox.take_matching_replies(name, in_reply_to, expected_from)?
        {
            result
        } else {
            self.transport
                .take_matching_replies(name, in_reply_to, expected_from)?
        };
        if !replies.is_empty() {
            let _ = self.send_receipts(name, &replies);
        }
        Ok(replies)
    }

    pub fn history(&self, name: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        if let Some(mailbox) = self.mailbox()
            && let Some(result) = mailbox.history(name, limit)?
        {
            return Ok(result);
        }
        self.transport.history(name, limit)
    }

    /// Construct and deliver a request through the same validated delivery
    /// path used by ordinary messages.
    pub fn request(
        &self,
        from: &str,
        to: &str,
        text: &str,
        subject: Option<String>,
        payload: Option<serde_json::Value>,
        expires_at: Option<String>,
    ) -> anyhow::Result<SentMessage> {
        let mut envelope = Envelope::new(from, to, text);
        envelope.r#type = MessageType::Request;
        envelope.subject = subject;
        envelope.payload = payload;
        envelope.expires_at = expires_at;
        self.send(envelope)
    }

    /// Construct and deliver a correlated reply. A reply without a stable
    /// request identity is rejected before it can enter either transport.
    #[allow(clippy::too_many_arguments)]
    pub fn reply(
        &self,
        from: &str,
        to: &str,
        in_reply_to: &str,
        text: &str,
        subject: Option<String>,
        payload: Option<serde_json::Value>,
        expires_at: Option<String>,
    ) -> anyhow::Result<SentMessage> {
        if !is_canonical_message_id(in_reply_to) {
            anyhow::bail!("in_reply_to must be a canonical message id");
        }
        let mut envelope = Envelope::new(from, to, text);
        envelope.r#type = MessageType::Reply;
        envelope.reply_address = Some(from.to_string());
        envelope.in_reply_to = Some(in_reply_to.to_string());
        envelope.subject = subject;
        envelope.payload = payload;
        envelope.expires_at = expires_at;
        self.send(envelope)
    }

    /// Construct and deliver a read receipt through the same message path.
    pub fn receipt(&self, from: &str, to: &str, in_reply_to: &str) -> anyhow::Result<SentMessage> {
        if !is_canonical_message_id(in_reply_to) {
            anyhow::bail!("receipt in_reply_to must be a canonical message id");
        }
        let mut envelope = Envelope::new(from, to, "已读");
        envelope.r#type = MessageType::Receipt;
        envelope.in_reply_to = Some(in_reply_to.to_string());
        envelope.reply_address = Some(from.to_string());
        self.send(envelope)
    }

    /// Wait for the first authenticated reply. `None` means the bounded wait
    /// expired; cancellation remains an explicit error so the tool layer can
    /// preserve the caller's cancellation semantics.
    pub async fn wait_for_reply(
        &self,
        recipient: &str,
        request_id: &str,
        expected_from: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Option<Envelope>> {
        validate_agent_name(recipient)?;
        validate_agent_name(expected_from)?;
        if !is_canonical_message_id(request_id) {
            anyhow::bail!("request id must be a canonical message id");
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut wake = self.subscribe();
        loop {
            let service = self.clone();
            let recipient = recipient.to_string();
            let request_id = request_id.to_string();
            let expected_from = expected_from.to_string();
            let found = tokio::task::spawn_blocking(move || {
                service.take_matching_replies(&recipient, &request_id, &expected_from)
            })
            .await??
            .into_iter()
            .next();
            if found.is_some() {
                return Ok(found);
            }
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                _ = tokio::time::sleep((deadline - now).min(Duration::from_secs(1))) => {},
                changed = wake.changed() => {
                    if changed.is_err() { return Ok(None); }
                    let _ = wake.borrow_and_update();
                }
            }
        }
    }

    pub async fn spawn_peer_session(
        &self,
        request: AgentSpawnRequest,
    ) -> anyhow::Result<AgentSpawnResult> {
        self.runtime()
            .ok_or_else(|| anyhow::anyhow!("agent spawn requires the session runtime"))?
            .spawn_peer_session(request)
            .await
    }

    pub async fn control_peer_session(
        &self,
        request: AgentControlRequest,
    ) -> anyhow::Result<AgentControlResult> {
        self.runtime()
            .ok_or_else(|| {
                anyhow::anyhow!("agent lifecycle operation requires the session runtime")
            })?
            .control_peer_session(request)
            .await
    }

    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<()> {
        if ids.iter().any(|id| !is_canonical_message_id(id)) {
            anyhow::bail!("ack message ids must be canonical message ids");
        }
        if let Some(mailbox) = self.mailbox()
            && mailbox.ack(recipient, ids)?.is_some()
        {
            return Ok(());
        }
        self.transport.ack(recipient, ids)
    }

    fn send_receipts(&self, recipient: &str, read: &[Envelope]) -> Vec<SendOutcome> {
        read.iter()
            .filter(|envelope| envelope.r#type != MessageType::Receipt && envelope.from != recipient)
            .filter_map(|envelope| {
                let to = envelope.reply_target().to_string();
                match self.receipt(recipient, &to, &envelope.id) {
                    Ok(sent) => Some(sent.outcome),
                    Err(error) => {
                        tracing::debug!(from = %recipient, to = %to, %error, "read receipt delivery skipped");
                        None
                    }
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct SentMessage {
    pub envelope: Envelope,
    pub outcome: SendOutcome,
}

/// A durable delivery lease returned by [`MessagingService::claim`].
///
/// `complete` acknowledges only the claimed ids and then emits read receipts.
/// If processing fails or the value is dropped, the processing file remains
/// durable and the messages are eligible for redelivery. This is the explicit
/// retry contract for the current file adapter.
#[derive(Debug)]
pub struct MessageClaim {
    service: MessagingService,
    recipient: String,
    envelopes: Vec<Envelope>,
}

impl MessageClaim {
    pub fn envelopes(&self) -> &[Envelope] {
        &self.envelopes
    }

    pub fn is_empty(&self) -> bool {
        self.envelopes.is_empty()
    }

    pub fn into_envelopes(self) -> Vec<Envelope> {
        self.envelopes
    }

    /// Acknowledge the claim and send read receipts. The receipt operation is
    /// best-effort by design; failure to notify a sender must not redeliver a
    /// message whose processing was already acknowledged.
    pub fn complete(self) -> anyhow::Result<Vec<SendOutcome>> {
        let MessageClaim {
            service,
            recipient,
            envelopes,
        } = self;
        let ids: Vec<String> = envelopes
            .iter()
            .map(|envelope| envelope.id.clone())
            .collect();
        service.ack(&recipient, &ids)?;
        Ok(service.send_receipts(&recipient, &envelopes))
    }

    /// Acknowledge only the selected ids from this claim. Unselected messages
    /// remain in the durable processing file and are eligible for redelivery.
    /// This is the primitive used by the explicit `agent.ack` operation so a
    /// newly-arrived message cannot be acknowledged accidentally with an old
    /// batch.
    pub fn complete_selected(self, selected_ids: &[String]) -> anyhow::Result<Vec<SendOutcome>> {
        let MessageClaim {
            service,
            recipient,
            envelopes,
        } = self;
        let selected: Vec<Envelope> = envelopes
            .into_iter()
            .filter(|envelope| selected_ids.iter().any(|id| id == &envelope.id))
            .collect();
        let ids: Vec<String> = selected
            .iter()
            .map(|envelope| envelope.id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        service.ack(&recipient, &ids)?;
        Ok(service.send_receipts(&recipient, &selected))
    }

    /// Explicitly abandon the claim. This is equivalent to dropping it and
    /// documents the caller's intent to retry later.
    pub fn retry(self) {
        drop(self);
    }
}

fn validate_message_envelope(to: &str, envelope: &Envelope) -> anyhow::Result<()> {
    validate_agent_name(to)?;
    validate_agent_name(&envelope.from)?;
    if envelope.to != to {
        anyhow::bail!(
            "message recipient mismatch: envelope targets '{}' but delivery targets '{}'",
            envelope.to,
            to
        );
    }
    if !is_canonical_message_id(&envelope.id) {
        anyhow::bail!(
            "invalid message id '{}': expected msg-{{uuid32}}",
            envelope.id
        );
    }
    if envelope.text.trim().is_empty() {
        anyhow::bail!("message text must not be empty");
    }
    if let Some(reply_address) = &envelope.reply_address {
        validate_agent_name(reply_address)?;
    }
    if let Some(in_reply_to) = &envelope.in_reply_to
        && !is_canonical_message_id(in_reply_to)
    {
        anyhow::bail!("in_reply_to must be a canonical message id");
    }
    chrono::DateTime::parse_from_rfc3339(&envelope.created_at)
        .map_err(|error| anyhow::anyhow!("invalid message created_at: {error}"))?;
    if let Some(expires_at) = &envelope.expires_at {
        chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|error| anyhow::anyhow!("invalid message expires_at: {error}"))?;
    }
    if envelope.r#type == MessageType::Receipt {
        let Some(in_reply_to) = envelope.in_reply_to.as_deref() else {
            anyhow::bail!("receipt messages require in_reply_to");
        };
        if !is_canonical_message_id(in_reply_to) {
            anyhow::bail!("receipt in_reply_to must be a canonical message id");
        }
    }
    if envelope.r#type == MessageType::Reply && envelope.in_reply_to.is_none() {
        anyhow::bail!("reply messages require in_reply_to");
    }
    Ok(())
}

/// Shared expiry policy for every message adapter and mailbox.
pub fn is_expired(envelope: &Envelope) -> bool {
    envelope.expires_at.as_deref().is_some_and(|expires_at| {
        chrono::DateTime::parse_from_rfc3339(expires_at)
            .map(|parsed| parsed <= chrono::Utc::now())
            .unwrap_or(false)
    })
}

fn is_canonical_message_id(id: &str) -> bool {
    let Some(suffix) = id.strip_prefix("msg-") else {
        return false;
    };
    suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (tempfile::TempDir, MessagingService) {
        let dir = tempfile::tempdir().unwrap();
        let bus = Arc::new(InboxBus::new(dir.path()));
        (dir, MessagingService::new(bus))
    }

    #[test]
    fn claim_is_redeliverable_until_complete() {
        let (_dir, service) = service();
        service.register("ses-a", &[]).unwrap();
        service.register("ses-b", &[]).unwrap();
        let envelope = Envelope::new("ses-a", "ses-b", "durable");
        service.deliver("ses-b", &envelope).unwrap();

        let claim = service.claim("ses-b").unwrap();
        assert_eq!(claim.envelopes()[0].id, envelope.id);
        assert_eq!(claim.envelopes()[0].delivery_attempt, 1);
        claim.retry();
        let retry = service.claim("ses-b").unwrap();
        assert_eq!(retry.envelopes()[0].id, envelope.id);
        assert_eq!(retry.envelopes()[0].delivery_attempt, 2);
        retry.retry();

        let claim = service.claim("ses-b").unwrap();
        claim.complete().unwrap();
        assert!(service.claim("ses-b").unwrap().is_empty());
    }

    #[test]
    fn rejects_unstable_identity_and_routing_mismatch() {
        let (_dir, service) = service();
        service.register("ses-a", &[]).unwrap();
        service.register("ses-b", &[]).unwrap();

        let mut envelope = Envelope::new("ses-a", "ses-b", "hello");
        envelope.id = "legacy-id".into();
        let error = service.deliver("ses-b", &envelope).unwrap_err();
        assert!(error.to_string().contains("invalid message id"));

        let envelope = Envelope::new("ses-a", "ses-b", "hello");
        let error = service.deliver("ses-a", &envelope).unwrap_err();
        assert!(error.to_string().contains("recipient mismatch"));

        let mut envelope = Envelope::new("ses-a", "ses-b", "hello");
        envelope.in_reply_to = Some("legacy-id".into());
        let error = service.deliver("ses-b", &envelope).unwrap_err();
        assert!(error.to_string().contains("in_reply_to"));
    }

    #[test]
    fn expired_messages_are_not_claimed_but_remain_in_history() {
        let (_dir, service) = service();
        service.register("ses-a", &[]).unwrap();
        service.register("ses-b", &[]).unwrap();
        let mut envelope = Envelope::new("ses-a", "ses-b", "expired");
        envelope.expires_at = Some("2000-01-01T00:00:00Z".into());
        service.deliver("ses-b", &envelope).unwrap();

        assert!(service.claim("ses-b").unwrap().is_empty());
        assert_eq!(service.history("ses-b", 10).unwrap()[0].id, envelope.id);
    }
}
