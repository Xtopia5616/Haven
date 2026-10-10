use chrono::DateTime;
use haven_common::types::is_canonical_id;

use crate::inbox::{Envelope, MessageType};

pub(crate) fn validate_session_id(session_id: &str) -> anyhow::Result<()> {
    if is_canonical_id(session_id, "ses") {
        Ok(())
    } else {
        anyhow::bail!("session_id must be a canonical session id")
    }
}

pub(crate) fn validate_message_id(message_id: &str, field: &str) -> anyhow::Result<()> {
    if is_canonical_id(message_id, "msg") {
        Ok(())
    } else {
        anyhow::bail!("{field} must be a canonical message id")
    }
}

pub(crate) fn validate_message_envelope(to: &str, envelope: &Envelope) -> anyhow::Result<()> {
    validate_session_id(to)?;
    validate_session_id(&envelope.from)?;
    if envelope.to != to {
        anyhow::bail!(
            "message recipient mismatch: envelope targets '{}' but delivery targets '{}'",
            envelope.to,
            to
        );
    }
    validate_message_id(&envelope.id, "message_id")?;
    if envelope.text.trim().is_empty() {
        anyhow::bail!("message text must not be empty");
    }
    if let Some(reply_address) = &envelope.reply_address {
        validate_session_id(reply_address)?;
    }
    if let Some(in_reply_to) = &envelope.in_reply_to {
        validate_message_id(in_reply_to, "in_reply_to")?;
    }
    DateTime::parse_from_rfc3339(&envelope.created_at)
        .map_err(|error| anyhow::anyhow!("invalid message created_at: {error}"))?;
    if let Some(expires_at) = &envelope.expires_at {
        DateTime::parse_from_rfc3339(expires_at)
            .map_err(|error| anyhow::anyhow!("invalid message expires_at: {error}"))?;
    }
    if envelope.r#type == MessageType::Receipt {
        let Some(in_reply_to) = envelope.in_reply_to.as_deref() else {
            anyhow::bail!("receipt messages require in_reply_to");
        };
        validate_message_id(in_reply_to, "receipt in_reply_to")?;
    }
    if envelope.r#type == MessageType::Reply && envelope.in_reply_to.is_none() {
        anyhow::bail!("reply messages require in_reply_to");
    }
    Ok(())
}

pub(crate) fn validate_message_batch(
    recipient_session_id: &str,
    envelopes: &[Envelope],
) -> anyhow::Result<()> {
    for envelope in envelopes {
        validate_message_envelope(recipient_session_id, envelope)?;
    }
    Ok(())
}

pub fn is_expired(envelope: &Envelope) -> bool {
    envelope.expires_at.as_deref().is_some_and(|expires_at| {
        DateTime::parse_from_rfc3339(expires_at)
            .map(|parsed| parsed <= chrono::Utc::now())
            .unwrap_or(false)
    })
}
