use crate::db::Database;
use crate::repositories::messages::Message;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fact {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub created_at: String,
}

/// A fact extracted by the rule-based inference engine, before it is
/// persisted to the database.
#[derive(Debug, Clone)]
pub struct InferredFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub tags: Vec<String>,
}

/// Parse a JSON-encoded tag array from a DB string column.
fn parse_tags(tags_str: &str) -> Vec<String> {
    serde_json::from_str(tags_str).unwrap_or_default()
}

/// Serialize a slice of tag strings into a JSON array string for storage.
fn serialize_tags(tags: &[&str]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".into())
}

/// Map a rusqlite Row (with the standard 8-column SELECT order) to a Fact.
/// Shared by all query methods to avoid drift when columns change.
fn fact_from_row(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
    let tags_str: String = row.get(6)?;
    Ok(Fact {
        id: row.get(0)?,
        subject: row.get(1)?,
        predicate: row.get(2)?,
        object: row.get(3)?,
        source: row.get(4)?,
        confidence: row.get(5)?,
        tags: parse_tags(&tags_str),
        created_at: row.get(7)?,
    })
}

/// Map a predicate to a default tag set used by the inference engine.
fn tags_for_predicate(predicate: &str) -> Vec<String> {
    match predicate {
        "likes" | "dislikes" | "uses" => vec!["preference".into()],
        "project_path" => vec!["workspace".into()],
        "name" | "works_at" => vec!["identity".into()],
        _ => vec![],
    }
}

