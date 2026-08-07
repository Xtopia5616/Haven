use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::facts::{is_sensitive_object, is_sensitive_predicate};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Query the user-fact memory: full-text search (`search`) or the top stored
/// facts (`list`).
///
/// Facts are short `(predicate, object)` statements extracted from previous
/// conversations (preferences, identity, workspace paths, ...) and are also
/// summarized in the system prompt. Use this tool when the summary may not
/// contain the detail you need — e.g. to check whether a fact exists, find a
/// specific value, or audit what Haven currently believes about the user.
/// Results are returned as a JSON array: each entry has `subject`,
/// `predicate`, `object`, `confidence` (0-1, recency-decayed), `source`
/// ("user" = explicitly stated, "inferred" = extracted) and `tags`.
pub struct FactsSearchTool {
    db: Option<Arc<Database>>,
}

impl FactsSearchTool {
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
}

#[async_trait]
impl Tool for FactsSearchTool {
    fn name(&self) -> String {
        "search_facts".into()
    }

    fn description(&self) -> String {
        "Search or list the facts Haven remembers about the user (preferences, identity, \
         workspace paths, ...) from previous conversations. operation=search (default) \
         with a free-text query returns matching facts; operation=list returns the top \
         stored facts. Use it when you need a detail not present in the facts summary."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        // Read-only access to the user's stored memory.
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["search", "list"],
                    "description": "search (default) = full-text search; list = top stored facts"
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
                }
            }
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
            "search" => {
                let query = input["query"]
                    .as_str()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("query is required for operation=search"))?;
                let limit = Self::parse_limit(&input, 10);
                let mut facts = self.visible_facts(db.search_facts(query)?);
                facts.truncate(limit);
                Ok(ToolResult::ok(self.to_output_rows(&facts)))
            }
            "list" => {
                let limit = Self::parse_limit(&input, 20);
                let mut facts = self.visible_facts(db.get_facts("user")?);
                facts.truncate(limit);
                Ok(ToolResult::ok(self.to_output_rows(&facts)))
            }
            other => anyhow::bail!("unknown operation '{}'", other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;

    fn test_tool() -> (FactsSearchTool, Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        (FactsSearchTool::new(Some(db.clone())), db, dir)
    }

    fn db_with_facts() -> (FactsSearchTool, Arc<Database>, tempfile::TempDir) {
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
        assert_eq!(FactsSearchTool::new(None).name(), "search_facts");
    }

    #[test]
    fn test_facts_tool_risk_is_safe() {
        let tool = FactsSearchTool::new(None);
        assert_eq!(
            tool.risk_level(&json!({"operation": "search", "query": "x"})),
            RiskLevel::Safe
        );
    }

    #[test]
    fn test_facts_tool_schema_has_operations() {
        let schema = FactsSearchTool::new(None).input_schema();
        let ops = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        assert!(ops.iter().any(|v| v == "search"));
        assert!(ops.iter().any(|v| v == "list"));
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
        let tool = FactsSearchTool::new(None);
        let result = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }
}
