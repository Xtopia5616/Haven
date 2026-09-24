//! Agent-owned LLM usage persistence and per-session cumulative totals.
//!
//! A session's operation queue preserves the former actor-mailbox ordering for
//! record/reset/invalidate calls. The async gate stays held across the full
//! seed, accumulation and persistence interval, including blocking DB waits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use haven_common::config::RequestKind;
use haven_common::types::{CacheAccounting, LlmCallKind};
use haven_memory::{Database, LlmCallUsageInput, SessionStore};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const USAGE_OPERATION_CAPACITY: usize = 128;

/// Provider-neutral input for one Agent-owned model call.
#[derive(Debug, Clone)]
pub(crate) struct UsageUpdate {
    pub call_kind: LlmCallKind,
    pub request: RequestKind,
    pub model: Option<String>,
    pub step_number: i32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_miss_tokens: u32,
    pub cache_accounting: CacheAccounting,
    pub cache_diagnostics: Option<String>,
    pub cost_usd: f64,
    pub has_cost: bool,
    pub duration_ms: Option<u64>,
    pub context_tokens: u32,
    pub context_window: Option<u32>,
    pub cancel: Option<CancellationToken>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CumulativeUsage {
    pub(crate) prompt_tokens: u32,
    pub(crate) completion_tokens: u32,
    pub(crate) total_tokens: u32,
    pub(crate) cached_tokens: u32,
    pub(crate) cache_creation_tokens: u32,
    pub(crate) cache_miss_tokens: u32,
    pub(crate) cost_usd: f64,
    pub(crate) has_cost: bool,
}

impl From<haven_memory::repositories::usage::SessionUsage> for CumulativeUsage {
    fn from(usage: haven_memory::repositories::usage::SessionUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_tokens: usage.cached_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            cache_miss_tokens: usage.cache_miss_tokens,
            cost_usd: usage.cost_usd,
            has_cost: usage.has_cost,
        }
    }
}

/// Cumulative totals after a usage accumulation pass.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CumulativeTotals {
    pub(crate) prompt_tokens: u32,
    pub(crate) completion_tokens: u32,
    pub(crate) total_tokens: u32,
    pub(crate) cached_tokens: u32,
    pub(crate) cache_creation_tokens: u32,
    pub(crate) cache_miss_tokens: u32,
    pub(crate) cost_usd: Option<f64>,
}

/// Process-local cumulative counters and rollback epochs.
pub(crate) struct UsageTracker {
    map: StdMutex<HashMap<String, CumulativeUsage>>,
    /// Shared with blocking persistence closures so rollback can invalidate a
    /// write even if its async caller has already returned.
    epochs: Arc<StdMutex<HashMap<String, u64>>>,
}

