use crate::app_state::AppState;
use crate::commands::{log_err, validate_command_id};
use haven_memory::{FactProposalDecision, MemoryQuery, MemoryRecallKind};
use std::sync::Arc;
use tauri::State;

/// Stable Tauri memory-recall item. It mirrors the typed `haven_memory` hit
/// while keeping the existing wire shape and field names.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryRecallItem {
    pub entity_id: String,
    pub text: String,
    pub score: f64,
    pub model: String,
}

impl From<haven_memory::MemoryHit> for MemoryRecallItem {
    fn from(hit: haven_memory::MemoryHit) -> Self {
        Self {
            entity_id: hit.entity_id,
            text: hit.text,
            score: hit.score,
            model: hit.model,
        }
    }
}

/// Public fact projection returned by Memory Tauri commands.
///
/// This keeps the IPC surface independent from the repository's `Fact` model;
/// update the projection deliberately when fields should reach the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFactSource {
    User,
    Inferred,
}

impl MemoryFactSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Inferred => "inferred",
        }
    }
}

impl TryFrom<&str> for MemoryFactSource {
    type Error = &'static str;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        match source {
            "user" => Ok(Self::User),
            "inferred" => Ok(Self::Inferred),
            _ => Err("unsupported fact source"),
        }
    }
}

/// Keyset position for a long-term fact page. Tauri input uses camelCase at
/// the renderer boundary; the nested fact projection remains snake_case.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFactCursor {
    pub created_at: String,
    pub fact_id: String,
}

impl From<haven_memory::FactPageCursor> for MemoryFactCursor {
    fn from(cursor: haven_memory::FactPageCursor) -> Self {
        Self {
            created_at: cursor.created_at,
            fact_id: cursor.fact_id,
        }
    }
}

