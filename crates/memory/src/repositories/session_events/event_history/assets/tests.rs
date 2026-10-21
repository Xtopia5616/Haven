use super::*;
use crate::repositories::session_events::SessionEventInput;
use crate::{Database, SessionStore};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn append(store: &SessionStore, session_id: &str, payload: Value) -> SessionEvent {
    store
        .append_if_session_exists(
            session_id,
            SessionEventInput::new(TRANSCRIPT_EVENT_TYPE, payload.to_string()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap()
}

fn asset_input(
    asset_id: &str,
    filename: &str,
    media_type: &str,
    cached_representation: Option<(&str, &str)>,
) -> Value {
    let mut representations = vec![json!({
        "representation": "managed_file_ref",
        "provenance": "original",
        "availability": {"state": "available"},
        "payload": {"kind": "managed_file_ref", "value": {"asset_id": asset_id, "filename": filename}}
    })];
    if let Some((representation, text)) = cached_representation {
        representations.push(json!({
            "representation": representation,
            "provenance": {"kind": "derived", "operation": "tool"},
            "availability": {"state": "available"},
            "payload": {"kind": "text", "value": text}
        }));
    }
    json!({
        "asset": {
            "asset_id": asset_id,
            "content_hash": "",
            "media_type": media_type,
            "size_bytes": 1024,
            "filename": filename,
            "source": "user_attachment",
            "lifecycle": "session"
        },
        "representations": representations
    })
}

#[tokio::test]
async fn asset_search_returns_context_and_cached_description_without_media_bytes() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("assets").unwrap();
    let store = SessionStore::new(db);
    let asset_id = "asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let input = asset_input(asset_id, "concept.png", "image/png", None);
    let input_event = append(
        &store,
        &session.session_id,
        json!({
            "type": "user_inject",
            "step_number": 1,
            "source": "user",
            "text": "Compare this concept sketch with the earlier blue dragon design.",
            "message_id": "msg-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "media_inputs": [input]
        }),
    )
    .await;
    append(
        &store,
        &session.session_id,
        json!({
            "type": "tool_result",
            "step_number": 2,
            "tool_call": {"tool_name": "media.describe", "tool_input": {"asset_id": asset_id}},
            "canonical_observation": json!({
                "operation": "describe",
                "media": {
                    "asset_id": asset_id,
                    "media_type": "image/png",
                    "modality": "image",
                    "representation": "image_description",
                    "content": "A blue dragon stands beside a stone bridge at sunset."
                }
            }).to_string()
        }),
    )
    .await;

    let found = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            Some("blue dragon"),
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].sequence, input_event.sequence);
    assert_eq!(
        found.matches[0].message_id.as_deref(),
        Some("msg-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(found.matches[0].context_excerpt.contains("concept sketch"));
    assert_eq!(found.matches[0].assets[0].asset_id, asset_id);
    assert_eq!(
        found.matches[0].assets[0].cached_representation.as_deref(),
        Some("image_description")
    );
    assert!(
        found.matches[0].assets[0]
            .cached_content_excerpt
            .as_deref()
            .unwrap()
            .contains("blue dragon")
    );
    let encoded = serde_json::to_string(&found).unwrap();
    assert!(!encoded.contains("base64"));
    assert!(!encoded.contains("/private/path"));

    let recent = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            None,
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(recent.matches.len(), 1);
    assert_eq!(recent.matches[0].assets[0].asset_id, asset_id);
}

#[tokio::test]
async fn asset_search_finds_generated_assets_from_tool_outputs() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("generated assets").unwrap();
    let store = SessionStore::new(db);
    let asset_id = "asset-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let event = append(
        &store,
        &session.session_id,
        json!({
            "type": "tool_result",
            "step_number": 7,
            "tool_call": {
                "tool_name": "media.generate",
                "tool_input": {"prompt": "A paper-cut illustration of a red fox in a snowy forest"}
            },
            "canonical_observation": json!({
                "operation": "generate",
                "asset_id": asset_id,
                "media": {
                    "asset_id": asset_id,
                    "media_type": "image/png",
                    "modality": "image",
                    "representation": "raw_image",
                    "filename": "generated.png"
                }
            }).to_string()
        }),
    )
    .await;

    let found = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            Some("snowy forest"),
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].sequence, event.sequence);
    assert_eq!(found.matches[0].source, "tool_output");
    assert_eq!(
        found.matches[0].source_tool.as_deref(),
        Some("media.generate")
    );
    assert_eq!(found.matches[0].assets[0].asset_id, asset_id);
    assert_eq!(found.matches[0].assets[0].media_kind, "image");
}

