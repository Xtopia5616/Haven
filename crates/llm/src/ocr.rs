//! OCR (image text extraction) capability.
//!
//! Unified dispatch entry point: [`build_ocr_client`] maps an `OcrConfig`
//! provider id to a concrete client (the OCR counterpart of `stt.rs`).
//! Providers:
//! - `none`: no client
//! - `baidu`: Baidu 通用文字识别（标准版）
//! - `azure`: Azure AI Vision (Computer Vision 3.2 OCR)
//! - `tencent`: Tencent Cloud 通用印刷体识别
//!
//! Every client takes raw image bytes (base64/raw body per provider wire
//! format) and returns [`OcrResult`]; providers that report per-word
//! confidence (Baidu, Tencent) fill `confidence` so the gateway's confidence
//! gate can fall back to the main model, providers that do not (Azure)
//! leave it `None` and fall back on error / empty text instead.

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use haven_common::config::OcrConfig;
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Outcome of an OCR call: recognized text plus an optional confidence
/// (0.0-1.0) reported by the provider. `None` confidence means the provider
/// does not report confidence; the caller's confidence gate treats that as
/// "no signal" and falls back on error / empty text.
#[derive(Debug, Clone)]
pub struct OcrResult {
    pub text: String,
    pub confidence: Option<f32>,
}

/// Trait for optical character recognition. Implementations receive raw
/// image bytes and return the extracted text.
#[async_trait]
pub trait OcrClient: Send + Sync {
    async fn recognize(&self, image_bytes: &[u8], media_type: &str) -> Result<OcrResult>;
}

/// Build the OCR client for a given config. Returns `None` when the
/// configured provider is `none`, and an error for an unknown provider id.
pub fn build_ocr_client(cfg: &OcrConfig) -> Result<Option<Box<dyn OcrClient>>> {
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let client: Box<dyn OcrClient> = match cfg.provider.as_str() {
        "none" => return Ok(None),
        "baidu" => Box::new(BaiduOcrClient::new(cfg, timeout)),
        "azure" => Box::new(AzureOcrClient::new(cfg, timeout)),
        "tencent" => Box::new(TencentOcrClient::new(cfg, timeout)),
        other => anyhow::bail!("unknown OCR provider: {}", other),
    };
    Ok(Some(client))
}

fn media_http_client(timeout: Duration) -> reqwest::Client {
    crate::client::http_client_builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default()
}

/// Error text extraction for media HTTP responses, so upstream error bodies
/// surface as helpful messages instead of raw HTTP status numbers.
fn media_error_body(kind: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        anyhow::anyhow!("{kind} request failed: HTTP {}", status)
    } else {
        let snippet = if trimmed.len() > 300 {
            &trimmed[..300]
        } else {
            trimmed
        };
        anyhow::anyhow!("{kind} request failed (HTTP {}): {}", status, snippet)
    }
}

/// Flatten recognized text from the Baidu response payload.
/// Sample: `{"words_result": [{"words": "你好", "probability": {"average": 0.99}}]}`
fn parse_baidu_response(body: &str) -> Result<OcrResult> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid Baidu OCR response: {e}"))?;
    if let Some(err) = v.get("error_code") {
        let msg = v
            .get("error_msg")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("Baidu OCR error {}: {}", err, msg);
    }
    let results = v
        .get("words_result")
        .and_then(|r| r.as_array())
        .ok_or_else(|| anyhow::anyhow!("Baidu OCR response missing 'words_result'"))?;
    let mut text = String::new();
    let mut confidences = Vec::new();
    for item in results {
        if let Some(words) = item.get("words").and_then(|w| w.as_str()) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(words);
        }
        if let Some(avg) = item
            .get("probability")
            .and_then(|p| p.get("average"))
            .and_then(|a| a.as_f64())
        {
            confidences.push(avg as f32);
        }
    }
    let confidence = if confidences.is_empty() {
        None
    } else {
        Some(confidences.iter().sum::<f32>() / confidences.len() as f32)
    };
    Ok(OcrResult { text, confidence })
}