impl From<MemoryFactCursor> for haven_memory::FactPageCursor {
    fn from(cursor: MemoryFactCursor) -> Self {
        Self {
            created_at: cursor.created_at,
            fact_id: cursor.fact_id,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryFactResponse {
    pub fact_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source: MemoryFactSource,
    pub memory_kind: haven_memory::MemoryFactKind,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub created_at: String,
    pub mention_count: i64,
    pub last_seen_at: Option<String>,
    pub source_ref: Option<MemoryFactSourceRef>,
    pub durability: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryFactPageResponse {
    pub facts: Vec<MemoryFactResponse>,
    pub next_cursor: Option<MemoryFactCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFactProposalCursor {
    pub created_at: String,
    pub proposal_id: String,
}

impl From<haven_memory::FactProposalCursor> for MemoryFactProposalCursor {
    fn from(cursor: haven_memory::FactProposalCursor) -> Self {
        Self {
            created_at: cursor.created_at,
            proposal_id: cursor.proposal_id,
        }
    }
}

impl From<MemoryFactProposalCursor> for haven_memory::FactProposalCursor {
    fn from(cursor: MemoryFactProposalCursor) -> Self {
        Self {
            created_at: cursor.created_at,
            proposal_id: cursor.proposal_id,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryFactProposalResponse {
    pub proposal_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub durability: f64,
    pub created_at: String,
    pub source_ref: Option<MemoryFactSourceRef>,
}

impl From<haven_memory::FactProposal> for MemoryFactProposalResponse {
    fn from(proposal: haven_memory::FactProposal) -> Self {
        Self {
            proposal_id: proposal.proposal_id,
            subject: proposal.subject,
            predicate: proposal.predicate,
            object: proposal.object,
            confidence: proposal.confidence,
            tags: proposal.tags,
            durability: proposal.durability,
            created_at: proposal.created_at,
            source_ref: proposal.source_ref.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryFactProposalPageResponse {
    pub proposals: Vec<MemoryFactProposalResponse>,
    pub next_cursor: Option<MemoryFactProposalCursor>,
}

impl From<haven_memory::FactProposalPage> for MemoryFactProposalPageResponse {
    fn from(page: haven_memory::FactProposalPage) -> Self {
        Self {
            proposals: page.proposals.into_iter().map(Into::into).collect(),
            next_cursor: page.next_cursor.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MemoryFactProposalReviewResponse {
    Accepted { fact_written: bool },
    Rejected,
    AlreadyReviewed,
}

impl From<haven_memory::FactProposalReviewOutcome> for MemoryFactProposalReviewResponse {
    fn from(outcome: haven_memory::FactProposalReviewOutcome) -> Self {
        match outcome {
            haven_memory::FactProposalReviewOutcome::Accepted { fact_written } => {
                Self::Accepted { fact_written }
            }
            haven_memory::FactProposalReviewOutcome::Rejected => Self::Rejected,
            haven_memory::FactProposalReviewOutcome::AlreadyReviewed => Self::AlreadyReviewed,
        }
    }
}

impl TryFrom<haven_memory::FactPage> for MemoryFactPageResponse {
    type Error = String;

    fn try_from(page: haven_memory::FactPage) -> Result<Self, Self::Error> {
        Ok(Self {
            facts: page
                .facts
                .into_iter()
                .map(MemoryFactResponse::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            next_cursor: page.next_cursor.map(Into::into),
        })
    }
}

impl TryFrom<haven_memory::Fact> for MemoryFactResponse {
    type Error = String;

    fn try_from(fact: haven_memory::Fact) -> Result<Self, Self::Error> {
        let source =
            MemoryFactSource::try_from(fact.source.as_str()).map_err(|error| error.to_string())?;

        Ok(Self {
            fact_id: fact.fact_id,
            subject: fact.subject,
            predicate: fact.predicate,
            object: fact.object,
            source,
            memory_kind: fact.memory_kind,
            confidence: fact.confidence,
            tags: fact.tags,
            created_at: fact.created_at,
            mention_count: fact.mention_count,
            last_seen_at: fact.last_seen_at,
            source_ref: fact.source_ref.map(Into::into),
            durability: fact.durability,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryFactSourceRef {
    pub message_id: String,
    pub snippet: String,
}

impl From<haven_memory::FactSourceRef> for MemoryFactSourceRef {
    fn from(source: haven_memory::FactSourceRef) -> Self {
        Self {
            message_id: source.message_id,
            snippet: source.snippet,
        }
    }
}

/// Run the full memory maintenance pass (fact dedup, sensitive purge,
/// stale-fact flush, embedding pruning, bounded embed catch-up). Hot-path
/// infer no longer runs this; the app scheduler owns it, and this command
/// exposes the same path for manual / admin use. Returns rows cleaned.
#[tauri::command]
pub async fn run_memory_maintenance(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    state
        .runtime
        .agent
        .run_memory_maintenance()
        .await
        .map_err(|e| log_err("run_memory_maintenance", e))
}

/// Recall memory items (facts or episodes) most relevant to a query. Uses
/// the `embedding_model` slot when configured, keyword search otherwise.
#[tauri::command]
pub async fn recall_memory(
    query: String,
    kind: Option<MemoryRecallKind>,
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<MemoryRecallItem>, String> {
    let kind = kind.unwrap_or(MemoryRecallKind::Fact);
    let limit = limit.unwrap_or(5);
    let query = MemoryQuery::new(&query, kind, limit).map_err(|e| log_err("recall_memory", e))?;
    state
        .runtime
        .agent
        .recall_memory_query(query)
        .await
        .map(|recall| {
            recall
                .hits
                .into_iter()
                .map(MemoryRecallItem::from)
                .collect()
        })
        .map_err(|e| log_err("recall_memory", e))
}

// M6-04: Fact management commands
#[tauri::command]
pub async fn list_facts(
    state: State<'_, Arc<AppState>>,
    source: Option<MemoryFactSource>,
    cursor: Option<MemoryFactCursor>,
    limit: Option<u32>,
) -> Result<MemoryFactPageResponse, String> {
    let limit = limit.unwrap_or(haven_memory::DEFAULT_FACT_PAGE_SIZE as u32);
    if limit == 0 || limit as usize > haven_memory::MAX_FACT_PAGE_SIZE {
        return Err(log_err(
            "list_facts",
            format!(
                "limit must be between 1 and {}",
                haven_memory::MAX_FACT_PAGE_SIZE
            ),
        ));
    }
    let source = source.map(|source| source.as_str().to_string());
    let page = state
        .runtime
        .memory_fact_store
        .list_facts_page(source, cursor.map(Into::into), limit as usize)
        .await
        .map_err(|e| log_err("list_facts", e))?;
    MemoryFactPageResponse::try_from(page).map_err(|e| log_err("list_facts", e))
}

#[tauri::command]
pub async fn list_fact_proposals(
    state: State<'_, Arc<AppState>>,
    cursor: Option<MemoryFactProposalCursor>,
    limit: Option<u32>,
) -> Result<MemoryFactProposalPageResponse, String> {
    let limit = limit.unwrap_or(haven_memory::MAX_FACT_PROPOSAL_PAGE_SIZE as u32);
    if limit == 0 || limit as usize > haven_memory::MAX_FACT_PROPOSAL_PAGE_SIZE {
        return Err(log_err(
            "list_fact_proposals",
            format!(
                "limit must be between 1 and {}",
                haven_memory::MAX_FACT_PROPOSAL_PAGE_SIZE
            ),
        ));
    }
    state
        .runtime
        .memory_fact_proposal_store
        .list_pending_page(cursor.map(Into::into), limit as usize)
        .await
        .map(MemoryFactProposalPageResponse::from)
        .map_err(|error| log_err("list_fact_proposals", error))
}

#[tauri::command]
pub async fn review_fact_proposal(
    state: State<'_, Arc<AppState>>,
    proposal_id: String,
    decision: FactProposalDecision,
) -> Result<MemoryFactProposalReviewResponse, String> {
    validate_command_id(&proposal_id, "proposal_id", "fprop")
        .map_err(|error| log_err("review_fact_proposal", error))?;
    state
        .runtime
        .memory_fact_proposal_store
        .review(proposal_id, decision)
        .await
        .map(MemoryFactProposalReviewResponse::from)
        .map_err(|error| log_err("review_fact_proposal", error))
}

#[derive(Debug, PartialEq, Eq)]
struct AddFactInput {
    subject: String,
    predicate: String,
    object: String,
    tags: Vec<String>,
}

fn validate_add_fact_input(
    subject: String,
    predicate: String,
    object: String,
    tags: Option<Vec<String>>,
) -> Result<AddFactInput, String> {
    use haven_memory::{is_sensitive_object, is_sensitive_predicate};

    let subject = subject.trim().to_string();
    let predicate = predicate.trim().to_string();
    let object = object.trim().to_string();
    if subject.is_empty() || predicate.is_empty() || object.is_empty() {
        return Err("subject, predicate, and object are required".into());
    }
    // Reject credential-like values up front so they never reach the facts
    // table (the maintenance pass would purge them eventually, but the user
    // should get immediate feedback instead of silent storage).
    if is_sensitive_predicate(&predicate) || is_sensitive_object(&object) {
        return Err("refusing to store credential-like facts".into());
    }
    let tags = tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();

    Ok(AddFactInput {
        subject,
        predicate,
        object,
        tags,
    })
}

#[tauri::command]
pub async fn add_fact(
    state: State<'_, Arc<AppState>>,
    subject: String,
    predicate: String,
    object: String,
    tags: Option<Vec<String>>,
) -> Result<MemoryFactResponse, String> {
    let input = validate_add_fact_input(subject, predicate, object, tags)
        .map_err(|error| log_err("add_fact", error))?;
    let fact = state
        .runtime
        .memory_fact_store
        .set_user_fact(input.subject, input.predicate, input.object, input.tags)
        .await
        .map_err(|e| log_err("add_fact", e))?;
    MemoryFactResponse::try_from(fact).map_err(|e| log_err("add_fact", e))
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, Arc<AppState>>, fact_id: String) -> Result<(), String> {
    validate_command_id(&fact_id, "fact_id", "fact")
        .map_err(|error| log_err("delete_fact", error))?;
    state
        .runtime
        .memory_fact_store
        .delete_fact(fact_id)
        .await
        .map_err(|e| log_err("delete_fact", e))
}

#[tauri::command]
pub async fn clear_facts(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    state
        .runtime
        .memory_fact_store
        .clear_facts()
        .await
        .map_err(|e| log_err("clear_facts", e))
}

#[cfg(test)]
mod tests {
    use super::{
        AddFactInput, MemoryFactCursor, MemoryFactResponse, MemoryFactSource,
        validate_add_fact_input,
    };

    #[test]
    fn add_fact_input_trims_fields_and_filters_empty_tags() {
        let input = validate_add_fact_input(
            " user ".into(),
            " likes ".into(),
            " Rust ".into(),
            Some(vec![" preference ".into(), "  ".into(), "workspace".into()]),
        )
        .unwrap();

        assert_eq!(
            input,
            AddFactInput {
                subject: "user".into(),
                predicate: "likes".into(),
                object: "Rust".into(),
                tags: vec!["preference".into(), "workspace".into()],
            }
        );
    }

    #[test]
    fn add_fact_input_preserves_empty_and_sensitive_rejections() {
        assert_eq!(
            validate_add_fact_input("user".into(), "  ".into(), "Rust".into(), None),
            Err("subject, predicate, and object are required".into())
        );
        assert_eq!(
            validate_add_fact_input("user".into(), "api_key".into(), "value".into(), None),
            Err("refusing to store credential-like facts".into())
        );
        assert_eq!(
            validate_add_fact_input("user".into(), "likes".into(), "sk-secret".into(), None),
            Err("refusing to store credential-like facts".into())
        );
    }

    #[test]
    fn memory_fact_cursor_uses_renderer_camel_case_fields() {
        let cursor = MemoryFactCursor {
            created_at: "2026-10-06T00:00:00Z".into(),
            fact_id: "fact-0123456789abcdef0123456789abcdef".into(),
        };
        let wire = serde_json::to_value(&cursor).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "createdAt": "2026-10-06T00:00:00Z",
                "factId": "fact-0123456789abcdef0123456789abcdef",
            })
        );
        assert_eq!(
            serde_json::from_value::<MemoryFactCursor>(wire).unwrap(),
            cursor
        );
    }

    #[test]
    fn memory_fact_response_exposes_only_its_explicit_wire_projection() {
        let mut fact = haven_memory::Fact {
            fact_id: "fact-0123456789abcdef0123456789abcdef".into(),
            subject: "user".into(),
            predicate: "likes".into(),
            object: "Rust".into(),
            source: "user".into(),
            memory_kind: haven_memory::MemoryFactKind::Profile,
            confidence: 1.0,
            tags: vec!["language".into()],
            created_at: "2026-10-06T00:00:00Z".into(),
            mention_count: 2,
            last_seen_at: Some("2026-10-06T00:00:00Z".into()),
            source_ref: Some(haven_memory::FactSourceRef {
                message_id: "msg-0123456789abcdef0123456789abcdef".into(),
                snippet: "I like Rust.".into(),
            }),
            durability: 0.9,
        };
        let response = MemoryFactResponse::try_from(fact.clone()).unwrap();

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "fact_id": "fact-0123456789abcdef0123456789abcdef",
                "subject": "user",
                "predicate": "likes",
                "object": "Rust",
                "source": "user",
                "memory_kind": "profile",
                "confidence": 1.0,
                "tags": ["language"],
                "created_at": "2026-10-06T00:00:00Z",
                "mention_count": 2,
                "last_seen_at": "2026-10-06T00:00:00Z",
                "source_ref": {
                    "message_id": "msg-0123456789abcdef0123456789abcdef",
                    "snippet": "I like Rust.",
                },
                "durability": 0.9,
            })
        );

        fact.source = "external".into();
        assert_eq!(
            MemoryFactResponse::try_from(fact).unwrap_err(),
            "unsupported fact source"
        );
    }

    #[test]
    fn memory_fact_source_uses_the_database_source_values() {
        for (wire, source) in [
            ("user", MemoryFactSource::User),
            ("inferred", MemoryFactSource::Inferred),
        ] {
            assert_eq!(
                serde_json::from_str::<MemoryFactSource>(&format!("\"{wire}\"")).unwrap(),
                source
            );
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{wire}\"")
            );
            assert_eq!(source.as_str(), wire);
        }

        for wire in ["", "external", "USER"] {
            assert!(serde_json::from_str::<MemoryFactSource>(&format!("\"{wire}\"")).is_err());
        }
    }
}
