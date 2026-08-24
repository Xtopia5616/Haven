use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::facts::{
    is_sensitive_object, is_sensitive_predicate, is_sensitive_text,
};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Desktop-wired recall callback (History `recall_memory` / InferenceEngine).
/// Args: `(query, kind, limit)` → hit rows `{entity_id,text,score,model}`.
pub type MemoryRecallFn = Arc<
    dyn Fn(String, String, usize) -> Pin<Box<dyn Future<Output = Vec<Value>> + Send>>
        + Send
        + Sync,
>;

/// Shared slot so catalog rebuilds keep the same callback.
pub type MemoryRecallSlot = Arc<RwLock<Option<MemoryRecallFn>>>;

pub fn new_memory_recall_slot() -> MemoryRecallSlot {
    Arc::new(RwLock::new(None))
}

/// Agent-facing entry to Haven's exclusive memory store (`haven.db` edges +
/// episodes). Same channel as the History「记忆」page and the system MEMORY fence.
///
/// Operations:
/// - `search` (default) — FTS over fact subject/predicate/object/tags.
/// - `list` — top stored facts (optional `subject`; omit for cross-subject).
/// - `remember` / `forget` — user-stated fact writes (credentials rejected).
/// - `recall` — unified retrieval for `kind=fact|episode` (vector when
///   embedding_model is configured, else keyword/FTS). Aligns with
///   History `recall_memory`.
pub struct MemoryTool {
    db: Option<Arc<Database>>,
    /// Prefer this over a local vector path so agent and History stay aligned.
    recall: MemoryRecallSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    Search,
    List,
    Remember,
    Forget,
    Recall,
}

/// Typed parameters for `MemoryTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `MemoryTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MemoryParams {
    /// Operation to perform; defaults to `search`.
    #[serde(default)]
    pub operation: Option<MemoryOperation>,
    /// Free-text query for search / recall.
    #[serde(default)]
    pub query: Option<String>,
    /// Maximum number of results (default 10 for search/recall, 20 for list; recall capped at 20).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Short attribute key (required for remember and forget).
    #[serde(default)]
    pub predicate: Option<String>,
    /// The value to remember; or a specific value to delete (optional for
    /// forget, required for remember).
    #[serde(default)]
    pub object: Option<String>,
    /// Optional for remember: identity, preference, workspace, project.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Fact subject. Defaults to `"user"` for remember/forget. For list,
    /// omit to return recent facts across all subjects.
    #[serde(default)]
    pub subject: Option<String>,
    /// Recall kind: `fact` (default) or `episode`.
    #[serde(default)]
    pub kind: Option<String>,
}

impl MemoryTool {
    pub fn new(db: Option<Arc<Database>>, recall: MemoryRecallSlot) -> Self {
        Self { db, recall }
    }

    fn filter_recall_hits(hits: Vec<Value>) -> Vec<Value> {
        hits.into_iter()
            .filter(|h| {
                h.get("text")
                    .and_then(|t| t.as_str())
                    .map(|t| !is_sensitive_text(t))
                    .unwrap_or(true)
            })
            .collect()
    }

    fn parse_limit(params: &MemoryParams, default: usize) -> usize {
        params
            .limit
            .map(|l| l.clamp(1, 50) as usize)
            .unwrap_or(default)
    }

