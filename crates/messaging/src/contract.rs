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
    validate_session_id(&envelope.from_session_id)?;
    if envelope.to_session_id != to {
        anyhow::bail!(
            "message recipient mismatch: envelope targets '{}' but delivery targets '{}'",
            envelope.to_session_id,
            to
        );
    }
    validate_message_id(&envelope.message_id, "message_id")?;
    if envelope.text.trim().is_empty() {
        anyhow::bail!("message text must not be empty");
    }
    if let Some(reply_to_session_id) = &envelope.reply_to_session_id {
        validate_session_id(reply_to_session_id)?;
    }
    if let Some(in_reply_to_message_id) = &envelope.in_reply_to_message_id {
        validate_message_id(in_reply_to_message_id, "in_reply_to_message_id")?;
    }
    DateTime::parse_from_rfc3339(&envelope.created_at)
        .map_err(|error| anyhow::anyhow!("invalid message created_at: {error}"))?;
    if let Some(expires_at) = &envelope.expires_at {
        DateTime::parse_from_rfc3339(expires_at)
            .map_err(|error| anyhow::anyhow!("invalid message expires_at: {error}"))?;
    }
    if envelope.r#type == MessageType::Receipt {
        let Some(in_reply_to_message_id) = envelope.in_reply_to_message_id.as_deref() else {
            anyhow::bail!("receipt messages require in_reply_to_message_id");
        };
        validate_message_id(in_reply_to_message_id, "receipt in_reply_to_message_id")?;
    }
    if envelope.r#type == MessageType::Reply && envelope.in_reply_to_message_id.is_none() {
        anyhow::bail!("reply messages require in_reply_to_message_id");
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