/// Flatten recognized text from the Azure Computer Vision 3.2 OCR response.
/// Sample: `{"regions": [{"lines": [{"words": [{"text": "你好"}]}]}]}`
fn parse_azure_response(body: &str) -> Result<OcrResult> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid Azure OCR response: {e}"))?;
    let regions = v
        .get("regions")
        .and_then(|r| r.as_array())
        .ok_or_else(|| anyhow::anyhow!("Azure OCR response missing 'regions'"))?;
    let mut text = String::new();
    for region in regions {
        if let Some(lines) = region.get("lines").and_then(|l| l.as_array()) {
            for line in lines {
                if let Some(words) = line.get("words").and_then(|w| w.as_array()) {
                    let mut line_text = String::new();
                    for word in words {
                        if let Some(t) = word.get("text").and_then(|t| t.as_str()) {
                            if !line_text.is_empty() {
                                line_text.push(' ');
                            }
                            line_text.push_str(t);
                        }
                    }
                    if !line_text.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&line_text);
                    }
                }
            }
        }
    }
    Ok(OcrResult {
        text,
        // Azure OCR v3.2 does not report per-word confidence.
        confidence: None,
    })
}

/// Flatten recognized text from the Tencent response payload.
/// Sample: `{"Response": {"TextDetections": [{"DetectedText": "你好", "Confidence": 99}]}}`
fn parse_tencent_response(body: &str) -> Result<OcrResult> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid Tencent OCR response: {e}"))?;
    let resp = v
        .get("Response")
        .ok_or_else(|| anyhow::anyhow!("Tencent OCR response missing 'Response'"))?;
    if let Some(err) = resp.get("Error") {
        let code = err
            .get("Code")
            .and_then(|c| c.as_str())
            .unwrap_or("Unknown");
        let msg = err
            .get("Message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("Tencent OCR error {code}: {msg}");
    }
    let detections = resp
        .get("TextDetections")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow::anyhow!("Tencent OCR response missing 'TextDetections'"))?;
    let mut text = String::new();
    let mut confidences = Vec::new();
    for item in detections {
        if let Some(t) = item.get("DetectedText").and_then(|t| t.as_str()) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(t);
        }
        if let Some(c) = item.get("Confidence").and_then(|c| c.as_f64()) {
            // Tencent reports confidence on a 0-100 scale; normalize to 0-1.
            confidences.push((c as f32) / 100.0);
        }
    }
    let confidence = if confidences.is_empty() {
        None
    } else {
        Some(confidences.iter().sum::<f32>() / confidences.len() as f32)
    };
    Ok(OcrResult { text, confidence })
}

/// Baidu 通用文字识别（标准版）client.
///
/// Auth is a two-step OAuth flow: `api_key`/`api_secret` (API Key / Secret
/// Key) are exchanged for an access token, which is then sent on the OCR
/// call. The token lasts ~30 days and is cached per client.
pub struct BaiduOcrClient {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
    token_cache: Mutex<Option<(String, Instant)>>,
}

impl BaiduOcrClient {
    pub fn new(cfg: &OcrConfig, timeout: Duration) -> Self {
        Self {
            client: media_http_client(timeout),
            api_key: cfg.api_key.clone(),
            api_secret: cfg.api_secret.clone(),
            token_cache: Mutex::new(None),
        }
    }

