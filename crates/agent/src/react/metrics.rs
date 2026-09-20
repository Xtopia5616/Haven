//! Bounded, non-blocking metrics for the live ReAct loop.
//!
//! This is intentionally an in-process baseline rather than a new telemetry
//! dependency. Every update is an atomic increment, and latency samples are
//! recorded in fixed buckets. The optional debug event contains only phase
//! identity and session/run/step correlation; it never carries prompt text,
//! credentials, or tool output.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Upper bounds, in milliseconds, for the latency histogram buckets.
const LATENCY_BUCKETS_MS: [u64; 16] = [
    1,
    2,
    5,
    10,
    25,
    50,
    100,
    250,
    500,
    1_000,
    2_500,
    5_000,
    10_000,
    30_000,
    60_000,
    u64::MAX,
];

const PHASE_COUNT: usize = 12;
const COUNTER_COUNT: usize = 12;
const GAUGE_COUNT: usize = 1;

/// ReAct boundaries whose latency is useful for the first performance baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    ContextInject,
    TokenEstimate,
    RequestContext,
    FirstToken,
    LlmStream,
    ToolAdmission,
    ToolExecution,
    OrderedCommit,
    EventAppend,
    Projection,
    Snapshot,
    SqliteLockWait,
}

impl Phase {
    fn index(self) -> usize {
        match self {
            Self::ContextInject => 0,
            Self::TokenEstimate => 1,
            Self::RequestContext => 2,
            Self::FirstToken => 3,
            Self::LlmStream => 4,
            Self::ToolAdmission => 5,
            Self::ToolExecution => 6,
            Self::OrderedCommit => 7,
            Self::EventAppend => 8,
            Self::Projection => 9,
            Self::Snapshot => 10,
            Self::SqliteLockWait => 11,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ContextInject => "context_inject",
            Self::TokenEstimate => "token_estimate",
            Self::RequestContext => "request_context",
            Self::FirstToken => "first_token",
            Self::LlmStream => "llm_stream",
            Self::ToolAdmission => "tool_admission",
            Self::ToolExecution => "tool_execution",
            Self::OrderedCommit => "ordered_commit",
            Self::EventAppend => "event_append",
            Self::Projection => "projection",
            Self::Snapshot => "snapshot",
            Self::SqliteLockWait => "sqlite_lock_wait",
        }
    }
}

#[derive(Debug)]
struct PhaseMetric {
    count: AtomicU64,
    total_ms: AtomicU64,
    buckets: [AtomicU64; 16],
}

