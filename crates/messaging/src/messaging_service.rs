//! Domain service for cross-session messaging.
//!
//! [`InboxBus`] is deliberately kept behind this boundary. Its JSONL files are
//! shared by local actors and independent processes, while callers use one
//! lifecycle: send, claim, process, and complete (or drop to retry). This
//! keeps transport recovery details out of tools and the ReAct loop.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::contract::{
    MAX_MESSAGE_HISTORY_LIMIT, validate_message_batch, validate_message_envelope,
    validate_message_envelope_for_delivery, validate_message_id, validate_session_id,
};
use crate::inbox::{AgentInfo, Envelope, InboxBus, MessageType, SendOutcome};

macro_rules! define_message_transport {
    ($visibility:vis) => {
        /// Transport port used internally by [`MessagingService`].
        ///
        /// `InboxBus` implements this port for the shared JSONL adapter used by
        /// both local actors and independent processes. This port is available
        /// to other crates only through the non-default test-support feature;
        /// production callers construct the service through its facade.
        $visibility trait MessageTransport: std::fmt::Debug + Send + Sync {
            fn subscribe(&self) -> watch::Receiver<u64>;
            fn register(&self, session_id: &str, capabilities: &[String]) -> anyhow::Result<()>;
            fn register_with_title(
                &self,
                session_id: &str,
                capabilities: &[String],
                title: Option<&str>,
            ) -> anyhow::Result<()>;
            fn register_with_profile(
                &self,
                session_id: &str,
                capabilities: &[String],
                title: Option<&str>,
                role: Option<&str>,
                parent_session_id: Option<&str>,
            ) -> anyhow::Result<()>;
            fn unregister(&self, session_id: &str) -> anyhow::Result<()>;
            fn mark_offline(&self, session_id: &str) -> anyhow::Result<()>;
            fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>>;
            fn list_children(&self, parent_session_id: &str) -> anyhow::Result<Vec<AgentInfo>>;
            fn count_registered_children(&self, parent_session_id: &str) -> anyhow::Result<usize>;
            fn list_descendants(&self, root_session_id: &str) -> anyhow::Result<Vec<String>>;
            fn delete_sessions(&self, session_ids: &[String]) -> anyhow::Result<()>;
            fn delete_all_sessions(&self, known_session_ids: &[String]) -> anyhow::Result<()>;
            fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<SendOutcome>;
            fn claim(&self, recipient: &str) -> anyhow::Result<Vec<Envelope>>;
            /// Best-effort claim for background polling. `None` means the transport
            /// is busy and the caller should retry later; the normal `claim` path may
            /// wait for the transport lock.
            fn try_claim(&self, recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>>;
            fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<()>;
            fn last_received(&self, session_id: &str) -> anyhow::Result<Option<Envelope>>;
            fn find_message(
                &self,
                session_id: &str,
                message_id: &str,
            ) -> anyhow::Result<Option<Envelope>>;
            fn take_matching_replies(
                &self,
                session_id: &str,
                in_reply_to_message_id: &str,
                expected_from_session_id: &str,
            ) -> anyhow::Result<Vec<Envelope>>;
            fn history(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<Envelope>>;
        }
    };
}

#[cfg(any(test, feature = "test-support"))]
define_message_transport!(pub);
#[cfg(not(any(test, feature = "test-support")))]
define_message_transport!(pub(crate));

impl MessageTransport for InboxBus {
    fn subscribe(&self) -> watch::Receiver<u64> {
        InboxBus::subscribe(self)
    }

    fn register(&self, session_id: &str, capabilities: &[String]) -> anyhow::Result<()> {
        InboxBus::register(self, session_id, capabilities)
    }

    fn register_with_title(
        &self,
        session_id: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        InboxBus::register_with_title(self, session_id, capabilities, title)
    }

    fn register_with_profile(
        &self,
        session_id: &str,
        capabilities: &[String],
        title: Option<&str>,
        role: Option<&str>,
        parent_session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        InboxBus::register_with_profile(
            self,
            session_id,
            capabilities,
            title,
            role,
            parent_session_id,
        )
    }

    fn unregister(&self, session_id: &str) -> anyhow::Result<()> {
        InboxBus::unregister(self, session_id)
    }

    fn mark_offline(&self, session_id: &str) -> anyhow::Result<()> {
        InboxBus::mark_offline(self, session_id)
    }

    fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        InboxBus::list_agents(self)
    }

    fn list_children(&self, parent_session_id: &str) -> anyhow::Result<Vec<AgentInfo>> {
        InboxBus::list_children(self, parent_session_id)
    }

    fn count_registered_children(&self, parent_session_id: &str) -> anyhow::Result<usize> {
        InboxBus::count_registered_children(self, parent_session_id)
    }

    fn list_descendants(&self, root_session_id: &str) -> anyhow::Result<Vec<String>> {
        InboxBus::list_descendants(self, root_session_id)
    }

    fn delete_sessions(&self, session_ids: &[String]) -> anyhow::Result<()> {
        InboxBus::delete_sessions(self, session_ids)
    }

    fn delete_all_sessions(&self, known_session_ids: &[String]) -> anyhow::Result<()> {
        InboxBus::delete_all_sessions(self, known_session_ids)
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

    fn last_received(&self, session_id: &str) -> anyhow::Result<Option<Envelope>> {
        InboxBus::last_received(self, session_id)
    }

    fn find_message(&self, session_id: &str, message_id: &str) -> anyhow::Result<Option<Envelope>> {
        InboxBus::find_message(self, session_id, message_id)
    }

    fn take_matching_replies(
        &self,
        session_id: &str,
        in_reply_to_message_id: &str,
        expected_from_session_id: &str,
    ) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::take_matching_replies(
            self,
            session_id,
            in_reply_to_message_id,
            expected_from_session_id,
        )
    }

    fn history(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::history(self, session_id, limit)
    }
}

/// Create the JSONL adapter over an isolated directory for cross-crate tests.
/// The concrete adapter stays private to `haven-messaging` in production.
#[cfg(feature = "test-support")]
pub fn file_transport_for_test(root: impl Into<std::path::PathBuf>) -> Arc<dyn MessageTransport> {
    Arc::new(InboxBus::new(root))
}

/// Runtime port for peer-session lifecycle operations.
///
/// This replaces the old mutable `Arc<dyn Fn>` spawn slot. Message delivery
/// and inbox state stay owned by the durable transport; this port only owns
/// peer creation and lifecycle control.
#[async_trait::async_trait]
pub trait MessagingRuntime: Send + Sync {
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
/// The service is the application-facing port. The transport is the sole
/// owner of message delivery, claims, acknowledgements, history and replies;
/// in-process actors only receive wake-up hints and poll this same source.
#[derive(Clone)]
pub struct MessagingService {
    transport: Arc<dyn MessageTransport>,
    runtime: Arc<OnceLock<Arc<dyn MessagingRuntime>>>,
}

impl std::fmt::Debug for MessagingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagingService")
            .field("transport", &self.transport)
            .field("has_runtime", &self.runtime.get().is_some())
            .finish()
    }
}

