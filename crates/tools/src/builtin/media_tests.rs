use super::*;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::media_reference::model_media_reference_with_capabilities;
use crate::{ManagedAssetRegistry, Tool};
use haven_common::media::MediaRepresentationKind;

struct DedicatedSttClient {
    calls: AtomicUsize,
}

struct DedicatedOcrClient;

struct FailingSttClient;

#[test]
fn operation_groups_keep_assets_and_device_effects_distinct() {
    assert!(MediaOperation::Transcribe.is_asset_operation());
    assert!(MediaOperation::Record.is_asset_operation());
    assert!(MediaOperation::Record.uses_audio_runtime());
    assert!(MediaOperation::Play.is_device_operation());
    assert!(MediaOperation::MuteSet.is_device_operation());
    assert!(!MediaOperation::Inspect.uses_audio_runtime());
}

#[async_trait]
impl haven_llm::OcrClient for DedicatedOcrClient {
    async fn recognize(
        &self,
        _image_bytes: &[u8],
        _media_type: &str,
    ) -> anyhow::Result<haven_llm::OcrResult> {
        Ok(haven_llm::OcrResult {
            text: "dedicated OCR".into(),
            confidence: Some(0.99),
        })
    }
}

struct DummyImageGenClient;

#[async_trait]
impl haven_llm::ImageGenClient for DummyImageGenClient {
    async fn generate(&self, _prompt: &str) -> anyhow::Result<haven_llm::GeneratedImage> {
        Ok(haven_llm::GeneratedImage {
            media_type: "image/png".into(),
            data: b"png".to_vec(),
        })
    }
}

#[async_trait]
impl haven_llm::SttClient for DedicatedSttClient {
    async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<haven_llm::SttResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(haven_llm::SttResult {
            text: "dedicated transcript".into(),
            confidence: None,
            usage: None,
            model: None,
        })
    }
}

#[async_trait]
impl haven_llm::SttClient for FailingSttClient {
    async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<haven_llm::SttResult> {
        Err(anyhow::anyhow!("provider unavailable"))
    }
}

fn registered_asset(root: &Path, name: &str, media_type: &str) -> (ManagedAssetRegistry, String) {
    let path = root.join(name);
    std::fs::write(&path, b"asset data").unwrap();
    let registry = ManagedAssetRegistry::default();
    let asset_id = haven_common::types::new_id("asset");
    assert!(registry.register_under_root(
        root,
        asset_id.clone(),
        path,
        Some(name.into()),
        media_type,
    ));
    (registry, asset_id)
}

#[test]
fn schema_is_operation_specific_and_capability_pruned() {
    let tool = MediaTool::new(None, ManagedAssetRegistry::default(), 1024, 10, 2_000);
    assert!(
        tool.validate_input(&json!({
            "operation": "inspect",
            "asset_id": "asset-0123456789abcdef0123456789abcdef"
        }))
        .is_ok()
    );
    assert!(
        tool.validate_input(&json!({
            "operation": "describe",
            "asset_id": "asset-0123456789abcdef0123456789abcdef"
        }))
        .is_err()
    );
    assert!(
        tool.validate_input(&json!({
            "operation": "describe",
            "asset_id": "asset-0123456789abcdef0123456789abcdef",
            "path": "C:\\secret.png"
        }))
        .is_err()
    );
    assert!(
        tool.validate_input(&json!({
            "operation": "play",
            "file_path": "C:\\audio\\sample.wav"
        }))
        .is_ok()
    );
    assert!(
        tool.validate_input(&json!({
            "operation": "play",
            "file_path": "C:\\audio\\sample.wav",
            "asset_id": "asset-0123456789abcdef0123456789abcdef"
        }))
        .is_err()
    );
    let schema = tool.input_schema();
    let operations = schema["properties"]["operation"]
        .as_object()
        .and_then(|operation| operation.get("enum"))
        .and_then(Value::as_array)
        .unwrap();
    assert!(!operations.iter().any(|operation| operation == "record"));
    assert!(!operations.iter().any(|operation| operation == "speak"));
}

