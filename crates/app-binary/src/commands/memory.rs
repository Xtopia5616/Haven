use crate::app_state::AppState;
use crate::commands::contracts::{MemoryFactResponse, MemoryFactSource, MemoryRecallItem};
use crate::commands::log_err;
use haven_memory::recall::{MemoryEntityKind, MemoryQuery};
use std::sync::Arc;
use tauri::State;

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
    kind: Option<MemoryEntityKind>,
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<MemoryRecallItem>, String> {
    let kind = kind.unwrap_or(MemoryEntityKind::Fact);
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
) -> Result<Vec<MemoryFactResponse>, String> {
    let source = source.map(|source| source.as_str().to_string());
    let facts = state
        .runtime
        .memory_fact_store
        .list_facts(source)
        .await
        .map_err(|e| log_err("list_facts", e))?;
    facts
        .into_iter()
        .map(MemoryFactResponse::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| log_err("list_facts", e))
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
    use haven_memory::repositories::facts::{is_sensitive_object, is_sensitive_predicate};

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
    use super::{AddFactInput, validate_add_fact_input};

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
}
