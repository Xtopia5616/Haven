//! AssemblyAI speech-to-text adapter (upload → job → poll).
//!
//! STT-only: chat / stream / embed leave the [`LlmClient`] defaults
//! (`UnsupportedCapability`). Wired through `adapter_for` when
//! `api_style` / `provider` is `assemblyai`.

use async_trait::async_trait;
use futures_util::Stream;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{build_client, send_request};
use crate::client::LlmClient;
use crate::types::{LlmError, LlmResponse, StreamChunk, SttResult};
use haven_common::config::ModelEndpoint;
use haven_common::types::CanonicalMessage;

pub struct AssemblyAiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    poll_interval: Duration,
}

impl AssemblyAiAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let client = build_client(&endpoint);
        Self {
            endpoint,
            client,
            poll_interval: Duration::from_secs(3),
        }
    }

    fn base_url(&self) -> String {
        if self.endpoint.base_url.trim().is_empty() {
            "https://api.assemblyai.com".to_string()
        } else {
            self.endpoint.base_url.trim_end_matches('/').to_string()
        }
    }

    fn auth(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let key = self.endpoint.api_key.trim();
        if !key.is_empty()
            && let Ok(v) = HeaderValue::from_str(key)
        {
            headers.insert("authorization", v);
        }
        headers
    }
}

#[async_trait]
impl LlmClient for AssemblyAiAdapter {
    fn style(&self) -> &'static str {
        "assemblyai"
    }

    async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        Err(LlmError::UnsupportedCapability(
            "assemblyai is speech-to-text only".into(),
        ))
    }

    async fn chat_stream(
        &self,
        _messages: Vec<CanonicalMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        Err(LlmError::UnsupportedCapability(
            "assemblyai is speech-to-text only".into(),
        ))
    }

    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult, LlmError> {
        let base = self.base_url();
        let mut upload_req = self
            .client
            .post(format!("{base}/v2/upload"))
            .headers(self.auth())
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(wav_data.to_vec());
        upload_req = upload_req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let upload_resp = send_request(upload_req, None).await?;
        let upload_body = upload_resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let upload_json: serde_json::Value = serde_json::from_str(&upload_body)
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let audio_url = upload_json["upload_url"].as_str().ok_or_else(|| {
            LlmError::InvalidResponse("AssemblyAI upload missing 'upload_url'".into())
        })?;

        let mut create_json = serde_json::json!({ "audio_url": audio_url });
        let model = self.endpoint.model_name.trim();
        if !model.is_empty() && model != "assemblyai_default" {
            create_json["speech_model"] = serde_json::json!(model);
        }
        let mut create_req = self
            .client
            .post(format!("{base}/v2/transcript"))
            .headers(self.auth())
            .json(&create_json);
        create_req = create_req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let create_resp = send_request(create_req, None).await?;
        let create_body = create_resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let create: serde_json::Value = serde_json::from_str(&create_body)
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let job_id = create["id"].as_str().ok_or_else(|| {
            LlmError::InvalidResponse("AssemblyAI response missing transcript id".into())
        })?;

        let job_url = format!("{base}/v2/transcript/{job_id}");
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.endpoint.timeout_secs.max(self.poll_interval.as_secs()));
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(LlmError::Timeout(format!(
                    "AssemblyAI transcription timed out after {}s",
                    self.endpoint.timeout_secs
                )));
            }
            let mut poll_req = self.client.get(&job_url).headers(self.auth());
            poll_req = poll_req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
            let poll_resp = send_request(poll_req, None).await?;
            let poll_body = poll_resp
                .text()
                .await
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
            let job: serde_json::Value = serde_json::from_str(&poll_body)
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
            match job["status"].as_str().unwrap_or("") {
                "completed" => {
                    let text = job["text"].as_str().unwrap_or("").trim().to_string();
                    let confidence = job["confidence"].as_f64().map(|c| c as f32);
                    return Ok(SttResult { text, confidence });
                }
                "error" => {
                    let err = job["error"].as_str().unwrap_or("unknown error");
                    return Err(LlmError::RequestFailed(format!(
                        "AssemblyAI transcription error: {err}"
                    )));
                }
                _ => tokio::time::sleep(self.poll_interval).await,
            }
        }
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        if self.endpoint.api_key.trim().is_empty() {
            return Err(LlmError::Auth("assemblyai api_key not configured".into()));
        }
        Ok(())
    }
}
