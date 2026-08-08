use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::facts::{is_sensitive_object, is_sensitive_predicate};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Read and write the user-fact memory in one place.
///
/// Facts are short `(predicate, object)` statements extracted from previous
/// conversations (preferences, identity, workspace paths, ...) and are also
/// summarized in the system prompt.
///
/// Operations:
/// - `search` (default) — full-text query over subject, predicate, object and
///   tags; use it when the summary may not contain the detail you need.
/// - `list` — return the top stored facts.
/// - `remember` — store a fact the user explicitly asked Haven to remember
///   (`source="user"`, confidence 1.0, never decays, replaces the previous
///   value of single-valued attributes). Credential-like values are rejected.
/// - `forget` — delete a fact the user explicitly asked to remove. With only
///   `predicate` all values of that attribute are removed; with `object` only
///   the matching one.
///
/// Results are returned as a JSON object: `facts` is an array of
/// `{ subject, predicate, object, confidence (0-1, recency-decayed), source
///   ("user" | "inferred"), tags }`; `remember` returns `stored`, `forget`
///   returns `deleted`.
pub struct FactsTool {
    db: Option<Arc<Database>>,
}

impl FactsTool {
    pub fn new(db: Option<Arc<Database>>) -> Self {
        Self { db }
    }

    fn parse_limit(input: &Value, default: usize) -> usize {
        input["limit"]
            .as_i64()
            .map(|l| l.clamp(1, 50) as usize)
            .unwrap_or(default)
    }

    /// Drop secrets before anything is shown to the model (defense in depth:
    /// the write path already purges them, this guards the read path too).
    fn visible_facts(
        &self,
        facts: Vec<haven_memory::repositories::facts::Fact>,
    ) -> Vec<haven_memory::repositories::facts::Fact> {
        facts
            .into_iter()
            .filter(|f| !is_sensitive_predicate(&f.predicate) && !is_sensitive_object(&f.object))
            .collect()
    }

    fn to_output_rows(&self, facts: &[haven_memory::repositories::facts::Fact]) -> Value {
        let rows: Vec<Value> = facts
            .iter()
            .map(|f| {
                json!({
                    "subject": f.subject,
                    "predicate": f.predicate,
                    "object": f.object,
                    "confidence": (haven_memory::repositories::facts::fact_effective_confidence(f) * 100.0).round() / 100.0,
                    "source": f.source,
                    "tags": f.tags,
                })
            })
            .collect();
        json!({ "facts": rows })
    }

    fn execute_search(&self, input: &Value, db: &Database) -> anyhow::Result<ToolResult> {
        let query = input["query"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("query is required for operation=search"))?;
        let limit = Self::parse_limit(input, 10);
        let mut facts = self.visible_facts(db.search_facts(query)?);
        facts.truncate(limit);
        Ok(ToolResult::ok(self.to_output_rows(&facts)))
    }

    fn execute_list(&self, input: &Value, db: &Database) -> anyhow::Result<ToolResult> {
        let limit = Self::parse_limit(input, 20);
        let mut facts = self.visible_facts(db.get_facts("user")?);
        facts.truncate(limit);
        Ok(ToolResult::ok(self.to_output_rows(&facts)))
    }