    async fn access_token(&self) -> Result<String> {
        if let Some((token, at)) = self.token_cache.lock().unwrap().as_ref() {
            // Tokens expire after ~30 days; refresh with a wide margin.
            if at.elapsed() < Duration::from_secs(25 * 24 * 3600) {
                return Ok(token.clone());
            }
        }
        let url = format!(
            "https://aip.baidubce.com/oauth/2.0/token?grant_type=client_credentials&client_id={}&client_secret={}",
            self.api_key, self.api_secret
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Baidu token request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        if !status.is_success() {
            return Err(media_error_body("Baidu token", status, &body));
        }
        let v: Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("invalid Baidu token response: {e}"))?;
        let token = v
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Baidu token response missing 'access_token': {body}")
            })?;
        let token = token.to_string();
        *self.token_cache.lock().unwrap() = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

#[async_trait]
impl OcrClient for BaiduOcrClient {
    async fn recognize(&self, image_bytes: &[u8], _media_type: &str) -> Result<OcrResult> {
        let token = self.access_token().await?;
        let image = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        // Baidu expects form-urlencoded; the base64 alphabet's `+` must be
        // percent-escaped (the rest of the alphabet is form-safe).
        let image_encoded = image.replace('+', "%2B");
        let url =
            format!("https://aip.baidubce.com/rest/2.0/ocr/v1/general_basic?access_token={token}");
        let body = format!(
            "image={}&detect_direction=true&probability=true",
            image_encoded
        );
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Baidu OCR request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        if !status.is_success() {
            return Err(media_error_body("Baidu OCR", status, &body));
        }
        parse_baidu_response(&body)
    }
}

/// Azure AI Vision (Computer Vision 3.2) OCR client. Raw image bytes are
/// POSTed with the subscription key; `base_url` is the resource endpoint
/// (e.g. `https://<resource>.cognitiveservices.azure.com`).
pub struct AzureOcrClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl AzureOcrClient {
    pub fn new(cfg: &OcrConfig, timeout: Duration) -> Self {
        Self {
            client: media_http_client(timeout),
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
        }
    }
}

#[async_trait]
impl OcrClient for AzureOcrClient {
    async fn recognize(&self, image_bytes: &[u8], _media_type: &str) -> Result<OcrResult> {
        if self.api_key.is_empty() {
            anyhow::bail!("Azure OCR requires an api_key (Ocp-Apim-Subscription-Key)");
        }
        if self.base_url.is_empty() {
            anyhow::bail!("Azure OCR requires a base_url (the resource endpoint)");
        }
        let url = format!(
            "{}/vision/v3.2/ocr?language=zh-Hans&detectOrientation=true",
            self.base_url
        );
        let resp = self
            .client
            .post(&url)
            .header("Ocp-Apim-Subscription-Key", &self.api_key)
            .header("Content-Type", "application/octet-stream")
            .body(image_bytes.to_vec())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Azure OCR request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        if !status.is_success() {
            return Err(media_error_body("Azure OCR", status, &body));
        }
        parse_azure_response(&body)
    }
}

/// Tencent Cloud 通用印刷体识别 client (TC3-HMAC-SHA256 signed).
/// `api_key` is the SecretId, `api_secret` the SecretKey.
pub struct TencentOcrClient {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
}

impl TencentOcrClient {
    pub fn new(cfg: &OcrConfig, timeout: Duration) -> Self {
        Self {
            client: media_http_client(timeout),
            api_key: cfg.api_key.clone(),
            api_secret: cfg.api_secret.clone(),
        }
    }
}

