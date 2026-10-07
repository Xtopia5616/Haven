use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EndpointCircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
pub(crate) struct EndpointCircuitBreaker {
    pub(crate) state: EndpointCircuitState,
    pub(crate) consecutive_failures: u32,
    pub(crate) last_failure_time: Option<Instant>,
    pub(crate) failure_count: u32,
    pub(crate) total_calls: u32,
    pub(crate) opened_at: Option<Instant>,
    /// Only one request may pass while the breaker is half-open. This is a
    /// state bit rather than an async mutex because callers already serialize
    /// health transitions under the router's health write lock.
    pub(crate) half_open_probe_in_flight: bool,
}

impl EndpointCircuitBreaker {
    pub(crate) fn new() -> Self {
        Self {
            state: EndpointCircuitState::Closed,
            consecutive_failures: 0,
            last_failure_time: None,
            failure_count: 0,
            total_calls: 0,
            opened_at: None,
            half_open_probe_in_flight: false,
        }
    }

    pub(crate) fn record_success(&mut self) {
        // A success from a request dispatched BEFORE the breaker tripped must
        // not close an Open breaker prematurely (M8). Concurrent in-flight
        // requests could otherwise keep the breaker perpetually closed despite
        // recent failures. Only a HalfOpen probe (or a Closed-state success) may
        // transition the breaker to Closed.
        match self.state {
            EndpointCircuitState::Open => return,
            EndpointCircuitState::HalfOpen => {
                self.half_open_probe_in_flight = false;
            }
            EndpointCircuitState::Closed => {}
        }
        self.consecutive_failures = 0;
        self.total_calls += 1;
        self.state = EndpointCircuitState::Closed;
        self.opened_at = None;
    }

    pub(crate) fn record_failure(&mut self) {
        // A completion from a request that was admitted before the breaker
        // opened must not extend the open window or mutate its counters.
        if self.state == EndpointCircuitState::Open {
            return;
        }
        let half_open_probe = self.state == EndpointCircuitState::HalfOpen;
        self.consecutive_failures += 1;
        self.failure_count += 1;
        self.total_calls += 1;
        self.last_failure_time = Some(Instant::now());

        // The breaker protects against a current outage. A historical success
        // rate must not mask a fresh run of consecutive failures.
        if half_open_probe || self.consecutive_failures >= 3 {
            self.state = EndpointCircuitState::Open;
            self.opened_at = Some(Instant::now());
            self.half_open_probe_in_flight = false;
        }
    }

    /// Let an explicit user retry bypass the current open window once the
    /// session's next request reaches the router. Historical call counters
    /// remain intact; only the consecutive-failure gate is cleared.
    pub(crate) fn reset_for_manual_retry(&mut self) {
        self.state = EndpointCircuitState::Closed;
        self.consecutive_failures = 0;
        self.last_failure_time = None;
        self.opened_at = None;
        self.half_open_probe_in_flight = false;
    }

    pub(crate) fn allow_request(&mut self) -> bool {
        match self.state {
            EndpointCircuitState::Closed => true,
            EndpointCircuitState::HalfOpen => {
                if self.half_open_probe_in_flight {
                    false
                } else {
                    self.half_open_probe_in_flight = true;
                    true
                }
            }
            EndpointCircuitState::Open => {
                // §2.6: 30s cool-down, then HalfOpen
                if let Some(opened) = self.opened_at {
                    if opened.elapsed() >= Duration::from_secs(30) {
                        self.state = EndpointCircuitState::HalfOpen;
                        self.half_open_probe_in_flight = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EndpointHealth {
    pub(crate) consecutive_failures: u32,
    pub(crate) last_failure_time: Option<Instant>,
    pub(crate) is_healthy: bool,
    pub(crate) circuit_breaker: EndpointCircuitBreaker,
}

impl EndpointHealth {
    pub(crate) fn new() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_time: None,
            is_healthy: true,
            circuit_breaker: EndpointCircuitBreaker::new(),
        }
    }

    pub(crate) fn record_success(&mut self) {
        // Mirror the circuit breaker: a stale success from a pre-open request
        // must not mark the endpoint healthy again (M8).
        if self.circuit_breaker.state == EndpointCircuitState::Open {
            return;
        }
        self.consecutive_failures = 0;
        self.is_healthy = true;
        self.circuit_breaker.record_success();
    }

    pub(crate) fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_failure_time = Some(Instant::now());
        self.circuit_breaker.record_failure();
        // Mark unhealthy after 3 consecutive failures
        if self.consecutive_failures >= 3 {
            self.is_healthy = false;
        }
    }

    pub(crate) fn reset_for_manual_retry(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure_time = None;
        self.is_healthy = true;
        self.circuit_breaker.reset_for_manual_retry();
    }

    pub(crate) fn allow_request(&mut self) -> bool {
        self.circuit_breaker.allow_request()
    }
}

/// Health is keyed by configured routed-model identity. A primary and its
/// fallback may share a semaphore but must never share circuit-breaker state.
pub(crate) type EndpointHealthMap = HashMap<String, EndpointHealth>;

pub(crate) fn new_endpoint_health_map(
    model_ids: impl IntoIterator<Item = String>,
) -> EndpointHealthMap {
    model_ids
        .into_iter()
        .map(|id| (id, EndpointHealth::new()))
        .collect()
}
