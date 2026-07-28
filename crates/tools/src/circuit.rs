use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Circuit breaker protecting a single tool from being called repeatedly
/// while it is failing. Mirrors the LLM-level `CircuitBreaker` pattern but
/// operates per tool-name.
///
/// State machine:
/// - **Closed**: normal operation. Consecutive failures increment the counter.
///   After `failure_threshold` consecutive failures, the breaker opens.
/// - **Open**: requests are rejected immediately (fast-fail) until
///   `cooldown` has elapsed since the breaker opened, then transitions to
///   HalfOpen.
/// - **HalfOpen**: a single probe request is allowed. On success the breaker
///   closes; on failure it re-opens.
pub struct ToolCircuitBreaker {
    state: CircuitState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    failure_threshold: u32,
    cooldown: Duration,
}

impl ToolCircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            opened_at: None,
            failure_threshold,
            cooldown,
        }
    }

    pub fn allow_request(&mut self) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if let Some(opened) = self.opened_at
                    && opened.elapsed() >= self.cooldown
                {
                    self.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.state = CircuitState::Closed;
        self.opened_at = None;
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.failure_threshold {
            self.state = CircuitState::Open;
            self.opened_at = Some(Instant::now());
        }
    }

    pub fn is_open(&self) -> bool {
        self.state == CircuitState::Open
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

impl Default for ToolCircuitBreaker {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(30))
    }
}

/// Registry of per-tool circuit breakers. Thread-safe via `std::sync::Mutex`
/// because all operations are O(1) and never held across `.await` points.
#[derive(Default)]
pub struct ToolCircuitRegistry {
    breakers: StdMutex<HashMap<String, ToolCircuitBreaker>>,
}

impl ToolCircuitRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_request(&self, tool_name: &str) -> bool {
        let mut breakers = self.breakers.lock().unwrap();
        let breaker = breakers
            .entry(tool_name.to_string())
            .or_default();
        breaker.allow_request()
    }

    pub fn record_success(&self, tool_name: &str) {
        let mut breakers = self.breakers.lock().unwrap();
        if let Some(b) = breakers.get_mut(tool_name) {
            b.record_success();
        }
    }

    pub fn record_failure(&self, tool_name: &str) {
        let mut breakers = self.breakers.lock().unwrap();
        let breaker = breakers
            .entry(tool_name.to_string())
            .or_default();
        breaker.record_failure()
    }

    pub fn is_open(&self, tool_name: &str) -> bool {
        let breakers = self.breakers.lock().unwrap();
        breakers.get(tool_name).is_some_and(|b| b.is_open())
    }

    pub fn reset(&self, tool_name: &str) {
        let mut breakers = self.breakers.lock().unwrap();
        if let Some(b) = breakers.get_mut(tool_name) {
            b.record_success();
        }
    }

    pub fn reset_all(&self) {
        let mut breakers = self.breakers.lock().unwrap();
        for b in breakers.values_mut() {
            b.record_success();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let mut cb = ToolCircuitBreaker::new(3, Duration::from_millis(100));
        assert!(cb.allow_request());
        assert!(!cb.is_open());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let mut cb = ToolCircuitBreaker::new(3, Duration::from_millis(100));
        cb.record_failure();
        cb.record_failure();
        assert!(cb.allow_request(), "still closed after 2 failures");
        cb.record_failure();
        assert!(cb.is_open(), "should be open after 3 consecutive failures");
        assert!(!cb.allow_request(), "should reject when open");
    }

    #[test]
    fn test_circuit_breaker_half_open_after_cooldown() {
        let mut cb = ToolCircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.allow_request(), "should allow probe after cooldown");
        assert!(!cb.is_open(), "HalfOpen is not Open");
    }

    #[test]
    fn test_circuit_breaker_closes_on_success() {
        let mut cb = ToolCircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.allow_request());
        cb.record_success();
        assert!(!cb.is_open(), "should close after success");
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_reopens_on_half_open_failure() {
        let mut cb = ToolCircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.allow_request());
        cb.record_failure();
        assert!(cb.is_open(), "should reopen on probe failure");
    }

    #[test]
    fn test_circuit_breaker_success_resets_consecutive() {
        let mut cb = ToolCircuitBreaker::new(3, Duration::from_millis(100));
        cb.record_failure();
        cb.record_failure();
        cb.record_success();
        cb.record_failure();
        cb.record_failure();
        assert!(cb.allow_request(), "only 2 after reset — not open");
    }

    #[test]
    fn test_circuit_registry_allow_and_record() {
        let reg = ToolCircuitRegistry::new();
        assert!(reg.allow_request("mytool"));
        for _ in 0..4 {
            reg.record_failure("mytool");
        }
        assert!(reg.allow_request("mytool"));
        reg.record_failure("mytool");
        assert!(reg.is_open("mytool"));
        assert!(!reg.allow_request("mytool"));
    }

    #[test]
    fn test_circuit_registry_reset() {
        let reg = ToolCircuitRegistry::new();
        for _ in 0..5 {
            reg.record_failure("mytool");
        }
        assert!(reg.is_open("mytool"));
        reg.reset("mytool");
        assert!(!reg.is_open("mytool"));
        assert!(reg.allow_request("mytool"));
    }

    #[test]
    fn test_circuit_registry_reset_all() {
        let reg = ToolCircuitRegistry::new();
        for _ in 0..5 {
            reg.record_failure("tool_a");
        }
        for _ in 0..5 {
            reg.record_failure("tool_b");
        }
        assert!(reg.is_open("tool_a"));
        assert!(reg.is_open("tool_b"));
        reg.reset_all();
        assert!(!reg.is_open("tool_a"));
        assert!(!reg.is_open("tool_b"));
    }

    #[test]
    fn test_circuit_registry_tools_are_independent() {
        let reg = ToolCircuitRegistry::new();
        for _ in 0..5 {
            reg.record_failure("failing_tool");
        }
        assert!(reg.is_open("failing_tool"));
        assert!(!reg.is_open("healthy_tool"));
        assert!(reg.allow_request("healthy_tool"));
    }
}
