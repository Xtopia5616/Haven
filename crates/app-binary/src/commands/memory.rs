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
    state
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
    let db = state.db.clone();
    db.run_blocking(move |db| {
        let facts = match source.as_deref().filter(|s| !s.is_empty()) {
            Some(src) => db.list_facts_by_source(src)?,
            None => db.list_facts()?,
        };
        Ok(MemoryRetriever::filter_visible_facts(facts))
    })
    .await
    .map_err(|e| log_err("list_facts", e))
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
    let tags_owned: Vec<String> = tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    let db = state.db.clone();
    db.run_blocking(move |db| {
        let tags: Vec<&str> = tags_owned.iter().map(String::as_str).collect();
        db.set_user_fact(&subject, &predicate, &object, &tags)
    })
    .await
    .map_err(|e| log_err("add_fact", e))
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, Arc<AppState>>, fact_id: String) -> Result<(), String> {
    let db = state.db.clone();
    db.run_blocking(move |db| db.delete_fact(&fact_id))
        .await
        .map_err(|e| log_err("delete_fact", e))
}
