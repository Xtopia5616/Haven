use std::collections::HashMap;
use std::time::{Duration, Instant};

use haven_common::config::EndpointRole;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
pub(crate) struct CircuitBreaker {
    pub(crate) state: CircuitState,
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

impl CircuitBreaker {
    pub(crate) fn new() -> Self {
        Self {
            state: CircuitState::Closed,
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
            CircuitState::Open => return,
            CircuitState::HalfOpen => {
                self.half_open_probe_in_flight = false;
            }
            CircuitState::Closed => {}
        }
        self.consecutive_failures = 0;
        self.total_calls += 1;
        self.state = CircuitState::Closed;
        self.opened_at = None;
    }

    pub(crate) fn record_failure(&mut self) {
        // A completion from a request that was admitted before the breaker
        // opened must not extend the open window or mutate its counters.
        if self.state == CircuitState::Open {
            return;
        }
        let half_open_probe = self.state == CircuitState::HalfOpen;
        self.consecutive_failures += 1;
        self.failure_count += 1;
        self.total_calls += 1;
        self.last_failure_time = Some(Instant::now());

        // The breaker protects against a current outage. A historical success
        // rate must not mask a fresh run of consecutive failures.
        if half_open_probe || self.consecutive_failures >= 3 {
            self.state = CircuitState::Open;
            self.opened_at = Some(Instant::now());
            self.half_open_probe_in_flight = false;
        }
    }

    pub(crate) fn allow_request(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => {
                if self.half_open_probe_in_flight {
                    false
                } else {
                    self.half_open_probe_in_flight = true;
                    true
                }
            }
            CircuitState::Open => {
                // §2.6: 30s cool-down, then HalfOpen
                if let Some(opened) = self.opened_at {
                    if opened.elapsed() >= Duration::from_secs(30) {
                        self.state = CircuitState::HalfOpen;
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
    pub(crate) circuit_breaker: CircuitBreaker,
}

impl EndpointHealth {
    pub(crate) fn new() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_time: None,
            is_healthy: true,
            circuit_breaker: CircuitBreaker::new(),
        }
    }

    pub(crate) fn record_success(&mut self) {
        // Mirror the circuit breaker: a stale success from a pre-open request
        // must not mark the endpoint healthy again (M8).
        if self.circuit_breaker.state == CircuitState::Open {
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

    pub(crate) fn allow_request(&mut self) -> bool {
        self.circuit_breaker.allow_request()
    }
}

/// Health is keyed by the configured routed-model identity, not by the
/// legacy request role. A primary and its fallback may share a semaphore but
/// must never share circuit-breaker state.
pub(crate) type EndpointHealthMap = HashMap<String, EndpointHealth>;

pub(crate) fn new_endpoint_health_map(
    model_ids: impl IntoIterator<Item = String>,
) -> EndpointHealthMap {
    model_ids
        .into_iter()
        .map(|id| (id, EndpointHealth::new()))
        .collect()
}

pub(crate) fn health_index(role: &EndpointRole) -> usize {
    match role {
        EndpointRole::SmallModel => 0,
        EndpointRole::DefaultModel => 1,
        EndpointRole::ImageModel => 2,
        EndpointRole::AudioModel => 3,
        EndpointRole::EmbeddingModel => 4,
    }
}
