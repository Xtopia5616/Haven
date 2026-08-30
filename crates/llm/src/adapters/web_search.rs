//! Shared built-in web-search payload normalization.
//!
//! Adapters capture provider-specific search events, while this module keeps
//! the common echo, deduplication and citation shape stable for the router and
//! UI.

use serde_json::Value;

/// DeepSeek's web-search round-trip: a `web_search_call` item captured from
/// the stream is echoed back verbatim into the next request's `input`.
/// DeepSeek's Responses-compat layer deserializes the echoed item against a
/// strict schema: the `action` field is an internally tagged enum
/// (`WebSearchAction`) with variants `search` / `open_page` / `find_in_page`,
/// and the `search` variant requires a `queries` string array. The
/// `output_item.added` skeleton (only `type`/`id`/`status`) and the
/// `web_search_call.*` status events lack `action` — echoing a bare skeleton
/// 400s ("missing field `action`") — so the full `output_item.done` payload
/// must be captured instead (see the adapter). As a last resort, fill the
/// action when absent or malformed with `{"type": "search", "queries": []}`
/// (verified accepted by DeepSeek); items that already carry a well-formed
/// object `action` — e.g. an `output_item.done` payload — pass through
/// untouched.
pub(crate) fn normalize_web_search_call_item(item: Value) -> Value {
    let mut item = item;
    if !item.is_object() {
        return item;
    }
    let has_valid_action = item.get("action").is_some_and(|a| a.is_object());
    if !has_valid_action {
        item["action"] = serde_json::json!({"type": "search", "queries": []});
    }
    item
}

/// Normalize one raw citation entry into `{title, url, snippet}`. Accepts
/// objects with `title`/`url`/`snippet` (OpenAI / DeepSeek `citations`,
/// Anthropic result rows) and plain URL strings (xAI Live Search citations).
fn citation_of(raw: &Value) -> Option<Value> {
    match raw {
        Value::String(url) if !url.is_empty() => {
            Some(serde_json::json!({"title": url, "url": url, "snippet": ""}))
        }
        Value::Object(object) => {
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let snippet = object
                .get("snippet")
                .and_then(Value::as_str)
                .or_else(|| object.get("content").and_then(Value::as_str))
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect::<String>();
            if title.is_empty() && url.is_empty() {
                return None;
            }
            Some(serde_json::json!({"title": title, "url": url, "snippet": snippet}))
        }
        _ => None,
    }
}

