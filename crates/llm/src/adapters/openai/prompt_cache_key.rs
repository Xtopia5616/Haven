use crate::adapters::current_epoch_seconds;
use crate::types::LlmError;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

const UNKNOWN: u8 = 0;
const ENABLED: u8 = 1;
const UNSUPPORTED: u8 = 2;
const REPROBE_AFTER_SECS: u64 = 300;

/// Per-adapter support state for the OpenAI prompt cache key extension.
pub(in crate::adapters) struct PromptCacheKeySupport {
    state: AtomicU8,
    retry_at: AtomicU64,
}

impl PromptCacheKeySupport {
    pub(in crate::adapters) fn new() -> Self {
        Self {
            state: AtomicU8::new(UNKNOWN),
            retry_at: AtomicU64::new(0),
        }
    }

    pub(in crate::adapters) fn should_attach(&self) -> bool {
        if self.state.load(Ordering::Relaxed) != UNSUPPORTED {
            return true;
        }
        let retry_at = self.retry_at.load(Ordering::Relaxed);
        retry_at != 0 && current_epoch_seconds() >= retry_at
    }

    pub(in crate::adapters) fn remember_rejection(&self) {
        self.state.store(UNSUPPORTED, Ordering::Relaxed);
        self.retry_at.store(
            current_epoch_seconds().saturating_add(REPROBE_AFTER_SECS),
            Ordering::Relaxed,
        );
    }

    pub(in crate::adapters) fn remember_success(&self) {
        self.retry_at.store(0, Ordering::Relaxed);
        self.state.store(ENABLED, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::adapters) fn force_unsupported_for_test(&self, retry_at: u64) {
        self.state.store(UNSUPPORTED, Ordering::Relaxed);
        self.retry_at.store(retry_at, Ordering::Relaxed);
    }
}

pub(in crate::adapters) fn is_unsupported_prompt_cache_key_error(error: &LlmError) -> bool {
    let LlmError::RequestFailed(message) = error else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    message.contains("prompt_cache_key")
        && [
            "unknown",
            "unsupported",
            "unrecognized",
            "extra field",
            "extra fields",
            "additional propert",
            "not allowed",
            "unexpected",
            "invalid parameter",
        ]
        .iter()
        .any(|hint| message.contains(hint))
}
