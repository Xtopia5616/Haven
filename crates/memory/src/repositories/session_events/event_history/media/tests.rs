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

fn image_input(asset_id: &str, filename: &str, cached_description: Option<&str>) -> Value {
    let mut representations = vec![json!({
        "representation": "raw_image",
        "provenance": "original",
        "availability": {"state": "available"},
        "payload": {"kind": "managed_file_ref", "value": {"asset_id": asset_id, "filename": filename}}
    })];
    if let Some(description) = cached_description {
        representations.push(json!({
            "representation": "image_description",
            "provenance": {"kind": "derived", "operation": "image_describe"},
            "availability": {"state": "available"},
            "payload": {"kind": "text", "value": description}
        }));
    }
    json!({
        "asset": {
            "asset_id": asset_id,
            "content_hash": "",
            "media_type": "image/png",
            "size_bytes": 1024,
            "filename": filename,
            "source": "user_attachment",
            "lifecycle": "session"
        },
        "representations": representations
    })
}

#[tokio::test]
async fn image_search_returns_context_and_cached_description_without_media_bytes() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("images").unwrap();
    let store = SessionStore::new(db);
    let asset_id = "asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let input = image_input(asset_id, "concept.png", None);
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
        .search_transcript_images_cancellable(
            &session.session_id,
            Some("blue dragon"),
            None,
            MAX_SESSION_IMAGE_SEARCH_RESULTS,
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
        .search_transcript_images_cancellable(
            &session.session_id,
            None,
            None,
            MAX_SESSION_IMAGE_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(recent.matches.len(), 1);
    assert_eq!(recent.matches[0].assets[0].asset_id, asset_id);
}

#[tokio::test]
async fn image_search_finds_generated_assets_from_tool_outputs() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("generated images").unwrap();
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
        .search_transcript_images_cancellable(
            &session.session_id,
            Some("snowy forest"),
            None,
            MAX_SESSION_IMAGE_SEARCH_RESULTS,
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
}

#[tokio::test]
async fn image_search_excludes_assets_from_rolled_back_input() {
    let db = std::sync::Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("rollback image").unwrap();
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
            "media_inputs": [image_input(asset_id, "rolled-back.png", None)]
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
        .search_transcript_images_cancellable(
            &session.session_id,
            None,
            None,
            MAX_SESSION_IMAGE_SEARCH_RESULTS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(found.matches.is_empty());
}
