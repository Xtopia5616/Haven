use crate::db::Database;
use crate::recall::{MemoryQuery, MemoryRecall, MemoryRetriever, normalize_memory_query};
use crate::repositories::fact_graph::FactGraph;
use crate::repositories::facts::{Fact, FactSourceRef};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use std::sync::Arc;

/// A fact already parsed, normalized, and sanitized by its owning caller,
/// ready for inferred-fact persistence.
#[derive(Debug, Clone)]
pub struct MemoryFactWrite {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub source_ref: Option<FactSourceRef>,
    pub durability: f64,
    /// The caller's canonical predicate policy, used only to decide whether
    /// an existing `(subject, predicate)` pair qualifies as an update below
    /// the new-fact confidence floor.
    pub is_single_valued_predicate: bool,
}

/// Async application-facing boundary for durable memory-fact operations.
///
/// This keeps SQLite blocking-pool scheduling and fact visibility policy in
/// `haven-memory`, while leaving IPC input validation with the app adapter.
#[derive(Clone)]
pub struct MemoryFactStore {
    db: Arc<Database>,
}

impl MemoryFactStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Persist a prepared batch of inferred facts in one blocking closure and
    /// one SQLite transaction. `new_fact_confidence_floor` is selected by the
    /// caller's extraction policy; exact re-confirmations and permitted
    /// single-valued updates still pass through for reinforcement/correction.
    /// Returns whether any fact was inserted, reinforced, or corrected.
    pub async fn persist_inferred_batch(
        &self,
        writes: Vec<MemoryFactWrite>,
        new_fact_confidence_floor: f64,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.with_fact_write(|| {
                    FactGraph::new(db).upsert_inferred_batch(&writes, new_fact_confidence_floor)
                })
            })
            .await
    }

    /// Commit inferred facts and the ordinary extraction cursor atomically.
    /// A failed cursor write rolls the fact mutations back with it, so a retry
    /// cannot reinforce the same observation twice.
    pub async fn commit_ordinary_extraction(
        &self,
        writes: Vec<MemoryFactWrite>,
        new_fact_confidence_floor: f64,
        session_id: &str,
        last_message_id: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(!last_message_id.trim().is_empty(), "message id is required");
        let session_id = session_id.to_owned();
        let last_message_id = last_message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let cursor_key = format!("fact_extraction.{session_id}");
                db.with_fact_write(|| {
                    FactGraph::new(db).upsert_inferred_batch_with_transaction_hook(
                        &writes,
                        new_fact_confidence_floor,
                        |_| Ok(true),
                        |conn| set_kv_on_connection(conn, &cursor_key, &last_message_id),
                    )
                })
            })
            .await
    }

    /// Read the per-episode completion ledger used to avoid repeating summary
    /// inference when durable outbox acknowledgement fails.
    pub async fn summary_extraction_completed(
        &self,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<bool> {
        let key = summary_extraction_done_key(session_id, episode_id)?;
        self.db
            .run_blocking(move |db| {
                let conn = db.conn();
                Ok(conn
                    .query_row(
                        "SELECT 1 FROM kv_store WHERE key = ?1",
                        rusqlite::params![key],
                        |row| row.get::<_, i32>(0),
                    )
                    .optional()?
                    .is_some())
            })
            .await
    }

    /// Commit one summary's facts and its durable completion marker together.
    /// The marker is keyed by session and episode, so out-of-order retries of
    /// older episodes remain independently idempotent.
    pub async fn commit_summary_extraction(
        &self,
        writes: Vec<MemoryFactWrite>,
        new_fact_confidence_floor: f64,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<bool> {
        let marker_key = summary_extraction_done_key(session_id, episode_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.with_fact_write(|| {
                    FactGraph::new(db).upsert_inferred_batch_with_transaction_hook(
                        &writes,
                        new_fact_confidence_floor,
                        |conn| {
                            Ok(conn
                                .query_row(
                                    "SELECT 1 FROM kv_store WHERE key = ?1",
                                    rusqlite::params![marker_key],
                                    |row| row.get::<_, i32>(0),
                                )
                                .optional()?
                                .is_none())
                        },
                        |conn| set_kv_on_connection(conn, &marker_key, &session_id),
                    )
                })
            })
            .await
    }

    /// List visible facts, optionally restricted to an exact source value.
    /// An absent or empty source retains the existing all-facts behavior.
    pub async fn list_facts(&self, source: Option<String>) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(move |db| {
                let facts = match source.as_deref().filter(|source| !source.is_empty()) {
                    Some(source) => db.list_facts_by_source(source)?,
                    None => db.list_facts()?,
                };
                Ok(MemoryRetriever::filter_visible_facts(facts))
            })
            .await
    }

    /// Persist a user-stated fact. Input normalization and validation belong
    /// to the IPC adapter; this method owns only the SQLite scheduling and
    /// repository call.
    pub async fn set_user_fact(
        &self,
        subject: String,
        predicate: String,
        object: String,
        tags: Vec<String>,
    ) -> anyhow::Result<Fact> {
        self.db
            .run_blocking(move |db| {
                let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
                db.set_user_fact(&subject, &predicate, &object, &tags)
            })
            .await
    }

    pub async fn delete_fact(&self, fact_id: String) -> anyhow::Result<()> {
        self.db
            .run_blocking(move |db| db.delete_fact(&fact_id))
            .await
    }

    /// Delete every saved fact, keeping fact graph/cache invalidation in the
    /// synchronous repository write boundary.
    pub async fn clear_facts(&self) -> anyhow::Result<u64> {
        self.db.run_blocking(|db| db.clear_facts()).await
    }

    /// Search facts with an optional exact subject scope. The caller may
    /// normalize the query for its own validation contract; this port also
    /// normalizes defensively and applies the shared visibility policy while
    /// preserving the repository's ranked order.
    pub async fn search_visible_facts(
        &self,
        query: String,
        subject: Option<String>,
    ) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(move |db| {
                let query = normalize_memory_query(&query)?;
                let facts = db.search_facts_scoped(&query, subject.as_deref())?;
                Ok(MemoryRetriever::filter_visible_facts(facts))
            })
            .await
    }

    /// List visible facts for one exact subject, retaining Database ordering.
    pub async fn list_visible_facts_for_subject(
        &self,
        subject: String,
    ) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(move |db| {
                let facts = db.list_facts_by_subject(&subject)?;
                Ok(MemoryRetriever::filter_visible_facts(facts))
            })
            .await
    }

    /// List visible facts across subjects in the existing recent/effective
    /// confidence order returned by Database.
    pub async fn list_recent_visible_facts(&self) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(|db| {
                let facts = db.list_facts()?;
                Ok(MemoryRetriever::filter_visible_facts(facts))
            })
            .await
    }

    /// List at most `limit` visible facts across subjects, preserving the
    /// effective-confidence order from `Database::list_facts`. Visibility is
    /// applied before truncation so sensitive rows do not consume result slots.
    pub async fn list_recent_visible_facts_limited(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(move |db| {
                let facts = db.list_facts()?;
                Ok(MemoryRetriever::filter_visible_facts(facts)
                    .into_iter()
                    .take(limit)
                    .collect())
            })
            .await
    }

    /// Delete facts matching an exact subject/predicate pair and optional
    /// exact object value.
    pub async fn delete_facts_by_triple(
        &self,
        subject: String,
        predicate: String,
        object: Option<String>,
    ) -> anyhow::Result<u64> {
        self.db
            .run_blocking(move |db| {
                db.delete_facts_by_triple(&subject, &predicate, object.as_deref())
            })
            .await
    }

    /// Keyword-only recall fallback. Embedding-aware callers should use their
    /// configured `MemoryRecallPort`; this method intentionally supplies no
    /// vector candidates.
    pub async fn recall_keyword(&self, query: MemoryQuery) -> anyhow::Result<MemoryRecall> {
        self.db
            .run_blocking(move |db| MemoryRetriever::new(db).retrieve(&query, None))
            .await
    }
}

