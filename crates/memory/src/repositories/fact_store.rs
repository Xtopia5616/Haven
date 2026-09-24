use crate::db::Database;
use crate::recall::{MemoryQuery, MemoryRecall, MemoryRetriever, normalize_memory_query};
use crate::repositories::facts::Fact;
use std::sync::Arc;

/// Async application-facing boundary for user-managed memory facts.
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
                let facts = db.get_facts(&subject)?;
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

#[cfg(test)]
mod tests {
    use super::MemoryFactStore;
    use crate::Database;
    use crate::recall::MemoryQuery;
    use crate::repositories::facts::FactSourceRef;
    use std::sync::Arc;

    fn store() -> (Arc<Database>, MemoryFactStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = MemoryFactStore::new(db.clone());
        (db, store)
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
            .recall_keyword(MemoryQuery::new("Rust", crate::recall::MemoryKind::Fact, 5).unwrap())
            .await
            .unwrap();
        assert_eq!(recall.hits.len(), 1);
        assert!(recall.hits[0].text.contains("Rust"));

        let empty = store
            .recall_keyword(
                MemoryQuery::new("not present", crate::recall::MemoryKind::Fact, 5).unwrap(),
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
