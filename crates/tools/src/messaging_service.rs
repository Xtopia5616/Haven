//! Domain service for cross-session messaging.
//!
//! [`InboxBus`] is deliberately kept behind this boundary. It remains the
//! file transport adapter for cross-process interoperability, while callers
//! use one lifecycle: send, claim, process, and complete (or drop to retry).
//! This keeps transport recovery details out of tools and the ReAct loop.

use std::sync::Arc;

use tokio::sync::watch;

use crate::inbox::{AgentInfo, Envelope, InboxBus, MessageType, SendOutcome, validate_agent_name};

/// Transport port used by [`MessagingService`].
///
/// `InboxBus` implements this port for the current cross-process JSONL
/// adapter. A `SessionActor` mailbox can implement the same port later; the
/// claim/complete contract remains above the transport boundary.
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
    fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>>;
    fn list_children(&self, parent: &str) -> anyhow::Result<Vec<AgentInfo>>;
    fn list_descendants(&self, parent: &str) -> anyhow::Result<Vec<String>>;
    fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<SendOutcome>;
    fn deliver_system_notice(&self, from: &str, to: &str, text: &str) -> anyhow::Result<()>;
    fn claim(&self, recipient: &str) -> anyhow::Result<Vec<Envelope>>;
    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<()>;
    fn last_received(&self, name: &str) -> anyhow::Result<Option<Envelope>>;
    fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Envelope>>;
    fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Vec<Envelope>>;
    fn send_receipts(&self, name: &str, read: &[Envelope]) -> Vec<SendOutcome>;
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

    fn deliver_system_notice(&self, from: &str, to: &str, text: &str) -> anyhow::Result<()> {
        InboxBus::deliver_system_notice(self, from, to, text)
    }

    fn claim(&self, recipient: &str) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::claim_and_archive(self, recipient)
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

    fn send_receipts(&self, name: &str, read: &[Envelope]) -> Vec<SendOutcome> {
        InboxBus::send_receipts(self, name, read)
    }

    fn history(&self, name: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        InboxBus::history(self, name, limit)
    }
}

/// Cross-session messaging service.
///
/// The service is the application-facing port. The current implementation is
/// backed by the JSONL [`InboxBus`] adapter so independent Haven processes can
/// exchange messages. A future in-process SessionActor mailbox can replace the
/// adapter without changing tool or ReAct lifecycle semantics.
#[derive(Debug, Clone)]
pub struct MessagingService {
    transport: Arc<dyn MessageTransport>,
}

impl MessagingService {
    /// Build a service over a transport adapter.
    pub fn new(transport: Arc<dyn MessageTransport>) -> Self {
        Self { transport }
    }

    /// Build the desktop service over Haven's shared cross-process inbox.
    pub fn default_root() -> Self {
        Self::new(Arc::new(InboxBus::default_root()))
    }

    /// Subscribe to delivery notifications. The signal is only a wake-up
    /// hint; callers must still claim from the service before processing.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.transport.subscribe()
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
        self.transport.deliver(to, envelope)
    }

    pub fn deliver_system_notice(&self, from: &str, to: &str, text: &str) -> anyhow::Result<()> {
        // The transport constructs the system envelope, so validate the
        // externally supplied addresses before delegating.
        validate_agent_name(from)?;
        validate_agent_name(to)?;
        self.transport.deliver_system_notice(from, to, text)
    }

    /// Claim messages for durable processing. The returned claim owns the
    /// delivery lease: dropping it leaves the processing file intact and the
    /// next claim redelivers the same stable envelope ids.
    pub fn claim(&self, recipient: &str) -> anyhow::Result<MessageClaim> {
        validate_agent_name(recipient)?;
        let envelopes = self.transport.claim(recipient)?;
        Ok(MessageClaim {
            transport: Arc::clone(&self.transport),
            recipient: recipient.to_string(),
            envelopes,
        })
    }

    pub fn last_received(&self, name: &str) -> anyhow::Result<Option<Envelope>> {
        self.transport.last_received(name)
    }

    pub fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Envelope>> {
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
        self.transport
            .take_matching_replies(name, in_reply_to, expected_from)
    }

    pub fn history(&self, name: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        self.transport.history(name, limit)
    }
}

/// A durable delivery lease returned by [`MessagingService::claim`].
///
/// `complete` acknowledges only the claimed ids and then emits read receipts.
/// If processing fails or the value is dropped, the processing file remains
/// durable and the messages are eligible for redelivery. This is the explicit
/// retry contract for the current file adapter.
#[derive(Debug)]
pub struct MessageClaim {
    transport: Arc<dyn MessageTransport>,
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
            transport,
            recipient,
            envelopes,
        } = self;
        let ids: Vec<String> = envelopes
            .iter()
            .map(|envelope| envelope.id.clone())
            .collect();
        transport.ack(&recipient, &ids)?;
        Ok(transport.send_receipts(&recipient, &envelopes))
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
    Ok(())
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