fn summary_extraction_done_key(session_id: &str, episode_id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
    anyhow::ensure!(!episode_id.trim().is_empty(), "episode id is required");
    Ok(format!(
        "fact_extraction_episode_done.{session_id}.{episode_id}"
    ))
}

fn set_kv_on_connection(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO kv_store (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        rusqlite::params![key, value, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MemoryFactStore, MemoryFactWrite};
    use crate::Database;
    use crate::recall::MemoryQuery;
    use crate::repositories::facts::FactSourceRef;
    use std::sync::Arc;

    fn store() -> (Arc<Database>, MemoryFactStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = MemoryFactStore::new(db.clone());
        (db, store)
    }

    fn inferred_write(subject: &str, predicate: &str, object: &str) -> MemoryFactWrite {
        MemoryFactWrite {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence: 0.8,
            tags: Vec::new(),
            source_ref: None,
            durability: 0.6,
            is_single_valued_predicate: false,
        }
    }

    #[tokio::test]
    async fn inferred_batch_empty_input_returns_false() {
        let (_db, store) = store();

        assert!(
            !store
                .persist_inferred_batch(Vec::new(), 0.55)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn inferred_batch_reinforces_existing_fact_and_persists_source_ref() {
        let (db, store) = store();
        db.insert_fact_with_source_ref(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.6,
            &["preference"],
            None,
            0.5,
        )
        .unwrap();
        let mut reinforced = inferred_write("user", "likes", "Rust");
        reinforced.confidence = 0.7;
        reinforced.tags = vec!["workspace".into()];
        reinforced.source_ref = Some(FactSourceRef {
            message_id: "msg-fact-batch-source".into(),
            snippet: "I use Rust at work.".into(),
        });
        reinforced.durability = 0.8;
        let mut inserted = inferred_write("user", "uses", "SQLite");
        inserted.confidence = 0.55;

        assert!(
            store
                .persist_inferred_batch(vec![reinforced, inserted], 0.55)
                .await
                .unwrap()
        );

        let facts = db.list_facts_by_subject("user").unwrap();
        let rust = facts.iter().find(|fact| fact.object == "Rust").unwrap();
        assert_eq!(rust.mention_count, 1);
        assert!(rust.confidence >= 0.7);
        assert_eq!(rust.durability, 0.8);
        assert_eq!(rust.tags, ["preference", "workspace"]);
        assert_eq!(
            rust.source_ref.as_ref().unwrap().message_id,
            "msg-fact-batch-source"
        );
        assert_eq!(
            rust.source_ref.as_ref().unwrap().snippet,
            "I use Rust at work."
        );
        assert!(facts.iter().any(|fact| fact.object == "SQLite"));
    }

    #[tokio::test]
    async fn inferred_batch_failure_rolls_back_prior_writes() {
        let (db, store) = store();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_selected_inferred_fact
                 BEFORE INSERT ON facts
                 WHEN NEW.object = 'blocked'
                 BEGIN
                    SELECT RAISE(ABORT, 'injected fact write failure');
                 END;",
            )
            .unwrap();

        let error = store
            .persist_inferred_batch(
                vec![
                    inferred_write("user", "likes", "SQLite"),
                    inferred_write("user", "likes", "blocked"),
                ],
                0.55,
            )
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed to persist fact 'user likes blocked'")
        );
        assert!(db.list_facts().unwrap().is_empty());
        let node_count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM memory_nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(node_count, 0, "node writes must roll back with the batch");
    }

    #[tokio::test]
    async fn list_facts_preserves_source_selection_and_visibility() {
        let (db, store) = store();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "SQLite", "inferred", 0.8, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "hidden-predicate", "user", 1.0, &[])
            .unwrap();
        db.insert_fact("user", "likes", "sk-secret", "user", 1.0, &[])
            .unwrap();
        db.insert_fact_with_source_ref(
            "user",
            "uses",
            "safe value",
            "user",
            0.9,
            &[],
            Some(&FactSourceRef {
                message_id: "msg-fact-source".into(),
                snippet: "password=hidden".into(),
            }),
            1.0,
        )
        .unwrap();
        {
            let conn = db.conn();
            conn.execute(
                "UPDATE facts SET provenance_snippet = 'password=hidden' WHERE object = 'safe value'",
                [],
            )
            .unwrap();
        }

        let all = store.list_facts(None).await.unwrap();
        let all_objects: Vec<&str> = all.iter().map(|fact| fact.object.as_str()).collect();
        assert!(all_objects.contains(&"Rust"));
        assert!(all_objects.contains(&"SQLite"));
        assert!(all_objects.contains(&"safe value"));
        assert!(!all_objects.contains(&"hidden-predicate"));
        assert!(!all_objects.contains(&"sk-secret"));
        assert_eq!(
            all.iter()
                .find(|fact| fact.object == "safe value")
                .unwrap()
                .source_ref
                .as_ref()
                .unwrap()
                .snippet,
            "[redacted]"
        );

        let empty_source = store.list_facts(Some(String::new())).await.unwrap();
        assert_eq!(empty_source.len(), all.len());

        let user_source = store.list_facts(Some("user".into())).await.unwrap();
        assert!(user_source.iter().all(|fact| fact.source == "user"));
        assert!(user_source.iter().any(|fact| fact.object == "Rust"));
        assert!(!user_source.iter().any(|fact| fact.object == "SQLite"));
    }

    #[tokio::test]
    async fn set_user_fact_persists_tags_and_delete_removes_fact() {
        let (_db, store) = store();
        let fact = store
            .set_user_fact(
                "user".into(),
                "likes".into(),
                "Rust".into(),
                vec!["preference".into(), "workspace".into()],
            )
            .await
            .unwrap();
        assert_eq!(fact.tags, ["preference", "workspace"]);

        store.delete_fact(fact.id.clone()).await.unwrap();
        assert!(store.list_facts(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn scoped_search_normalizes_and_filters_sensitive_facts() {
        let (db, store) = store();
        db.insert_fact("alice", "likes", "green tea", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("bob", "likes", "green tea", "inferred", 0.8, &[])
            .unwrap();
        db.insert_fact("alice", "api_key", "green tea secret", "inferred", 1.0, &[])
            .unwrap();

        let all = store
            .search_visible_facts(" green   tea ".into(), None)
            .await
            .unwrap();
        let expected_order = db
            .search_facts_scoped("green tea", None)
            .unwrap()
            .into_iter()
            .filter(crate::recall::MemoryRetriever::visible_fact)
            .map(|fact| fact.id)
            .collect::<Vec<_>>();
        assert_eq!(
            all.iter().map(|fact| fact.id.clone()).collect::<Vec<_>>(),
            expected_order
        );
        assert_eq!(all.len(), 2);

        let scoped = store
            .search_visible_facts("green tea".into(), Some("bob".into()))
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].subject, "bob");
    }

    #[tokio::test]
    async fn subject_and_recent_lists_preserve_database_order() {
        let (db, store) = store();
        db.insert_fact("alice", "likes", "lower", "inferred", 0.4, &[])
            .unwrap();
        db.insert_fact("alice", "likes", "higher", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("bob", "likes", "middle", "inferred", 0.7, &[])
            .unwrap();

        let subject = store
            .list_visible_facts_for_subject("alice".into())
            .await
            .unwrap();
        assert_eq!(
            subject
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["higher", "lower"]
        );

        let recent = store.list_recent_visible_facts().await.unwrap();
        assert_eq!(
            recent
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["higher", "middle", "lower"]
        );

        let limited = store.list_recent_visible_facts_limited(2).await.unwrap();
        assert_eq!(
            limited
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["higher", "middle"]
        );
        assert!(
            store
                .list_recent_visible_facts_limited(0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn limited_recent_facts_filter_sensitive_rows_before_limit() {
        let (db, store) = store();
        db.insert_fact("user", "api_key", "hidden", "user", 1.0, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.95, &[])
            .unwrap();
        db.insert_fact("user", "token", "hidden-token", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "SQLite", "user", 0.85, &[])
            .unwrap();

        let facts = store.list_recent_visible_facts_limited(2).await.unwrap();

        assert_eq!(
            facts
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["Rust", "SQLite"]
        );
        assert!(
            facts
                .iter()
                .all(crate::recall::MemoryRetriever::visible_fact)
        );
    }

    #[tokio::test]
    async fn delete_facts_by_triple_supports_optional_object_scope() {
        let (db, store) = store();
        db.insert_fact("alice", "likes", "tea", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("alice", "likes", "coffee", "inferred", 0.8, &[])
            .unwrap();
        db.insert_fact("bob", "likes", "tea", "inferred", 0.7, &[])
            .unwrap();

        assert_eq!(
            store
                .delete_facts_by_triple("alice".into(), "likes".into(), Some("tea".into()))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .delete_facts_by_triple("alice".into(), "likes".into(), None)
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.list_recent_visible_facts().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn keyword_recall_fallback_returns_facts_and_actionable_empty_result() {
        let (db, store) = store();
        db.insert_fact("alice", "uses", "Rust", "inferred", 0.9, &[])
            .unwrap();

        let recall = store
            .recall_keyword(
                MemoryQuery::new("Rust", crate::recall::MemoryEntityKind::Fact, 5).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(recall.hits.len(), 1);
        assert!(recall.hits[0].text.contains("Rust"));

        let empty = store
            .recall_keyword(
                MemoryQuery::new("not present", crate::recall::MemoryEntityKind::Fact, 5).unwrap(),
            )
            .await
            .unwrap();
        assert!(empty.hits.is_empty());
        assert_eq!(
            empty.empty_reason,
            Some(crate::recall::MemoryRecallEmptyReason::NoHits)
        );
        assert_eq!(
            empty.diagnostics.unwrap().vector,
            crate::recall::MemoryRecallSourceStatus::NotConfigured
        );
    }
}