    fn execute_remember(&self, input: &Value, db: &Database) -> anyhow::Result<ToolResult> {
        let predicate = input["predicate"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("predicate is required for operation=remember"))?;
        let object = input["object"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("object is required for operation=remember"))?;
        if is_sensitive_predicate(predicate) || is_sensitive_object(object) {
            anyhow::bail!("refusing to remember credential-like values");
        }
        let tags: Vec<&str> = input["tags"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let fact = db.set_user_fact("user", predicate, object, &tags)?;
        Ok(ToolResult::ok(json!({
            "stored": {
                "subject": fact.subject,
                "predicate": fact.predicate,
                "object": fact.object,
                "source": fact.source,
                "confidence": fact.confidence,
                "tags": fact.tags,
            }
        })))
    }

    fn execute_forget(&self, input: &Value, db: &Database) -> anyhow::Result<ToolResult> {
        let predicate = input["predicate"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("predicate is required for operation=forget"))?;
        let object = input["object"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let deleted = db.delete_facts_by_triple("user", predicate, object)?;
        Ok(ToolResult::ok(json!({ "deleted": deleted })))
    }
}

#[async_trait]
impl Tool for FactsTool {
    fn name(&self) -> String {
        "facts".into()
    }
    fn description(&self) -> String {
        "Read and write the facts Haven remembers about the user (preferences, identity, \
         workspace paths, ...) from previous conversations. operation=search (default) with \
         a free-text query returns matching facts; operation=list returns the top stored facts; \
         operation=remember stores a fact the user explicitly asked to remember; operation=forget \
         deletes a fact the user explicitly asked to remove."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        // Reads are safe; writes only happen at the user's explicit request.
        match input["operation"].as_str() {
            Some("remember") | Some("forget") => RiskLevel::Medium,
            _ => RiskLevel::Safe,
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["search", "list", "remember", "forget"],
                    "description": "search (default) = full-text search; list = top stored facts; remember = store a user-stated fact; forget = delete a fact"
                },
                "query": {
                    "type": "string",
                    "description": "Free-text query matched against fact subject, predicate, object and tags (operation=search only)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Maximum number of results (default 10 for search, 20 for list)"
                },
                "predicate": {
                    "type": "string",
                    "description": "Short attribute key, e.g. name, language, likes, uses, project_path (required for remember and forget)"
                },
                "object": {
                    "type": "string",
                    "description": "The value to remember; or a specific value to delete (optional for forget, required for remember)"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional for remember: identity, preference, workspace, project"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let Some(db) = self.db.as_ref() else {
            anyhow::bail!("facts database is not available");
        };
        let operation = input["operation"].as_str().unwrap_or("search");

        match operation {
            "search" => self.execute_search(&input, db),
            "list" => self.execute_list(&input, db),
            "remember" => self.execute_remember(&input, db),
            "forget" => self.execute_forget(&input, db),
            other => anyhow::bail!("unknown operation '{}'", other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;

    fn test_tool() -> (FactsTool, Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        (FactsTool::new(Some(db.clone())), db, dir)
    }

    fn temp_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        (db, dir)
    }

    fn db_with_facts() -> (FactsTool, Arc<Database>, tempfile::TempDir) {
        let (tool, db, dir) = test_tool();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &["preference"])
            .unwrap();
        db.insert_fact("user", "likes", "Coffee", "inferred", 0.8, &["preference"])
            .unwrap();
        db.insert_fact(
            "user",
            "project_path",
            "/home/alice/app",
            "inferred",
            0.7,
            &["workspace"],
        )
        .unwrap();
        db.insert_fact(
            "user",
            "tavily_api_key",
            "tvly-dev-secret",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();
        (tool, db, dir)
    }

    #[test]
    fn test_facts_tool_name() {
        assert_eq!(FactsTool::new(None).name(), "facts");
    }

    #[test]
    fn test_facts_tool_read_risk_is_safe() {
        let tool = FactsTool::new(None);
        assert_eq!(
            tool.risk_level(&json!({"operation": "search", "query": "x"})),
            RiskLevel::Safe
        );
        assert_eq!(tool.risk_level(&json!({"operation": "list"})), RiskLevel::Safe);
    }

    #[test]
    fn test_facts_tool_write_risk_is_medium() {
        let tool = FactsTool::new(None);
        assert_eq!(
            tool.risk_level(&json!({"operation": "remember"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "forget"})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_facts_tool_schema_has_operations() {
        let schema = FactsTool::new(None).input_schema();
        let ops = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        for op in ["search", "list", "remember", "forget"] {
            assert!(ops.iter().any(|v| v == op));
        }
    }

    #[tokio::test]
    async fn test_search_returns_matching_facts() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(
                json!({"operation": "search", "query": "Rust"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let facts = result.output["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["predicate"], "likes");
        assert_eq!(facts[0]["object"], "Rust");
        assert_eq!(facts[0]["source"], "inferred");
        assert!(facts[0]["confidence"].as_f64().unwrap() > 0.0);
    }

    #[tokio::test]
    async fn test_search_requires_query() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(json!({"operation": "search"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_no_match_returns_empty() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(
                json!({"operation": "search", "query": "nonexistentterm"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["facts"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_search_excludes_sensitive_facts() {
        let (tool, _db, _dir) = db_with_facts();
        // Searching for the secret's object must not surface it.
        let result = tool
            .execute(
                json!({"operation": "search", "query": "tvly"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["facts"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_list_returns_top_facts() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        let facts = result.output["facts"].as_array().unwrap();
        // Sensitive fact excluded; the three real facts remain.
        assert_eq!(facts.len(), 3);
        let objs: Vec<&str> = facts
            .iter()
            .map(|f| f["object"].as_str().unwrap())
            .collect();
        assert!(objs.contains(&"Rust"));
        assert!(objs.contains(&"/home/alice/app"));
    }

    #[tokio::test]
    async fn test_list_respects_limit() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(
                json!({"operation": "list", "limit": 2}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["facts"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_unknown_operation_errors() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(json!({"operation": "delete"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_db_errors() {
        let tool = FactsTool::new(None);
        let result = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remember_stores_user_fact() {
        let (tool, db, _dir) = test_tool();
        let result = tool
            .execute(
                json!({"operation": "remember", "predicate": "email", "object": "alice@example.com", "tags": ["identity"]}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["stored"]["source"], "user");
        assert_eq!(result.output["stored"]["object"], "alice@example.com");
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "email");
        assert_eq!(facts[0].source, "user");
    }

    #[tokio::test]
    async fn test_remember_rejects_credentials() {
        let (db, _dir) = temp_db();
        let tool = FactsTool::new(Some(db));
        let result = tool
            .execute(
                json!({"operation": "remember", "predicate": "tavily_api_key", "object": "tvly-dev-secret"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "credential-like facts must be rejected");
        let result2 = tool
            .execute(
                json!({"operation": "remember", "predicate": "notes", "object": "sk-abc123"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result2.is_err(), "secret-looking objects must be rejected");
    }

    #[tokio::test]
    async fn test_remember_requires_predicate_and_object() {
        let (db, _dir) = temp_db();
        let tool = FactsTool::new(Some(db));
        assert!(
            tool.execute(
                json!({"operation": "remember", "object": "x"}),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(
            tool.execute(
                json!({"operation": "remember", "predicate": "x"}),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn test_forget_deletes_by_predicate() {
        let (_, db, _dir) = db_with_facts();
        let tool = FactsTool::new(Some(db.clone()));
        let result = tool
            .execute(json!({"operation": "forget", "predicate": "likes"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.output["deleted"], 2);
        let remaining = db.get_facts("user").unwrap();
        assert!(
            remaining.iter().all(|f| f.predicate != "likes"),
            "all likes must be gone"
        );
    }

    #[tokio::test]
    async fn test_forget_deletes_single_value() {
        let (_, db, _dir) = db_with_facts();
        let tool = FactsTool::new(Some(db.clone()));
        let result = tool
            .execute(
                json!({"operation": "forget", "predicate": "likes", "object": "Rust"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["deleted"], 1);
        let likes: Vec<_> = db
            .get_facts("user")
            .unwrap()
            .into_iter()
            .filter(|f| f.predicate == "likes")
            .collect();
        assert_eq!(likes.len(), 1);
        assert_eq!(likes[0].object, "Coffee");
    }

    #[tokio::test]
    async fn test_forget_requires_predicate() {
        let (db, _dir) = temp_db();
        let tool = FactsTool::new(Some(db));
        assert!(
            tool.execute(json!({"operation": "forget"}), CancellationToken::new())
                .await
                .is_err()
        );
    }
}