fn collect_citations(source: &Value, out: &mut Vec<Value>) {
    match source {
        Value::Array(items) => {
            for item in items {
                if let Some(citation) = citation_of(item) {
                    out.push(citation);
                }
            }
        }
        Value::Object(object) => {
            for key in ["results", "citations", "web_search_results"] {
                if let Some(items) = object.get(key).and_then(Value::as_array) {
                    for item in items {
                        if let Some(citation) = citation_of(item) {
                            out.push(citation);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Extract the tool return of a provider built-in web search from a
/// `web_search_call` item: a compact `{queries, results}` payload where each
/// result is `{title, url, snippet}`. Reads `action.search.citations` /
/// `action.search.results` (OpenAI / DeepSeek / xAI), `action.result`
/// (Anthropic `web_search_tool_result` blocks) and flat `citations` arrays
/// (xAI Live Search). Returns `None` when the item carries no usable content
/// (e.g. a bare Gemini grounding skeleton or an in-progress call), so the
/// UI card falls back to the status label.
pub fn web_search_result_of(item: &Value) -> Option<Value> {
    let action = item.as_object()?.get("action")?.as_object()?;
    let mut queries: Vec<String> = Vec::new();
    if let Some(query_items) = action.get("queries").and_then(Value::as_array) {
        queries.extend(
            query_items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    let mut results: Vec<Value> = Vec::new();
    for key in ["citations", "results", "result"] {
        if let Some(source) = action.get(key) {
            collect_citations(source, &mut results);
        }
    }
    // Anthropic web_search_tool_result puts the payload in `result.query`.
    if queries.is_empty()
        && let Some(query) = action
            .get("result")
            .and_then(Value::as_object)
            .and_then(|result| result.get("query"))
            .and_then(Value::as_str)
    {
        queries.push(query.to_string());
    }
    if queries.is_empty() && results.is_empty() {
        return None;
    }
    Some(serde_json::json!({"queries": queries, "results": results}))
}

/// Insert a captured `web_search_call` item into `calls`, replacing any
/// earlier item with the same `id`. The `output_item.added` skeleton arrives
/// first; a later `web_search_call.completed` payload — when the provider
/// sends one — is the authoritative version. Both must never be echoed into
/// the next request's input as duplicates.
fn item_rank(item: &Value) -> u8 {
    let mut rank = 0u8;
    if item.get("action").is_some_and(|action| action.is_object()) {
        rank += 1;
    }
    if web_search_result_of(item).is_some() {
        rank += 2;
    }
    rank
}

pub(crate) fn upsert_web_search_call(calls: &mut Vec<Value>, item: Value) {
    let id = item.get("id").and_then(Value::as_str);
    if let Some(id) = id
        && let Some(position) = calls
            .iter()
            .position(|call| call.get("id").and_then(Value::as_str) == Some(id))
    {
        if item_rank(&item) >= item_rank(&calls[position]) {
            calls[position] = item;
        }
    } else {
        calls.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_call_item_fills_missing_action() {
        let skeleton = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress"
        });
        let output = normalize_web_search_call_item(skeleton);
        assert_eq!(
            output["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
        assert_eq!(output["type"], "web_search_call");
        assert_eq!(output["id"], "ws_1");
        assert_eq!(output["status"], "in_progress");
    }

    #[test]
    fn normalize_call_item_replaces_malformed_string_action() {
        let skeleton = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress",
            "action": "web_search"
        });
        let output = normalize_web_search_call_item(skeleton);
        assert_eq!(
            output["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
    }

    #[test]
    fn normalize_call_item_keeps_existing_action() {
        let complete = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {"type": "open_page", "url": "https://example.com"},
            "query": "foo"
        });
        assert_eq!(normalize_web_search_call_item(complete.clone()), complete);
    }

    #[test]
    fn normalize_call_item_skips_non_objects() {
        assert_eq!(normalize_web_search_call_item(Value::Null), Value::Null);
    }

    #[test]
    fn result_of_reads_deepseek_citations() {
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": ["capital of France"],
                "citations": [
                    {"id": "c1", "title": "Paris — Wikipedia", "url": "https://en.wikipedia.org/wiki/Paris", "snippet": "Paris is the capital of France.", "source": "wikipedia"},
                    {"id": "c2", "title": "France", "url": "https://example.com/france"}
                ]
            }
        });
        let result = web_search_result_of(&item).expect("result payload");
        assert_eq!(result["queries"], serde_json::json!(["capital of France"]));
        assert_eq!(result["results"][0]["title"], "Paris — Wikipedia");
        assert_eq!(
            result["results"][0]["url"],
            "https://en.wikipedia.org/wiki/Paris"
        );
        assert_eq!(result["results"][1]["snippet"], "");
    }

    #[test]
    fn result_of_accepts_flat_string_citations() {
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "xai_citations",
            "status": "completed",
            "action": {"type": "search", "queries": [], "citations": ["https://a.com", "https://b.com"]}
        });
        let result = web_search_result_of(&item).expect("result payload");
        assert_eq!(result["results"][0]["url"], "https://a.com");
        assert_eq!(result["results"][0]["title"], "https://a.com");
    }

    #[test]
    fn result_of_reads_anthropic_result_payload() {
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_result_3",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": [],
                "result": {
                    "query": "best laptop 2026",
                    "results": [
                        {"title": "Top Laptops", "url": "https://reviews.example/laptops", "content": "long content…"}
                    ]
                }
            }
        });
        let result = web_search_result_of(&item).expect("result payload");
        assert_eq!(result["queries"], serde_json::json!(["best laptop 2026"]));
        assert_eq!(result["results"][0]["title"], "Top Laptops");
        assert_eq!(result["results"][0]["snippet"], "long content…");
    }

    #[test]
    fn result_of_empty_item_returns_none() {
        assert!(
            web_search_result_of(&serde_json::json!({
                "type": "web_search_call",
                "id": "ws_1",
                "status": "in_progress"
            }))
            .is_none()
        );
        assert!(
            web_search_result_of(&serde_json::json!({
                "type": "web_search_call",
                "id": "gemini_grounding",
                "status": "completed",
                "action": {"type": "search", "queries": ["foo"]}
            }))
            .is_some()
        );
        assert!(web_search_result_of(&serde_json::json!("nope")).is_none());
    }

    #[test]
    fn upsert_call_replaces_same_id_and_appends_new() {
        let mut calls = vec![serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress"
        })];
        upsert_web_search_call(
            &mut calls,
            serde_json::json!({"type": "web_search_call", "id": "ws_1", "status": "completed", "action": {"type": "search", "queries": ["capital of France"]}}),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["status"], "completed");
        assert_eq!(
            calls[0]["action"],
            serde_json::json!({"type": "search", "queries": ["capital of France"]})
        );
        upsert_web_search_call(
            &mut calls,
            serde_json::json!({"type": "web_search_call", "id": "ws_2", "status": "in_progress"}),
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["id"], "ws_2");
    }

    #[test]
    fn upsert_call_keeps_rich_item_over_skeleton() {
        let mut calls = vec![serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": ["capital of France"],
                "citations": [{"title": "Paris", "url": "https://ex"}]
            }
        })];
        upsert_web_search_call(
            &mut calls,
            serde_json::json!({"type": "web_search_call", "id": "ws_1", "status": "completed"}),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["action"]["citations"][0]["url"], "https://ex");
    }
}
