//! Byte accumulation with one declared and observed size limit.

use std::error::Error;
use std::fmt;

/// Error returned when a declared or observed body exceeds its byte limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedBytesLimitError {
    limit: usize,
}

impl fmt::Display for BoundedBytesLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "body exceeds the {} byte limit", self.limit)
    }
}

impl Error for BoundedBytesLimitError {}

/// A byte buffer that rejects content beyond a fixed maximum size.
///
/// Network transports use this value to share the content-length precheck,
/// bounded reservation, and per-chunk size invariant while retaining their
/// own streaming, timeout, cancellation, and protocol error behavior.
pub struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    /// Create a buffer and reject an oversized declared length before reserve.
    pub fn new(limit: usize, content_length: Option<u64>) -> Result<Self, BoundedBytesLimitError> {
        let capacity = match content_length {
            Some(length) => {
                let length =
                    usize::try_from(length).map_err(|_| BoundedBytesLimitError { limit })?;
                if length > limit {
                    return Err(BoundedBytesLimitError { limit });
                }
                length
            }
            None => 0,
        };

        Ok(Self {
            bytes: Vec::with_capacity(capacity.min(limit)),
            limit,
        })
    }

    /// Append one chunk if the resulting buffer stays within its byte limit.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), BoundedBytesLimitError> {
        if chunk.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(BoundedBytesLimitError { limit: self.limit });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    /// Return the accumulated bytes after the transport has reached EOF.
    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}