impl UsageTracker {
    fn new() -> Self {
        Self {
            map: StdMutex::new(HashMap::new()),
            epochs: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    fn epoch(&self, session_id: &str) -> u64 {
        self.epochs
            .lock()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    fn epochs_handle(&self) -> Arc<StdMutex<HashMap<String, u64>>> {
        Arc::clone(&self.epochs)
    }

    fn needs_seed(&self, session_id: &str) -> bool {
        !self.map.lock().unwrap().contains_key(session_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_with_seed<F>(
        &self,
        session_id: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
        cache_creation_tokens: u32,
        cache_miss_tokens: u32,
        step_cost: Option<f64>,
        seed: F,
    ) -> CumulativeTotals
    where
        F: FnOnce() -> CumulativeUsage,
    {
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(session_id.to_string()).or_insert_with(seed);
        entry.prompt_tokens = entry.prompt_tokens.saturating_add(prompt_tokens);
        entry.completion_tokens = entry.completion_tokens.saturating_add(completion_tokens);
        entry.total_tokens = entry.total_tokens.saturating_add(total_tokens);
        entry.cached_tokens = entry.cached_tokens.saturating_add(cached_tokens);
        entry.cache_creation_tokens = entry
            .cache_creation_tokens
            .saturating_add(cache_creation_tokens);
        entry.cache_miss_tokens = entry.cache_miss_tokens.saturating_add(cache_miss_tokens);
        if let Some(cost) = step_cost {
            entry.cost_usd += cost;
            entry.has_cost = true;
        }
        CumulativeTotals {
            prompt_tokens: entry.prompt_tokens,
            completion_tokens: entry.completion_tokens,
            total_tokens: entry.total_tokens,
            cached_tokens: entry.cached_tokens,
            cache_creation_tokens: entry.cache_creation_tokens,
            cache_miss_tokens: entry.cache_miss_tokens,
            cost_usd: entry.has_cost.then_some(entry.cost_usd),
        }
    }

    fn reset(&self, session_id: &str) {
        self.map.lock().unwrap().remove(session_id);
    }

    fn invalidate_after_truncate(&self, session_id: &str) {
        self.map.lock().unwrap().remove(session_id);
        let mut epochs = self.epochs.lock().unwrap();
        let next = epochs
            .get(session_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        epochs.insert(session_id.to_string(), next);
    }
}

struct SessionUsageState {
    tracker: UsageTracker,
    /// Spans every await in one usage operation. Do not rely on the tracker's
    /// short-lived std mutexes for cross-await serialization.
    operation_gate: Mutex<()>,
}

#[derive(Clone)]
struct SessionUsageHandle {
    tx: mpsc::Sender<UsageOperation>,
    // Retain state alongside the sender. Entries are intentionally not
    // removed while detached blocking workers may still observe its epoch map.
    _state: Arc<SessionUsageState>,
}

enum UsageOperation {
    Record {
        update: UsageUpdate,
        reply: oneshot::Sender<anyhow::Result<CumulativeTotals>>,
    },
    Reset,
    Invalidate,
}

/// Owns the complete Agent usage path independently of the session actor.
/// Per-session FIFO queues preserve call/reset/truncate ordering while the
/// async operation gate protects each complete seed-to-persist interval.
pub(crate) struct UsageRuntime {
    db: Arc<Database>,
    store: SessionStore,
    sessions: StdMutex<HashMap<String, SessionUsageHandle>>,
}

impl UsageRuntime {
    pub(crate) fn new(db: Arc<Database>, store: SessionStore) -> Self {
        Self {
            db,
            store,
            sessions: StdMutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn record(
        &self,
        session_id: &str,
        update: UsageUpdate,
    ) -> anyhow::Result<CumulativeTotals> {
        let session = self.session(session_id)?;
        let (reply, rx) = oneshot::channel();
        session
            .tx
            .send(UsageOperation::Record { update, reply })
            .await
            .map_err(|_| anyhow::anyhow!("usage runtime for session '{session_id}' stopped"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("usage runtime dropped session '{session_id}' result"))?
    }

    /// Persist tool-owned usage outside the Agent cumulative FIFO. The batch
    /// remains one SessionStore transaction and uses the caller's original
    /// cancellable blocking path.
    pub(crate) async fn append_tool_usage_batch(
        &self,
        session_id: &str,
        inputs: Vec<LlmCallUsageInput>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }

        let store = self.store.clone();
        let session_id = session_id.to_string();
        let persist =
            move |_db: &Database| store.append_usage_batch(&session_id, &inputs).map(|_| ());
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
    }

    /// Queue a reset in the same FIFO as record operations. This remains
    /// synchronous for existing lifecycle call sites, matching actor try_send.
    pub(crate) fn reset(&self, session_id: &str) {
        self.enqueue_control(session_id, UsageOperation::Reset);
    }

    /// Queue an epoch invalidation after the rollback transaction has rebuilt
    /// durable usage. It shares the record FIFO and the same async gate.
    pub(crate) fn invalidate_after_truncate(&self, session_id: &str) {
        self.enqueue_control(session_id, UsageOperation::Invalidate);
    }

    fn enqueue_control(&self, session_id: &str, operation: UsageOperation) {
        let session = self.sessions.lock().unwrap().get(session_id).cloned();
        let Some(session) = session else { return };
        // Preserve the prior fire-and-forget bounded-mailbox behavior if the
        // queue is full or the runtime has shut down.
        let _ = session.tx.try_send(operation);
    }

    fn session(&self, session_id: &str) -> anyhow::Result<SessionUsageHandle> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(session_id) {
            return Ok(session.clone());
        }

        let state = Arc::new(SessionUsageState {
            tracker: UsageTracker::new(),
            operation_gate: Mutex::new(()),
        });
        let (tx, rx) = mpsc::channel(USAGE_OPERATION_CAPACITY);
        let session = SessionUsageHandle {
            tx,
            _state: Arc::clone(&state),
        };
        let db = Arc::clone(&self.db);
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let worker_session_id = session_id.clone();
        runtime.spawn(async move {
            run_session_usage_operations(rx, db, store, worker_session_id, state).await;
        });
        sessions.insert(session_id, session.clone());
        Ok(session)
    }
}

async fn run_session_usage_operations(
    mut rx: mpsc::Receiver<UsageOperation>,
    db: Arc<Database>,
    store: SessionStore,
    session_id: String,
    state: Arc<SessionUsageState>,
) {
    while let Some(operation) = rx.recv().await {
        let _gate = state.operation_gate.lock().await;
        match operation {
            UsageOperation::Record { update, reply } => {
                let result = record_usage(&db, &store, &session_id, &state.tracker, update).await;
                let _ = reply.send(result);
            }
            UsageOperation::Reset => state.tracker.reset(&session_id),
            UsageOperation::Invalidate => state.tracker.invalidate_after_truncate(&session_id),
        }
    }
}

async fn record_usage(
    db: &Arc<Database>,
    store: &SessionStore,
    session_id: &str,
    tracker: &UsageTracker,
    update: UsageUpdate,
) -> anyhow::Result<CumulativeTotals> {
    let UsageUpdate {
        call_kind,
        request,
        model,
        step_number,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        cache_creation_tokens,
        cache_miss_tokens,
        cache_accounting,
        cache_diagnostics,
        cost_usd,
        has_cost,
        duration_ms,
        context_tokens,
        context_window,
        cancel,
    } = update;
    anyhow::ensure!(
        call_kind == LlmCallKind::Agent,
        "Agent usage updates must use call_kind=agent"
    );
    let seed = if tracker.needs_seed(session_id) {
        let db = Arc::clone(db);
        let sid = session_id.to_string();
        let read = move |db: &Database| -> anyhow::Result<CumulativeUsage> {
            Ok(db
                .get_session_usage(&sid)?
                .map(CumulativeUsage::from)
                .unwrap_or_default())
        };
        match cancel.clone() {
            Some(cancel) => db.run_blocking_cancellable(cancel, read).await?,
            None => db.run_blocking(read).await?,
        }
    } else {
        CumulativeUsage::default()
    };

    let totals = tracker.record_with_seed(
        session_id,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        cache_creation_tokens,
        cache_miss_tokens,
        if has_cost { Some(cost_usd) } else { None },
        || seed,
    );

    let persist_epoch = tracker.epoch(session_id);
    let epochs = tracker.epochs_handle();
    let persist_session_id = session_id.to_string();
    let usage_input = LlmCallUsageInput {
        step_number: Some(step_number),
        request_kind: request,
        call_kind,
        model,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        cache_creation_tokens,
        cache_miss_tokens,
        cache_accounting,
        cache_diagnostics,
        cost_usd,
        has_cost,
        duration_ms,
        context_tokens,
        context_window,
    };
    let store = store.clone();
    let persist = move |_db: &Database| -> anyhow::Result<()> {
        let epoch_now = || {
            epochs
                .lock()
                .unwrap()
                .get(&persist_session_id)
                .copied()
                .unwrap_or(0)
        };
        if epoch_now() != persist_epoch {
            return Ok(());
        }
        let record = store.append_usage(&persist_session_id, &usage_input)?;
        if epoch_now() != persist_epoch {
            store.discard_usage(&persist_session_id, &record.id)?;
        }
        Ok(())
    };
    match cancel {
        Some(cancel) => db.run_blocking_cancellable(cancel, persist).await?,
        None => db.run_blocking(persist).await?,
    }
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_usage_input(
        call_kind: LlmCallKind,
        model: &str,
        prompt_tokens: u32,
    ) -> LlmCallUsageInput {
        LlmCallUsageInput {
            step_number: Some(3),
            request_kind: RequestKind::Chat,
            call_kind,
            model: Some(model.into()),
            prompt_tokens,
            completion_tokens: 2,
            total_tokens: prompt_tokens + 2,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cache_miss_tokens: prompt_tokens,
            cache_accounting: CacheAccounting::Inclusive,
            cache_diagnostics: None,
            cost_usd: 0.01,
            has_cost: true,
            duration_ms: Some(12),
            context_tokens: prompt_tokens,
            context_window: Some(4096),
        }
    }

    fn update(prompt_tokens: u32) -> UsageUpdate {
        UsageUpdate {
            call_kind: LlmCallKind::Agent,
            request: RequestKind::Chat,
            model: Some("test-model".into()),
            step_number: 1,
            prompt_tokens,
            completion_tokens: 0,
            total_tokens: prompt_tokens,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cache_miss_tokens: prompt_tokens,
            cache_accounting: CacheAccounting::Inclusive,
            cache_diagnostics: None,
            cost_usd: 0.0,
            has_cost: false,
            duration_ms: None,
            context_tokens: prompt_tokens,
            context_window: Some(4096),
            cancel: None,
        }
    }

    #[tokio::test]
    async fn concurrent_calls_for_one_session_accumulate_without_lost_updates() {
        let directory = tempfile::tempdir().expect("temporary DB directory");
        let db = Arc::new(Database::open(&directory.path().join("usage-concurrent.db")).unwrap());
        let session = db.create_session("concurrent usage").unwrap();
        let runtime = Arc::new(UsageRuntime::new(
            Arc::clone(&db),
            SessionStore::new(Arc::clone(&db)),
        ));

        let mut tasks = Vec::new();
        for _ in 0..32 {
            let runtime = Arc::clone(&runtime);
            let session_id = session.id.clone();
            tasks.push(tokio::spawn(async move {
                runtime.record(&session_id, update(1)).await.unwrap()
            }));
        }
        let mut max_prompt = 0;
        for task in tasks {
            max_prompt = max_prompt.max(task.await.unwrap().prompt_tokens);
        }

        assert_eq!(max_prompt, 32);
        assert_eq!(
            db.get_session_usage(&session.id)
                .unwrap()
                .unwrap()
                .prompt_tokens,
            32
        );
        assert_eq!(db.get_session_llm_usage(&session.id).unwrap().len(), 32);
    }

    #[tokio::test]
    async fn agent_usage_runtime_rejects_non_agent_call_kind() {
        let directory = tempfile::tempdir().expect("temporary DB directory");
        let db = Arc::new(Database::open(&directory.path().join("usage-kind.db")).unwrap());
        let session = db.create_session("usage kind").unwrap();
        let runtime = UsageRuntime::new(Arc::clone(&db), SessionStore::new(Arc::clone(&db)));

        let mut update = update(1);
        update.call_kind = LlmCallKind::Media;
        assert!(runtime.record(&session.id, update).await.is_err());
        assert!(db.get_session_llm_usage(&session.id).unwrap().is_empty());
        assert!(db.get_session_usage(&session.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn reset_and_invalidate_are_ordered_with_record_operations() {
        let directory = tempfile::tempdir().expect("temporary DB directory");
        let db = Arc::new(Database::open(&directory.path().join("usage-order.db")).unwrap());
        let session = db.create_session("usage ordering").unwrap();
        let store = SessionStore::new(Arc::clone(&db));
        let runtime = UsageRuntime::new(Arc::clone(&db), store.clone());

        runtime.record(&session.id, update(3)).await.unwrap();
        store
            .append_usage(
                &session.id,
                &LlmCallUsageInput {
                    step_number: Some(2),
                    request_kind: RequestKind::Chat,
                    call_kind: LlmCallKind::Agent,
                    model: Some("external-seed".into()),
                    prompt_tokens: 40,
                    completion_tokens: 0,
                    total_tokens: 40,
                    cached_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_miss_tokens: 40,
                    cache_accounting: CacheAccounting::Inclusive,
                    cache_diagnostics: None,
                    cost_usd: 0.0,
                    has_cost: false,
                    duration_ms: None,
                    context_tokens: 40,
                    context_window: Some(4096),
                },
            )
            .unwrap();

        runtime.reset(&session.id);
        let after_reset = runtime.record(&session.id, update(2)).await.unwrap();
        assert_eq!(after_reset.prompt_tokens, 45);

        db.delete_llm_usage_from(&session.id, "1970-01-01T00:00:00.000Z")
            .unwrap();
        db.rebuild_session_usage_from_calls(&session.id).unwrap();
        runtime.invalidate_after_truncate(&session.id);
        let after_invalidate = runtime.record(&session.id, update(7)).await.unwrap();
        assert_eq!(after_invalidate.prompt_tokens, 7);
        assert_eq!(after_invalidate.total_tokens, 7);

        // Keep the tracker and epoch registry alive for the runtime lifetime;
        // in particular, invalidation never removes/recreates session state.
        assert_eq!(runtime.sessions.lock().unwrap().len(), 1);
        let session_state = runtime.sessions.lock().unwrap()[&session.id]._state.clone();
        assert_eq!(session_state.tracker.epoch(&session.id), 1);
    }

    #[tokio::test]
    async fn tool_usage_batch_appends_events_and_projects_rows_in_order() {
        let directory = tempfile::tempdir().expect("temporary DB directory");
        let db = Arc::new(Database::open(&directory.path().join("tool-usage.db")).unwrap());
        let session = db.create_session("tool usage batch").unwrap();
        let store = SessionStore::new(Arc::clone(&db));
        let runtime = UsageRuntime::new(Arc::clone(&db), store.clone());

        runtime
            .append_tool_usage_batch(
                &session.id,
                vec![
                    tool_usage_input(LlmCallKind::Tool, "tool-model", 11),
                    tool_usage_input(LlmCallKind::Media, "media-model", 17),
                ],
                None,
            )
            .await
            .unwrap();

        let usage_events = store
            .read_all(&session.id)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == "usage_recorded")
            .map(|event| serde_json::from_str::<serde_json::Value>(&event.payload).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(usage_events.len(), 2);
        assert_eq!(usage_events[0]["model"].as_str(), Some("tool-model"));
        assert_eq!(usage_events[0]["call_kind"], "tool");
        assert_eq!(usage_events[1]["model"].as_str(), Some("media-model"));
        assert_eq!(usage_events[1]["call_kind"], "media");

        let projected = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(projected.len(), 2);
        assert!(projected.iter().any(|row| {
            row.model.as_deref() == Some("tool-model")
                && row.call_kind == "tool"
                && row.prompt_tokens == 11
        }));
        assert!(projected.iter().any(|row| {
            row.model.as_deref() == Some("media-model")
                && row.call_kind == "media"
                && row.prompt_tokens == 17
        }));
    }

    #[tokio::test]
    async fn cancelled_tool_usage_batch_interrupts_blocking_store_write() {
        let directory = tempfile::tempdir().expect("temporary DB directory");
        let db = Arc::new(Database::open(&directory.path().join("tool-usage-cancel.db")).unwrap());
        let session = db.create_session("cancel tool usage batch").unwrap();
        let store = SessionStore::new(Arc::clone(&db));
        let runtime = Arc::new(UsageRuntime::new(Arc::clone(&db), store.clone()));

        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let lock_db = Arc::clone(&db);
        let lock_holder = std::thread::spawn(move || {
            let conn = lock_db.conn();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            conn.execute_batch("ROLLBACK").unwrap();
        });
        locked_rx.recv().unwrap();

        let cancel = CancellationToken::new();
        let task_runtime = Arc::clone(&runtime);
        let task_session_id = session.id.clone();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            task_runtime
                .append_tool_usage_batch(
                    &task_session_id,
                    vec![tool_usage_input(LlmCallKind::Tool, "cancelled-model", 5)],
                    Some(task_cancel),
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.cancel();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), task).await;
        release_tx.send(()).unwrap();
        lock_holder.join().unwrap();
        assert!(
            result
                .expect("cancellation must stop the blocking write promptly")
                .unwrap()
                .is_err()
        );
        assert!(
            store
                .read_all(&session.id)
                .unwrap()
                .iter()
                .all(|event| event.event_type != "usage_recorded")
        );
        assert!(db.get_session_llm_usage(&session.id).unwrap().is_empty());
    }

    #[test]
    fn tracker_invalidation_clears_totals_and_increments_epoch() {
        let tracker = UsageTracker::new();
        assert_eq!(tracker.epoch("ses-tracker-test"), 0);
        tracker.record_with_seed(
            "ses-tracker-test",
            10,
            5,
            15,
            0,
            0,
            10,
            None,
            CumulativeUsage::default,
        );
        tracker.invalidate_after_truncate("ses-tracker-test");
        assert_eq!(tracker.epoch("ses-tracker-test"), 1);
        let totals = tracker.record_with_seed(
            "ses-tracker-test",
            1,
            1,
            2,
            0,
            0,
            1,
            None,
            CumulativeUsage::default,
        );
        assert_eq!(totals.total_tokens, 2);
    }
}