/// Tencent Cloud TC3-HMAC-SHA256 request signing (see Tencent docs,
/// "签名方法 v3"). Deterministic and unit-testable: builds the canonical
/// request, string-to-sign, and the `Authorization` header for a JSON POST.
fn tencent_authorization(
    secret_id: &str,
    secret_key: &str,
    host: &str,
    action: &str,
    payload: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    use sha2::{Digest, Sha256};

    // HMAC-SHA256 built directly on sha2 (no hmac crate dependency): the
    // RFC 2104 construction with a 64-byte block key.
    fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
        const BLOCK: usize = 64;
        let mut k = [0u8; BLOCK];
        if key.len() > BLOCK {
            let digest = Sha256::digest(key);
            k[..32].copy_from_slice(&digest);
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0x36u8; BLOCK];
        let mut opad = [0x5cu8; BLOCK];
        for i in 0..BLOCK {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let mut inner = Sha256::new();
        inner.update(ipad);
        inner.update(data);
        let inner_hash = inner.finalize();
        let mut outer = Sha256::new();
        outer.update(opad);
        outer.update(inner_hash);
        outer.finalize().into()
    }

    let timestamp = now.timestamp();
    let date = now.format("%Y-%m-%d").to_string();
    let content_type = "application/json; charset=utf-8".to_string();
    let x_tc_action = action.to_ascii_lowercase();

    let canonical_headers = format!(
        "content-type:{}\nhost:{}\nx-tc-action:{}\n",
        content_type.to_ascii_lowercase(),
        host,
        x_tc_action
    );
    let signed_headers = "content-type;host;x-tc-action";
    let payload_hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
    let canonical_request = format!(
        "POST\n/\n\n{}\n{}\n{}",
        canonical_headers, signed_headers, payload_hash
    );
    let canonical_request_hash = format!("{:x}", Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{}\n{}/ocr/tc3_request\n{}",
        timestamp, date, canonical_request_hash
    );

    let secret_date = hmac_sha256(format!("TC3{}", secret_key).as_bytes(), date.as_bytes());
    let secret_service = hmac_sha256(&secret_date, b"ocr");
    let secret_signing = hmac_sha256(&secret_service, b"tc3_request");
    let signature = hmac_sha256(&secret_signing, string_to_sign.as_bytes())
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    format!(
        "TC3-HMAC-SHA256 Credential={}/{}/ocr/tc3_request, SignedHeaders={}, Signature={}",
        secret_id, date, signed_headers, signature
    )
}

