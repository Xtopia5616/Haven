use crate::cache::{CacheGeneration, QueryResultCache};
use rusqlite::{Connection, InterruptHandle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

thread_local! {
    /// Interrupt handles for SQLite connections checked out by the current
    /// cancellable blocking operation. SQLite exposes interruption at the
    /// connection boundary, so the registry is scoped to the blocking worker
    /// rather than stored on `Database` globally.
    static ACTIVE_DB_INTERRUPTS: RefCell<Option<Arc<DbCancellation>>>
        = const { RefCell::new(None) };
}

struct DbCancellation {
    cancel: CancellationToken,
    interrupts: Mutex<Vec<InterruptHandle>>,
}

fn check_cancellation() -> anyhow::Result<()> {
    ACTIVE_DB_INTERRUPTS.with(|active| {
        if let Some(active) = active.borrow().as_ref() {
            anyhow::ensure!(!active.cancel.is_cancelled(), "SQLite operation cancelled");
        }
        Ok(())
    })
}

// Restore the worker-local scope even when a repository panics.
struct CancellationScope(Option<Arc<DbCancellation>>);

impl Drop for CancellationScope {
    fn drop(&mut self) {
        ACTIVE_DB_INTERRUPTS.with(|active| active.replace(self.0.take()));
    }
}

fn register_interrupt_handle(conn: &Connection) {
    ACTIVE_DB_INTERRUPTS.with(|active| {
        let Some(active) = active.borrow().as_ref().cloned() else {
            return;
        };
        // SQLite's default busy handler sleeps in native code. Keep that
        // sleep bounded for cancellable work so a cancellation can be
        // observed even on builds where sqlite3_interrupt does not wake a
        // busy-handler sleep immediately.
        let _ = conn.busy_timeout(Duration::from_millis(100));
        // An interrupt only affects currently running SQLite calls. A progress
        // handler also rejects later statements (including cached statements,
        // BEGIN and COMMIT) after cancellation, even if no call was running
        // when the interrupt was issued. Check every VM instruction so short
        // writes cannot slip through between cancellation and execution.
        let cancel = active.cancel.clone();
        conn.progress_handler(1, Some(move || cancel.is_cancelled()));
        if let Ok(mut handles) = active.interrupts.lock() {
            handles.push(conn.get_interrupt_handle());
        }
    });
}

fn unregister_interrupt_handle() {
    ACTIVE_DB_INTERRUPTS.with(|active| {
        if let Some(active) = active.borrow().as_ref().cloned()
            && let Ok(mut handles) = active.interrupts.lock()
        {
            // DB repository methods check out one connection at a time on a
            // blocking worker, so the registry behaves as a small stack.
            // Removing the handle on drop prevents a later cancellation from
            // interrupting a connection that has already been returned to
            // the pool and reused by another operation.
            handles.pop();
        }
    });
}

/// Pooled SQLite connections for a file-backed database (WAL mode: one
/// writer + many readers can proceed concurrently). Sized comfortably above
/// the session concurrency ceiling plus the direct (non-`run_blocking`) DB
/// callers (tools, Tauri commands), so a busy step rarely blocks on
/// checkout while other connections are still doing real work.
const FILE_POOL_MAX_CONNECTIONS: usize = 16;

/// A tiny bounded pool of rusqlite connections. The previous design wrapped a
/// SINGLE connection in a `Mutex`, which serialized every DB access across all
/// concurrent sessions (and background tool_runs / title generation / memory
/// maintenance) on one global lock. SQLite in WAL mode supports one writer
/// plus several concurrent readers, so a small pool lets parallel sessions' reads
/// (message history, step lists, snapshots) run side-by-side instead of
/// queueing on the mutex.
///
/// `PooledConnection` hands a checked-out connection back to the pool on drop;
/// `get()` blocks (condvar) when the pool is exhausted, which is fine because
/// `Database::run_blocking` already moves DB work off the async runtime, and
/// per-session DB use is short-lived.
struct ConnectionPool {
    state: Mutex<PoolState>,
    cv: Condvar,
    max: usize,
    opener: Box<dyn Fn() -> anyhow::Result<Connection> + Send + Sync>,
}

struct PoolState {
    idle: Vec<Connection>,
    active: usize,
}

impl ConnectionPool {
    /// Build a pool with an already-open connection (the bootstrap connection
    /// that ran migrations). Required for in-memory databases: a shared-cache
    /// `mode=memory` database is destroyed when its LAST connection closes,
    /// so dropping the bootstrap before the pool opened its own would delete
    /// the migrated schema. For file databases the bootstrap is simply the
    /// first pooled connection.
    fn with_initial(
        max: usize,
        opener: Box<dyn Fn() -> anyhow::Result<Connection> + Send + Sync>,
        initial: Option<Connection>,
    ) -> Self {
        let mut idle = Vec::new();
        if let Some(conn) = initial {
            idle.push(conn);
        }
        Self {
            state: Mutex::new(PoolState { idle, active: 0 }),
            cv: Condvar::new(),
            max,
            opener,
        }
    }

    fn get(&self) -> anyhow::Result<PooledConnection<'_>> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("database pool lock poisoned"))?;
        loop {
            check_cancellation()?;
            if let Some(conn) = st.idle.pop() {
                st.active += 1;
                drop(st);
                let pooled = PooledConnection {
                    pool: self,
                    conn: Some(conn),
                };
                register_interrupt_handle(&pooled);
                check_cancellation()?;
                return Ok(pooled);
            }
            if st.active < self.max {
                st.active += 1;
                drop(st);
                let conn = match (self.opener)() {
                    Ok(c) => c,
                    Err(e) => {
                        let mut st = self
                            .state
                            .lock()
                            .map_err(|_| anyhow::anyhow!("database pool lock poisoned"))?;
                        st.active -= 1;
                        self.cv.notify_one();
                        return Err(e);
                    }
                };
                let pooled = PooledConnection {
                    pool: self,
                    conn: Some(conn),
                };
                register_interrupt_handle(&pooled);
                check_cancellation()?;
                return Ok(pooled);
            }
            // All connections are checked out. Poll cancellable waits so they
            // can exit without a connection being returned. Direct callers on
            // runtime threads retain the 30s warning for an exhausted pool.
            let cancellable = ACTIVE_DB_INTERRUPTS.with(|active| active.borrow().is_some());
            let wait = if cancellable {
                Duration::from_millis(25)
            } else {
                Duration::from_secs(30)
            };
            let (new_st, timeout) = self
                .cv
                .wait_timeout(st, wait)
                .map_err(|_| anyhow::anyhow!("database pool lock poisoned"))?;
            st = new_st;
            if timeout.timed_out() && !cancellable {
                tracing::warn!(
                    "database pool exhausted ({} connections in use) for 30s; \
                     the waiting caller is stalled on a runtime thread",
                    self.max
                );
            }
        }
    }
}

