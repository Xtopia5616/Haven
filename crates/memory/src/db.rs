use rusqlite::Connection;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Pooled SQLite connections for a file-backed database (WAL mode: one
/// writer + many readers can proceed concurrently). Sized comfortably above
/// the session concurrency ceiling plus the direct (non-`run_blocking`) DB
/// callers (tools, Tauri commands), so a busy step rarely blocks on
/// checkout while other connections are still doing real work.
const FILE_POOL_MAX_CONNECTIONS: usize = 16;

/// A tiny bounded pool of rusqlite connections. The previous design wrapped a
/// SINGLE connection in a `Mutex`, which serialized every DB access across all
/// concurrent sessions (and background tasks / title generation / memory
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
            if let Some(conn) = st.idle.pop() {
                st.active += 1;
                return Ok(PooledConnection {
                    pool: self,
                    conn: Some(conn),
                });
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
                return Ok(PooledConnection {
                    pool: self,
                    conn: Some(conn),
                });
            }
            // All connections are checked out. Some callers reach `conn()`
            // directly from async runtime threads (tools, commands), so a
            // long wait here occupies a tokio worker; log once per 30s to
            // keep the stall observable instead of silently hanging.
            let (new_st, timeout) = self
                .cv
                .wait_timeout(st, Duration::from_secs(30))
                .map_err(|_| anyhow::anyhow!("database pool lock poisoned"))?;
            st = new_st;
            if timeout.timed_out() {
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
            // Defensive rollback: a caller whose transaction failed midway
            // (e.g. `clear_sessions` propagates errors without ROLLBACK) would
            // otherwise re-pool a connection with a write transaction still
            // open — subsequent statements would run inside the abandoned
            // transaction and the held write lock would block the other
            // pooled connections. Fails silently when no transaction is open.
            let _ = conn.execute_batch("ROLLBACK");
            let mut st = self.pool.state.lock().unwrap();
            st.idle.push(conn);
            st.active -= 1;
            self.pool.cv.notify_one();
        }
    }
}

#[derive(Clone)]
struct CacheEntry<T: Clone> {
    data: T,
    expiry: Instant,
}

#[derive(Clone)]
struct QueryCache {
    messages: Option<CacheEntry<Vec<crate::repositories::messages::Message>>>,
    sessions: Option<CacheEntry<Vec<crate::repositories::sessions::Session>>>,
    facts: Option<CacheEntry<Vec<crate::repositories::facts::Fact>>>,
    embeddings: Option<CacheEntry<Vec<crate::embeddings::EmbeddedText>>>,
    // Bumped on every cache_invalidate_* so a stale cache_put_* (whose DB
    // query ran before an invalidation) can detect it was superseded and
    // skip the write instead of overwriting fresh state with stale data.
    generation: u64,
}

pub struct Database {
    pool: ConnectionPool,
    cache: Mutex<HashMap<String, QueryCache>>,
}

impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        tracing::info!("opening database at {}", path.display());
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
            FILE_POOL_MAX_CONNECTIONS,
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
            cache: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
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
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn conn(&self) -> PooledConnection<'_> {
        self.pool
            .get()
            .expect("database connection checkout failed")
    }

    /// Run a blocking DB closure on the tokio blocking thread pool. Keeps
    /// synchronous SQLite work (including WAL fsyncs) off the async runtime
    /// so a slow write cannot stall unrelated async sessions that don't touch
    /// the DB. The closure borrows `&Database`; owned arguments must be
    /// cloned into the closure by the caller (it is `'static`).
    pub async fn run_blocking<T, F>(self: &Arc<Self>, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> anyhow::Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || f(&db)).await?
    }

    pub fn cache_get_messages(
        &self,
        session_id: &str,
    ) -> Option<Vec<crate::repositories::messages::Message>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(session_id)?.messages.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    /// Returns the current cache generation for a key. Callers capture this
    /// before querying the DB and pass it to the corresponding `cache_put_*`
    /// to guard against stale-overwrite after a concurrent invalidation.
    pub fn cache_generation(&self, key: &str) -> u64 {
        self.cache
            .lock()
            .ok()
            .and_then(|c| c.get(key).map(|qc| qc.generation))
            .unwrap_or(0)
    }

    /// Shared write path for every `cache_put_*`: upsert the key's
    /// `QueryCache` entry, skip the write if the generation moved (a
    /// concurrent invalidation superseded this query's result), then store
    /// `data` into the slot chosen by `set`.
    fn cache_put<T: Clone + Send>(
        &self,
        key: String,
        data: T,
        ttl_secs: u64,
        expected_gen: u64,
        set: impl FnOnce(&mut QueryCache, CacheEntry<T>),
    ) {
        if let Ok(mut cache) = self.cache.lock() {
            let qc = cache.entry(key).or_insert(QueryCache {
                messages: None,
                sessions: None,
                facts: None,
                embeddings: None,
                generation: expected_gen,
            });
            if qc.generation != expected_gen {
                return;
            }
            set(
                qc,
                CacheEntry {
                    expiry: Instant::now() + std::time::Duration::from_secs(ttl_secs),
                    data,
                },
            );
        }
    }

    pub fn cache_put_messages(
        &self,
        session_id: &str,
        data: Vec<crate::repositories::messages::Message>,
        ttl_secs: u64,
        expected_gen: u64,
    ) {
        self.cache_put(
            session_id.to_string(),
            data,
            ttl_secs,
            expected_gen,
            |qc, entry| qc.messages = Some(entry),
        );
    }

    pub fn cache_get_sessions(&self) -> Option<Vec<crate::repositories::sessions::Session>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get("_sessions")?.sessions.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    pub fn cache_put_sessions(
        &self,
        data: Vec<crate::repositories::sessions::Session>,
        ttl_secs: u64,
        expected_gen: u64,
    ) {
        self.cache_put(
            "_sessions".to_string(),
            data,
            ttl_secs,
            expected_gen,
            |qc, entry| qc.sessions = Some(entry),
        );
    }

    pub fn cache_get_facts(&self, subject: &str) -> Option<Vec<crate::repositories::facts::Fact>> {
        let cache = self.cache.lock().ok()?;
        let key = format!("_facts_{}", subject);
        let entry = cache.get(&key)?.facts.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    pub fn cache_put_facts(
        &self,
        subject: &str,
        data: Vec<crate::repositories::facts::Fact>,
        ttl_secs: u64,
        expected_gen: u64,
    ) {
        self.cache_put(
            format!("_facts_{}", subject),
            data,
            ttl_secs,
            expected_gen,
            |qc, entry| qc.facts = Some(entry),
        );
    }

    pub fn cache_invalidate_messages(&self, session_id: &str) {
        if let Ok(mut cache) = self.cache.lock()
            && let Some(qc) = cache.get_mut(session_id)
        {
            qc.messages = None;
            qc.generation = qc.generation.wrapping_add(1);
        }
    }

    pub fn cache_invalidate_sessions(&self) {
        if let Ok(mut cache) = self.cache.lock()
            && let Some(qc) = cache.get_mut("_sessions")
        {
            qc.sessions = None;
            qc.generation = qc.generation.wrapping_add(1);
        }
    }

    /// Cached copy of the full facts table (`list_facts`), keyed separately
    /// from the per-subject cache. Invalidated together with the subject
    /// cache on any fact mutation, so the global list cannot drift from the
    /// subject views.
    pub fn cache_get_facts_all(&self) -> Option<Vec<crate::repositories::facts::Fact>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get("_facts_all")?.facts.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    pub fn cache_put_facts_all(
        &self,
        data: Vec<crate::repositories::facts::Fact>,
        ttl_secs: u64,
        expected_gen: u64,
    ) {
        self.cache_put(
            "_facts_all".to_string(),
            data,
            ttl_secs,
            expected_gen,
            |qc, entry| qc.facts = Some(entry),
        );
    }

    pub fn cache_invalidate_facts(&self, subject: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            // The subject view...
            let key = format!("_facts_{}", subject);
            if let Some(qc) = cache.get_mut(&key) {
                qc.facts = None;
                qc.generation = qc.generation.wrapping_add(1);
            }
            // ...and the all-subjects list: a mutation to ANY subject makes
            // the global list stale too.
            if let Some(qc) = cache.get_mut("_facts_all") {
                qc.facts = None;
                qc.generation = qc.generation.wrapping_add(1);
            }
        }
    }

    /// Cached copy of one memory domain's embedding list (`list_embeddings`).
    /// Keyed by entity_type so vector recall skips the full-table read + blob
    /// decode on every query; invalidated on any embedding write.
    pub fn cache_get_embeddings(
        &self,
        entity_type: &str,
    ) -> Option<Vec<crate::embeddings::EmbeddedText>> {
        let cache = self.cache.lock().ok()?;
        let key = format!("_embeddings_{}", entity_type);
        let entry = cache.get(&key)?.embeddings.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    pub fn cache_put_embeddings(
        &self,
        entity_type: &str,
        data: Vec<crate::embeddings::EmbeddedText>,
        ttl_secs: u64,
        expected_gen: u64,
    ) {
        self.cache_put(
            format!("_embeddings_{}", entity_type),
            data,
            ttl_secs,
            expected_gen,
            |qc, entry| qc.embeddings = Some(entry),
        );
    }

    /// Invalidate one domain's embeddings list cache. Called from the fact
    /// UPDATE/DELETE paths (the facts_embed_del/upd triggers delete embedding
    /// rows directly in SQL) and from every embedding write — without this
    /// the cached list would keep serving removed/stale vectors (with old
    /// surface text) for the whole TTL. Deliberately NOT called on plain fact
    /// INSERTs — those fire no trigger and leave the embedding rows
    /// untouched, so invalidating would only thrash the cache.
    pub fn cache_invalidate_embeddings(&self, entity_type: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            let key = format!("_embeddings_{}", entity_type);
            if let Some(qc) = cache.get_mut(&key) {
                qc.embeddings = None;
                qc.generation = qc.generation.wrapping_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn make_msg(id: &str, session_id: &str) -> crate::repositories::messages::Message {
        crate::repositories::messages::Message {
            id: id.into(),
            session_id: session_id.into(),
            role: "user".into(),
            content: format!("content-{}", id),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            attachments: vec![],
            voice: false,
        }
    }

    fn make_session(id: &str) -> crate::repositories::sessions::Session {
        crate::repositories::sessions::Session {
            id: id.into(),
            input_text: format!("input-{}", id),
            title: None,
            status: "pending".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            transcript: "".into(),
            react_state: None,
        }
    }

    fn make_fact(id: &str, subject: &str) -> crate::repositories::facts::Fact {
        crate::repositories::facts::Fact {
            id: id.into(),
            subject: subject.into(),
            predicate: "likes".into(),
            object: "rust".into(),
            source: "user".into(),
            confidence: 0.9,
            tags: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            mention_count: 0,
            last_seen_at: Some("2026-01-01T00:00:00Z".into()),
            source_ref: None,
            durability: 1.0,
        }
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

    #[test]
    fn test_cache_messages_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        assert!(db.cache_get_messages(sid).is_none());
        let msgs = vec![make_msg("1", sid), make_msg("2", sid)];
        db.cache_put_messages(sid, msgs.clone(), 60, 0);
        let cached = db.cache_get_messages(sid).unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(cached[0].id, "1");
    }

    #[test]
    fn test_cache_messages_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        let msgs = vec![make_msg("1", sid)];
        db.cache_put_messages(sid, msgs, 1, 0);
        assert!(db.cache_get_messages(sid).is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_messages(sid).is_none());
    }

    #[test]
    fn test_cache_invalidate_messages() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        let msgs = vec![make_msg("1", sid)];
        db.cache_put_messages(sid, msgs, 60, 0);
        assert!(db.cache_get_messages(sid).is_some());
        db.cache_invalidate_messages(sid);
        assert!(db.cache_get_messages(sid).is_none());
    }

    #[test]
    fn test_cache_tasks_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.cache_get_sessions().is_none());
        let sessions = vec![make_session("1"), make_session("2")];
        db.cache_put_sessions(sessions.clone(), 60, 0);
        let cached = db.cache_get_sessions().unwrap();
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn test_cache_tasks_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let sessions = vec![make_session("1")];
        db.cache_put_sessions(sessions, 1, 0);
        assert!(db.cache_get_sessions().is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_sessions().is_none());
    }

    #[test]
    fn test_cache_invalidate_sessions() {
        let db = Database::open_in_memory().unwrap();
        let sessions = vec![make_session("1")];
        db.cache_put_sessions(sessions, 60, 0);
        assert!(db.cache_get_sessions().is_some());
        db.cache_invalidate_sessions();
        assert!(db.cache_get_sessions().is_none());
    }

    #[test]
    fn test_cache_facts_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        assert!(db.cache_get_facts(subj).is_none());
        let facts = vec![make_fact("1", subj), make_fact("2", subj)];
        db.cache_put_facts(subj, facts.clone(), 60, 0);
        let cached = db.cache_get_facts(subj).unwrap();
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn test_cache_facts_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        let facts = vec![make_fact("1", subj)];
        db.cache_put_facts(subj, facts, 1, 0);
        assert!(db.cache_get_facts(subj).is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_facts(subj).is_none());
    }

    #[test]
    fn test_cache_invalidate_facts() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        let facts = vec![make_fact("1", subj)];
        db.cache_put_facts(subj, facts, 60, 0);
        assert!(db.cache_get_facts(subj).is_some());
        db.cache_invalidate_facts(subj);
        assert!(db.cache_get_facts(subj).is_none());
    }

    #[test]
    fn test_cache_different_subjects_independent() {
        let db = Database::open_in_memory().unwrap();
        let f1 = vec![make_fact("1", "subject-a")];
        let f2 = vec![make_fact("2", "subject-b")];
        db.cache_put_facts("subject-a", f1, 60, 0);
        db.cache_put_facts("subject-b", f2, 60, 0);
        assert!(db.cache_get_facts("subject-a").is_some());
        assert!(db.cache_get_facts("subject-b").is_some());
        db.cache_invalidate_facts("subject-a");
        assert!(db.cache_get_facts("subject-a").is_none());
        assert!(db.cache_get_facts("subject-b").is_some());
    }
}
