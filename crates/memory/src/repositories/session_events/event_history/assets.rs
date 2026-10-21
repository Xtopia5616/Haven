use super::*;
use haven_common::media_detection::{
    DetectedMediaKind, media_kind_from_mime_type, mime_type_from_extension,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

const MAX_SESSION_ASSET_CONTEXT_BYTES: usize = 768;
const MAX_SESSION_ASSET_CONTENT_BYTES: usize = 1024;
const MAX_SESSION_ASSET_METADATA_BYTES: usize = 256;
const MAX_SESSION_ASSET_REFERENCES: usize = 10_000;
const MAX_SESSION_ASSET_REFERENCES_PER_EVENT: usize = 8;
const MAX_TOOL_RESULT_ASSETS: usize = 32;

#[derive(Debug, Clone)]
struct CachedAssetContent {
    sequence: i64,
    representation: String,
    content: String,
}

#[derive(Debug, Clone)]
struct AssetReference {
    asset_id: String,
    media_type: String,
    media_kind: String,
    filename: Option<String>,
    cached_content: Option<(String, String)>,
}

#[derive(Debug, Clone)]
struct AssetCandidate {
    context: SessionTranscriptAssetMatch,
    full_context: String,
}

impl SessionStore {
    /// Find managed assets attached to, or produced during, the current session.
    /// Results carry only bounded context and opaque asset ids; asset content
    /// remains behind the existing file, clipboard, and media tool boundaries.
    pub async fn search_transcript_assets_cancellable(
        &self,
        session_id: &str,
        query: Option<&str>,
        before_sequence: Option<i64>,
        limit: usize,
        cancel: CancellationToken,
    ) -> anyhow::Result<SessionTranscriptAssetSearch> {
        ensure_session_id(session_id)?;
        let query = query.map(str::trim).filter(|value| !value.is_empty());
        if let Some(query) = query {
            anyhow::ensure!(
                query.len() <= MAX_SESSION_HISTORY_QUERY_BYTES,
                "asset search query exceeds {MAX_SESSION_HISTORY_QUERY_BYTES} bytes"
            );
        }
        if let Some(before_sequence) = before_sequence {
            anyhow::ensure!(before_sequence >= 1, "before_sequence must be positive");
        }
        anyhow::ensure!(
            (1..=MAX_SESSION_ASSET_SEARCH_RESULTS).contains(&limit),
            "asset search limit must be between 1 and {MAX_SESSION_ASSET_SEARCH_RESULTS}"
        );

        let store = self.clone();
        let session_id = session_id.to_owned();
        let query = query.map(str::to_owned);
        self.db
            .run_blocking_cancellable(cancel.clone(), move |_| {
                store.search_transcript_assets_sync(
                    &session_id,
                    query.as_deref(),
                    before_sequence,
                    limit,
                    &cancel,
                )
            })
            .await
    }

    fn search_transcript_assets_sync(
        &self,
        session_id: &str,
        query: Option<&str>,
        before_sequence: Option<i64>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> anyhow::Result<SessionTranscriptAssetSearch> {
        ensure_not_cancelled(cancel)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            let latest_sequence = latest_sequence_in_connection(&conn, session_id)?;
            let (active_sequences, scanned_events, scanned_bytes) =
                active_transcript_sequences(&conn, session_id, latest_sequence, cancel)?;
            let cached_content = Self::cached_asset_content(
                &conn,
                session_id,
                latest_sequence,
                &active_sequences,
                cancel,
            )?;
            let query_lower = query.map(str::to_lowercase);
            let mut selected = VecDeque::with_capacity(limit);
            let mut matching_count = 0usize;
            let mut asset_reference_count = 0usize;
            let mut known_tool_assets = HashSet::new();
            let mut statement = conn.prepare(
                "SELECT session_id, sequence, event_type, payload,
                        created_at, run_id, step_number
                 FROM session_events
                 WHERE session_id = ?1 AND event_type = ?2 AND sequence <= ?3
                 ORDER BY sequence ASC",
            )?;
            let mut rows = statement.query(rusqlite::params![
                session_id,
                TRANSCRIPT_EVENT_TYPE,
                latest_sequence
            ])?;
            while let Some(row) = rows.next()? {
                ensure_not_cancelled(cancel)?;
                let event = session_event_from_row(row)?;
                if active_sequences.binary_search(&event.sequence).is_err() {
                    continue;
                }
                let Some(candidate) = asset_candidate_from_event(
                    &event,
                    &cached_content,
                    &mut known_tool_assets,
                    &mut asset_reference_count,
                )?
                else {
                    continue;
                };
                if before_sequence.is_some_and(|before| candidate.context.sequence >= before) {
                    continue;
                }
                let mut matched = candidate.context;
                let matching_assets = std::mem::take(&mut matched.assets)
                    .into_iter()
                    .filter(|asset| {
                        query_lower.as_deref().is_none_or(|query| {
                            candidate.full_context.to_lowercase().contains(query)
                                || asset
                                    .filename
                                    .as_deref()
                                    .is_some_and(|value| value.to_lowercase().contains(query))
                                || asset.media_type.to_lowercase().contains(query)
                                || asset.media_kind.to_lowercase().contains(query)
                                || asset.asset_id.to_lowercase().contains(query)
                                || cached_content.get(&asset.asset_id).is_some_and(|cached| {
                                    cached.content.to_lowercase().contains(query)
                                })
                        })
                    })
                    .collect::<Vec<_>>();
                if matching_assets.is_empty() {
                    continue;
                }
                matching_count += 1;
                matched.assets = matching_assets
                    .into_iter()
                    .map(|mut asset| {
                        if let Some(cached) = cached_content.get(&asset.asset_id) {
                            let (excerpt, truncated) = content_excerpt(
                                &cached.content,
                                query,
                                MAX_SESSION_ASSET_CONTENT_BYTES,
                            );
                            asset.cached_representation = Some(cached.representation.clone());
                            asset.cached_content_excerpt = Some(excerpt);
                            asset.cached_content_truncated = truncated;
                        }
                        asset
                    })
                    .collect();
                matched.context_excerpt = content_excerpt(
                    &candidate.full_context,
                    query,
                    MAX_SESSION_ASSET_CONTEXT_BYTES,
                )
                .0;
                selected.push_back(matched);
                if selected.len() > limit {
                    selected.pop_front();
                }
            }
            Ok(SessionTranscriptAssetSearch {
                has_more: matching_count > limit,
                matches: selected.into_iter().rev().collect(),
                latest_sequence,
                before_sequence,
                scanned_events,
                scanned_bytes,
            })
        })();
        finish_read_transaction(&conn, result)
    }

    fn cached_asset_content(
        conn: &rusqlite::Connection,
        session_id: &str,
        latest_sequence: i64,
        active_sequences: &[i64],
        cancel: &CancellationToken,
    ) -> anyhow::Result<HashMap<String, CachedAssetContent>> {
        let mut cached = HashMap::new();
        let mut statement = conn.prepare(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND event_type = ?2 AND sequence <= ?3
             ORDER BY sequence ASC",
        )?;
        let mut rows = statement.query(rusqlite::params![
            session_id,
            TRANSCRIPT_EVENT_TYPE,
            latest_sequence
        ])?;
        while let Some(row) = rows.next()? {
            ensure_not_cancelled(cancel)?;
            let event = session_event_from_row(row)?;
            if active_sequences.binary_search(&event.sequence).is_err() {
                continue;
            }
            let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
                continue;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("user_inject") => {
                    if let Some(inputs) = payload.get("media_inputs").and_then(Value::as_array) {
                        for input in inputs {
                            for asset in asset_references_from_media_input(input) {
                                if let Some((representation, content)) = asset.cached_content {
                                    insert_cached_content(
                                        &mut cached,
                                        &asset.asset_id,
                                        event.sequence,
                                        representation,
                                        content,
                                    )?;
                                }
                            }
                        }
                    }
                }
                Some("tool_result") => {
                    for asset in asset_references_from_tool_result(&payload) {
                        if let Some((representation, content)) = asset.cached_content {
                            insert_cached_content(
                                &mut cached,
                                &asset.asset_id,
                                event.sequence,
                                representation,
                                content,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(cached)
    }
}

fn asset_candidate_from_event(
    event: &SessionEvent,
    cached_content: &HashMap<String, CachedAssetContent>,
    known_tool_assets: &mut HashSet<String>,
    asset_reference_count: &mut usize,
) -> anyhow::Result<Option<AssetCandidate>> {
    let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
        return Ok(None);
    };
    let event_type = payload.get("type").and_then(Value::as_str);
    let (source, source_tool, full_context, message_id, mut assets) = match event_type {
        Some("user_inject") => {
            let assets = payload
                .get("media_inputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(asset_references_from_media_input)
                .collect::<Vec<_>>();
            (
                "user_input".to_owned(),
                None,
                payload
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .filter(|id| haven_common::types::is_canonical_id(id, "msg"))
                    .map(str::to_owned),
                assets,
            )
        }
        Some("tool_result") => {
            let tool_call = payload.get("tool_call");
            let tool_name = tool_call
                .and_then(|call| call.get("tool_name"))
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned);
            let tool_input = tool_call.and_then(|call| call.get("tool_input"));
            let context = tool_input
                .and_then(|input| {
                    ["prompt", "focus", "query", "text"]
                        .iter()
                        .find_map(|key| input.get(*key).and_then(Value::as_str))
                })
                .unwrap_or_default();
            let source_tool = tool_name.map(|name| bounded_metadata(&name));
            let full_context = match (source_tool.as_deref(), context.trim()) {
                (Some(tool), value) if !value.is_empty() => format!("{tool}: {value}"),
                (Some(tool), _) => tool.to_owned(),
                (None, value) => value.to_owned(),
            };
            let assets = asset_references_from_tool_result(&payload);
            let is_content_operation = payload
                .get("canonical_observation")
                .and_then(Value::as_str)
                .and_then(|observation| serde_json::from_str::<Value>(observation).ok())
                .and_then(|observation| {
                    observation
                        .get("operation")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|operation| {
                    matches!(
                        operation.as_str(),
                        "inspect" | "describe" | "ocr" | "transcribe" | "extract" | "render"
                    )
                });
            let assets = if is_content_operation {
                Vec::new()
            } else {
                assets
                    .into_iter()
                    .filter(|asset| known_tool_assets.insert(asset.asset_id.clone()))
                    .collect()
            };
            (
                "tool_output".to_owned(),
                source_tool,
                full_context,
                None,
                assets,
            )
        }
        _ => return Ok(None),
    };
    if assets.is_empty() {
        return Ok(None);
    }
    let mut seen = HashSet::new();
    assets.retain(|asset| seen.insert(asset.asset_id.clone()));
    anyhow::ensure!(
        assets.len() <= MAX_SESSION_ASSET_REFERENCES_PER_EVENT,
        "one transcript event contains more than {MAX_SESSION_ASSET_REFERENCES_PER_EVENT} asset references"
    );
    *asset_reference_count = asset_reference_count.saturating_add(assets.len());
    anyhow::ensure!(
        *asset_reference_count <= MAX_SESSION_ASSET_REFERENCES,
        "session asset history exceeds the {MAX_SESSION_ASSET_REFERENCES}-reference search limit"
    );
    let assets = assets
        .into_iter()
        .map(|asset| {
            let cached = cached_content
                .get(&asset.asset_id)
                .map(|cached| (cached.representation.clone(), cached.content.clone()))
                .or(asset.cached_content);
            let (cached_representation, cached_content_excerpt, cached_content_truncated) = cached
                .map_or((None, None, false), |(representation, content)| {
                    let (excerpt, truncated) =
                        content_excerpt(&content, None, MAX_SESSION_ASSET_CONTENT_BYTES);
                    (Some(representation), Some(excerpt), truncated)
                });
            SessionTranscriptAsset {
                asset_id: asset.asset_id,
                media_type: asset.media_type,
                media_kind: asset.media_kind,
                filename: asset.filename,
                cached_representation,
                cached_content_excerpt,
                cached_content_truncated,
            }
        })
        .collect::<Vec<_>>();
    Ok(Some(AssetCandidate {
        context: SessionTranscriptAssetMatch {
            sequence: event.sequence,
            created_at: event.created_at.clone(),
            run_id: event.run_id,
            step_number: event.step_number,
            message_id,
            source,
            source_tool,
            context_excerpt: String::new(),
            assets,
        },
        full_context,
    }))
}

fn asset_references_from_media_input(value: &Value) -> Vec<AssetReference> {
    let Some(asset) = value.get("asset") else {
        return Vec::new();
    };
    let asset_id = asset
        .get("asset_id")
        .and_then(Value::as_str)
        .filter(|id| haven_common::types::is_canonical_id(id, "asset"));
    let media_type = asset.get("media_type").and_then(Value::as_str);
    let filename = asset
        .get("filename")
        .and_then(Value::as_str)
        .map(bounded_metadata);
    let Some(asset_id) = asset_id else {
        return Vec::new();
    };
    let cached_content = value
        .get("representations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(text_representation)
        .next_back();
    vec![AssetReference {
        asset_id: asset_id.to_owned(),
        media_type: media_type
            .map(bounded_metadata)
            .unwrap_or_else(|| "application/octet-stream".to_owned()),
        media_kind: classify_asset_media_kind(media_type, filename.as_deref(), None),
        filename,
        cached_content,
    }]
}

fn text_representation(value: &Value) -> Option<(String, String)> {
    let representation = value.get("representation").and_then(Value::as_str)?;
    if !is_textual_representation(representation)
        || value
            .get("availability")
            .and_then(|availability| availability.get("state"))
            .and_then(Value::as_str)
            .is_some_and(|state| state != "available")
    {
        return None;
    }
    let payload = value.get("payload")?;
    (payload.get("kind").and_then(Value::as_str) == Some("text"))
        .then(|| payload.get("value").and_then(Value::as_str))
        .flatten()
        .filter(|content| !content.is_empty())
        .map(|content| (representation.to_owned(), content.to_owned()))
}

fn is_textual_representation(representation: &str) -> bool {
    matches!(
        representation,
        "extracted_text"
            | "transcript"
            | "ocr_text"
            | "image_description"
            | "document_pages"
            | "table_data"
    )
}

fn asset_references_from_tool_result(payload: &Value) -> Vec<AssetReference> {
    let Some(observation) = payload
        .get("canonical_observation")
        .and_then(Value::as_str)
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
    else {
        return Vec::new();
    };
    let mut references = Vec::new();
    let mut seen = HashSet::new();
    collect_tool_asset_references(&observation, 0, &mut references, &mut seen);
    references
}

fn collect_tool_asset_references(
    value: &Value,
    depth: usize,
    references: &mut Vec<AssetReference>,
    seen: &mut HashSet<String>,
) {
    if depth > 8 || references.len() >= MAX_TOOL_RESULT_ASSETS {
        return;
    }
    let Some(object) = value.as_object() else {
        if let Some(values) = value.as_array() {
            for child in values {
                collect_tool_asset_references(child, depth + 1, references, seen);
                if references.len() >= MAX_TOOL_RESULT_ASSETS {
                    break;
                }
            }
        }
        return;
    };
    // Prefer nested MediaReference/FileReference records over outer result
    // envelopes that repeat only the asset id.
    for key in ["media", "asset", "assets", "files", "attachments"] {
        if let Some(child) = object.get(key) {
            collect_tool_asset_references(child, depth + 1, references, seen);
        }
    }
    let media_type = object
        .get("media_type")
        .or_else(|| object.get("mime_type"))
        .and_then(Value::as_str);
    let filename = object
        .get("filename")
        .and_then(Value::as_str)
        .map(bounded_metadata);
    let modality = object
        .get("modality")
        .or_else(|| object.get("file_kind"))
        .and_then(Value::as_str);
    let representation = object.get("representation").and_then(Value::as_str);
    if let Some(asset_id) = object
        .get("asset_id")
        .and_then(Value::as_str)
        .filter(|id| haven_common::types::is_canonical_id(id, "asset"))
        && seen.insert(asset_id.to_owned())
    {
        let cached_content = object
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.is_empty())
            .and_then(|content| {
                representation
                    .filter(|kind| is_textual_representation(kind))
                    .map(|kind| (kind.to_owned(), content.to_owned()))
            });
        references.push(AssetReference {
            asset_id: asset_id.to_owned(),
            media_type: media_type
                .map(bounded_metadata)
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            media_kind: classify_asset_media_kind(media_type, filename.as_deref(), modality),
            filename,
            cached_content,
        });
    }
}

fn classify_asset_media_kind(
    media_type: Option<&str>,
    filename: Option<&str>,
    declared_kind: Option<&str>,
) -> String {
    if let Some(kind) = declared_kind.and_then(canonical_media_kind) {
        return kind.to_owned();
    }
    let from_mime = media_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(media_kind_from_mime_type)
        .filter(|kind| *kind != DetectedMediaKind::Unknown);
    let from_extension = filename
        .and_then(mime_type_from_extension)
        .map(media_kind_from_mime_type)
        .filter(|kind| *kind != DetectedMediaKind::Unknown);
    from_mime
        .or(from_extension)
        .unwrap_or(DetectedMediaKind::Unknown)
        .as_str()
        .to_owned()
}

fn canonical_media_kind(value: &str) -> Option<&'static str> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "text" => "text",
        "image" => "image",
        "audio" => "audio",
        "video" => "video",
        "document" | "pdf" | "office" => "document",
        "unknown" | "binary" | "archive" | "executable" => return None,
        _ => return None,
    })
}

fn bounded_metadata(value: &str) -> String {
    let mut end = value.len().min(MAX_SESSION_ASSET_METADATA_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn insert_cached_content(
    cache: &mut HashMap<String, CachedAssetContent>,
    asset_id: &str,
    sequence: i64,
    representation: String,
    content: String,
) -> anyhow::Result<()> {
    if !haven_common::types::is_canonical_id(asset_id, "asset") || content.is_empty() {
        return Ok(());
    }
    if let Some(existing) = cache.get(asset_id)
        && existing.sequence >= sequence
    {
        return Ok(());
    }
    anyhow::ensure!(
        cache.contains_key(asset_id) || cache.len() < MAX_SESSION_ASSET_REFERENCES,
        "session asset history exceeds the {MAX_SESSION_ASSET_REFERENCES}-asset index limit"
    );
    cache.insert(
        asset_id.to_owned(),
        CachedAssetContent {
            sequence,
            representation,
            content,
        },
    );
    Ok(())
}

fn content_excerpt(value: &str, query: Option<&str>, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let offset = query
        .filter(|query| !query.is_empty())
        .and_then(|query| {
            value
                .to_ascii_lowercase()
                .find(&query.to_ascii_lowercase())
                .or_else(|| value.find(query))
        })
        .unwrap_or(0);
    let mut start = offset.saturating_sub(max_bytes / 3);
    while !value.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start.saturating_add(max_bytes).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[start..end].to_owned(), true)
}

#[cfg(test)]
mod tests;
