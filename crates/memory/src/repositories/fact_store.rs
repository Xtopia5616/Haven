use crate::db::Database;
use crate::recall::MemoryRetriever;
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
}

#[cfg(test)]
mod tests {
    use super::MemoryFactStore;
    use crate::Database;
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
}
