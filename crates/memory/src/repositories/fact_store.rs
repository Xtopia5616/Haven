use crate::db::Database;
use crate::recall::normalize_memory_query;
#[cfg(any(test, feature = "test-support"))]
use crate::repositories::fact_graph::FactGraph;
use crate::repositories::facts::{Fact, FactSourceRef, FactValidity, MemoryFactKind};
use chrono::{DateTime, SecondsFormat, Utc};
use haven_common::types::is_canonical_id;
use std::sync::Arc;

/// Default number of facts returned by the paged long-term fact listing.
pub const DEFAULT_FACT_PAGE_SIZE: usize = 50;
/// Hard bound for one long-term fact listing response.
pub const MAX_FACT_PAGE_SIZE: usize = 100;
/// Hard bound for an explicit system-time fact snapshot.
pub const MAX_FACT_HISTORY_RESULTS: usize = 50;

/// Stable keyset position for a fact page. Ordering is `created_at DESC, id
/// DESC`; both fields come from the last returned row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactPageCursor {
    pub created_at: String,
    pub fact_id: String,
}

/// Bounded result of listing visible long-term facts.
#[derive(Debug, Clone)]
pub struct FactPage {
    pub facts: Vec<Fact>,
    pub next_cursor: Option<FactPageCursor>,
}

/// Bounded set of facts known at a requested system-recorded instant.
#[derive(Debug, Clone)]
pub struct FactHistorySnapshot {
    pub at: String,
    pub facts: Vec<Fact>,
    pub truncated: bool,
}

