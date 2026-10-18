//! Application adapter for ChatGPT plan OAuth credentials.
//!
//! The secure credential store remains owned by `ConfigService`; the LLM
//! provider receives only this narrow token capability.

use crate::chatgpt_plan_login::serialize_credential;
use haven_common::config::ConfigService;
use haven_llm::LlmError;
use haven_llm::{ChatGptPlanAuthPort, ChatGptPlanCredential, refresh_chatgpt_plan_credential};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct ConfigChatGptPlanAuthPort {
    config: Arc<ConfigService>,
    refresh_gate: tokio::sync::Mutex<()>,
}

impl ConfigChatGptPlanAuthPort {
    pub(crate) fn new(config: Arc<ConfigService>) -> Self {
        Self {
            config,
            refresh_gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Return a stable installation-scoped host identifier. The config path
    /// is normalized for Windows case-insensitivity before hashing; no user
    /// path or identifier is sent in logs.
    pub(crate) fn host_id(&self) -> Result<String, String> {
        stable_host_id(
            &self
                .config
                .path()
                .map_err(|_| "configuration path unavailable")?,
        )
    }
}

#[async_trait::async_trait]
impl ChatGptPlanAuthPort for ConfigChatGptPlanAuthPort {
    async fn access_token(&self, credential_ref: &str) -> Result<String, LlmError> {
        let _guard = self.refresh_gate.lock().await;
        let serialized = self
            .config
            .read_chatgpt_plan_credential(credential_ref)
            .map_err(|_| {
                LlmError::Configuration("ChatGPT plan credentials are unavailable".into())
            })?;
        let mut credential: ChatGptPlanCredential = serde_json::from_str(&serialized)
            .map_err(|_| LlmError::Configuration("ChatGPT plan credentials are invalid".into()))?;
        if credential
            .scopes
            .iter()
            .all(|scope| scope != "chatgpt.tokens.use.direct")
        {
            return Err(LlmError::Configuration(
                "ChatGPT plan usage permission is unavailable".into(),
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        if credential.expires_at <= now.saturating_add(60) {
            refresh_chatgpt_plan_credential(&mut credential).await?;
            let updated = serialize_credential(&credential).map_err(LlmError::Configuration)?;
            self.config
                .replace_chatgpt_plan_credential(credential_ref, &updated)
                .map_err(|_| {
                    LlmError::Configuration("ChatGPT plan credentials could not be saved".into())
                })?;
            if credential
                .scopes
                .iter()
                .all(|scope| scope != "chatgpt.tokens.use.direct")
            {
                return Err(LlmError::Configuration(
                    "ChatGPT plan usage permission is unavailable".into(),
                ));
            }
        }
        Ok(credential.access_token)
    }
}

fn stable_host_id(path: &std::path::Path) -> Result<String, String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = canonical.to_string_lossy().to_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let uuid = Uuid::from_bytes(bytes);
    Ok(format!("urn:uuid:{uuid}"))
}