impl MessagingService {
    fn new(transport: Arc<dyn MessageTransport>) -> Self {
        Self {
            transport,
            runtime: Arc::new(OnceLock::new()),
        }
    }

    /// Build a service over an explicitly supplied transport for integration
    /// tests. Production code uses [`Self::default_root`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_test(transport: Arc<dyn MessageTransport>) -> Self {
        Self::new(transport)
    }

    /// Build the desktop service over Haven's shared cross-process inbox.
    pub fn default_root() -> Self {
        Self::new(Arc::new(InboxBus::default_root()))
    }

    /// Bind the runtime once at the composition boundary. Existing service
    /// clones observe the same immutable capability port.
    pub fn bind_runtime(&self, runtime: Arc<dyn MessagingRuntime>) -> anyhow::Result<()> {
        self.runtime
            .set(runtime)
            .map_err(|_| anyhow::anyhow!("messaging runtime is already bound"))?;
        Ok(())
    }

    fn runtime(&self) -> Option<Arc<dyn MessagingRuntime>> {
        self.runtime.get().cloned()
    }

    /// Subscribe to delivery notifications. The signal is only a wake-up
    /// hint; callers must still claim from the service before processing.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.transport.subscribe()
    }

    pub fn register(&self, session_id: &str, capabilities: &[String]) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        self.transport.register(session_id, capabilities)
    }

    pub fn register_with_title(
        &self,
        session_id: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        self.transport
            .register_with_title(session_id, capabilities, title)
    }

    pub fn register_with_profile(
        &self,
        session_id: &str,
        capabilities: &[String],
        title: Option<&str>,
        role: Option<&str>,
        parent_session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        if let Some(parent_session_id) = parent_session_id {
            validate_session_id(parent_session_id)?;
        }
        self.transport.register_with_profile(
            session_id,
            capabilities,
            title,
            role,
            parent_session_id,
        )
    }

    pub fn unregister(&self, session_id: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        self.transport.unregister(session_id)
    }

    /// Retain a peer entry and make its liveness explicitly stale. Keeping
    /// the parent session link after a child finishes lets a parent perform a final
    /// lifecycle lookup without racing asynchronous registry cleanup.
    pub fn mark_offline(&self, session_id: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        self.transport.mark_offline(session_id)
    }

    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        let agents = self.transport.list_agents()?;
        for agent in &agents {
            validate_session_id(&agent.session_id)?;
            if let Some(parent_session_id) = agent.parent_session_id.as_deref() {
                validate_session_id(parent_session_id)?;
            }
        }
        Ok(agents)
    }

    pub fn list_children(&self, parent_session_id: &str) -> anyhow::Result<Vec<AgentInfo>> {
        validate_session_id(parent_session_id)?;
        let children = self.transport.list_children(parent_session_id)?;
        for child in &children {
            validate_session_id(&child.session_id)?;
            if child.parent_session_id.as_deref() != Some(parent_session_id) {
                anyhow::bail!("child session result has a mismatched parent_session_id");
            }
        }
        Ok(children)
    }

    /// Count all child identities registered under a parent, including deleted
    /// lineage tombstones. This preserves the cumulative child-cap contract
    /// while public child listings omit deleted sessions.
    pub fn count_registered_children(&self, parent_session_id: &str) -> anyhow::Result<usize> {
        validate_session_id(parent_session_id)?;
        self.transport.count_registered_children(parent_session_id)
    }

    pub fn list_descendants(&self, root_session_id: &str) -> anyhow::Result<Vec<String>> {
        validate_session_id(root_session_id)?;
        let descendants = self.transport.list_descendants(root_session_id)?;
        for session_id in &descendants {
            validate_session_id(session_id)?;
        }
        Ok(descendants)
    }

    /// Permanently block Messaging access for deleted sessions while retaining
    /// only the parent links needed to preserve descendant lineage.
    pub fn delete_sessions(&self, session_ids: &[String]) -> anyhow::Result<()> {
        for session_id in session_ids {
            validate_session_id(session_id)?;
        }
        self.transport.delete_sessions(session_ids)
    }

    /// Tombstone database sessions and every additional identity still present
    /// in the shared inbox files, including legacy unregistered mailboxes.
    pub fn delete_all_sessions(&self, known_session_ids: &[String]) -> anyhow::Result<()> {
        for session_id in known_session_ids {
            validate_session_id(session_id)?;
        }
        self.transport.delete_all_sessions(known_session_ids)
    }

    /// Deliver one already-constructed envelope through the authoritative
    /// transport. Identity and routing are checked before the adapter writes,
    /// so local sessions and independent processes share one durable source.
    pub fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<SendOutcome> {
        validate_message_envelope_for_delivery(to, envelope)?;
        self.transport.deliver(to, envelope)
    }

    /// Send one envelope and return both its stable identity and delivery
    /// outcome. All message kinds, including requests, replies, and receipts,
    /// use this one entry point after construction.
    pub fn send(&self, envelope: Envelope) -> anyhow::Result<SentMessage> {
        let to = envelope.to_session_id.clone();
        let outcome = self.deliver(&to, &envelope)?;
        Ok(SentMessage { envelope, outcome })
    }

    pub fn deliver_system_notice(&self, from: &str, to: &str, text: &str) -> anyhow::Result<()> {
        // Validate the externally supplied addresses before constructing the
        // system envelope.
        validate_session_id(from)?;
        validate_session_id(to)?;
        let mut envelope = Envelope::new(from, to, text);
        envelope.r#type = MessageType::System;
        match self.send(envelope) {
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::debug!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "system notice skipped"
                );
                Ok(())
            }
        }
    }

    /// Claim messages from the authoritative transport. Dropping the returned
    /// claim leaves that transport's unacknowledged records available for
    /// redelivery with their stable envelope ids.
    pub fn claim(&self, recipient: &str) -> anyhow::Result<MessageClaim> {
        validate_session_id(recipient)?;
        let envelopes = self.transport.claim(recipient)?;
        validate_message_batch(recipient, &envelopes)?;
        Ok(MessageClaim {
            service: self.clone(),
            recipient: recipient.to_string(),
            envelopes,
        })
    }

    /// Best-effort claim used by automatic heartbeat polling. Unlike
    /// [`Self::claim`], it never waits on the transport's global lock.
    /// A `None` result is an ordinary busy/no-op outcome, not a delivery
    /// failure.
    pub fn try_claim(&self, recipient: &str) -> anyhow::Result<Option<MessageClaim>> {
        validate_session_id(recipient)?;
        let envelopes = self.transport.try_claim(recipient)?;
        if let Some(envelopes) = envelopes.as_deref() {
            validate_message_batch(recipient, envelopes)?;
        }
        Ok(envelopes.map(|envelopes| MessageClaim {
            service: self.clone(),
            recipient: recipient.to_string(),
            envelopes,
        }))
    }

    pub fn last_received(&self, session_id: &str) -> anyhow::Result<Option<Envelope>> {
        validate_session_id(session_id)?;
        let result = self.transport.last_received(session_id)?;
        if let Some(envelope) = result.as_ref() {
            validate_message_envelope(session_id, envelope)?;
        }
        Ok(result)
    }

    pub fn find_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<Envelope>> {
        validate_session_id(session_id)?;
        validate_message_id(message_id, "message_id")?;
        let result = self.transport.find_message(session_id, message_id)?;
        if let Some(envelope) = result.as_ref() {
            validate_message_envelope(session_id, envelope)?;
            if envelope.message_id != message_id {
                anyhow::bail!("message lookup returned a different message_id");
            }
        }
        Ok(result)
    }

    /// Selectively consume a reply for an outstanding request. This is a
    /// request/reply operation, not a second general inbox-consumption model:
    /// all bulk inbox delivery goes through [`Self::claim`].
    pub fn take_matching_replies(
        &self,
        session_id: &str,
        in_reply_to_message_id: &str,
        expected_from_session_id: &str,
    ) -> anyhow::Result<Vec<Envelope>> {
        validate_session_id(session_id)?;
        validate_message_id(in_reply_to_message_id, "in_reply_to_message_id")?;
        validate_session_id(expected_from_session_id)?;
        let replies = self.transport.take_matching_replies(
            session_id,
            in_reply_to_message_id,
            expected_from_session_id,
        )?;
        validate_message_batch(session_id, &replies)?;
        if !replies.is_empty() {
            let _ = self.send_receipts(session_id, &replies);
        }
        Ok(replies)
    }

    pub fn history(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        validate_session_id(session_id)?;
        let entries = self
            .transport
            .history(session_id, limit.min(MAX_MESSAGE_HISTORY_LIMIT))?;
        validate_message_batch(session_id, &entries)?;
        Ok(entries)
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
        in_reply_to_message_id: &str,
        text: &str,
        subject: Option<String>,
        payload: Option<serde_json::Value>,
        expires_at: Option<String>,
    ) -> anyhow::Result<SentMessage> {
        validate_message_id(in_reply_to_message_id, "in_reply_to_message_id")?;
        let mut envelope = Envelope::new(from, to, text);
        envelope.r#type = MessageType::Reply;
        envelope.reply_to_session_id = Some(from.to_string());
        envelope.in_reply_to_message_id = Some(in_reply_to_message_id.to_string());
        envelope.subject = subject;
        envelope.payload = payload;
        envelope.expires_at = expires_at;
        self.send(envelope)
    }

    /// Construct and deliver a read receipt through the same message path.
    pub fn receipt(
        &self,
        from: &str,
        to: &str,
        in_reply_to_message_id: &str,
    ) -> anyhow::Result<SentMessage> {
        validate_message_id(in_reply_to_message_id, "receipt in_reply_to_message_id")?;
        let mut envelope = Envelope::new(from, to, "已读");
        envelope.r#type = MessageType::Receipt;
        envelope.in_reply_to_message_id = Some(in_reply_to_message_id.to_string());
        envelope.reply_to_session_id = Some(from.to_string());
        self.send(envelope)
    }

    /// Wait for the first authenticated reply. `None` means the bounded wait
    /// expired; cancellation remains an explicit error so the tool layer can
    /// preserve the caller's cancellation semantics.
    pub async fn wait_for_reply(
        &self,
        recipient: &str,
        request_id: &str,
        expected_from_session_id: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Option<Envelope>> {
        validate_session_id(recipient)?;
        validate_session_id(expected_from_session_id)?;
        validate_message_id(request_id, "request_id")?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut wake = self.subscribe();
        loop {
            let service = self.clone();
            let recipient = recipient.to_string();
            let request_id = request_id.to_string();
            let expected_from_session_id = expected_from_session_id.to_string();
            let found = tokio::task::spawn_blocking(move || {
                service.take_matching_replies(&recipient, &request_id, &expected_from_session_id)
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
        validate_session_id(&request.parent_session_id)?;
        let result = self
            .runtime()
            .ok_or_else(|| anyhow::anyhow!("agent spawn requires the session runtime"))?
            .spawn_peer_session(request)
            .await?;
        validate_session_id(&result.session_id)?;
        Ok(result)
    }

    pub async fn control_peer_session(
        &self,
        request: AgentControlRequest,
    ) -> anyhow::Result<AgentControlResult> {
        validate_session_id(&request.requester_session_id)?;
        validate_session_id(&request.target_session_id)?;
        let expected_session_id = request.target_session_id.clone();
        let result = self
            .runtime()
            .ok_or_else(|| {
                anyhow::anyhow!("agent lifecycle operation requires the session runtime")
            })?
            .control_peer_session(request)
            .await?;
        validate_session_id(&result.session_id)?;
        if result.session_id != expected_session_id {
            anyhow::bail!("agent control result session_id does not match its target");
        }
        Ok(result)
    }

    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<()> {
        validate_session_id(recipient)?;
        for id in ids {
            validate_message_id(id, "message_id")?;
        }
        self.transport.ack(recipient, ids)
    }

    fn send_receipts(&self, recipient: &str, read: &[Envelope]) -> Vec<SendOutcome> {
        read.iter()
            .filter(|envelope| {
                envelope.r#type != MessageType::Receipt && envelope.from_session_id != recipient
            })
            .filter_map(|envelope| {
                let to = envelope.reply_target_session_id().to_string();
                match self.receipt(recipient, &to, &envelope.message_id) {
                    Ok(sent) => Some(sent.outcome),
                    Err(error) => {
                        tracing::debug!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "read receipt delivery skipped"
                        );
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

/// A process-local handle for one unacknowledged message batch returned by
/// [`MessagingService::claim`]. It does not grant exclusive ownership: another
/// consumer can observe the same stable message ids until one acknowledges them.
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
            .map(|envelope| envelope.message_id.clone())
            .collect();
        service.ack(&recipient, &ids)?;
        Ok(service.send_receipts(&recipient, &envelopes))
    }

    /// Acknowledge only the selected ids from this claim. Unselected messages
    /// remain in the durable processing file and are eligible for redelivery.
    /// This is the primitive used by the explicit `agent.acknowledge` operation so a
    /// newly-arrived message cannot be acknowledged accidentally with an old
    /// batch.
    pub fn complete_selected(self, selected_ids: &[String]) -> anyhow::Result<Vec<SendOutcome>> {
        let MessageClaim {
            service,
            recipient,
            envelopes,
        } = self;
        let selected_envelopes: Vec<Envelope> = envelopes
            .into_iter()
            .filter(|envelope| selected_ids.iter().any(|id| id == &envelope.message_id))
            .collect();
        let ids: Vec<String> = selected_envelopes
            .iter()
            .map(|envelope| envelope.message_id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        service.ack(&recipient, &ids)?;
        Ok(service.send_receipts(&recipient, &selected_envelopes))
    }

    /// Explicitly abandon the claim. This is equivalent to dropping it and
    /// documents the caller's intent to retry later.
    pub fn retry(self) {
        drop(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        MAX_MESSAGE_HISTORY_LIMIT, MAX_MESSAGE_PAYLOAD_BYTES, MAX_MESSAGE_SUBJECT_BYTES,
        MAX_MESSAGE_TEXT_BYTES, MAX_MESSAGE_TIMESTAMP_BYTES,
    };

    fn service() -> (tempfile::TempDir, MessagingService) {
        let dir = tempfile::tempdir().unwrap();
        let bus = Arc::new(InboxBus::new(dir.path()));
        (dir, MessagingService::new(bus))
    }

    #[test]
    fn direct_service_delivery_rejects_oversized_fields_before_write() {
        let (dir, service) = service();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        service.register(recipient, &[]).unwrap();

        let mut oversized_text =
            Envelope::new("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "valid");
        oversized_text.text = "x".repeat(MAX_MESSAGE_TEXT_BYTES + 1);

        let mut oversized_subject =
            Envelope::new("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "valid");
        oversized_subject.subject = Some("x".repeat(MAX_MESSAGE_SUBJECT_BYTES + 1));

        let mut oversized_payload =
            Envelope::new("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "valid");
        oversized_payload.payload = Some(serde_json::Value::String(
            "x".repeat(MAX_MESSAGE_PAYLOAD_BYTES - 1),
        ));

        let mut oversized_timestamp =
            Envelope::new("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "valid");
        oversized_timestamp.expires_at = Some("x".repeat(MAX_MESSAGE_TIMESTAMP_BYTES + 1));

        for envelope in [
            oversized_text,
            oversized_subject,
            oversized_payload,
            oversized_timestamp,
        ] {
            assert!(service.deliver(recipient, &envelope).is_err());
        }

        let mailbox = dir.path().join(format!("{recipient}.jsonl"));
        assert_eq!(std::fs::read_to_string(mailbox).unwrap(), "");
    }

    #[test]
    fn direct_service_history_applies_the_shared_result_limit() {
        let (_dir, service) = service();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        service.register(recipient, &[]).unwrap();
        for index in 0..110 {
            service
                .deliver(
                    recipient,
                    &Envelope::new(
                        "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        recipient,
                        &format!("message {index}"),
                    ),
                )
                .unwrap();
        }

        let history = service.history(recipient, usize::MAX).unwrap();

        assert_eq!(history.len(), MAX_MESSAGE_HISTORY_LIMIT);
    }

    #[test]
    fn claim_is_redeliverable_until_complete() {
        let (_dir, service) = service();
        service
            .register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        service
            .register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "durable",
        );
        service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &envelope)
            .unwrap();

        let claim = service
            .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(claim.envelopes()[0].message_id, envelope.message_id);
        assert_eq!(claim.envelopes()[0].delivery_attempt, 1);
        claim.retry();
        let retry = service
            .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(retry.envelopes()[0].message_id, envelope.message_id);
        assert_eq!(retry.envelopes()[0].delivery_attempt, 2);
        retry.retry();

        let claim = service
            .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        claim.complete().unwrap();
        assert!(
            service
                .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn stale_claim_completion_after_deletion_cannot_recreate_mailbox_or_send_receipt() {
        let (dir, service) = service();
        let sender = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        service.register(sender, &[]).unwrap();
        service.register(recipient, &[]).unwrap();
        let envelope = Envelope::new(sender, recipient, "pending before deletion");
        service.deliver(recipient, &envelope).unwrap();
        let stale_claim = service.claim(recipient).unwrap();

        service.delete_sessions(&[recipient.to_string()]).unwrap();
        assert!(stale_claim.complete().is_err());

        assert!(!dir.path().join(format!("{recipient}.jsonl")).exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(format!("{sender}.jsonl"))).unwrap(),
            "",
            "a rejected stale completion must not emit a receipt"
        );
        assert!(service.history(recipient, 10).is_err());
    }

    #[test]
    fn completing_a_claim_sends_validated_receipts_without_receipt_loops() {
        let (_dir, service) = service();
        service
            .register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        service
            .register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let first = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "第一封",
        );
        let second = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "第二封",
        );
        service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &first)
            .unwrap();
        service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &second)
            .unwrap();

        let outcomes = service
            .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap()
            .complete()
            .unwrap();
        assert_eq!(outcomes.len(), 2, "one receipt per read message");
        assert!(
            outcomes
                .iter()
                .all(|outcome| outcome.to_session_id == "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );

        let receipts = service
            .claim("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        assert_eq!(receipts.envelopes().len(), 2);
        assert!(receipts.envelopes().iter().all(|envelope| {
            envelope.r#type == MessageType::Receipt
                && envelope.from_session_id == "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                && envelope.reply_to_session_id.as_deref()
                    == Some("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        }));
        assert!(
            receipts
                .envelopes()
                .iter()
                .any(|envelope| envelope.in_reply_to_message_id.as_deref()
                    == Some(first.message_id.as_str()))
        );
        assert!(
            receipts
                .envelopes()
                .iter()
                .any(|envelope| envelope.in_reply_to_message_id.as_deref()
                    == Some(second.message_id.as_str()))
        );
        assert!(receipts.complete().unwrap().is_empty(), "no receipt loops");

        let self_message = Envelope::new(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "给自己",
        );
        service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &self_message)
            .unwrap();
        assert!(
            service
                .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
                .complete()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_unstable_identity_and_routing_mismatch() {
        let (_dir, service) = service();
        service
            .register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        service
            .register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();

        let mut envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "hello",
        );
        envelope.message_id = "invalid-id".into();
        let error = service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &envelope)
            .unwrap_err();
        assert!(error.to_string().contains("canonical message id"));

        let envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "hello",
        );
        let error = service
            .deliver("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &envelope)
            .unwrap_err();
        assert!(error.to_string().contains("recipient mismatch"));

        let mut envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "hello",
        );
        envelope.in_reply_to_message_id = Some("invalid-id".into());
        let error = service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &envelope)
            .unwrap_err();
        assert!(error.to_string().contains("in_reply_to_message_id"));
    }

    #[test]
    fn expired_messages_are_not_claimed_but_remain_in_history() {
        let (_dir, service) = service();
        service
            .register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        service
            .register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "expired",
        );
        envelope.expires_at = Some("2000-01-01T00:00:00Z".into());
        service
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &envelope)
            .unwrap();

        assert!(
            service
                .claim("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            service
                .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 10)
                .unwrap()[0]
                .message_id,
            envelope.message_id
        );
    }
}