#[async_trait]
impl OcrClient for TencentOcrClient {
    async fn recognize(&self, image_bytes: &[u8], _media_type: &str) -> Result<OcrResult> {
        if self.api_key.is_empty() || self.api_secret.is_empty() {
            anyhow::bail!("Tencent OCR requires api_key (SecretId) and api_secret (SecretKey)");
        }
        let host = "ocr.tencentcloudapi.com";
        let action = "GeneralBasicOCR";
        let version = "2018-11-19";
        let region = "ap-guangzhou";
        let image = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        let payload = serde_json::json!({
            "ImageBase64": image,
            "LanguageType": "zh",
        })
        .to_string();

        let now = chrono::Utc::now();
        let auth =
            tencent_authorization(&self.api_key, &self.api_secret, host, action, &payload, now);

        let resp = self
            .client
            .post(format!("https://{}", host))
            .header("Content-Type", "application/json; charset=utf-8")
            .header("Host", host)
            .header("X-TC-Action", action)
            .header("X-TC-Version", version)
            .header("X-TC-Timestamp", now.timestamp().to_string())
            .header("X-TC-Region", region)
            .header("Authorization", auth)
            .body(payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Tencent OCR request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        if !status.is_success() {
            return Err(media_error_body("Tencent OCR", status, &body));
        }
        parse_tencent_response(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::OcrConfig;
    use std::time::Duration;

    #[test]
    fn ocr_default_cfg_dispatches_none() {
        let cfg = OcrConfig::default();
        let client = build_ocr_client(&cfg).unwrap();
        assert!(client.is_none());
    }

    #[test]
    fn ocr_unknown_provider_errors() {
        let cfg = OcrConfig {
            provider: "not-a-provider".into(),
            ..Default::default()
        };
        let err = build_ocr_client(&cfg).err().expect("expected error");
        assert!(err.to_string().contains("unknown OCR provider"));
    }

    #[test]
    fn ocr_dispatch_known_providers() {
        for provider in ["baidu", "azure", "tencent"] {
            let cfg = OcrConfig {
                provider: provider.into(),
                api_key: "k".into(),
                api_secret: "s".into(),
                ..Default::default()
            };
            let client = build_ocr_client(&cfg).unwrap();
            assert!(client.is_some(), "provider {provider} should build");
        }
    }

    #[test]
    fn parse_baidu_response_joins_words_and_averages_confidence() {
        let body = r#"{
            "words_result_num": 2,
            "words_result": [
                {"words": "你好", "probability": {"average": 0.99}},
                {"words": "世界", "probability": {"average": 0.95}}
            ]
        }"#;
        let res = parse_baidu_response(body).unwrap();
        assert_eq!(res.text, "你好\n世界");
        let conf = res.confidence.unwrap();
        assert!((conf - 0.97).abs() < 1e-6);
    }

    #[test]
    fn parse_baidu_response_without_probability_has_no_confidence() {
        let body = r#"{"words_result": [{"words": "hi"}]}"#;
        let res = parse_baidu_response(body).unwrap();
        assert_eq!(res.text, "hi");
        assert!(res.confidence.is_none());
    }

    #[test]
    fn parse_baidu_response_surfaces_api_error() {
        let body = r#"{"error_code": 17, "error_msg": "Open api daily request limit reached"}"#;
        let err = parse_baidu_response(body).unwrap_err();
        assert!(err.to_string().contains("17"));
    }

    #[test]
    fn parse_azure_response_flattens_regions_lines_words() {
        let body = r#"{
            "regions": [{
                "lines": [
                    {"words": [{"text": "Hello"}, {"text": "world"}]},
                    {"words": [{"text": "你好"}]}
                ]
            }]
        }"#;
        let res = parse_azure_response(body).unwrap();
        assert_eq!(res.text, "Hello world\n你好");
        assert!(res.confidence.is_none());
    }

    #[test]
    fn parse_tencent_response_joins_detections_and_averages_confidence() {
        let body = r#"{
            "Response": {
                "TextDetections": [
                    {"DetectedText": "第一行", "Confidence": 99},
                    {"DetectedText": "第二行", "Confidence": 95}
                ]
            }
        }"#;
        let res = parse_tencent_response(body).unwrap();
        assert_eq!(res.text, "第一行\n第二行");
        let conf = res.confidence.unwrap();
        assert!((conf - 0.97).abs() < 1e-6);
    }

    #[test]
    fn parse_tencent_response_surfaces_api_error() {
        let body = r#"{"Response": {"Error": {"Code": "AuthFailure", "Message": "bad secret"}}}"#;
        let err = parse_tencent_response(body).unwrap_err();
        assert!(err.to_string().contains("AuthFailure"));
    }

    #[test]
    fn tencent_authorization_produces_expected_shape() {
        use chrono::TimeZone;
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 15, 12, 0, 0).unwrap();
        let auth = tencent_authorization(
            "AKID-example",
            "secret-key",
            "ocr.tencentcloudapi.com",
            "GeneralBasicOCR",
            "{}",
            now,
        );
        assert!(auth.starts_with("TC3-HMAC-SHA256 Credential=AKID-example/2026-08-15/ocr/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature="));
        assert_eq!(auth.split("Signature=").nth(1).unwrap().len(), 64);
    }

    #[test]
    fn baidu_client_defaults() {
        let cfg = OcrConfig {
            provider: "baidu".into(),
            api_key: "k".into(),
            api_secret: "s".into(),
            ..Default::default()
        };
        let client = BaiduOcrClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.api_key, "k");
        assert!(client.token_cache.lock().unwrap().is_none());
    }

    #[test]
    fn azure_client_requires_endpoint_at_call_time() {
        let cfg = OcrConfig {
            provider: "azure".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        let client = AzureOcrClient::new(&cfg, Duration::from_secs(10));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(client.recognize(b"fake", "image/png"))
            .unwrap_err();
        assert!(err.to_string().contains("base_url"));
    }
}