/// A checked-out pool connection. Returns itself to the pool on drop so the
/// connection (and its prepared-statement caches) is reused, not reopened.
pub struct PooledConnection<'p> {
    pool: &'p ConnectionPool,
    conn: Option<Connection>,
}

impl Deref for PooledConnection<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("pooled connection already returned")
    }
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            unregister_interrupt_handle();
            // Cleanup must remain executable after cancellation, and pooled
            // connections must not retain another operation's token.
            conn.progress_handler(0, None::<fn() -> bool>);
            // Defensive rollback: a caller whose transaction failed midway
            // (e.g. `delete_all_sessions` propagates errors without ROLLBACK) would
            // otherwise re-pool a connection with a write transaction still
            // open — subsequent statements would run inside the abandoned
            // transaction and the held write lock would block the other
            // pooled connections. Fails silently when no transaction is open.
            let _ = conn.execute_batch("ROLLBACK");
            let _ = conn.busy_timeout(Duration::from_secs(30));
            match self.pool.state.lock() {
                Ok(mut st) => {
                    st.idle.push(conn);
                    st.active -= 1;
                    self.pool.cv.notify_one();
                }
                Err(poisoned) => {
                    tracing::error!(
                        "database pool lock poisoned while returning connection; recovering state"
                    );
                    let mut st = poisoned.into_inner();
                    st.idle.push(conn);
                    st.active = st.active.saturating_sub(1);
                    self.pool.cv.notify_one();
                }
            }
        }
    }
}

pub struct Database {
    pool: ConnectionPool,
    /// Bounded process-local query cache. The cache mechanics are isolated in
    /// [`crate::cache::QueryResultCache`]; the database facade only exposes
    /// repository-facing helpers and owns invalidation timing.
    cache: QueryResultCache,
    /// Monotonic in-process revision for all memory reads, including facts,
    /// episodes, and their embedding index. It is intentionally not persisted
    /// or part of the database schema; consumers use it only for cache keys.
    memory_revision: AtomicU64,
    /// The requested model for an in-flight embedding batch, keyed by the
    /// entity being embedded. Providers may return a canonical/aliased model
    /// name; persistence must retain the configured index identity instead.
    pending_embedding_models: Mutex<HashMap<(String, String), String>>,
    /// Serializes fact-graph mutations within one application process. SQLite
    /// still provides the durable transaction boundary, while this gate keeps
    /// read/decide/write graph policies from interleaving across pooled
    /// connections (for example, single-valued fact replacement).
    fact_write_gate: Mutex<()>,
}

impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_with_pool_size(path, FILE_POOL_MAX_CONNECTIONS)
    }

    #[cfg(test)]
    pub(crate) fn open_single_connection_for_test(path: &Path) -> anyhow::Result<Self> {
        Self::open_with_pool_size(path, 1)
    }

    fn open_with_pool_size(path: &Path, pool_size: usize) -> anyhow::Result<Self> {
        tracing::info!("opening database");
        // Bootstrap connection: set WAL and create the current schema exactly
        // once (every pooled connection later sees an already-initialized
        // database). The bootstrap is seeded into the pool and is its
        // most-reused connection, so it gets the same 30s busy timeout as the
        // opener connections.
        let bootstrap = Connection::open(path)?;
        bootstrap.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        bootstrap.busy_timeout(Duration::from_secs(30))?;
        crate::schema::init_schema(&bootstrap)?;
        let path = path.to_path_buf();
        let pool = ConnectionPool::with_initial(
            pool_size,
            Box::new(move || {
                let conn = Connection::open(&path)?;
                conn.busy_timeout(Duration::from_secs(30))?;
                conn.execute_batch("PRAGMA foreign_keys=ON;")?;
                Ok(conn)
            }),
            Some(bootstrap),
        );
        Ok(Self {
            pool,
            cache: QueryResultCache::new(),
            memory_revision: AtomicU64::new(0),
            pending_embedding_models: Mutex::new(HashMap::new()),
            fact_write_gate: Mutex::new(()),
        })
    }

    /// Open an isolated shared-cache in-memory database for tests. Its pool is
    /// limited to one connection because SQLite serializes writers in this
    /// mode. This constructor is available only to this crate's unit tests or
    /// downstream `test-support` builds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        tracing::debug!("opening in-memory database");
        // Shared-cache URI so every pooled connection sees the SAME in-memory
        // database (a plain `:memory:` would give each connection its own
        // empty DB — schema setup on the bootstrap connection would be
        // invisible to the pooled one). The unique name keeps parallel tests
        // from sharing a database.
        let uri = format!(
            "file:hvnmem-{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
        let bootstrap = Connection::open_with_flags(&uri, flags)?;
        bootstrap.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        bootstrap.busy_timeout(Duration::from_secs(30))?;
        crate::schema::init_schema(&bootstrap)?;
        // In-memory shared-cache databases use a single-writer locking model
        // where a second writer fails with SQLITE_LOCKED (not BUSY, which
        // busy_timeout handles), so cap the pool at one connection — tests get
        // the same serialized semantics as the pre-pool era, while the
        // file-backed path (production) gets real read concurrency.
        let uri2 = uri.clone();
        let pool = ConnectionPool::with_initial(
            1,
            Box::new(move || {
                let conn = Connection::open_with_flags(&uri2, flags)?;
                conn.busy_timeout(Duration::from_secs(30))?;
                conn.execute_batch("PRAGMA foreign_keys=ON;")?;
                Ok(conn)
            }),
            Some(bootstrap),
        );
        Ok(Self {
            pool,
            cache: QueryResultCache::new(),
            memory_revision: AtomicU64::new(0),
            pending_embedding_models: Mutex::new(HashMap::new()),
            fact_write_gate: Mutex::new(()),
        })
    }

    #[cfg(feature = "test-support")]
    pub fn conn(&self) -> PooledConnection<'_> {
        self.checkout_connection()
    }

    #[cfg(all(test, not(feature = "test-support")))]
    pub(crate) fn conn(&self) -> PooledConnection<'_> {
        self.checkout_connection()
    }

    #[cfg(any(test, feature = "test-support"))]
    fn checkout_connection(&self) -> PooledConnection<'_> {
        self.try_conn()
            .expect("database connection checkout failed")
    }

    pub(crate) fn try_conn(&self) -> anyhow::Result<PooledConnection<'_>> {
        self.pool.get()
    }

    /// Run one fact-graph mutation while excluding other fact mutations issued
    /// through this database handle. The closure stays synchronous so callers
    /// can compose several SQL statements into one transaction when needed.
    pub(crate) fn with_fact_write<T>(
        &self,
        f: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let _guard = self
            .fact_write_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("fact write gate poisoned"))?;
        check_cancellation()?;
        f()
    }

    /// Run a blocking DB closure on the tokio blocking thread pool. Keeps
    /// synchronous SQLite work (including WAL fsyncs) off the async runtime
    /// so a slow write cannot stall unrelated async sessions that don't touch
    /// the DB. The closure borrows `&Database`; owned arguments must be
    /// cloned into the closure by the caller (it is `'static`).
    pub(crate) async fn run_blocking<T, F>(self: &Arc<Self>, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> anyhow::Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || f(&db)).await?
    }

    /// Run a raw database closure on a blocking worker for cross-crate test
    /// fixtures. Production code must use a typed Memory store instead.
    #[cfg(feature = "test-support")]
    pub async fn run_blocking_for_test<T, F>(self: &Arc<Self>, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> anyhow::Result<T> + Send + 'static,
    {
        self.run_blocking(f).await
    }

    /// Run a blocking SQLite closure with cooperative cancellation that also
    /// interrupts SQLite calls already blocked on a checked-out connection.
    ///
    /// Dropping a future cannot stop `spawn_blocking`; registering the
    /// connection interrupt handles closes that gap for busy reads/writes.
    /// Cancellation is checked before starting the closure, while waiting for
    /// a connection, after checkout, and by SQLite's progress handler for every
    /// subsequent statement. Repository checkout errors propagate normally.
    /// The bounded join keeps a broken pool/extension from holding the async
    /// caller forever. In that exceptional case the worker remains detached,
    /// but the caller receives an explicit failure instead of a false success.
    pub(crate) async fn run_blocking_cancellable<T, F>(
        self: &Arc<Self>,
        cancel: CancellationToken,
        f: F,
    ) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> anyhow::Result<T> + Send + 'static,
    {
        let db = self.clone();
        anyhow::ensure!(!cancel.is_cancelled(), "SQLite operation cancelled");
        let interrupts = Arc::new(DbCancellation {
            cancel: cancel.clone(),
            interrupts: Mutex::new(Vec::new()),
        });
        let worker_interrupts = interrupts.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let _scope = CancellationScope(
                ACTIVE_DB_INTERRUPTS.with(|active| active.replace(Some(worker_interrupts))),
            );
            check_cancellation()?;
            f(&db)
        });

        tokio::select! {
            result = &mut task => result?,
            _ = cancel.cancelled() => {
                if let Ok(handles) = interrupts.interrupts.lock() {
                    for handle in handles.iter() {
                        handle.interrupt();
                    }
                }
                match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
                    Ok(result) => result?,
                    Err(_) => anyhow::bail!(
                        "cancellable SQLite operation did not stop after cancellation"
                    ),
                }
            }
        }
    }

    pub(crate) fn cache_get_messages(
        &self,
        session_id: &str,
    ) -> Option<Vec<crate::repositories::messages::Message>> {
        self.cache.get_messages(session_id)
    }

    /// Returns the current cache generation for a key. Callers capture this
    /// before querying the DB and pass it to the corresponding `cache_put_*`
    /// to guard against stale-overwrite after a concurrent invalidation.
    pub(crate) fn cache_generation(&self, key: &str) -> CacheGeneration {
        self.cache.generation(key)
    }

    pub(crate) fn cache_put_messages(
        &self,
        session_id: &str,
        data: Vec<crate::repositories::messages::Message>,
        ttl_secs: u64,
        expected_gen: CacheGeneration,
    ) {
        self.cache
            .put_messages(session_id, data, ttl_secs, expected_gen);
    }

    pub(crate) fn cache_get_session_history_page(
        &self,
    ) -> Option<Vec<crate::repositories::sessions::Session>> {
        self.cache.get_session_history_page()
    }

    pub(crate) fn cache_put_session_history_page(
        &self,
        data: Vec<crate::repositories::sessions::Session>,
        ttl_secs: u64,
        expected_gen: CacheGeneration,
    ) {
        self.cache
            .put_session_history_page(data, ttl_secs, expected_gen);
    }

    pub(crate) fn cache_get_facts(
        &self,
        subject: &str,
    ) -> Option<Vec<crate::repositories::facts::Fact>> {
        self.cache.get_facts(subject)
    }

    pub(crate) fn cache_put_facts(
        &self,
        subject: &str,
        data: Vec<crate::repositories::facts::Fact>,
        ttl_secs: u64,
        expected_gen: CacheGeneration,
    ) {
        self.cache.put_facts(subject, data, ttl_secs, expected_gen);
    }

    pub(crate) fn cache_invalidate_messages(&self, session_id: &str) {
        self.cache.invalidate_messages(session_id);
    }

    /// Invalidate every per-session message cache. Bulk session deletion and
    /// retention cannot efficiently enumerate all cached session keys, so a
    /// global sweep is the only way to prevent deleted transcripts from being
    /// served by the read cache.
    pub(crate) fn cache_invalidate_all_messages(&self) {
        self.cache.invalidate_all_messages();
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn cache_invalidate_session_history_page(&self) {
        self.cache.invalidate_session_history_page();
    }

    /// Fixture-only escape hatch for tests that alter the sessions table with
    /// raw SQL. Production consumers must go through `SessionStore` writes.
    #[cfg(feature = "test-support")]
    pub fn cache_invalidate_session_history_page(&self) {
        self.cache.invalidate_session_history_page();
    }

    /// Cached copy of the full facts table (`list_facts`), keyed separately
    /// from the per-subject cache. Invalidated together with the subject
    /// cache on any fact mutation, so the global list cannot drift from the
    /// subject views.
    pub(crate) fn cache_get_facts_all(&self) -> Option<Vec<crate::repositories::facts::Fact>> {
        self.cache.get_facts_all()
    }

    pub(crate) fn cache_put_facts_all(
        &self,
        data: Vec<crate::repositories::facts::Fact>,
        ttl_secs: u64,
        expected_gen: CacheGeneration,
    ) {
        self.cache.put_facts_all(data, ttl_secs, expected_gen);
    }

    pub(crate) fn cache_invalidate_facts(&self, subject: &str) {
        self.bump_memory_revision();
        self.cache.invalidate_facts(subject);
    }

    /// Bump every facts cache entry (subject views + the all-facts result). Used by
    /// bulk maintenance that may touch arbitrary subjects (P1-6).
    pub(crate) fn cache_invalidate_all_facts(&self) {
        self.bump_memory_revision();
        self.cache.invalidate_all_facts();
    }

    /// Cached copy of one memory domain's embedding list (`list_embeddings`).
    /// Keyed by entity_type so vector recall skips the full-table read + blob
    /// decode on every query; invalidated on any embedding write.
    pub(crate) fn cache_get_embeddings(
        &self,
        entity_type: &str,
    ) -> Option<Vec<crate::embeddings::EmbeddedText>> {
        self.cache.get_embeddings(entity_type)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn cache_put_embeddings(
        &self,
        entity_type: &str,
        data: Vec<crate::embeddings::EmbeddedText>,
        ttl_secs: u64,
        expected_gen: CacheGeneration,
    ) {
        self.cache
            .put_embeddings(entity_type, data, ttl_secs, expected_gen);
    }

    /// Invalidate one domain's embeddings list cache. Called from the fact
    /// UPDATE/DELETE paths (the facts_embed_del/upd triggers delete embedding
    /// rows directly in SQL) and from every embedding write — without this
    /// the cached list would keep serving removed/stale vectors (with old
    /// surface text) for the whole TTL. Deliberately NOT called on plain fact
    /// INSERTs — those fire no trigger and leave the embedding rows
    /// untouched, so invalidating would only thrash the cache.
    pub(crate) fn cache_invalidate_embeddings(&self, entity_type: &str) {
        self.bump_memory_revision();
        self.cache.invalidate_embeddings(entity_type);
    }

    /// Invalidate all memory-derived caches after a session deletion or
    /// retention pass. Episodes are owned by sessions, so deleting a session
    /// changes the readable memory set even though no fact row changed.
    pub(crate) fn cache_invalidate_memory(&self) {
        self.bump_memory_revision();
        self.cache.invalidate_memory();
    }

    /// Current process-local revision of memory-readable state. Any fact,
    /// episode, or embedding mutation advances it, allowing prompt recall
    /// caches to remain valid across unrelated dirty notifications.
    pub(crate) fn memory_revision(&self) -> u64 {
        self.memory_revision.load(Ordering::Acquire)
    }

    fn bump_memory_revision(&self) {
        self.memory_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn register_pending_embedding_model(
        &self,
        entity_type: &str,
        entity_id: &str,
        model: &str,
    ) {
        if model.is_empty() {
            return;
        }
        if let Ok(mut pending) = self.pending_embedding_models.lock() {
            pending.insert(
                (entity_type.to_string(), entity_id.to_string()),
                model.to_string(),
            );
        }
    }

    pub(crate) fn pending_embedding_model(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Option<String> {
        self.pending_embedding_models
            .lock()
            .ok()
            .and_then(|pending| {
                pending
                    .get(&(entity_type.to_string(), entity_id.to_string()))
                    .cloned()
            })
    }

    pub(crate) fn clear_pending_embedding_model(&self, entity_type: &str, entity_id: &str) {
        if let Ok(mut pending) = self.pending_embedding_models.lock() {
            pending.remove(&(entity_type.to_string(), entity_id.to_string()));
        }
    }

    pub(crate) fn clear_pending_embedding_models(&self) {
        if let Ok(mut pending) = self.pending_embedding_models.lock() {
            pending.clear();
        }
    }

    pub(crate) fn clear_pending_embedding_models_for_ids(
        &self,
        entity_type: &str,
        entity_ids: &[String],
    ) {
        if let Ok(mut pending) = self.pending_embedding_models.lock() {
            for entity_id in entity_ids {
                pending.remove(&(entity_type.to_string(), entity_id.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{QUERY_CACHE_MAX_KEYS, fact_subject_cache_key};
    use std::thread;
    use std::time::Duration;

    fn make_msg(id: &str, session_id: &str) -> crate::repositories::messages::Message {
        crate::repositories::messages::Message {
            message_id: id.into(),
            session_id: session_id.into(),
            role: haven_common::types::CanonicalRole::User,
            content: format!("content-{}", id),
            message_type: Some(haven_common::types::TranscriptMessageKind::Text),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            attachments: vec![],
            media_inputs: vec![],
            voice: false,
            ingress_seq: 0,
        }
    }

    fn make_session(id: &str) -> crate::repositories::sessions::Session {
        crate::repositories::sessions::Session {
            session_id: id.into(),
            input_text: format!("input-{}", id),
            title: None,
            status: haven_common::lifecycle::SessionStatus::Pending,
            run_end_reason: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            creation: crate::repositories::sessions::SessionCreation::user_new(),
        }
    }

    fn make_fact(id: &str, subject: &str) -> crate::repositories::facts::Fact {
        crate::repositories::facts::Fact {
            fact_id: id.into(),
            subject: subject.into(),
            predicate: "likes".into(),
            object: "rust".into(),
            source: "user".into(),
            memory_kind: crate::repositories::facts::MemoryFactKind::Profile,
            confidence: 0.9,
            tags: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            valid_from: None,
            valid_to: None,
            mention_count: 0,
            last_seen_at: Some("2026-01-01T00:00:00Z".into()),
            source_ref: None,
            durability: 1.0,
        }
    }

    #[test]
    fn domain_models_serialize_entity_specific_identity_fields() {
        let session =
            serde_json::to_value(make_session("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")).unwrap();
        let message = serde_json::to_value(make_msg(
            "msg-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ))
        .unwrap();
        let fact = serde_json::to_value(make_fact("fact-cccccccccccccccccccccccccccccccc", "user"))
            .unwrap();

        assert_eq!(
            session["session_id"],
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(session.get("id").is_none());
        assert_eq!(
            message["message_id"],
            "msg-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert!(message.get("id").is_none());
        assert_eq!(fact["fact_id"], "fact-cccccccccccccccccccccccccccccccc");
        assert!(fact.get("id").is_none());
    }

    #[test]
    fn test_open_creates_db() {
        let dir = std::env::temp_dir().join(format!("haven-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let db = Database::open(&path).unwrap();
        let _guard = db.conn();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_conn_returns_lock_guard() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let count: i32 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn cancellation_rejects_later_statements_and_rolls_back_transactions() {
        for phase in 0..3 {
            let db = Arc::new(Database::open_in_memory().unwrap());
            let session = db.create_session("input").unwrap();
            let cancel = CancellationToken::new();
            let worker_cancel = cancel.clone();
            let session_id = session.session_id.clone();
            let result = db
                .run_blocking_cancellable(cancel, move |db| {
                    let conn = db.try_conn()?;
                    // Prepare before cancelling to cover cached SQL as well.
                    let mut write = conn
                        .prepare_cached("UPDATE sessions SET input_text = 'late' WHERE id = ?1")?;
                    if phase == 0 {
                        worker_cancel.cancel();
                    }
                    conn.execute_batch("BEGIN IMMEDIATE")?;
                    if phase == 1 {
                        worker_cancel.cancel();
                    }
                    write.execute([session_id])?;
                    if phase == 2 {
                        worker_cancel.cancel();
                    }
                    conn.execute_batch("COMMIT")?;
                    Ok(())
                })
                .await;
            assert!(result.is_err(), "cancellation phase {phase} must fail");
            let conn = db.conn();
            assert!(conn.is_autocommit(), "cancelled transaction must be closed");
            let input: String = conn
                .query_row("SELECT input_text FROM sessions", [], |row| row.get(0))
                .unwrap();
            assert_eq!(input, "input", "phase {phase} must not commit a late write");
            conn.execute("UPDATE sessions SET input_text = 'reused'", [])
                .expect("pooled connection must not retain cancellation hooks");
        }
    }

    #[test]
    fn cancellation_while_blocking_worker_is_queued_skips_closure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.await.unwrap();
            let db = Arc::new(Database::open_in_memory().unwrap());
            let cancel = CancellationToken::new();
            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_called = called.clone();
            let operation = db.run_blocking_cancellable(cancel.clone(), move |_| {
                worker_called.store(true, Ordering::SeqCst);
                Ok(())
            });
            tokio::pin!(operation);
            tokio::select! {
                biased;
                _ = &mut operation => panic!("worker must be queued"),
                _ = tokio::task::yield_now() => {}
            }
            cancel.cancel();
            release_tx.send(()).unwrap();
            assert!(operation.await.is_err());
            blocker.await.unwrap();
            assert!(!called.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn cancellation_during_connection_open_returns_capacity_without_checkout() {
        let cancel = CancellationToken::new();
        let opener_cancel = cancel.clone();
        let pool = ConnectionPool::with_initial(
            1,
            Box::new(move || {
                opener_cancel.cancel();
                Ok(Connection::open_in_memory()?)
            }),
            None,
        );
        let scope = CancellationScope(ACTIVE_DB_INTERRUPTS.with(|active| {
            active.replace(Some(Arc::new(DbCancellation {
                cancel,
                interrupts: Mutex::new(Vec::new()),
            })))
        }));
        assert!(pool.get().is_err());
        drop(scope);
        let conn = pool
            .get()
            .expect("cancelled opener must return pool capacity");
        conn.execute_batch("CREATE TABLE reused (value INTEGER)")
            .unwrap();
    }

    #[tokio::test]
    async fn cancelled_pool_wait_cannot_write_checkpoint_after_connection_release() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("input").unwrap();
        let held = db.conn();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
        let cancel = CancellationToken::new();
        let operation = {
            let db = db.clone();
            let cancel = cancel.clone();
            let session_id = session.session_id.clone();
            tokio::spawn(async move {
                db.run_blocking_cancellable(cancel, move |db| {
                    let _ = started_tx.send(());
                    let result = db.checkpoint_memory_event_cursor(&session_id, 7);
                    let _ = finished_tx.send(());
                    result
                })
                .await
            })
        };
        started_rx.await.unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(3), operation)
            .await
            .expect("cancelled checkout must return while the pool is exhausted")
            .unwrap();
        assert!(
            finished_rx.try_recv().is_ok(),
            "cancelled pool waiter must exit before the connection is released"
        );
        drop(held);
        assert!(result.is_err());
        assert_eq!(
            db.memory_event_cursor_optional(&session.session_id)
                .unwrap(),
            None,
            "a cancelled pool waiter must never write a late checkpoint"
        );
        db.checkpoint_memory_event_cursor(&session.session_id, 8)
            .unwrap();
    }

    #[tokio::test]
    async fn cancellable_blocking_write_interrupts_sqlite_lock_wait() {
        let dir = std::env::temp_dir().join(format!("haven-db-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let db = Arc::new(Database::open(&path).unwrap());
        let session = db.create_session("input").unwrap();

        let holder_db = db.clone();
        let (lock_ready_tx, lock_ready_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let conn = holder_db.conn();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            lock_ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(5));
        });
        lock_ready_rx.recv().unwrap();

        let (checkout_tx, checkout_rx) = tokio::sync::oneshot::channel();
        let cancel = CancellationToken::new();
        let cancel_task = {
            let db = db.clone();
            let session_id = session.session_id.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                db.run_blocking_cancellable(cancel, move |db| {
                    let conn = db.conn();
                    let _ = checkout_tx.send(());
                    conn.execute(
                        "UPDATE sessions SET input_text = 'blocked' WHERE id = ?1",
                        rusqlite::params![session_id],
                    )?;
                    Ok(())
                })
                .await
            })
        };
        checkout_rx.await.unwrap();
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), cancel_task)
            .await
            .expect("cancellable SQLite write must return promptly")
            .unwrap();
        assert!(result.is_err(), "interrupted write must not report success");

        holder.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_cache_messages_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        assert!(db.cache_get_messages(sid).is_none());
        let msgs = vec![make_msg("1", sid), make_msg("2", sid)];
        let generation = db.cache_generation(sid);
        db.cache_put_messages(sid, msgs.clone(), 60, generation);
        let cached = db.cache_get_messages(sid).unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(cached[0].message_id, "1");
    }

    #[test]
    fn test_cache_messages_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        let msgs = vec![make_msg("1", sid)];
        let generation = db.cache_generation(sid);
        db.cache_put_messages(sid, msgs, 1, generation);
        assert!(db.cache_get_messages(sid).is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_messages(sid).is_none());
    }

    #[test]
    fn test_cache_invalidate_messages() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        let msgs = vec![make_msg("1", sid)];
        let generation = db.cache_generation(sid);
        db.cache_put_messages(sid, msgs, 60, generation);
        assert!(db.cache_get_messages(sid).is_some());
        db.cache_invalidate_messages(sid);
        assert!(db.cache_get_messages(sid).is_none());
    }

    #[test]
    fn test_cache_tool_runs_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.cache_get_session_history_page().is_none());
        let sessions = vec![make_session("1"), make_session("2")];
        let generation = db.cache_generation("_session_history_page");
        db.cache_put_session_history_page(sessions.clone(), 60, generation);
        let cached = db.cache_get_session_history_page().unwrap();
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn test_cache_tool_runs_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let sessions = vec![make_session("1")];
        let generation = db.cache_generation("_session_history_page");
        db.cache_put_session_history_page(sessions, 1, generation);
        assert!(db.cache_get_session_history_page().is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_session_history_page().is_none());
    }

    #[test]
    fn test_cache_invalidate_session_history_page() {
        let db = Database::open_in_memory().unwrap();
        let sessions = vec![make_session("1")];
        let generation = db.cache_generation("_session_history_page");
        db.cache_put_session_history_page(sessions, 60, generation);
        assert!(db.cache_get_session_history_page().is_some());
        db.cache_invalidate_session_history_page();
        assert!(db.cache_get_session_history_page().is_none());
    }

    #[test]
    fn test_cache_facts_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        assert!(db.cache_get_facts(subj).is_none());
        let facts = vec![make_fact("1", subj), make_fact("2", subj)];
        let generation = db.cache_generation(&fact_subject_cache_key(subj));
        db.cache_put_facts(subj, facts.clone(), 60, generation);
        let cached = db.cache_get_facts(subj).unwrap();
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn test_cache_facts_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        let facts = vec![make_fact("1", subj)];
        let generation = db.cache_generation(&fact_subject_cache_key(subj));
        db.cache_put_facts(subj, facts, 1, generation);
        assert!(db.cache_get_facts(subj).is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_facts(subj).is_none());
    }

    #[test]
    fn test_cache_invalidate_facts() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        let facts = vec![make_fact("1", subj)];
        let generation = db.cache_generation(&fact_subject_cache_key(subj));
        db.cache_put_facts(subj, facts, 60, generation);
        assert!(db.cache_get_facts(subj).is_some());
        db.cache_invalidate_facts(subj);
        assert!(db.cache_get_facts(subj).is_none());
    }

    #[test]
    fn test_cache_different_subjects_independent() {
        let db = Database::open_in_memory().unwrap();
        let f1 = vec![make_fact("1", "subject-a")];
        let f2 = vec![make_fact("2", "subject-b")];
        let generation_a = db.cache_generation(&fact_subject_cache_key("subject-a"));
        let generation_b = db.cache_generation(&fact_subject_cache_key("subject-b"));
        db.cache_put_facts("subject-a", f1, 60, generation_a);
        db.cache_put_facts("subject-b", f2, 60, generation_b);
        assert!(db.cache_get_facts("subject-a").is_some());
        assert!(db.cache_get_facts("subject-b").is_some());
        db.cache_invalidate_facts("subject-a");
        assert!(db.cache_get_facts("subject-a").is_none());
        assert!(db.cache_get_facts("subject-b").is_some());
    }

    #[test]
    fn test_cache_is_bounded_and_evicts_least_recently_used_key() {
        let db = Database::open_in_memory().unwrap();

        for index in 0..QUERY_CACHE_MAX_KEYS {
            let session_id = format!("session-{index}");
            let generation = db.cache_generation(&session_id);
            db.cache_put_messages(
                &session_id,
                vec![make_msg(&index.to_string(), &session_id)],
                60,
                generation,
            );
        }

        // Refresh the oldest entry so the next insertion must evict the
        // second entry instead.
        assert!(db.cache_get_messages("session-0").is_some());
        let overflow_id = "session-overflow";
        let generation = db.cache_generation(overflow_id);
        db.cache_put_messages(
            overflow_id,
            vec![make_msg("overflow", overflow_id)],
            60,
            generation,
        );

        assert!(db.cache_get_messages("session-0").is_some());
        assert!(db.cache_get_messages(overflow_id).is_some());
        assert!(
            db.cache_get_messages("session-1").is_none(),
            "the least recently used logical key should be evicted at capacity"
        );
    }

    #[test]
    fn test_cache_generation_is_key_scoped_and_rejects_stale_writes() {
        let db = Database::open_in_memory().unwrap();
        let session_id = "session-a";
        let generation = db.cache_generation(session_id);

        // Invalidating another session must not make this independent write
        // miss, which was the old process-wide epoch behaviour.
        db.cache_invalidate_messages("session-b");
        db.cache_put_messages(session_id, vec![make_msg("a", session_id)], 60, generation);
        assert!(db.cache_get_messages(session_id).is_some());

        let stale_generation = db.cache_generation(session_id);
        db.cache_invalidate_messages(session_id);
        db.cache_put_messages(
            session_id,
            vec![make_msg("stale", session_id)],
            60,
            stale_generation,
        );
        assert!(
            db.cache_get_messages(session_id).is_none(),
            "a query that started before invalidation must not repopulate stale data"
        );
    }
}
