use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EndpointCircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Ownership token for the single request admitted as a half-open probe.
/// The breaker keeps only a weak reference, so dropping the in-flight request
/// releases the probe slot even when its future is cancelled or discarded.
pub(crate) struct EndpointProbePermit {
    identity: Arc<()>,
}

#[derive(Debug)]
pub(crate) struct EndpointCircuitBreaker {
    pub(crate) state: EndpointCircuitState,
    pub(crate) consecutive_failures: u32,
    pub(crate) last_failure_time: Option<Instant>,
    pub(crate) failure_count: u32,
    pub(crate) total_calls: u32,
    pub(crate) opened_at: Option<Instant>,
    /// Only one request may pass while the breaker is half-open. A weak
    /// reference makes ownership of the slot follow the request future.
    half_open_probe: Option<Weak<()>>,
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
            half_open_probe: None,
        }
    }

    pub(crate) fn record_success(&mut self, probe: Option<&EndpointProbePermit>) {
        // A success from a request dispatched BEFORE the breaker tripped must
        // not close an Open breaker prematurely (M8). Concurrent in-flight
        // requests could otherwise keep the breaker perpetually closed despite
        // recent failures. Only a HalfOpen probe (or a Closed-state success) may
        // transition the breaker to Closed.
        match self.state {
            EndpointCircuitState::Open => return,
            EndpointCircuitState::HalfOpen => {
                if !self.owns_probe(probe) {
                    return;
                }
                self.half_open_probe = None;
            }
            EndpointCircuitState::Closed if probe.is_some() => return,
            EndpointCircuitState::Closed => {}
        }
        self.consecutive_failures = 0;
        self.total_calls += 1;
        self.state = EndpointCircuitState::Closed;
        self.opened_at = None;
    }

    pub(crate) fn record_failure(&mut self, probe: Option<&EndpointProbePermit>) {
        // A completion from a request that was admitted before the breaker
        // opened must not extend the open window or mutate its counters.
        if self.state == EndpointCircuitState::Open {
            return;
        }
        let half_open_probe = self.state == EndpointCircuitState::HalfOpen;
        if half_open_probe {
            if !self.owns_probe(probe) {
                return;
            }
            self.half_open_probe = None;
        } else if probe.is_some() {
            return;
        }
        self.consecutive_failures += 1;
        self.failure_count += 1;
        self.total_calls += 1;
        self.last_failure_time = Some(Instant::now());

        // The breaker protects against a current outage. A historical success
        // rate must not mask a fresh run of consecutive failures.
        if half_open_probe || self.consecutive_failures >= 3 {
            self.state = EndpointCircuitState::Open;
            self.opened_at = Some(Instant::now());
            self.half_open_probe = None;
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
        self.half_open_probe = None;
    }

    /// Admit a request, returning a permit when it owns the half-open probe.
    /// A dropped permit expires the weak reservation automatically.
    pub(crate) fn try_acquire_request(&mut self) -> Result<Option<EndpointProbePermit>, ()> {
        match self.state {
            EndpointCircuitState::Closed => Ok(None),
            EndpointCircuitState::HalfOpen => self.acquire_probe(),
            EndpointCircuitState::Open => {
                // §2.6: 30s cool-down, then HalfOpen
                if let Some(opened) = self.opened_at {
                    if opened.elapsed() >= Duration::from_secs(30) {
                        self.state = EndpointCircuitState::HalfOpen;
                        self.acquire_probe()
                    } else {
                        Err(())
                    }
                } else {
                    Err(())
                }
            }
        }
    }

    fn acquire_probe(&mut self) -> Result<Option<EndpointProbePermit>, ()> {
        if self
            .half_open_probe
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            return Err(());
        }

        let identity = Arc::new(());
        self.half_open_probe = Some(Arc::downgrade(&identity));
        Ok(Some(EndpointProbePermit { identity }))
    }

    fn owns_probe(&self, probe: Option<&EndpointProbePermit>) -> bool {
        let Some(probe) = probe else {
            return false;
        };
        self.half_open_probe
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|identity| Arc::ptr_eq(&identity, &probe.identity))
    }
}

/// Circuit state is keyed by configured routed-model identity. A primary and
/// its fallback may share a semaphore but must never share breaker state.
pub(crate) type EndpointCircuitBreakerMap = HashMap<String, EndpointCircuitBreaker>;

pub(crate) fn new_endpoint_circuit_breaker_map(
    model_config_ids: impl IntoIterator<Item = String>,
) -> EndpointCircuitBreakerMap {
    model_config_ids
        .into_iter()
        .map(|model_config_id| (model_config_id, EndpointCircuitBreaker::new()))
        .collect()
}