#[tokio::test]
async fn asset_search_includes_audio_and_document_text_representations() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("audio and documents").unwrap();
    let store = SessionStore::new(db);
    let pdf_id = "asset-dddddddddddddddddddddddddddddddd";
    let audio_id = "asset-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    append(
        &store,
        &session.session_id,
        json!({
            "type": "user_inject",
            "step_number": 1,
            "source": "user",
            "text": "Compare the attached report and recording.",
            "media_inputs": [
                asset_input(pdf_id, "quarterly.pdf", "application/pdf", Some(("document_pages", "Quarterly revenue forecast: 18 percent growth."))),
                asset_input(audio_id, "briefing.wav", "audio/wav", Some(("transcript", "The blue harbor project starts in May.")))
            ]
        }),
    )
    .await;

    let document = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            Some("revenue forecast"),
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(document.matches.len(), 1);
    assert_eq!(document.matches[0].assets[0].asset_id, pdf_id);
    assert_eq!(document.matches[0].assets[0].media_kind, "document");
    assert_eq!(
        document.matches[0].assets[0]
            .cached_representation
            .as_deref(),
        Some("document_pages")
    );

    let audio = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            Some("blue harbor"),
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(audio.matches.len(), 1);
    assert_eq!(audio.matches[0].assets[0].asset_id, audio_id);
    assert_eq!(audio.matches[0].assets[0].media_kind, "audio");
    assert_eq!(
        audio.matches[0].assets[0].cached_representation.as_deref(),
        Some("transcript")
    );
}

#[tokio::test]
async fn asset_search_finds_non_media_files_in_tool_results() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("clipboard file").unwrap();
    let store = SessionStore::new(db);
    let asset_id = "asset-ffffffffffffffffffffffffffffffff";
    append(
        &store,
        &session.session_id,
        json!({
            "type": "tool_result",
            "step_number": 2,
            "tool_call": {"tool_name": "clipboard.read", "tool_input": {"operation": "read", "format": "files"}},
            "canonical_observation": json!({
                "operation": "read",
                "files": [{"asset_id": asset_id, "media_type": "text/plain", "filename": "notes.txt"}]
            }).to_string()
        }),
    )
    .await;

    let found = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            Some("notes.txt"),
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(
        found.matches[0].source_tool.as_deref(),
        Some("clipboard.read")
    );
    assert_eq!(found.matches[0].assets[0].asset_id, asset_id);
    assert_eq!(found.matches[0].assets[0].media_kind, "text");
}

#[tokio::test]
async fn asset_search_excludes_assets_from_rolled_back_input() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("rollback asset").unwrap();
    let store = SessionStore::new(db);
    let asset_id = "asset-cccccccccccccccccccccccccccccccc";
    append(
        &store,
        &session.session_id,
        json!({
            "type": "user_inject",
            "step_number": 1,
            "source": "user",
            "text": "See this image",
            "media_inputs": [asset_input(asset_id, "rolled-back.png", "image/png", None)]
        }),
    )
    .await;
    store
        .append_if_session_exists(
            &session.session_id,
            SessionEventInput::new(
                TIMELINE_ROLLBACK_EVENT_TYPE,
                json!({"to_sequence": 0}).to_string(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let found = store
        .search_transcript_assets_cancellable(
            &session.session_id,
            None,
            None,
            MAX_SESSION_ASSET_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(found.matches.is_empty());
}