#[test]
fn schema_rejects_cross_operation_media_arguments() {
    let tool = MediaTool::new(None, ManagedAssetRegistry::default(), 1024, 10, 2_000)
        .with_image_gen_client(Some(Arc::new(DummyImageGenClient)))
        .with_stt_client(Some(Arc::new(DedicatedSttClient {
            calls: AtomicUsize::new(0),
        })));
    let asset_id = "asset-0123456789abcdef0123456789abcdef";
    for invalid in [
        json!({"operation": "inspect", "asset_id": asset_id, "focus": "text"}),
        json!({"operation": "inspect", "asset_id": asset_id, "prompt": "text"}),
        json!({"operation": "describe", "asset_id": asset_id, "prompt": "text"}),
        json!({"operation": "transcribe", "asset_id": asset_id, "focus": "text"}),
        json!({"operation": "generate", "prompt": "text", "asset_id": asset_id}),
        json!({"operation": "generate", "prompt": "text", "focus": "text"}),
    ] {
        assert!(tool.validate_input(&invalid).is_err(), "accepted {invalid}");
    }
    assert!(
        tool.validate_input(&json!({"operation": "inspect", "asset_id": asset_id}))
            .is_ok()
    );
    assert!(
        tool.validate_input(&json!({"operation": "generate", "prompt": "text"}))
            .is_ok()
    );
}

#[tokio::test]
async fn inspect_returns_compact_media_reference_without_host_path() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000);
    let result = tool
        .execute(
            json!({"operation": "inspect", "asset_id": asset_id}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    let serialized = serde_json::to_string(&result.output).unwrap();
    assert!(serialized.contains("managed_file_ref"));
    assert!(!serialized.contains(&root.path().to_string_lossy().to_string()));
}

#[tokio::test]
async fn inspect_supports_video_assets_and_uses_typed_representation() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "clip.mts", "video/mp2t");
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000);
    let result = tool
        .execute(
            json!({"operation": "inspect", "asset_id": asset_id}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["modality"], "video");
    assert_eq!(result.output["representation"], "managed_file_ref");
    assert!(result.output.get("path").is_none());
}

#[test]
fn model_media_reference_has_one_content_slot_and_no_runtime_metadata() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
    let asset = registry.resolve(&asset_id).unwrap();
    let output = model_media_reference_with_capabilities(
        &asset,
        MediaRepresentationKind::ImageDescription,
        Some("same text"),
        true,
        true,
        true,
    );
    let serialized = serde_json::to_string(&output).unwrap();
    assert_eq!(serialized.matches("same text").count(), 1);
    assert!(!serialized.contains("content_hash"));
    assert!(!serialized.contains("expires_at"));
    assert!(!serialized.contains("source"));
    assert!(!serialized.contains("provenance"));
}

#[tokio::test]
async fn describe_without_router_is_explicitly_unavailable() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000);
    let result = tool
        .run(
            MediaParams {
                operation: MediaOperation::Describe,
                asset_id: Some(asset_id),
                focus: None,
                prompt: None,
                page_index: None,
                file_path: None,
                text: None,
                duration: None,
                volume: None,
                muted: None,
                session_id: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["available"], false);
    assert!(
        result.output["media"]["asset_id"]
            .as_str()
            .unwrap()
            .starts_with("asset-")
    );
}