impl PhaseMetric {
    fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ms: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn record(&self, duration: Duration) {
        let duration_ms = duration.as_millis().min(u64::MAX as u128) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ms.fetch_add(duration_ms, Ordering::Relaxed);
        let bucket = LATENCY_BUCKETS_MS
            .iter()
            .position(|upper_bound| duration_ms <= *upper_bound)
            .expect("latency histogram has an overflow bucket");
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn snapshot(&self) -> PhaseSnapshot {
        let count = self.count.load(Ordering::Relaxed);
        let mut buckets = [0; 16];
        for (index, bucket) in self.buckets.iter().enumerate() {
            buckets[index] = bucket.load(Ordering::Relaxed);
        }
        PhaseSnapshot {
            count,
            total_ms: self.total_ms.load(Ordering::Relaxed),
            p50_ms: percentile(&buckets, count, 50),
            p95_ms: percentile(&buckets, count, 95),
        }
    }
}

impl Default for PhaseMetric {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PhaseSnapshot {
    pub count: u64,
    pub total_ms: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Counter {
    TurnStarts,
    FirstTokens,
    StreamChunks,
    ChunkDrops,
    CheckpointPending,
    BranchPointFailures,
    SnapshotFailures,
    ProjectionFailures,
    InboxAckFailures,
    ActionResultRetries,
    ActionResultDuplicates,
    WebSearchDrops,
}

impl Counter {
    fn index(self) -> usize {
        match self {
            Self::TurnStarts => 0,
            Self::FirstTokens => 1,
            Self::StreamChunks => 2,
            Self::ChunkDrops => 3,
            Self::CheckpointPending => 4,
            Self::BranchPointFailures => 5,
            Self::SnapshotFailures => 6,
            Self::ProjectionFailures => 7,
            Self::InboxAckFailures => 8,
            Self::ActionResultRetries => 9,
            Self::ActionResultDuplicates => 10,
            Self::WebSearchDrops => 11,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CounterSnapshot {
    pub turn_starts: u64,
    pub first_tokens: u64,
    pub stream_chunks: u64,
    pub chunk_drops: u64,
    pub checkpoint_pending: u64,
    pub branch_point_failures: u64,
    pub snapshot_failures: u64,
    pub projection_failures: u64,
    pub inbox_ack_failures: u64,
    pub action_result_retries: u64,
    pub action_result_duplicates: u64,
    pub web_search_drops: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct GaugeSnapshot {
    pub context_queue_items: u64,
}

/// Renderer-side stream counters supplied when the frontend asks for the
/// unified performance snapshot. They are kept separate from ReAct atomics
/// because the renderer owns the animation-frame lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, Serialize)]
pub struct UiMetricsSnapshot {
    pub frames: u64,
    pub chunks: u64,
    pub drops: u64,
}

/// Shared metrics for one [`ReActEngine`]. It has no locks and a fixed memory
/// footprint, so instrumentation cannot create an unbounded queue or block a
/// turn on a metrics consumer.
#[derive(Debug)]
pub(crate) struct ReActMetrics {
    phases: [PhaseMetric; PHASE_COUNT],
    counters: [AtomicU64; COUNTER_COUNT],
    gauges: [AtomicU64; GAUGE_COUNT],
}

impl ReActMetrics {
    pub(crate) fn new() -> Self {
        Self {
            phases: std::array::from_fn(|_| PhaseMetric::new()),
            counters: std::array::from_fn(|_| AtomicU64::new(0)),
            gauges: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    pub(crate) fn start<'a>(
        &'a self,
        phase: Phase,
        session_id: &'a str,
        run_id: u64,
        step_number: u32,
    ) -> PhaseTimer<'a> {
        PhaseTimer {
            metrics: self,
            phase,
            session_id,
            run_id,
            step_number,
            started: Instant::now(),
        }
    }

    pub(crate) fn increment(&self, counter: Counter) {
        self.counters[counter.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn observe(&self, phase: Phase, duration: Duration) {
        self.phases[phase.index()].record(duration);
    }

    pub(crate) fn decrement(&self, counter: Counter) {
        self.counters[counter.index()].fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn set_context_queue_items(&self, items: usize) {
        self.gauges[0].store(items as u64, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> MetricsSnapshot {
        let phases = std::array::from_fn(|index| self.phases[index].snapshot());
        MetricsSnapshot {
            phases,
            counters: CounterSnapshot {
                turn_starts: self.counters[Counter::TurnStarts.index()].load(Ordering::Relaxed),
                first_tokens: self.counters[Counter::FirstTokens.index()].load(Ordering::Relaxed),
                stream_chunks: self.counters[Counter::StreamChunks.index()].load(Ordering::Relaxed),
                chunk_drops: self.counters[Counter::ChunkDrops.index()].load(Ordering::Relaxed),
                checkpoint_pending: self.counters[Counter::CheckpointPending.index()]
                    .load(Ordering::Relaxed),
                branch_point_failures: self.counters[Counter::BranchPointFailures.index()]
                    .load(Ordering::Relaxed),
                snapshot_failures: self.counters[Counter::SnapshotFailures.index()]
                    .load(Ordering::Relaxed),
                projection_failures: self.counters[Counter::ProjectionFailures.index()]
                    .load(Ordering::Relaxed),
                inbox_ack_failures: self.counters[Counter::InboxAckFailures.index()]
                    .load(Ordering::Relaxed),
                action_result_retries: self.counters[Counter::ActionResultRetries.index()]
                    .load(Ordering::Relaxed),
                action_result_duplicates: self.counters[Counter::ActionResultDuplicates.index()]
                    .load(Ordering::Relaxed),
                web_search_drops: self.counters[Counter::WebSearchDrops.index()]
                    .load(Ordering::Relaxed),
            },
            gauges: GaugeSnapshot {
                context_queue_items: self.gauges[0].load(Ordering::Relaxed),
            },
            ui: None,
        }
    }
}

impl Default for ReActMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    pub phases: [PhaseSnapshot; PHASE_COUNT],
    pub counters: CounterSnapshot,
    pub gauges: GaugeSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UiMetricsSnapshot>,
}

impl MetricsSnapshot {
    #[allow(dead_code)]
    pub(crate) fn phase(&self, phase: Phase) -> PhaseSnapshot {
        self.phases[phase.index()]
    }
}

/// A latency sample that records on every exit path, including cancellation
/// and errors. Dropping the timer performs only atomics and a bounded debug
/// event, never an await or a lock acquisition.
pub(crate) struct PhaseTimer<'a> {
    metrics: &'a ReActMetrics,
    phase: Phase,
    session_id: &'a str,
    run_id: u64,
    step_number: u32,
    started: Instant,
}

impl Drop for PhaseTimer<'_> {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        self.metrics.phases[self.phase.index()].record(elapsed);
        tracing::debug!(
            session_id = self.session_id,
            run_id = self.run_id,
            step_number = self.step_number,
            phase = self.phase.as_str(),
            duration_ms = elapsed.as_millis() as u64,
            "react phase observed"
        );
    }
}

#[allow(dead_code)]
fn percentile(buckets: &[u64; 16], count: u64, percentile: u64) -> u64 {
    if count == 0 {
        return 0;
    }
    let rank = (count.saturating_mul(percentile).saturating_add(99)) / 100;
    let mut seen: u64 = 0;
    for (index, bucket_count) in buckets.iter().enumerate() {
        seen = seen.saturating_add(*bucket_count);
        if seen >= rank {
            return LATENCY_BUCKETS_MS[index];
        }
    }
    u64::MAX
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn snapshots_expose_bounded_percentiles_and_counts() {
        let metrics = ReActMetrics::new();
        metrics.phases[Phase::LlmStream.index()].record(Duration::from_millis(3));
        metrics.phases[Phase::LlmStream.index()].record(Duration::from_millis(1_200));
        metrics.increment(Counter::StreamChunks);
        metrics.increment(Counter::ChunkDrops);
        metrics.observe(Phase::SqliteLockWait, Duration::from_millis(7));
        metrics.set_context_queue_items(12);

        let snapshot = metrics.snapshot();
        let stream = snapshot.phase(Phase::LlmStream);
        assert_eq!(stream.count, 2);
        assert_eq!(stream.total_ms, 1_203);
        assert_eq!(stream.p50_ms, 5);
        assert_eq!(stream.p95_ms, 2_500);
        assert_eq!(snapshot.counters.stream_chunks, 1);
        assert_eq!(snapshot.counters.chunk_drops, 1);
        assert_eq!(snapshot.phase(Phase::SqliteLockWait).count, 1);
        assert_eq!(snapshot.gauges.context_queue_items, 12);
        let exported = serde_json::to_value(snapshot).expect("metrics snapshot is exportable");
        assert_eq!(exported["gauges"]["context_queue_items"], 12);
    }

    #[test]
    fn atomic_updates_are_safe_under_concurrent_recording() {
        let metrics = std::sync::Arc::new(ReActMetrics::new());
        let workers = (0..4)
            .map(|_| {
                let metrics = metrics.clone();
                thread::spawn(move || {
                    for _ in 0..100 {
                        metrics.increment(Counter::TurnStarts);
                        metrics.phases[Phase::TokenEstimate.index()]
                            .record(Duration::from_millis(1));
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("metrics worker should finish");
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.counters.turn_starts, 400);
        assert_eq!(snapshot.phase(Phase::TokenEstimate).count, 400);
    }

    #[test]
    fn phase_timer_preserves_correlation_without_recording_content() {
        let metrics = ReActMetrics::new();
        {
            let _timer = metrics.start(Phase::Projection, "ses-test", 7, 3);
        }
        assert_eq!(metrics.snapshot().phase(Phase::Projection).count, 1);
    }
}
