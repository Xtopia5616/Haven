use crate::app_state::AppState;
use crate::commands::contracts::MemoryRecallItem;
use crate::commands::log_err;
use haven_memory::recall::MemoryRetriever;
use std::sync::Arc;
use tauri::State;

/// Run the full memory maintenance pass (fact dedup, sensitive purge,
/// stale-fact flush, embedding pruning, bounded embed catch-up). Hot-path
/// infer no longer runs this; the app scheduler owns it, and this command
/// exposes the same path for manual / admin use. Returns rows cleaned.
#[tauri::command]
pub async fn run_memory_maintenance(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    Ok(state.agent.run_memory_maintenance().await)
}

/// Recall memory items (facts or episodes) most relevant to a query. Uses
/// the `embedding_model` slot when configured, keyword search otherwise.
#[tauri::command]
pub async fn recall_memory(
    query: String,
    kind: Option<String>,
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<MemoryRecallItem>, String> {
    let kind = kind.as_deref().unwrap_or("fact");
    let limit = limit.unwrap_or(5);
    state
        .agent
        .recall_memory(&query, kind, limit)
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
    source: Option<String>,
) -> Result<Vec<haven_memory::repositories::facts::Fact>, String> {
    match source.as_deref().filter(|s| !s.is_empty()) {
        Some(src) => state
            .db
            .list_facts_by_source(src)
            .map(MemoryRetriever::filter_visible_facts)
            .map_err(|e| log_err("list_facts", e)),
        None => state
            .db
            .list_facts()
            .map(MemoryRetriever::filter_visible_facts)
            .map_err(|e| log_err("list_facts", e)),
    }
}

#[tauri::command]
pub async fn add_fact(
    state: State<'_, Arc<AppState>>,
    subject: String,
    predicate: String,
    object: String,
    tags: Option<Vec<String>>,
) -> Result<haven_memory::repositories::facts::Fact, String> {
    use haven_memory::repositories::facts::{is_sensitive_object, is_sensitive_predicate};
    let subject = subject.trim();
    let predicate = predicate.trim();
    let object = object.trim();
    if subject.is_empty() || predicate.is_empty() || object.is_empty() {
        return Err("subject, predicate, and object are required".into());
    }
    // Reject credential-like values up front so they never reach the facts
    // table (the maintenance pass would purge them eventually, but the user
    // should get immediate feedback instead of silent storage).
    if is_sensitive_predicate(predicate) || is_sensitive_object(object) {
        return Err("refusing to store credential-like facts".into());
    }
    let tags_owned: Vec<String> = tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    let tags: Vec<&str> = tags_owned.iter().map(String::as_str).collect();
    state
        .db
        .set_user_fact(subject, predicate, object, &tags)
        .map_err(|e| log_err("add_fact", e))
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, Arc<AppState>>, fact_id: String) -> Result<(), String> {
    state
        .db
        .delete_fact(&fact_id)
        .map_err(|e| log_err("delete_fact", e))
}