    /// Resolve subject for write ops; empty/whitespace → `"user"`.
    fn write_subject(params: &MemoryParams) -> String {
        params
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("user")
            .to_string()
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
                let mut row = json!({
                    "subject": f.subject,
                    "predicate": f.predicate,
                    "object": f.object,
                    "confidence": (haven_memory::repositories::facts::fact_effective_confidence(f) * 100.0).round() / 100.0,
                    "source": f.source,
                    "tags": f.tags,
                });
                if let Some(snippet) = f
                    .source_ref
                    .as_ref()
                    .map(|r| r.snippet.trim())
                    .filter(|s| !s.is_empty() && !is_sensitive_text(s))
                {
                    row["source_snippet"] = json!(snippet);
                }
                row
            })
            .collect();
        json!({ "facts": rows })
    }

    fn execute_search(&self, params: &MemoryParams, db: &Database) -> anyhow::Result<ToolResult> {
        let query = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("query is required for operation=search"))?;
        let limit = Self::parse_limit(params, 10);
        let mut facts = self.visible_facts(db.search_facts(query)?);
        facts.truncate(limit);
        Ok(ToolResult::ok(self.to_output_rows(&facts)))
    }

    fn execute_list(&self, params: &MemoryParams, db: &Database) -> anyhow::Result<ToolResult> {
        let limit = Self::parse_limit(params, 20);
        let subject = params
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut facts = self.visible_facts(match subject {
            Some(s) => db.get_facts(s)?,
            // Cross-subject recent N (already effective-confidence ordered).
            None => db.list_facts()?,
        });
        facts.truncate(limit);
        Ok(ToolResult::ok(self.to_output_rows(&facts)))
    }

    fn execute_remember(&self, params: &MemoryParams, db: &Database) -> anyhow::Result<ToolResult> {
        let predicate = params
            .predicate
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("predicate is required for operation=remember"))?;
        let object = params
            .object
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("object is required for operation=remember"))?;
        if is_sensitive_predicate(predicate) || is_sensitive_object(object) {
            anyhow::bail!("refusing to remember credential-like values");
        }
        let tags: Vec<&str> = params
            .tags
            .iter()
            .flatten()
            .map(|t| t.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let subject = Self::write_subject(params);
        let fact = db.set_user_fact(&subject, predicate, object, &tags)?;
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

    fn execute_forget(&self, params: &MemoryParams, db: &Database) -> anyhow::Result<ToolResult> {
        let predicate = params
            .predicate
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("predicate is required for operation=forget"))?;
        let object = params
            .object
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let subject = Self::write_subject(params);
        let deleted = db.delete_facts_by_triple(&subject, predicate, object)?;
        Ok(ToolResult::ok(json!({ "deleted": deleted })))
    }

    /// Unified recall: prefer the desktop-wired History/`InferenceEngine`
    /// callback; fall back to keyword/FTS when unset (tests/headless).
    async fn execute_recall(
        &self,
        params: &MemoryParams,
        db: &Database,
    ) -> anyhow::Result<ToolResult> {
        let query = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("query is required for operation=recall"))?;
        let kind_raw = params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("fact");
        let entity = if kind_raw == "episode" || kind_raw == "episodes" {
            haven_memory::embeddings::entity_kind::EPISODE
        } else if kind_raw == "fact" || kind_raw == "facts" {
            haven_memory::embeddings::entity_kind::FACT
        } else {
            anyhow::bail!("kind must be fact or episode");
        };
        let limit = params
            .limit
            .map(|l| l.clamp(1, 20) as usize)
            .unwrap_or(5);

        if let Some(recall) = self.recall.read().await.clone() {
            let hits = recall(query.to_string(), entity.to_string(), limit).await;
            let hits = Self::filter_recall_hits(hits);
            return Ok(ToolResult::ok(json!({
                "kind": entity,
                "hits": hits,
                "mode": "shared",
            })));
        }

        let hits: Vec<Value> = if entity == haven_memory::embeddings::entity_kind::EPISODE {
            let terms = haven_common::text::memory_recall_terms(query);
            let term_refs = haven_common::text::memory_recall_term_sample(&terms, 6);
            Self::filter_recall_hits(
                db.search_episodes_by_keywords(&term_refs, limit)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|text| {
                        json!({
                            "entity_id": "",
                            "text": text,
                            "score": 0.0,
                            "model": "",
                        })
                    })
                    .collect(),
            )
        } else {
            self.visible_facts(db.search_facts(query)?)
                .into_iter()
                .take(limit)
                .map(|f| {
                    json!({
                        "entity_id": f.id,
                        "text": format!("{}={}", f.predicate, f.object),
                        "score": haven_memory::repositories::facts::fact_effective_confidence(&f),
                        "model": "",
                    })
                })
                .collect()
        };

        Ok(ToolResult::ok(json!({
            "kind": entity,
            "hits": hits,
            "mode": "keyword",
        })))
    }

    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: MemoryParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let Some(db) = self.db.as_ref() else {
            anyhow::bail!("memory database is not available");
        };

        match params.operation.unwrap_or(MemoryOperation::Search) {
            MemoryOperation::Search => self.execute_search(&params, db),
            MemoryOperation::List => self.execute_list(&params, db),
            MemoryOperation::Remember => self.execute_remember(&params, db),
            MemoryOperation::Forget => self.execute_forget(&params, db),
            MemoryOperation::Recall => self.execute_recall(&params, db).await,
        }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> String {
        "memory".into()
    }
    fn description(&self) -> String {
        "Haven memory (SPO edges + past conversation items). \
         search/list/remember/forget manage SPO facts; \
         recall(query, kind=fact|episode) is the unified retrieval entry \
         (same path as History memory search: vector when configured, else keyword)."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
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
                    "enum": ["search", "list", "remember", "forget", "recall"],
                    "description": "search/list/remember/forget = facts CRUD; recall = unified fact/episode retrieval"
                },
                "query": {
                    "type": "string",
                    "description": "Free-text query (search / recall)"
                },
                "kind": {
                    "type": "string",
                    "enum": ["fact", "episode"],
                    "default": "fact",
                    "description": "recall only: fact (default) or episode"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Max results (default 10 search / 20 list / 5 recall; recall max 20)"
                },
                "predicate": {
                    "type": "string",
                    "description": "Short attribute key for remember/forget"
                },
                "object": {
                    "type": "string",
                    "description": "Value to remember, or specific value to forget"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional for remember: identity, preference, workspace, project"
                },
                "subject": {
                    "type": "string",
                    "description": "Fact subject. remember/forget default to \"user\". For list, omit for all subjects"
                }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `MemoryParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<MemoryParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;

    fn test_tool() -> (MemoryTool, Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        (MemoryTool::new(Some(db.clone()), new_memory_recall_slot()), db, dir)
    }

    fn temp_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        (db, dir)
    }

    fn db_with_facts() -> (MemoryTool, Arc<Database>, tempfile::TempDir) {
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
    fn test_memory_tool_name() {
        assert_eq!(MemoryTool::new(None, new_memory_recall_slot()).name(), "memory");
    }

    #[test]
    fn test_memory_tool_read_risk_is_safe() {
        let tool = MemoryTool::new(None, new_memory_recall_slot());
        assert_eq!(
            tool.risk_level(&json!({"operation": "search", "query": "x"})),
            RiskLevel::Safe
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "list"})),
            RiskLevel::Safe
        );
    }

    #[test]
    fn test_memory_tool_write_risk_is_medium() {
        let tool = MemoryTool::new(None, new_memory_recall_slot());
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
    fn test_memory_tool_schema_has_operations() {
        let schema = MemoryTool::new(None, new_memory_recall_slot()).input_schema();
        let ops = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        for op in ["search", "list", "remember", "forget", "recall"] {
            assert!(ops.iter().any(|v| v == op));
        }
        assert!(schema["properties"]["kind"].is_object());
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
    async fn test_source_snippet_skips_sensitive_text() {
        let (tool, db, _dir) = test_tool();
        let source = haven_memory::repositories::facts::FactSourceRef {
            message_id: "msg-1".into(),
            snippet: "sk-abc123secret".into(),
        };
        db.insert_fact_with_source_ref(
            "user",
            "likes",
            "Rust",
            "inferred",
            0.9,
            &["preference"],
            Some(&source),
            1.0,
        )
        .unwrap();
        let result = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        let facts = result.output["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1);
        assert!(facts[0].get("source_snippet").is_none());
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
    async fn test_list_and_remember_optional_subject() {
        let (tool, db, _dir) = test_tool();
        db.insert_fact("alice", "likes", "Tea", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Coffee", "inferred", 0.8, &[])
            .unwrap();
        // Cross-subject list (no subject).
        let all = tool
            .execute(json!({"operation": "list", "limit": 10}), CancellationToken::new())
            .await
            .unwrap();
        let subjects: Vec<&str> = all.output["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["subject"].as_str().unwrap())
            .collect();
        assert!(subjects.contains(&"alice") && subjects.contains(&"user"));
        // Subject-filtered list.
        let alice = tool
            .execute(
                json!({"operation": "list", "subject": "alice"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let facts = alice.output["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["object"], "Tea");
        // remember with non-user subject.
        tool.execute(
            json!({
                "operation": "remember",
                "subject": "bob",
                "predicate": "role",
                "object": "admin"
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let bob = db.get_facts("bob").unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].object, "admin");
        // forget scoped to subject.
        let deleted = tool
            .execute(
                json!({
                    "operation": "forget",
                    "subject": "bob",
                    "predicate": "role"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.output["deleted"], 1);
        assert!(db.get_facts("bob").unwrap().is_empty());
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
        let tool = MemoryTool::new(None, new_memory_recall_slot());
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
        let tool = MemoryTool::new(Some(db), new_memory_recall_slot());
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
        let tool = MemoryTool::new(Some(db), new_memory_recall_slot());
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
        let tool = MemoryTool::new(Some(db.clone()), new_memory_recall_slot());
        let result = tool
            .execute(
                json!({"operation": "forget", "predicate": "likes"}),
                CancellationToken::new(),
            )
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
        let tool = MemoryTool::new(Some(db.clone()), new_memory_recall_slot());
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
        let tool = MemoryTool::new(Some(db), new_memory_recall_slot());
        assert!(
            tool.execute(json!({"operation": "forget"}), CancellationToken::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_native_entry_lands_in_run() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .run(
                MemoryParams {
                    operation: Some(MemoryOperation::Search),
                    query: Some("Rust".into()),
                    limit: None,
                    predicate: None,
                    object: None,
                    tags: None,
                    subject: None,
                    kind: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let facts = result.output["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["object"], "Rust");
    }

    #[tokio::test]
    async fn test_recall_facts_keyword() {
        let (tool, _db, _dir) = db_with_facts();
        let result = tool
            .execute(
                json!({"operation": "recall", "query": "Rust", "kind": "fact"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["kind"], "fact");
        assert_eq!(result.output["mode"], "keyword");
        let hits = result.output["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0]["text"].as_str().unwrap().contains("Rust"));
    }

    #[tokio::test]
    async fn test_recall_episodes_keyword() {
        let (tool, db, _dir) = test_tool();
        let session = db.create_session("t", "").unwrap();
        db.add_episode(&session.id, "User prefers a dark theme for the IDE")
            .unwrap();
        let result = tool
            .execute(
                json!({
                    "operation": "recall",
                    "query": "dark theme",
                    "kind": "episode",
                    "limit": 5
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["kind"], "episode");
        let hits = result.output["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits[0]["text"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("dark")
        );
    }

    #[tokio::test]
    async fn test_recall_requires_query() {
        let (tool, _db, _dir) = db_with_facts();
        assert!(
            tool.execute(json!({"operation": "recall"}), CancellationToken::new())
                .await
                .is_err()
        );
    }
}