#[tokio::test]
async fn transcribe_prefers_dedicated_stt_without_router() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "recording.wav", "audio/wav");
    let client = Arc::new(DedicatedSttClient {
        calls: AtomicUsize::new(0),
    });
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000)
        .with_stt_client(Some(client.clone()))
        .with_capabilities(false, false);
    assert!(
        tool.input_schema()["properties"]["operation"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation == "transcribe")
    );

    let result = tool
        .run(
            MediaParams {
                operation: MediaOperation::Transcribe,
                asset_id: Some(asset_id),
                focus: None,
                prompt: None,
                page_index: None,
                file_path: None,
                text: None,
                duration: None,
                volume: None,
                muted: None,
                session_id: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(result.success);
    assert_eq!(result.output["media"]["content"], "dedicated transcript");
    assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    assert!(result.llm_usage.is_empty());
}

#[tokio::test]
async fn transcribe_provider_failure_keeps_full_media_navigation_reference() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "recording.wav", "audio/wav");
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000)
        .with_stt_client(Some(Arc::new(FailingSttClient)))
        .with_capabilities(false, false);

    let result = tool
        .run(
            MediaParams {
                operation: MediaOperation::Transcribe,
                asset_id: Some(asset_id.clone()),
                focus: None,
                prompt: None,
                page_index: None,
                file_path: None,
                text: None,
                duration: None,
                volume: None,
                muted: None,
                session_id: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(!result.success);
    assert_eq!(result.output["asset_id"], asset_id);
    assert_eq!(result.output["media"]["asset_id"], asset_id);
    assert_eq!(result.output["media"]["representation"], "managed_file_ref");
}

#[tokio::test]
async fn cancelled_transcription_keeps_full_media_navigation_reference() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "recording.wav", "audio/wav");
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000).with_stt_client(Some(Arc::new(
        DedicatedSttClient {
            calls: AtomicUsize::new(0),
        },
    )));
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = tool
        .run(
            MediaParams {
                operation: MediaOperation::Transcribe,
                asset_id: Some(asset_id.clone()),
                focus: None,
                prompt: None,
                page_index: None,
                file_path: None,
                text: None,
                duration: None,
                volume: None,
                muted: None,
                session_id: None,
            },
            cancel,
        )
        .await
        .unwrap();

    assert_eq!(result.outcome, crate::ToolExecutionOutcome::Cancelled);
    assert_eq!(result.output["media"]["asset_id"], asset_id);
}

#[tokio::test]
async fn ocr_prefers_dedicated_provider_without_router() {
    let root = TempDir::new().unwrap();
    let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
    let tool = MediaTool::new(None, registry, 1024, 10, 2_000)
        .with_ocr_client(Some(Arc::new(DedicatedOcrClient)))
        .with_capabilities(false, false);
    let result = tool
        .run(
            MediaParams {
                operation: MediaOperation::Ocr,
                asset_id: Some(asset_id),
                focus: None,
                prompt: None,
                page_index: None,
                file_path: None,
                text: None,
                duration: None,
                volume: None,
                muted: None,
                session_id: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(result.success);
    assert_eq!(result.output["media"]["content"], "dedicated OCR");
    assert!(result.llm_usage.is_empty());
}

#[test]
fn generation_is_explicit_and_capability_pruned() {
    let unavailable = MediaTool::new(None, ManagedAssetRegistry::default(), 1024, 10, 2_000);
    assert!(
        !unavailable.input_schema()["properties"]["operation"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation == "generate")
    );

    let available = unavailable.with_image_gen_client(Some(Arc::new(DummyImageGenClient)));
    assert!(
        available
            .validate_input(&json!({
                "operation": "generate",
                "prompt": "a red fox in watercolor"
            }))
            .is_ok()
    );
    assert!(
        available
            .validate_input(&json!({
                "operation": "generate",
                "prompt": "a red fox in watercolor",
                "asset_id": "asset-0123456789abcdef0123456789abcdef"
            }))
            .is_err()
    );
    assert!(
        available
            .validate_input(&json!({
                "operation": "inspect",
                "asset_id": "asset-0123456789abcdef0123456789abcdef",
                "prompt": "unexpected"
            }))
            .is_err()
    );
    assert!(
        available
            .validate_input(&json!({"operation": "generate"}))
            .is_err()
    );
}

#[test]
fn generated_asset_is_registered_with_expiry_and_opaque_metadata() {
    let root = TempDir::new().unwrap();
    let path = root
        .path()
        .join("file-0123456789abcdef0123456789abcdef.png");
    std::fs::write(&path, b"png-bytes").unwrap();
    let registry = ManagedAssetRegistry::default();
    let asset = register_generated_asset(
        &registry,
        None,
        root.path(),
        path.clone(),
        Some("screenshot.png".into()),
        "image/png",
        9,
    )
    .unwrap();
    assert!(asset.asset_id.starts_with("asset-"));
    assert!(asset.expires_at.is_some());
    let serialized = serde_json::to_string(&model_media_reference_with_capabilities(
        &asset,
        MediaRepresentationKind::ManagedFileRef,
        None,
        true,
        true,
        true,
    ))
    .unwrap();
    assert!(!serialized.contains(&path.to_string_lossy().to_string()));
    assert!(serialized.contains("screenshot.png"));
}
