use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone)]
struct CacheEntry<T: Clone> {
    data: T,
    expiry: Instant,
}

#[derive(Clone)]
struct QueryCache {
    messages: Option<CacheEntry<Vec<crate::repositories::messages::Message>>>,
    tasks: Option<CacheEntry<Vec<crate::repositories::tasks::Task>>>,
    facts: Option<CacheEntry<Vec<crate::repositories::facts::Fact>>>,
}

pub struct Database {
    conn: Mutex<Connection>,
    cache: Mutex<HashMap<String, QueryCache>>,
}

impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        tracing::info!("opening database at {}", path.display());
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        crate::migrations::run_migrations(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        tracing::debug!("opening in-memory database");
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        crate::migrations::run_migrations(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("database lock poisoned")
    }

    pub fn cache_get_messages(&self, session_id: &str) -> Option<Vec<crate::repositories::messages::Message>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(session_id)?.messages.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    pub fn cache_put_messages(&self, session_id: &str, data: Vec<crate::repositories::messages::Message>, ttl_secs: u64) {
        if let Ok(mut cache) = self.cache.lock() {
            let qc = cache.entry(session_id.to_string()).or_insert(QueryCache {
                messages: None,
                tasks: None,
                facts: None,
            });
            qc.messages = Some(CacheEntry {
                expiry: Instant::now() + std::time::Duration::from_secs(ttl_secs),
                data,
            });
        }
    }

    pub fn cache_get_tasks(&self) -> Option<Vec<crate::repositories::tasks::Task>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get("_tasks")?.tasks.as_ref()?;
        if entry.expiry > Instant::now() {
            Some(entry.data.clone())
        } else {
            None
        }
    }

    pub fn cache_put_tasks(&self, data: Vec<crate::repositories::tasks::Task>, ttl_secs: u64) {
        if let Ok(mut cache) = self.cache.lock() {
            let qc = cache.entry("_tasks".to_string()).or_insert(QueryCache {
                messages: None,
                tasks: None,
                facts: None,
            });
            qc.tasks = Some(CacheEntry {
                expiry: Instant::now() + std::time::Duration::from_secs(ttl_secs),
                data,
            });
        }
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

    pub fn cache_put_facts(&self, subject: &str, data: Vec<crate::repositories::facts::Fact>, ttl_secs: u64) {
        if let Ok(mut cache) = self.cache.lock() {
            let key = format!("_facts_{}", subject);
            let qc = cache.entry(key).or_insert(QueryCache {
                messages: None,
                tasks: None,
                facts: None,
            });
            qc.facts = Some(CacheEntry {
                expiry: Instant::now() + std::time::Duration::from_secs(ttl_secs),
                data,
            });
        }
    }

    pub fn cache_invalidate_messages(&self, session_id: &str) {
        if let Ok(mut cache) = self.cache.lock()
            && let Some(qc) = cache.get_mut(session_id)
        {
            qc.messages = None;
        }
    }

    pub fn cache_invalidate_tasks(&self) {
        if let Ok(mut cache) = self.cache.lock()
            && let Some(qc) = cache.get_mut("_tasks")
        {
            qc.tasks = None;
        }
    }

    pub fn cache_invalidate_facts(&self, subject: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            let key = format!("_facts_{}", subject);
            if let Some(qc) = cache.get_mut(&key) {
                qc.facts = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use std::thread;

    fn make_msg(id: &str, session_id: &str) -> crate::repositories::messages::Message {
        crate::repositories::messages::Message {
            id: id.into(),
            session_id: session_id.into(),
            role: "user".into(),
            content: format!("content-{}", id),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
        }
    }

    fn make_task(id: &str) -> crate::repositories::tasks::Task {
        crate::repositories::tasks::Task {
            id: id.into(),
            session_id: None,
            input_text: format!("input-{}", id),
            status: "pending".into(),
            classification: "NEW_TASK".into(),
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
            created_at: "2026-01-01T00:00:00Z".into(),
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
        db.cache_put_messages(sid, msgs.clone(), 60);
        let cached = db.cache_get_messages(sid).unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(cached[0].id, "1");
    }

    #[test]
    fn test_cache_messages_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        let msgs = vec![make_msg("1", sid)];
        db.cache_put_messages(sid, msgs, 1);
        assert!(db.cache_get_messages(sid).is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_messages(sid).is_none());
    }

    #[test]
    fn test_cache_invalidate_messages() {
        let db = Database::open_in_memory().unwrap();
        let sid = "session-1";
        let msgs = vec![make_msg("1", sid)];
        db.cache_put_messages(sid, msgs, 60);
        assert!(db.cache_get_messages(sid).is_some());
        db.cache_invalidate_messages(sid);
        assert!(db.cache_get_messages(sid).is_none());
    }

    #[test]
    fn test_cache_tasks_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.cache_get_tasks().is_none());
        let tasks = vec![make_task("1"), make_task("2")];
        db.cache_put_tasks(tasks.clone(), 60);
        let cached = db.cache_get_tasks().unwrap();
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn test_cache_tasks_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let tasks = vec![make_task("1")];
        db.cache_put_tasks(tasks, 1);
        assert!(db.cache_get_tasks().is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_tasks().is_none());
    }

    #[test]
    fn test_cache_invalidate_tasks() {
        let db = Database::open_in_memory().unwrap();
        let tasks = vec![make_task("1")];
        db.cache_put_tasks(tasks, 60);
        assert!(db.cache_get_tasks().is_some());
        db.cache_invalidate_tasks();
        assert!(db.cache_get_tasks().is_none());
    }

    #[test]
    fn test_cache_facts_hit_and_miss() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        assert!(db.cache_get_facts(subj).is_none());
        let facts = vec![make_fact("1", subj), make_fact("2", subj)];
        db.cache_put_facts(subj, facts.clone(), 60);
        let cached = db.cache_get_facts(subj).unwrap();
        assert_eq!(cached.len(), 2);
    }

    #[test]
    fn test_cache_facts_ttl_expiry() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        let facts = vec![make_fact("1", subj)];
        db.cache_put_facts(subj, facts, 1);
        assert!(db.cache_get_facts(subj).is_some());
        thread::sleep(Duration::from_secs(2));
        assert!(db.cache_get_facts(subj).is_none());
    }

    #[test]
    fn test_cache_invalidate_facts() {
        let db = Database::open_in_memory().unwrap();
        let subj = "user";
        let facts = vec![make_fact("1", subj)];
        db.cache_put_facts(subj, facts, 60);
        assert!(db.cache_get_facts(subj).is_some());
        db.cache_invalidate_facts(subj);
        assert!(db.cache_get_facts(subj).is_none());
    }

    #[test]
    fn test_cache_different_subjects_independent() {
        let db = Database::open_in_memory().unwrap();
        let f1 = vec![make_fact("1", "subject-a")];
        let f2 = vec![make_fact("2", "subject-b")];
        db.cache_put_facts("subject-a", f1, 60);
        db.cache_put_facts("subject-b", f2, 60);
        assert!(db.cache_get_facts("subject-a").is_some());
        assert!(db.cache_get_facts("subject-b").is_some());
        db.cache_invalidate_facts("subject-a");
        assert!(db.cache_get_facts("subject-a").is_none());
        assert!(db.cache_get_facts("subject-b").is_some());
    }
}