/// Bounded set of facts explicitly valid at a requested real-world instant.
#[derive(Debug, Clone)]
pub struct FactValiditySnapshot {
    pub at: String,
    pub facts: Vec<Fact>,
    pub truncated: bool,
}

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
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
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
    #[cfg(feature = "test-support")]
    pub fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    fn from_database(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Persist a prepared batch for Agent's direct fact-write test fixture.
    /// Production extraction uses the atomic cursor/marker commit methods.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn persist_inferred_batch_for_test(
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

    /// List a bounded page of visible facts, optionally restricted to an exact
    /// source value. Pages use stable `(created_at DESC, id DESC)` keyset
    /// order. Each request observes current database state; inserts newer than
    /// a cursor appear before that cursor and are seen after restarting the
    /// listing. Visibility is applied before the SQL limit so sensitive facts
    /// never consume page slots.
    pub async fn list_facts_page(
        &self,
        source: Option<String>,
        cursor: Option<FactPageCursor>,
        limit: usize,
    ) -> anyhow::Result<FactPage> {
        let limit = limit.min(MAX_FACT_PAGE_SIZE);
        if limit == 0 {
            return Ok(FactPage {
                facts: Vec::new(),
                next_cursor: None,
            });
        }
        if let Some(cursor) = cursor.as_ref() {
            anyhow::ensure!(
                is_canonical_id(&cursor.fact_id, "fact"),
                "fact page cursor fact_id must be canonical"
            );
            anyhow::ensure!(
                cursor.created_at.len() <= 64,
                "fact page cursor created_at is too long"
            );
            chrono::DateTime::parse_from_rfc3339(&cursor.created_at)
                .map_err(|_| anyhow::anyhow!("fact page cursor created_at must be RFC3339"))?;
        }
        self.db
            .run_blocking(move |db| {
                let source = source.as_deref().filter(|source| !source.is_empty());
                let cursor_key = cursor
                    .as_ref()
                    .map(|cursor| (cursor.created_at.as_str(), cursor.fact_id.as_str()));
                let facts = db.list_facts_page(source, cursor_key, limit + 1)?;
                let mut facts = crate::repositories::facts::filter_visible_facts(facts);
                let has_more = facts.len() > limit;
                facts.truncate(limit);
                let next_cursor = if has_more {
                    facts.last().map(|fact| FactPageCursor {
                        created_at: fact.created_at.clone(),
                        fact_id: fact.fact_id.clone(),
                    })
                } else {
                    None
                };
                Ok(FactPage { facts, next_cursor })
            })
            .await
    }

    /// Read facts that were present at a system-recorded RFC3339 instant.
    /// This is an explicit history path; ordinary search, list and recall keep
    /// reading only the current facts table.
    pub async fn list_visible_facts_as_of(
        &self,
        at: String,
        subject: Option<String>,
        limit: usize,
    ) -> anyhow::Result<FactHistorySnapshot> {
        anyhow::ensure!(at.len() <= 64, "history timestamp is too long");
        let at = DateTime::parse_from_rfc3339(&at)
            .map_err(|_| anyhow::anyhow!("history timestamp must be RFC3339"))?
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let limit = limit.min(MAX_FACT_HISTORY_RESULTS);
        if limit == 0 {
            return Ok(FactHistorySnapshot {
                at,
                facts: Vec::new(),
                truncated: false,
            });
        }
        let subject = subject
            .map(|subject| subject.trim().to_string())
            .filter(|subject| !subject.is_empty());
        self.db
            .run_blocking(move |db| {
                let rows = db.list_facts_as_of(&at, subject.as_deref(), limit + 1)?;
                let mut facts = crate::repositories::facts::filter_visible_facts(rows);
                let truncated = facts.len() > limit;
                facts.truncate(limit);
                Ok(FactHistorySnapshot {
                    at,
                    facts,
                    truncated,
                })
            })
            .await
    }

    /// Read a bounded snapshot by real-world validity time. Unknown intervals
    /// are omitted rather than inferred from when Haven first recorded them.
    pub async fn list_visible_facts_valid_at(
        &self,
        at: String,
        subject: Option<String>,
        limit: usize,
    ) -> anyhow::Result<FactValiditySnapshot> {
        let at = crate::repositories::facts::normalize_fact_validity(Some(&at), None)?
            .0
            .ok_or_else(|| anyhow::anyhow!("valid_at is required"))?;
        let limit = limit.min(MAX_FACT_HISTORY_RESULTS);
        if limit == 0 {
            return Ok(FactValiditySnapshot {
                at,
                facts: Vec::new(),
                truncated: false,
            });
        }
        let subject = subject
            .map(|subject| subject.trim().to_string())
            .filter(|subject| !subject.is_empty());
        self.db
            .run_blocking(move |db| {
                let rows = db.list_facts_valid_at(&at, subject.as_deref(), limit + 1)?;
                let mut facts = crate::repositories::facts::filter_visible_facts(rows);
                let truncated = facts.len() > limit;
                facts.truncate(limit);
                Ok(FactValiditySnapshot {
                    at,
                    facts,
                    truncated,
                })
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
        self.set_user_fact_with_kind(subject, predicate, object, tags, MemoryFactKind::Profile)
            .await
    }

    /// Persist a user-maintained profile or procedural memory.
    pub async fn set_user_fact_with_kind(
        &self,
        subject: String,
        predicate: String,
        object: String,
        tags: Vec<String>,
        memory_kind: MemoryFactKind,
    ) -> anyhow::Result<Fact> {
        self.set_user_fact_with_kind_and_validity(
            subject,
            predicate,
            object,
            tags,
            memory_kind,
            FactValidity::default(),
        )
        .await
    }

    /// Persist an explicitly stated fact with optional real-world validity
    /// bounds. Missing bounds remain unknown and are never inferred.
    pub async fn set_user_fact_with_kind_and_validity(
        &self,
        subject: String,
        predicate: String,
        object: String,
        tags: Vec<String>,
        memory_kind: MemoryFactKind,
        validity: FactValidity,
    ) -> anyhow::Result<Fact> {
        anyhow::ensure!(
            memory_kind != MemoryFactKind::Semantic,
            "user-maintained memory kind must be profile or procedural"
        );
        self.db
            .run_blocking(move |db| {
                let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
                db.set_user_fact_with_kind_and_validity(
                    &subject,
                    &predicate,
                    &object,
                    &tags,
                    memory_kind,
                    &validity,
                )
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

    /// Search facts with an optional exact subject scope and return at most
    /// `limit` visible matches. Query normalization, visibility and result
    /// bounding stay inside Memory; filtering precedes truncation so sensitive
    /// rows never consume result slots.
    pub async fn search_visible_facts_limited(
        &self,
        query: String,
        subject: Option<String>,
        limit: usize,
    ) -> anyhow::Result<Vec<Fact>> {
        let query = normalize_memory_query(&query)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.db
            .run_blocking(move |db| {
                let facts = db.search_facts_scoped(&query, subject.as_deref())?;
                Ok(crate::repositories::facts::filter_visible_facts(facts)
                    .into_iter()
                    .take(limit)
                    .collect())
            })
            .await
    }

    /// Return at most `limit` visible facts for one exact subject, retaining
    /// Database ordering and applying visibility before truncation.
    pub async fn list_visible_facts_for_subject_limited(
        &self,
        subject: String,
        limit: usize,
    ) -> anyhow::Result<Vec<Fact>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.db
            .run_blocking(move |db| {
                let facts = db.list_facts_by_subject(&subject)?;
                Ok(crate::repositories::facts::filter_visible_facts(facts)
                    .into_iter()
                    .take(limit)
                    .collect())
            })
            .await
    }

    /// Return at most `limit` visible facts across subjects, ordered by
    /// effective confidence and then by last-seen and creation time. Visibility
    /// is applied before truncation so sensitive rows do not consume result
    /// slots.
    pub async fn top_visible_facts_by_effective_confidence(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<Fact>> {
        let limit = limit.min(haven_common::config::MAX_FACTS_IN_EXTRACTION_PROMPT);
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.db
            .run_blocking(move |db| {
                let facts = db.list_facts()?;
                Ok(crate::repositories::facts::filter_visible_facts(facts)
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
}

#[cfg(test)]
mod tests {
    use super::{MAX_FACT_HISTORY_RESULTS, MAX_FACT_PAGE_SIZE, MemoryFactStore, MemoryFactWrite};
    use crate::Database;
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
            valid_from: None,
            valid_to: None,
        }
    }

    #[tokio::test]
    async fn fact_pages_are_bounded_stable_source_filtered_and_hide_sensitive_rows() {
        let (db, store) = store();
        let mut safe_ids = std::collections::HashSet::new();
        for index in 0..257 {
            let source = if index % 2 == 0 { "user" } else { "inferred" };
            let predicate = if index % 7 == 0 { "api_key" } else { "likes" };
            let subject = if index % 13 == 1 {
                "password-holder"
            } else {
                "user"
            };
            let object = if index % 17 == 2 {
                "sk-secret-value".to_string()
            } else {
                format!("item-{index}")
            };
            let hidden = index % 7 == 0 || index % 13 == 1 || index % 17 == 2;
            let fact = db
                .insert_fact(subject, predicate, &object, source, 0.8, &[])
                .unwrap();
            if !hidden {
                safe_ids.insert(fact.fact_id);
            }
        }
        db.conn()
            .expect("test database connection checkout failed")
            .execute("UPDATE facts SET created_at = '2026-10-01T00:00:00Z'", [])
            .unwrap();

        let first = store.list_facts_page(None, None, usize::MAX).await.unwrap();
        assert_eq!(first.facts.len(), MAX_FACT_PAGE_SIZE);
        assert!(first.next_cursor.is_some());

        let mut all_facts = first.facts;
        let mut cursor = first.next_cursor;
        while let Some(next_cursor) = cursor {
            let page = store
                .list_facts_page(None, Some(next_cursor), MAX_FACT_PAGE_SIZE)
                .await
                .unwrap();
            all_facts.extend(page.facts);
            cursor = page.next_cursor;
        }
        assert_eq!(all_facts.len(), safe_ids.len());
        assert!(
            all_facts
                .iter()
                .all(|fact| safe_ids.contains(&fact.fact_id))
        );
        assert_eq!(
            all_facts
                .iter()
                .map(|fact| fact.fact_id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            all_facts.len(),
            "keyset pages must not repeat a fact"
        );
        assert!(all_facts.windows(2).all(|pair| {
            pair[0].created_at > pair[1].created_at
                || (pair[0].created_at == pair[1].created_at && pair[0].fact_id > pair[1].fact_id)
        }));

        let user_page = store
            .list_facts_page(Some("user".into()), None, 3)
            .await
            .unwrap();
        assert_eq!(user_page.facts.len(), 3);
        assert!(user_page.facts.iter().all(|fact| fact.source == "user"));
    }

    #[tokio::test]
    async fn fact_page_rejects_invalid_cursor_identity_and_timestamp() {
        let (_db, store) = store();
        let invalid_id = super::FactPageCursor {
            created_at: "2026-10-01T00:00:00Z".into(),
            fact_id: "fact-invalid".into(),
        };
        assert!(
            store
                .list_facts_page(None, Some(invalid_id), 10)
                .await
                .is_err()
        );

        let invalid_timestamp = super::FactPageCursor {
            created_at: "yesterday".into(),
            fact_id: "fact-0123456789abcdef0123456789abcdef".into(),
        };
        assert!(
            store
                .list_facts_page(None, Some(invalid_timestamp), 10)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn inferred_batch_empty_input_returns_false() {
        let (_db, store) = store();

        assert!(
            !store
                .persist_inferred_batch_for_test(Vec::new(), 0.55)
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
        let source_message_id = haven_common::types::new_id("msg");
        reinforced.source_ref = Some(FactSourceRef {
            message_id: source_message_id.clone(),
            snippet: "I use Rust at work.".into(),
        });
        reinforced.durability = 0.8;
        let mut inserted = inferred_write("user", "uses", "SQLite");
        inserted.confidence = 0.55;

        assert!(
            store
                .persist_inferred_batch_for_test(vec![reinforced, inserted], 0.55)
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
            source_message_id
        );
        assert_eq!(
            rust.source_ref.as_ref().unwrap().snippet,
            "I use Rust at work."
        );
        assert!(facts.iter().any(|fact| fact.object == "SQLite"));
    }

    #[tokio::test]
    async fn inferred_batch_uses_memory_predicate_policy_for_low_confidence_updates() {
        let (db, store) = store();
        db.insert_fact("user", "works_at", "Old Company", "inferred", 0.8, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Coffee", "inferred", 0.8, &[])
            .unwrap();

        let mut single_value_update = inferred_write("user", "works_at", "New Company");
        single_value_update.confidence = 0.5;
        let mut multi_value_fact = inferred_write("user", "likes", "Rust");
        multi_value_fact.confidence = 0.5;

        assert!(
            store
                .persist_inferred_batch_for_test(vec![single_value_update, multi_value_fact], 0.55)
                .await
                .unwrap()
        );

        let facts = db.list_facts_by_subject("user").unwrap();
        assert!(
            facts
                .iter()
                .any(|fact| { fact.predicate == "works_at" && fact.object == "New Company" })
        );
        assert!(facts.iter().any(|fact| {
            fact.predicate == "works_at" && fact.object == "Old Company" && fact.confidence < 0.8
        }));
        assert!(!facts.iter().any(|fact| fact.object == "Rust"));
    }

    #[tokio::test]
    async fn inferred_batch_failure_rolls_back_prior_writes() {
        let (db, store) = store();
        db.conn()
            .expect("test database connection checkout failed")
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
            .persist_inferred_batch_for_test(
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
            .expect("test database connection checkout failed")
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
                message_id: haven_common::types::new_id("msg"),
                snippet: "password=hidden".into(),
            }),
            1.0,
        )
        .unwrap();
        {
            let conn = db.conn().expect("test database connection checkout failed");
            conn.execute(
                "UPDATE facts SET provenance_snippet = 'password=hidden' WHERE object = 'safe value'",
                [],
            )
            .unwrap();
        }

        let all = store.list_facts_page(None, None, 10).await.unwrap().facts;
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

        let empty_source = store
            .list_facts_page(Some(String::new()), None, 10)
            .await
            .unwrap()
            .facts;
        assert_eq!(empty_source.len(), all.len());

        let user_source = store
            .list_facts_page(Some("user".into()), None, 10)
            .await
            .unwrap()
            .facts;
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

        store.delete_fact(fact.fact_id.clone()).await.unwrap();
        assert!(
            store
                .list_facts_page(None, None, 10)
                .await
                .unwrap()
                .facts
                .is_empty()
        );
    }

    #[tokio::test]
    async fn user_memory_write_defaults_to_profile_and_supports_procedures() {
        let (_db, store) = store();
        let profile = store
            .set_user_fact("user".into(), "likes".into(), "tea".into(), vec![])
            .await
            .unwrap();
        assert_eq!(profile.memory_kind, crate::MemoryFactKind::Profile);

        let procedure = store
            .set_user_fact_with_kind(
                "user".into(),
                "prepare_tea".into(),
                "heat water, steep leaves, serve".into(),
                vec![],
                crate::MemoryFactKind::Procedural,
            )
            .await
            .unwrap();
        assert_eq!(procedure.memory_kind, crate::MemoryFactKind::Procedural);

        let error = store
            .set_user_fact_with_kind(
                "user".into(),
                "not_allowed".into(),
                "semantic writes are inferred".into(),
                vec![],
                crate::MemoryFactKind::Semantic,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("profile or procedural"));
    }

    #[tokio::test]
    async fn history_snapshot_restores_replaced_fact_until_explicit_forget() {
        let (db, store) = store();
        let old = store
            .set_user_fact("user".into(), "city".into(), "Oslo".into(), vec![])
            .await
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET created_at = '2020-01-01T00:00:00Z', recorded_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1",
                rusqlite::params![old.fact_id],
            )
            .unwrap();

        let before_correction = store
            .list_visible_facts_as_of("2020-01-02T00:00:00Z".into(), Some("user".into()), 10)
            .await
            .unwrap();
        assert_eq!(before_correction.facts.len(), 1);
        assert_eq!(before_correction.facts[0].object, "Oslo");
        assert_eq!(before_correction.at, "2020-01-02T00:00:00.000Z");

        store
            .set_user_fact("user".into(), "city".into(), "Tokyo".into(), vec![])
            .await
            .unwrap();

        let before = store
            .list_visible_facts_as_of("2020-01-02T00:00:00Z".into(), None, 10)
            .await
            .unwrap();
        assert_eq!(
            before
                .facts
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["Oslo"]
        );
        let after = store
            .list_visible_facts_as_of("2100-01-01T00:00:00Z".into(), None, 10)
            .await
            .unwrap();
        assert_eq!(
            after
                .facts
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["Tokyo"]
        );

        let current = store
            .list_visible_facts_for_subject_limited("user".into(), 10)
            .await
            .unwrap();
        assert_eq!(
            current
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["Tokyo"]
        );

        assert_eq!(
            store
                .delete_facts_by_triple("user".into(), "city".into(), None)
                .await
                .unwrap(),
            1
        );
        let forgotten = store
            .list_visible_facts_as_of("2020-01-02T00:00:00Z".into(), None, 10)
            .await
            .unwrap();
        assert!(forgotten.facts.is_empty());
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM fact_history", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn history_snapshot_validates_time_and_bounds_visible_rows() {
        let (db, store) = store();
        assert!(
            store
                .list_visible_facts_as_of("not a timestamp".into(), None, 10)
                .await
                .is_err()
        );
        for index in 0..(MAX_FACT_HISTORY_RESULTS + 3) {
            db.insert_fact(
                "alice",
                "likes",
                &format!("item-{index}"),
                "inferred",
                0.9,
                &[],
            )
            .unwrap();
        }
        db.insert_fact("alice", "api_key", "hidden", "inferred", 1.0, &[])
            .unwrap();

        let snapshot = store
            .list_visible_facts_as_of(
                "2100-01-01T00:00:00Z".into(),
                Some("alice".into()),
                usize::MAX,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.facts.len(), MAX_FACT_HISTORY_RESULTS);
        assert!(snapshot.truncated);
        assert!(
            snapshot
                .facts
                .iter()
                .all(crate::repositories::facts::is_visible_fact)
        );
        assert!(snapshot.facts.iter().all(|fact| fact.subject == "alice"));
    }

    #[tokio::test]
    async fn valid_at_uses_explicit_intervals_and_single_value_corrections_close_old_ranges() {
        let (_db, store) = store();
        let old = store
            .set_user_fact_with_kind_and_validity(
                "user".into(),
                "employer".into(),
                "Acme".into(),
                vec![],
                crate::MemoryFactKind::Profile,
                crate::FactValidity {
                    valid_from: Some("2024-01-01".into()),
                    valid_to: None,
                },
            )
            .await
            .unwrap();
        store
            .set_user_fact_with_kind_and_validity(
                "user".into(),
                "employer".into(),
                "Northwind".into(),
                vec![],
                crate::MemoryFactKind::Profile,
                crate::FactValidity {
                    valid_from: Some("2025-01-01".into()),
                    valid_to: None,
                },
            )
            .await
            .unwrap();
        store
            .set_user_fact("user".into(), "likes".into(), "tea".into(), vec![])
            .await
            .unwrap();

        let before = store
            .list_visible_facts_valid_at("2024-12-31".into(), Some("user".into()), 10)
            .await
            .unwrap();
        assert_eq!(before.at, "2024-12-31T00:00:00.000Z");
        assert_eq!(before.facts.len(), 1);
        assert_eq!(before.facts[0].fact_id, old.fact_id);
        assert_eq!(before.facts[0].object, "Acme");
        assert_eq!(
            before.facts[0].valid_to.as_deref(),
            Some("2025-01-01T00:00:00.000Z")
        );

        let boundary = store
            .list_visible_facts_valid_at("2025-01-01".into(), None, 10)
            .await
            .unwrap();
        assert_eq!(
            boundary
                .facts
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["Northwind"],
            "valid_to is exclusive and the corrected value starts at valid_from"
        );
    }

    #[tokio::test]
    async fn out_of_order_single_value_correction_stops_before_the_known_later_value() {
        let (_db, store) = store();
        store
            .set_user_fact_with_kind_and_validity(
                "user".into(),
                "employer".into(),
                "Northwind".into(),
                vec![],
                crate::MemoryFactKind::Profile,
                crate::FactValidity {
                    valid_from: Some("2025-01-01".into()),
                    valid_to: None,
                },
            )
            .await
            .unwrap();
        store
            .set_user_fact_with_kind_and_validity(
                "user".into(),
                "employer".into(),
                "Acme".into(),
                vec![],
                crate::MemoryFactKind::Profile,
                crate::FactValidity {
                    valid_from: Some("2024-01-01".into()),
                    valid_to: None,
                },
            )
            .await
            .unwrap();

        let before = store
            .list_visible_facts_valid_at("2024-06-01".into(), None, 10)
            .await
            .unwrap();
        assert_eq!(before.facts.len(), 1);
        assert_eq!(before.facts[0].object, "Acme");
        assert_eq!(
            before.facts[0].valid_to.as_deref(),
            Some("2025-01-01T00:00:00.000Z")
        );

        let later = store
            .list_visible_facts_valid_at("2025-06-01".into(), None, 10)
            .await
            .unwrap();
        assert_eq!(later.facts.len(), 1);
        assert_eq!(later.facts[0].object, "Northwind");
    }

    #[tokio::test]
    async fn explicit_forget_erases_valid_time_history_and_unknown_intervals_are_omitted() {
        let (_db, store) = store();
        store
            .set_user_fact_with_kind_and_validity(
                "user".into(),
                "city".into(),
                "Oslo".into(),
                vec![],
                crate::MemoryFactKind::Profile,
                crate::FactValidity {
                    valid_from: Some("2020-01-01".into()),
                    valid_to: None,
                },
            )
            .await
            .unwrap();
        store
            .set_user_fact("user".into(), "likes".into(), "tea".into(), vec![])
            .await
            .unwrap();
        let known = store
            .list_visible_facts_valid_at("2021-01-01".into(), None, 10)
            .await
            .unwrap();
        assert_eq!(known.facts.len(), 1);
        assert_eq!(known.facts[0].object, "Oslo");

        store
            .delete_facts_by_triple("user".into(), "city".into(), None)
            .await
            .unwrap();
        let forgotten = store
            .list_visible_facts_valid_at("2021-01-01".into(), None, 10)
            .await
            .unwrap();
        assert!(forgotten.facts.is_empty());
    }

    #[tokio::test]
    async fn deleting_by_id_and_clearing_facts_purge_history() {
        let (db, store) = store();
        let old = store
            .set_user_fact("user".into(), "city".into(), "Oslo".into(), vec![])
            .await
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET created_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
                rusqlite::params![old.fact_id],
            )
            .unwrap();
        let current = store
            .set_user_fact("user".into(), "city".into(), "Tokyo".into(), vec![])
            .await
            .unwrap();

        store.delete_fact(current.fact_id).await.unwrap();
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM fact_history", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "deleting a fact by id must erase its historical versions"
        );

        store
            .set_user_fact("user".into(), "city".into(), "Oslo".into(), vec![])
            .await
            .unwrap();
        store
            .set_user_fact("user".into(), "city".into(), "Tokyo".into(), vec![])
            .await
            .unwrap();
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM fact_history", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );

        store.clear_facts().await.unwrap();
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM fact_history", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "clearing current facts must erase history as well"
        );
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
            .search_visible_facts_limited(" green   tea ".into(), None, usize::MAX)
            .await
            .unwrap();
        let expected_order = db
            .search_facts_scoped("green tea", None)
            .unwrap()
            .into_iter()
            .filter(crate::repositories::facts::is_visible_fact)
            .map(|fact| fact.fact_id)
            .collect::<Vec<_>>();
        assert_eq!(
            all.iter()
                .map(|fact| fact.fact_id.clone())
                .collect::<Vec<_>>(),
            expected_order
        );
        assert_eq!(all.len(), 2);

        let first_match = store
            .search_visible_facts_limited("green tea".into(), None, 1)
            .await
            .unwrap();
        assert_eq!(first_match.len(), 1);
        assert_eq!(first_match[0].fact_id, expected_order[0]);
        assert!(
            store
                .search_visible_facts_limited("   ".into(), None, 0)
                .await
                .is_err(),
            "query validation must still run when the requested limit is zero"
        );

        let scoped = store
            .search_visible_facts_limited("green tea".into(), Some("bob".into()), 1)
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].subject, "bob");
    }

    #[tokio::test]
    async fn subject_and_ranked_lists_preserve_database_order() {
        let (db, store) = store();
        db.insert_fact("alice", "likes", "lower", "inferred", 0.4, &[])
            .unwrap();
        db.insert_fact("alice", "likes", "higher", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("alice", "api_key", "hidden", "inferred", 1.0, &[])
            .unwrap();
        db.insert_fact("bob", "likes", "middle", "inferred", 0.7, &[])
            .unwrap();

        let subject = store
            .list_visible_facts_for_subject_limited("alice".into(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            subject
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["higher", "lower"]
        );
        let top_subject = store
            .list_visible_facts_for_subject_limited("alice".into(), 1)
            .await
            .unwrap();
        assert_eq!(top_subject.len(), 1);
        assert_eq!(top_subject[0].object, "higher");
        assert!(
            store
                .list_visible_facts_for_subject_limited("alice".into(), 0)
                .await
                .unwrap()
                .is_empty()
        );

        let all_ranked = store
            .top_visible_facts_by_effective_confidence(usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            all_ranked
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["higher", "middle", "lower"]
        );

        let top_ranked = store
            .top_visible_facts_by_effective_confidence(2)
            .await
            .unwrap();
        assert_eq!(
            top_ranked
                .iter()
                .map(|fact| fact.object.as_str())
                .collect::<Vec<_>>(),
            ["higher", "middle"]
        );
        assert!(
            store
                .top_visible_facts_by_effective_confidence(0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn top_ranked_facts_filter_sensitive_rows_before_limit() {
        let (db, store) = store();
        db.insert_fact("user", "api_key", "hidden", "user", 1.0, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.95, &[])
            .unwrap();
        db.insert_fact("user", "token", "hidden-token", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "SQLite", "user", 0.85, &[])
            .unwrap();

        let facts = store
            .top_visible_facts_by_effective_confidence(2)
            .await
            .unwrap();

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
                .all(crate::repositories::facts::is_visible_fact)
        );
    }

    #[tokio::test]
    async fn top_ranked_facts_enforce_the_extraction_prompt_hard_ceiling() {
        let (db, store) = store();
        for index in 0..(haven_common::config::MAX_FACTS_IN_EXTRACTION_PROMPT + 5) {
            db.insert_fact("user", "likes", &format!("item-{index}"), "user", 0.5, &[])
                .unwrap();
        }

        let facts = store
            .top_visible_facts_by_effective_confidence(usize::MAX)
            .await
            .unwrap();

        assert_eq!(
            facts.len(),
            haven_common::config::MAX_FACTS_IN_EXTRACTION_PROMPT
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
        assert_eq!(
            store
                .top_visible_facts_by_effective_confidence(usize::MAX)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
