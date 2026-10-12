use chrono::DateTime;
use haven_common::types::is_canonical_id;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};

use crate::inbox::{Envelope, MessageType};

/// Maximum UTF-8 byte length for message text on the durable messaging wire.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 16 * 1024;
/// Maximum UTF-8 byte length for an optional message subject.
pub const MAX_MESSAGE_SUBJECT_BYTES: usize = 512;
/// Maximum serialized JSON byte length for an optional structured payload.
pub const MAX_MESSAGE_PAYLOAD_BYTES: usize = 64 * 1024;
/// RFC3339 timestamp budget: a 4-digit date, nanosecond fraction, and numeric
/// timezone offset (`YYYY-MM-DDTHH:MM:SS.fffffffff+HH:MM`).
pub const MAX_MESSAGE_TIMESTAMP_BYTES: usize = 35;

// JSON string escaping can expand a control byte to six bytes (`\\u00XX`).
// The framing value below is derived from Envelope's field names, quotes,
// commas, braces, and the fixed-size canonical IDs/type/attempt fields. Keep it
// in sync with the serialized Envelope definition when fields are added.
const ENVELOPE_JSON_FRAMING_BYTES: usize = (10 + 4 + 15 + 13 + 19 + 22 + 7 + 4 + 7 + 10 + 10 + 16) // field names
        + (12 * 3) // quoted field names and colons
        + 11 // commas
        + 2; // braces

/// Maximum serialized envelope body, derived from the bounded fields and
/// fixed JSON framing. The JSONL newline is not included.
pub const MAX_MESSAGE_ENVELOPE_BYTES: usize = ENVELOPE_JSON_FRAMING_BYTES
    + (36 + 2) * 5 // canonical message/session IDs, including JSON quotes
    + (9 + 2) // largest current MessageType (`broadcast`), including quotes
    + (MAX_MESSAGE_SUBJECT_BYTES * 6 + 2)
    + (MAX_MESSAGE_TEXT_BYTES * 6 + 2)
    + MAX_MESSAGE_PAYLOAD_BYTES
    + (MAX_MESSAGE_TIMESTAMP_BYTES + 2) * 2
    + 10; // u32 delivery_attempt in decimal

struct BoundedCounter {
    limit: usize,
    bytes_written: usize,
    exceeded: bool,
}

impl BoundedCounter {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes_written: 0,
            exceeded: false,
        }
    }
}

impl Write for BoundedCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes_written) {
            self.exceeded = true;
            return Err(io::Error::other("serialized value exceeds byte limit"));
        }
        self.bytes_written += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_serialized_size<T: Serialize>(
    value: &T,
    max_bytes: usize,
    description: &str,
) -> anyhow::Result<()> {
    let mut counter = BoundedCounter::new(max_bytes);
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(()),
        Err(_) if counter.exceeded => {
            anyhow::bail!("{description} exceeds maximum of {max_bytes} bytes")
        }
        Err(error) => Err(error.into()),
    }
}

/// Validate a payload without allocating a serialized copy of it.
pub fn validate_message_payload_size(payload: &Value) -> anyhow::Result<()> {
    validate_serialized_size(payload, MAX_MESSAGE_PAYLOAD_BYTES, "message payload")
}

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

/// Validate an envelope before writing it to the durable JSONL transport.
/// Existing durable rows continue to use the structural validator above so
/// adding an outbound limit does not make an old pending message unclaimable.
pub(crate) fn validate_message_envelope_for_delivery(
    to: &str,
    envelope: &Envelope,
) -> anyhow::Result<()> {
    if envelope.text.len() > MAX_MESSAGE_TEXT_BYTES {
        anyhow::bail!("message text exceeds maximum of {MAX_MESSAGE_TEXT_BYTES} bytes");
    }
    if let Some(subject) = &envelope.subject
        && subject.len() > MAX_MESSAGE_SUBJECT_BYTES
    {
        anyhow::bail!("message subject exceeds maximum of {MAX_MESSAGE_SUBJECT_BYTES} bytes");
    }
    if envelope.created_at.len() > MAX_MESSAGE_TIMESTAMP_BYTES {
        anyhow::bail!("message created_at exceeds maximum of {MAX_MESSAGE_TIMESTAMP_BYTES} bytes");
    }
    if let Some(expires_at) = &envelope.expires_at
        && expires_at.len() > MAX_MESSAGE_TIMESTAMP_BYTES
    {
        anyhow::bail!("message expires_at exceeds maximum of {MAX_MESSAGE_TIMESTAMP_BYTES} bytes");
    }
    if let Some(payload) = &envelope.payload {
        validate_message_payload_size(payload)?;
    }
    validate_message_envelope(to, envelope)?;
    validate_serialized_size(
        envelope,
        MAX_MESSAGE_ENVELOPE_BYTES,
        "serialized message envelope",
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_budget_covers_exact_field_limits_and_json_escaping() {
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            recipient,
            &"\0".repeat(MAX_MESSAGE_TEXT_BYTES),
        );
        envelope.r#type = MessageType::Broadcast;
        envelope.reply_to_session_id = Some("ses-cccccccccccccccccccccccccccccccc".into());
        envelope.in_reply_to_message_id = Some("msg-dddddddddddddddddddddddddddddddd".into());
        envelope.subject = Some("\0".repeat(MAX_MESSAGE_SUBJECT_BYTES));
        envelope.payload = Some(Value::String("x".repeat(MAX_MESSAGE_PAYLOAD_BYTES - 2)));
        let max_timestamp = "2026-01-01T00:00:00.123456789+00:00";
        assert_eq!(max_timestamp.len(), MAX_MESSAGE_TIMESTAMP_BYTES);
        envelope.created_at = max_timestamp.into();
        envelope.expires_at = Some(max_timestamp.into());
        envelope.delivery_attempt = u32::MAX;

        validate_message_envelope_for_delivery(recipient, &envelope).unwrap();

        let serialized = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(serialized.len(), MAX_MESSAGE_ENVELOPE_BYTES);
    }

    #[test]
    fn timestamp_length_is_checked_before_parsing() {
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut envelope =
            Envelope::new("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "hello");
        envelope.created_at = "x".repeat(MAX_MESSAGE_TIMESTAMP_BYTES + 1);

        let error = validate_message_envelope_for_delivery(recipient, &envelope).unwrap_err();

        assert!(error.to_string().contains("created_at"));
        assert!(error.to_string().contains("maximum"));
    }
}