impl Database {
    pub fn insert_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        confidence: f64,
        tags: &[&str],
    ) -> anyhow::Result<Fact> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_json = serialize_tags(tags);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, source, confidence, created_at, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![id, subject, predicate, object, source, confidence, now, tags_json],
        )?;
        self.cache_invalidate_facts(subject);
        Ok(Fact {
            id,
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            source: source.into(),
            confidence,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            created_at: now,
        })
    }

    pub fn get_facts(&self, subject: &str) -> anyhow::Result<Vec<Fact>> {
        if let Some(cached) = self.cache_get_facts(subject) {
            return Ok(cached);
        }
        let key = format!("_facts_{}", subject);
        let cache_gen = self.cache_generation(&key);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at
             FROM facts WHERE subject = ?1 ORDER BY confidence DESC, created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![subject], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        self.cache_put_facts(subject, facts.clone(), 60, cache_gen);
        Ok(facts)
    }

    pub fn list_facts(&self) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at
             FROM facts ORDER BY confidence DESC, created_at DESC",
        )?;
        let rows = stmt.query_map([], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    pub fn list_facts_by_source(&self, source: &str) -> anyhow::Result<Vec<Fact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at
             FROM facts WHERE source = ?1 ORDER BY confidence DESC, created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![source], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// Full-text keyword search across subject, predicate, object, and tags.
    pub fn search_facts(&self, query: &str) -> anyhow::Result<Vec<Fact>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at
             FROM facts
             WHERE subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1 OR tags LIKE ?1
             ORDER BY confidence DESC, created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// Return all facts that carry the given tag.
    pub fn get_facts_by_tag(&self, tag: &str) -> anyhow::Result<Vec<Fact>> {
        let pattern = format!("%\"{}\"%", tag);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, source, confidence, tags, created_at
             FROM facts WHERE tags LIKE ?1
             ORDER BY confidence DESC, created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    pub fn delete_fact(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        // Query subject before deletion so we can invalidate the right cache.
        let subject: Option<String> = conn
            .query_row(
                "SELECT subject FROM facts WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .ok();
        conn.execute("DELETE FROM facts WHERE id = ?1", rusqlite::params![id])?;
        if let Some(s) = subject {
            self.cache_invalidate_facts(&s);
        }
        Ok(())
    }

    pub fn dedup_facts(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        // Find duplicates (same subject, predicate, object, tags) and keep
        // the one with highest confidence.  The correlated subquery picks
        // exactly the id of the max-confidence row per group, so all other
        // rows in the group are selected for deletion.
        let mut stmt = conn.prepare(
            "SELECT id FROM facts
             WHERE id NOT IN (
                 SELECT f1.id FROM facts f1
                 WHERE f1.confidence = (
                     SELECT MAX(f2.confidence) FROM facts f2
                     WHERE f2.subject = f1.subject
                       AND f2.predicate = f1.predicate
                       AND f2.object = f1.object
                       AND f2.tags = f1.tags
                 )
             )",
        )?;
        let mut ids_to_delete: Vec<String> = Vec::new();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            ids_to_delete.push(row?);
        }
        let count = ids_to_delete.len() as u64;
        for id in &ids_to_delete {
            conn.execute("DELETE FROM facts WHERE id = ?1", rusqlite::params![id])?;
        }
        self.cache_invalidate_facts("user");
        Ok(count)
    }

    pub fn flush_low_confidence(&self, threshold: f64) -> anyhow::Result<u64> {
        let conn = self.conn();
        let count = conn.execute(
            "DELETE FROM facts WHERE confidence < ?1",
            rusqlite::params![threshold],
        )?;
        self.cache_invalidate_facts("user");
        Ok(count as u64)
    }

    pub fn infer_facts_from_messages(&self, messages: &[Message]) -> Vec<InferredFact> {
        let mut facts: Vec<InferredFact> = Vec::new();
        let mut corrected_predicates: Vec<String> = Vec::new();

        for msg in messages {
            let content = msg.content.to_lowercase();
            let content_orig = &msg.content;

            // Rule 1: "I like/love/prefer X" -> ("user", "likes", "X", 0.9)
            for pattern in &["i like ", "i love ", "i prefer ", "my favorite "] {
                if let Some(idx) = content.find(pattern) {
                    let obj = content_orig[idx + pattern.len()..]
                        .split(['.', ',', '!', '?'])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !obj.is_empty() {
                        facts.push(InferredFact {
                            subject: "user".into(),
                            predicate: "likes".into(),
                            object: obj,
                            confidence: 0.9,
                            tags: tags_for_predicate("likes"),
                        });
                    }
                }
            }

            // Rule 2: "I don't like / I hate X" -> ("user", "dislikes", "X", 0.8)
            for pattern in &["i don't like ", "i hate ", "i dislike "] {
                if let Some(idx) = content.find(pattern) {
                    let obj = content_orig[idx + pattern.len()..]
                        .split(['.', ',', '!', '?'])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !obj.is_empty() {
                        facts.push(InferredFact {
                            subject: "user".into(),
                            predicate: "dislikes".into(),
                            object: obj,
                            confidence: 0.8,
                            tags: tags_for_predicate("dislikes"),
                        });
                    }
                }
            }

            // Rule 3: "my project at /path" -> ("user", "project_path", "/path", 0.7)
            let path_patterns = ["my project at ", "my project is at ", "my project is in ", "my code is at ", "my workspace is "];
            for pattern in &path_patterns {
                if let Some(idx) = content.find(pattern) {
                    let obj = content_orig[idx + pattern.len()..]
                        .split(['.', ',', '!', '?'])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !obj.is_empty() {
                        facts.push(InferredFact {
                            subject: "user".into(),
                            predicate: "project_path".into(),
                            object: obj,
                            confidence: 0.7,
                            tags: tags_for_predicate("project_path"),
                        });
                    }
                }
            }

            // Rule 4: "actually I prefer Y" -> correction: lower confidence of existing facts with same predicate
            if content.contains("actually") && (content.contains("prefer") || content.contains("use") || content.contains("want")) {
                corrected_predicates.push("likes".into());
            }

            // Rule 5: "my name is X" or "I am X" -> ("user", "name", "X", 0.85)
            for pattern in &["my name is ", "i am ", "call me ", "i'm ", "i am called "] {
                if let Some(idx) = content.find(pattern) {
                    let after = &content[idx + pattern.len()..];
                    let stop_words = ["looking", "trying", "going", "using", "working", "doing"];
                    let first_word = after.split_whitespace().next().unwrap_or("");
                    if !stop_words.contains(&first_word) {
                        let obj = content_orig[idx + pattern.len()..]
                            .split(['.', ',', '!', '?'])
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !obj.is_empty() && obj.split_whitespace().count() <= 4 {
                            facts.push(InferredFact {
                                subject: "user".into(),
                                predicate: "name".into(),
                                object: obj,
                                confidence: 0.85,
                                tags: tags_for_predicate("name"),
                            });
                        }
                    }
                }
            }

            // Rule 6: "I use X" -> ("user", "uses", "X", 0.7)
            if let Some(idx) = content.find("i use ") {
                let obj = content_orig[idx + 6..]
                    .split(['.', ',', '!', '?'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !obj.is_empty() {
                    facts.push(InferredFact {
                        subject: "user".into(),
                        predicate: "uses".into(),
                        object: obj,
                        confidence: 0.7,
                        tags: tags_for_predicate("uses"),
                    });
                }
            }

            // Rule 7: "I work at/for X" -> ("user", "works_at", "X", 0.75)
            for pattern in &["i work at ", "i work for ", "i work in "] {
                if let Some(idx) = content.find(pattern) {
                    let obj = content_orig[idx + pattern.len()..]
                        .split(['.', ',', '!', '?'])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !obj.is_empty() {
                        facts.push(InferredFact {
                            subject: "user".into(),
                            predicate: "works_at".into(),
                            object: obj,
                            confidence: 0.75,
                            tags: tags_for_predicate("works_at"),
                        });
                    }
                }
            }
        }

        // Lower confidence of corrected facts
        if !corrected_predicates.is_empty() {
            let conn = self.conn();
            for pred in &corrected_predicates {
                let _ = conn.execute(
                    "UPDATE facts SET confidence = confidence * 0.5 WHERE subject = 'user' AND predicate = ?1 AND source = 'inferred'",
                    rusqlite::params![pred],
                );
            }
        }

        facts
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn make_message(content: &str) -> crate::repositories::messages::Message {
        crate::repositories::messages::Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "s1".into(),
            role: "user".into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
        }
    }

    #[test]
    fn test_insert_fact() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        assert!(!fact.id.is_empty());
        assert_eq!(fact.subject, "user");
        assert_eq!(fact.predicate, "likes");
        assert_eq!(fact.object, "Rust");
        assert_eq!(fact.source, "user");
        assert_eq!(fact.confidence, 0.9);
        assert_eq!(fact.tags, vec!["preference"]);
        assert!(!fact.created_at.is_empty());
    }

    #[test]
    fn test_insert_fact_no_tags() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "custom", "some value", "user", 0.5, &[])
            .unwrap();
        assert!(fact.tags.is_empty());
    }

    #[test]
    fn test_get_facts_by_subject() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "user", 0.7, &["preference"])
            .unwrap();
        db.insert_fact("other", "likes", "Go", "user", 0.5, &[])
            .unwrap();

        let user_facts = db.get_facts("user").unwrap();
        assert_eq!(user_facts.len(), 2);
        assert_eq!(user_facts[0].object, "Rust");
        assert_eq!(user_facts[1].object, "Python");
        assert_eq!(user_facts[0].tags, vec!["preference"]);
    }

    #[test]
    fn test_get_facts_ordering() {
        let db = create_db();
        db.insert_fact("user", "likes", "A", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "likes", "B", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "C", "user", 0.7, &[])
            .unwrap();

        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 3);
        assert!(facts[0].confidence >= facts[1].confidence);
        assert!(facts[1].confidence >= facts[2].confidence);
    }

    #[test]
    fn test_list_facts() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("other", "dislikes", "Java", "user", 0.5, &[])
            .unwrap();

        let all = db.list_facts().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].confidence >= all[1].confidence);
    }

    #[test]
    fn test_list_facts_by_source() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "inferred", 0.7, &[])
            .unwrap();

        let user_sourced = db.list_facts_by_source("user").unwrap();
        assert_eq!(user_sourced.len(), 1);
        assert_eq!(user_sourced[0].object, "Rust");

        let inferred = db.list_facts_by_source("inferred").unwrap();
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].object, "Python");

        let none = db.list_facts_by_source("unknown").unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn test_search_facts() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust programming", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "user", 0.5, &["preference"])
            .unwrap();
        db.insert_fact("user", "uses", "TypeScript", "inferred", 0.7, &["preference"])
            .unwrap();

        let results = db.search_facts("programming").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Rust programming");

        let results = db.search_facts("Rust").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_facts("Java").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_facts("nonexistent").unwrap();
        assert!(results.is_empty());

        let results = db.search_facts("likes").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_facts_by_tag() {
        let db = create_db();
        db.insert_fact("user", "name", "Alice", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "project_path", "/home/app", "user", 0.8, &["workspace"])
            .unwrap();

        let results = db.search_facts("preference").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Rust");

        let results = db.search_facts("identity").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Alice");

        let results = db.search_facts("workspace").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "/home/app");
    }

    #[test]
    fn test_get_facts_by_tag() {
        let db = create_db();
        db.insert_fact("user", "name", "Alice", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "works_at", "Acme", "user", 0.8, &["identity"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();

        let results = db.get_facts_by_tag("identity").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.get_facts_by_tag("preference").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.get_facts_by_tag("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_get_facts_by_tag_partial_match_excluded() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Go", "user", 0.8, &["preferences"])
            .unwrap();

        // "preference" should NOT match "preferences" — tag matching is exact.
        let results = db.get_facts_by_tag("preference").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].object, "Rust");
    }

    #[test]
    fn test_delete_fact_existing() {
        let db = create_db();
        let fact = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.delete_fact(&fact.id).unwrap();
        let remaining = db.get_facts("user").unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_delete_fact_non_existing() {
        let db = create_db();
        let result = db.delete_fact("non-existent-id");
        assert!(result.is_ok());
    }

    #[test]
    fn test_dedup_facts_keeps_one_per_triple() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.3, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "user", 0.7, &[])
            .unwrap();

        let count = db.dedup_facts().unwrap();
        assert!(count > 0);

        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 2);
        let rust_facts: Vec<_> = remaining.iter().filter(|f| f.object == "Rust").collect();
        assert_eq!(rust_facts.len(), 1);
    }

    #[test]
    fn test_dedup_facts_keeps_highest_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.3, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.5, &[])
            .unwrap();

        db.dedup_facts().unwrap();

        let remaining = db.get_facts("user").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].confidence, 0.9, "should keep highest confidence");
    }

    #[test]
    fn test_dedup_facts_different_tags_not_merged() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.7, &["workspace"])
            .unwrap();

        let count = db.dedup_facts().unwrap();
        assert_eq!(count, 0);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_dedup_facts_no_duplicates() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "user", 0.5, &[])
            .unwrap();

        let count = db.dedup_facts().unwrap();
        assert_eq!(count, 0);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_flush_low_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Java", "inferred", 0.3, &[])
            .unwrap();

        let count = db.flush_low_confidence(0.6).unwrap();
        assert_eq!(count, 2);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].object, "Rust");
    }

    #[test]
    fn test_flush_low_confidence_none_below() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();

        let count = db.flush_low_confidence(0.5).unwrap();
        assert_eq!(count, 0);
        let remaining = db.list_facts().unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_flush_low_confidence_all_below() {
        let db = create_db();
        db.insert_fact("user", "likes", "A", "inferred", 0.1, &[])
            .unwrap();
        db.insert_fact("user", "likes", "B", "inferred", 0.2, &[])
            .unwrap();

        let count = db.flush_low_confidence(1.0).unwrap();
        assert_eq!(count, 2);
        let remaining = db.list_facts().unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_infer_rule_likes() {
        let db = create_db();
        let msgs = vec![make_message("I like Rust programming very much.")];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].subject, "user");
        assert_eq!(facts[0].predicate, "likes");
        assert_eq!(facts[0].object, "Rust programming very much");
        assert_eq!(facts[0].confidence, 0.9);
        assert_eq!(facts[0].tags, vec!["preference"]);
    }

    #[test]
    fn test_infer_rule_likes_variants() {
        let db = create_db();
        let msgs = vec![
            make_message("I love TypeScript."),
            make_message("I prefer dark themes."),
            make_message("My favorite language is Go."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "likes");
            assert_eq!(f.confidence, 0.9);
            assert_eq!(f.tags, vec!["preference"]);
        }
    }

    #[test]
    fn test_infer_rule_dislikes() {
        let db = create_db();
        let msgs = vec![
            make_message("I don't like Java at all."),
            make_message("I hate slow builds."),
            make_message("I dislike complicated configs."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "dislikes");
            assert_eq!(f.confidence, 0.8);
            assert_eq!(f.tags, vec!["preference"]);
        }
    }

    #[test]
    fn test_infer_rule_project_path() {
        let db = create_db();
        let msgs = vec![
            make_message("my project at /home/user/myapp works fine."),
            make_message("my project is at /workspace/backend"),
            make_message("my code is at /Users/dev/src"),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "project_path");
            assert_eq!(f.confidence, 0.7);
            assert_eq!(f.tags, vec!["workspace"]);
        }
        assert_eq!(facts[0].object, "/home/user/myapp works fine");
        assert_eq!(facts[1].object, "/workspace/backend");
        assert_eq!(facts[2].object, "/Users/dev/src");
    }

    #[test]
    fn test_infer_rule_name() {
        let db = create_db();
        let msgs = vec![
            make_message("my name is Alice Johnson."),
            make_message("I am Bob"),
            make_message("call me Charlie."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 3);
        for f in &facts {
            assert_eq!(f.predicate, "name");
            assert_eq!(f.confidence, 0.85);
            assert_eq!(f.tags, vec!["identity"]);
        }
    }

    #[test]
    fn test_infer_rule_name_skips_action_words() {
        let db = create_db();
        let msgs = vec![
            make_message("I am looking for a new laptop."),
            make_message("I am trying to fix the build."),
            make_message("I am working on a feature."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        for f in &facts {
            assert_ne!(f.predicate, "name");
        }
    }

    #[test]
    fn test_infer_rule_uses() {
        let db = create_db();
        let msgs = vec![make_message("I use VSCode for development.")];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "uses");
        assert_eq!(facts[0].object, "VSCode for development");
        assert_eq!(facts[0].confidence, 0.7);
        assert_eq!(facts[0].tags, vec!["preference"]);
    }

    #[test]
    fn test_infer_rule_works_at() {
        let db = create_db();
        let msgs = vec![
            make_message("I work at Acme Corp since last year."),
            make_message("I work for Tech Inc."),
        ];
        let facts = db.infer_facts_from_messages(&msgs);
        assert_eq!(facts.len(), 2);
        for f in &facts {
            assert_eq!(f.predicate, "works_at");
            assert_eq!(f.confidence, 0.75);
            assert_eq!(f.tags, vec!["identity"]);
        }
    }

    #[test]
    fn test_infer_rule_correction_lowers_confidence() {
        let db = create_db();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Python", "inferred", 0.8, &["preference"])
            .unwrap();

        let msgs = vec![make_message("actually I prefer Go now.")];
        let facts = db.infer_facts_from_messages(&msgs);
        assert!(!facts.is_empty());

        let updated = db.get_facts("user").unwrap();
        for f in &updated {
            if f.predicate == "likes" && f.source == "inferred" {
                assert!(f.confidence < 0.9, "confidence should be lowered");
            }
        }
    }

    #[test]
    fn test_infer_multiple_rules_from_message() {
        let db = create_db();
        let msgs = vec![make_message(
            "I like Rust. I use VSCode. I work at Acme. My name is Dave.",
        )];
        let facts = db.infer_facts_from_messages(&msgs);
        let predicates: Vec<&str> = facts.iter().map(|f| f.predicate.as_str()).collect();
        assert!(predicates.contains(&"likes"));
        assert!(predicates.contains(&"uses"));
        assert!(predicates.contains(&"works_at"));
        assert!(predicates.contains(&"name"));
    }

    #[test]
    fn test_fact_cache_invalidation_on_insert() {
        let db = create_db();
        db.insert_fact("cache-test", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        let cached = db.get_facts("cache-test").unwrap();
        assert_eq!(cached.len(), 1);

        db.insert_fact("cache-test", "dislikes", "Java", "user", 0.5, &[])
            .unwrap();
        let fresh = db.get_facts("cache-test").unwrap();
        assert_eq!(fresh.len(), 2);
    }
}
