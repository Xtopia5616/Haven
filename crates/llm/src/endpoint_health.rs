use std::time::{Duration, Instant};

use haven_common::config::EndpointRole;

pub(crate) const ENDPOINT_COUNT: usize = 5;

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
        }
    }

    pub(crate) fn record_success(&mut self) {
        // A success from a request dispatched BEFORE the breaker tripped must
        // not close an Open breaker prematurely (M8). Concurrent in-flight
        // requests could otherwise keep the breaker perpetually closed despite
        // recent failures. Only a HalfOpen probe (or a Closed-state success) may
        // transition the breaker to Closed.
        if self.state == CircuitState::Open {
            return;
        }
        self.consecutive_failures = 0;
        self.total_calls += 1;
        self.state = CircuitState::Closed;
        self.opened_at = None;
    }

    pub(crate) fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.failure_count += 1;
        self.total_calls += 1;
        self.last_failure_time = Some(Instant::now());

        // Open if >50% failure rate and >=3 consecutive failures
        if self.consecutive_failures >= 3
            && self.total_calls > 0
            && (self.failure_count as f32 / self.total_calls as f32) > 0.5
        {
            self.state = CircuitState::Open;
            self.opened_at = Some(Instant::now());
        }
    }

    pub(crate) fn allow_request(&mut self) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                // §2.6: 30s cool-down, then HalfOpen
                if let Some(opened) = self.opened_at {
                    if opened.elapsed() >= Duration::from_secs(30) {
                        self.state = CircuitState::HalfOpen;
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

pub(crate) type EndpointHealthSlots = [EndpointHealth; ENDPOINT_COUNT];

pub(crate) fn new_endpoint_health_slots() -> EndpointHealthSlots {
    [
        EndpointHealth::new(),
        EndpointHealth::new(),
        EndpointHealth::new(),
        EndpointHealth::new(),
        EndpointHealth::new(),
    ]
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
