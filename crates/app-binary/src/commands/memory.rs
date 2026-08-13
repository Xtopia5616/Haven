use crate::app_state::AppState;
use crate::commands::log_err;
use std::sync::Arc;
use tauri::State;

/// Run the memory maintenance pass (fact dedup, sensitive purge, stale-fact
/// flush, embedding pruning). The agent already runs it after each inference;
/// this exposes the same pass for periodic app-level scheduling.
#[tauri::command]
pub async fn run_memory_maintenance(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let db = state.db.clone();
    let result = db
        .run_blocking(move |db| {
            let deduped = db.dedup_facts()?;
            let purged = db.delete_sensitive_facts()?;
            let flushed = db.flush_low_confidence(0.3)?;
            let pruned = db.prune_orphaned_embeddings()?;
            Ok::<u64, anyhow::Error>(deduped + purged + flushed + pruned)
        })
        .await
        .map_err(|e| log_err("run_memory_maintenance", e))?;
    Ok(result)
}

/// Recall memory items (facts or episodes) most relevant to a query. Uses
/// the `embedding_model` slot when configured, keyword search otherwise.
#[tauri::command]
pub async fn recall_memory(
    query: String,
    kind: Option<String>,
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<serde_json::Value>, String> {
    let kind = kind.as_deref().unwrap_or("fact");
    let limit = limit.unwrap_or(5);
    Ok(state.agent.recall_memory(&query, kind, limit).await)
}

// M6-04: Fact management commands
#[tauri::command]
pub async fn list_facts(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<haven_memory::repositories::facts::Fact>, String> {
    state.db.list_facts().map_err(|e| log_err("list_facts", e))
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
    // Reject credential-like values up front so they never reach the facts
    // table (the maintenance pass would purge them eventually, but the user
    // should get immediate feedback instead of silent storage).
    if is_sensitive_predicate(&predicate) || is_sensitive_object(&object) {
        return Err("refusing to store credential-like facts".into());
    }
    let tags_owned = tags.unwrap_or_default();
    let tags: Vec<&str> = tags_owned.iter().map(|s| s.as_str()).collect();
    state
        .db
        .insert_fact(&subject, &predicate, &object, "user", 1.0, &tags)
        .map_err(|e| log_err("add_fact", e))
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, Arc<AppState>>, fact_id: String) -> Result<(), String> {
    state
        .db
        .delete_fact(&fact_id)
        .map_err(|e| log_err("delete_fact", e))
}
