//! Public Sign in with ChatGPT OAuth and model-listing operations.
//!
//! This module uses the documented OAuth and `/v1` endpoints only. The app
//! owns loopback callback handling and secure credential persistence; this
//! module owns the provider protocol and token validation/refresh.

use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use url::Url;
use uuid::Uuid;

const AUTHORIZATION_ENDPOINT: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY_ENDPOINT: &str = "https://auth.openai.com/.well-known/openid-configuration";
const RESOURCE: &str = "https://api.openai.com/v1";
const REQUIRED_SCOPE: &str = "chatgpt.tokens.use.direct";

#[async_trait::async_trait]
pub trait ChatGptPlanAuthPort: Send + Sync {
    /// Resolve an opaque `cred-*` reference into a current access token.
    async fn access_token(&self, credential_ref: &str) -> Result<String, crate::LlmError>;
}

/// OAuth record stored only in the platform credential store. Its Debug
/// representation deliberately never includes identity or token material.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatGptPlanCredential {
    pub client_id: String,
    pub ext_agent_host_id: String,
    pub subject: String,
    #[serde(default)]
    pub email: Option<String>,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub scopes: Vec<String>,
}

impl fmt::Debug for ChatGptPlanCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptPlanCredential")
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

/// One in-flight OAuth attempt. State, nonce, PKCE verifier, and exact
/// loopback URI are kept in memory until its callback is consumed.
pub struct ChatGptPlanAuthorization {
    state: String,
    nonce: String,
    verifier: String,
    redirect_uri: String,
    host_id: String,
    returning: Option<ChatGptPlanCredential>,
    url: String,
}

impl fmt::Debug for ChatGptPlanAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptPlanAuthorization")
            .field("authorization", &"[REDACTED]")
            .finish()
    }
}

impl ChatGptPlanAuthorization {
    pub fn new(
        host_id: &str,
        redirect_uri: &str,
        returning: Option<ChatGptPlanCredential>,
    ) -> Result<Self, crate::LlmError> {
        let redirect = Url::parse(redirect_uri)
            .map_err(|_| crate::LlmError::Configuration("invalid OAuth callback URL".into()))?;
        if redirect.scheme() != "http"
            || redirect.host_str() != Some("127.0.0.1")
            || redirect.path() != "/auth/callback"
            || redirect.port().is_none()
        {
            return Err(crate::LlmError::Configuration(
                "ChatGPT OAuth requires an HTTP loopback callback on 127.0.0.1/auth/callback"
                    .into(),
            ));
        }
        let host_uuid = host_id.strip_prefix("urn:uuid:").and_then(|value| {
            Uuid::parse_str(value)
                .ok()
                .filter(|uuid| uuid.to_string() == value)
        });
        if host_uuid.is_none() {
            return Err(crate::LlmError::Configuration(
                "invalid ChatGPT OAuth host identity".into(),
            ));
        }

        let state = random_url_safe(32);
        let nonce = random_url_safe(32);
        let verifier = random_url_safe(64);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let client_id = returning
            .as_ref()
            .map(|credential| credential.client_id.as_str())
            .unwrap_or("dynamic_agent_client");
        let mut url = Url::parse(AUTHORIZATION_ENDPOINT)
            .map_err(|_| crate::LlmError::Configuration("invalid OAuth endpoint".into()))?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("client_id", client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", redirect_uri)
                .append_pair(
                    "scope",
                    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                )
                .append_pair("resource", RESOURCE)
                .append_pair("state", &state)
                .append_pair("nonce", &nonce)
                .append_pair("code_challenge_method", "S256")
                .append_pair("code_challenge", &challenge)
                .append_pair("ext_agent_host_id", host_id);
            if let Some(credential) = returning.as_ref() {
                if !credential
                    .scopes
                    .iter()
                    .any(|scope| scope == REQUIRED_SCOPE)
                {
                    // The user explicitly clicked the sign-in button again
                    // to enable plan usage after a prior identity-only grant.
                    query.append_pair("prompt", "consent");
                }
            } else {
                query.append_pair("agent_name_hint", "Haven");
            }
        }
        Ok(Self {
            state,
            nonce,
            verifier,
            redirect_uri: redirect_uri.to_string(),
            host_id: host_id.to_string(),
            returning,
            url: url.to_string(),
        })
    }

    pub fn authorization_url(&self) -> &str {
        &self.url
    }

    pub fn expected_state(&self) -> &str {
        &self.state
    }

    /// Exchange a callback code and validate the returned identity and plan
    /// permission before producing a record suitable for secure storage.
    pub async fn exchange(
        self,
        code: &str,
        returned_client_id: Option<&str>,
    ) -> Result<ChatGptPlanCredential, crate::LlmError> {
        if code.is_empty() {
            return Err(crate::LlmError::RequestFailed(
                "ChatGPT authorization returned no code".into(),
            ));
        }
        let client_id = match (&self.returning, returned_client_id) {
            (Some(previous), Some(returned)) if returned != previous.client_id => {
                return Err(crate::LlmError::RequestFailed(
                    "ChatGPT authorization returned a different client identity".into(),
                ));
            }
            (Some(previous), _) => previous.client_id.clone(),
            (None, Some(returned)) if !returned.trim().is_empty() => returned.to_string(),
            (None, _) => {
                return Err(crate::LlmError::RequestFailed(
                    "ChatGPT registration did not return a client identity".into(),
                ));
            }
        };
        if client_id == "dynamic_agent_client" {
            return Err(crate::LlmError::RequestFailed(
                "ChatGPT registration returned an incomplete client identity".into(),
            ));
        }

        let http = oauth_http_client()?;
        let response = http
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code),
                ("code_verifier", self.verifier.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| crate::LlmError::RequestFailed("ChatGPT token exchange failed".into()))?;
        let token = parse_token_response(response).await?;
        let scopes = token.scopes()?.ok_or_else(|| {
            crate::LlmError::RequestFailed("ChatGPT did not return granted scopes".into())
        })?;
        let id_token = token
            .id_token
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                crate::LlmError::RequestFailed("ChatGPT did not return an identity token".into())
            })?;
        let refresh_token = token
            .refresh_token
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                crate::LlmError::RequestFailed("ChatGPT did not return a refresh token".into())
            })?;
        let claims = validate_id_token(&http, &id_token, &client_id, &self.nonce).await?;
        if let Some(previous) = self.returning.as_ref()
            && previous.subject != claims.sub
        {
            return Err(crate::LlmError::RequestFailed(
                "ChatGPT account identity changed during reauthorization".into(),
            ));
        }
        Ok(ChatGptPlanCredential {
            client_id,
            ext_agent_host_id: self.host_id,
            subject: claims.sub,
            email: claims.email,
            access_token: token.access_token,
            refresh_token,
            expires_at: unix_now().saturating_add(token.expires_in),
            scopes,
        })
    }
}

pub async fn refresh_credential(
    credential: &mut ChatGptPlanCredential,
) -> Result<(), crate::LlmError> {
    if credential.refresh_token.is_empty() || credential.client_id.is_empty() {
        return Err(crate::LlmError::Configuration(
            "ChatGPT credential cannot be refreshed".into(),
        ));
    }
    let http = oauth_http_client()?;
    let response = http
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", credential.client_id.as_str()),
            ("refresh_token", credential.refresh_token.as_str()),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("ChatGPT token refresh failed".into()))?;
    let token = parse_token_response(response).await?;
    let scopes = token.scopes()?.unwrap_or_else(|| credential.scopes.clone());
    let refresh_token = token
        .refresh_token
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            crate::LlmError::RequestFailed("ChatGPT did not rotate the refresh token".into())
        })?;
    credential.access_token = token.access_token;
    credential.refresh_token = refresh_token;
    credential.expires_at = unix_now().saturating_add(token.expires_in);
    credential.scopes = scopes;
    Ok(())
}

/// Fetch account-visible models from the documented `/v1/models` endpoint.
pub async fn list_models(
    access_token: &str,
    proxy_url: Option<&str>,
    no_proxy: Option<&str>,
) -> Result<Vec<crate::ModelInfo>, crate::LlmError> {
    let endpoint = haven_common::config::ModelEndpoint {
        base_url: RESOURCE.into(),
        proxy_url: proxy_url.map(str::to_string),
        no_proxy: no_proxy.map(str::to_string),
        ..Default::default()
    };
    let http = crate::adapters::build_client(&endpoint).map_err(|_| {
        crate::LlmError::Configuration("ChatGPT model-list client unavailable".into())
    })?;
    let response = http
        .get(format!("{RESOURCE}/models"))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("ChatGPT model listing failed".into()))?;
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT model listing".into()))?;
    let rows = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| crate::LlmError::RequestFailed("invalid ChatGPT model listing".into()))?;
    Ok(rows
        .iter()
        .filter(|row| row.get("visibility").and_then(serde_json::Value::as_str) == Some("list"))
        .filter_map(|row| {
            let id = row
                .get("slug")
                .and_then(serde_json::Value::as_str)
                .filter(|slug| !slug.is_empty())?;
            let name = row
                .get("display_name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            Some(crate::ModelInfo {
                id: id.to_string(),
                provider: "openai".into(),
                name: name.to_string(),
                context_window: 0,
                supports_streaming: true,
                supports_tools: true,
                supports_vision: false,
                cost_per_1k_input_tokens: None,
                cost_per_1k_output_tokens: None,
            })
        })
        .collect())
}

fn random_url_safe(bytes: usize) -> String {
    let mut raw = Vec::with_capacity(bytes);
    while raw.len() < bytes {
        raw.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw[..bytes])
}

fn oauth_http_client() -> Result<reqwest::Client, crate::LlmError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| crate::LlmError::Configuration("ChatGPT OAuth client unavailable".into()))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
}

impl TokenResponse {
    fn scopes(&self) -> Result<Option<Vec<String>>, crate::LlmError> {
        if !self.token_type.eq_ignore_ascii_case("bearer") || self.access_token.is_empty() {
            return Err(crate::LlmError::RequestFailed(
                "ChatGPT returned an incomplete token response".into(),
            ));
        }
        Ok(self.scope.as_deref().map(|scope| {
            scope
                .split_ascii_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        }))
    }
}

async fn parse_token_response(
    response: reqwest::Response,
) -> Result<TokenResponse, crate::LlmError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response
        .json()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT token response".into()))
}

async fn response_error(response: reqwest::Response) -> crate::LlmError {
    #[derive(Deserialize)]
    struct OAuthError {
        error: Option<String>,
    }
    let status = response.status();
    let error = response
        .json::<OAuthError>()
        .await
        .ok()
        .and_then(|body| body.error)
        .filter(|value| {
            value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
        .unwrap_or_else(|| "request_failed".into());
    crate::LlmError::RequestFailed(format!("ChatGPT request failed ({status}: {error})"))
}

#[derive(Deserialize)]
struct OidcMetadata {
    issuer: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    alg: Option<String>,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct IdClaims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: u64,
    nonce: String,
    email: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl IdClaims {
    fn aud_matches(&self, client_id: &str) -> bool {
        match &self.aud {
            Audience::One(value) => value == client_id,
            Audience::Many(values) => values.iter().any(|value| value == client_id),
        }
    }
}

async fn validate_id_token(
    http: &reqwest::Client,
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<IdClaims, crate::LlmError> {
    let metadata: OidcMetadata = http
        .get(DISCOVERY_ENDPOINT)
        .send()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("ChatGPT identity validation failed".into()))?
        .error_for_status()
        .map_err(|_| crate::LlmError::RequestFailed("ChatGPT identity validation failed".into()))?
        .json()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT identity metadata".into()))?;
    let jwks_url = Url::parse(&metadata.jwks_uri)
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT identity metadata".into()))?;
    if metadata.issuer != "https://auth.openai.com"
        || jwks_url.scheme() != "https"
        || jwks_url.host_str() != Some("auth.openai.com")
    {
        return Err(crate::LlmError::RequestFailed(
            "invalid ChatGPT identity metadata".into(),
        ));
    }
    let jwks: Jwks = http
        .get(jwks_url)
        .send()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("ChatGPT identity validation failed".into()))?
        .error_for_status()
        .map_err(|_| crate::LlmError::RequestFailed("ChatGPT identity validation failed".into()))?
        .json()
        .await
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT identity keys".into()))?;
    let header = decode_header(token)
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT identity token".into()))?;
    if header.alg != Algorithm::RS256 {
        return Err(crate::LlmError::RequestFailed(
            "unsupported ChatGPT identity token signature".into(),
        ));
    }
    let key = jwks
        .keys
        .iter()
        .find(|key| {
            key.kid == header.kid.as_deref().unwrap_or_default()
                && key.kty == "RSA"
                && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
        })
        .ok_or_else(|| {
            crate::LlmError::RequestFailed("unrecognized ChatGPT identity signer".into())
        })?;
    let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)
        .map_err(|_| crate::LlmError::RequestFailed("invalid ChatGPT identity key".into()))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&["https://auth.openai.com"]);
    validation.set_audience(&[client_id]);
    validation.leeway = 60;
    let claims = decode::<IdClaims>(token, &decoding_key, &validation)
        .map_err(|_| {
            crate::LlmError::RequestFailed("ChatGPT identity token validation failed".into())
        })?
        .claims;
    if claims.iss != "https://auth.openai.com"
        || !claims.aud_matches(client_id)
        || claims.exp <= unix_now()
        || claims.nonce != nonce
        || claims.sub.is_empty()
    {
        return Err(crate::LlmError::RequestFailed(
            "ChatGPT identity token validation failed".into(),
        ));
    }
    Ok(claims)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(scopes: &[&str]) -> ChatGptPlanCredential {
        ChatGptPlanCredential {
            client_id: "issued-client".into(),
            ext_agent_host_id: "urn:uuid:123e4567-e89b-42d3-a456-426614174000".into(),
            subject: "subject".into(),
            email: Some("person@example.com".into()),
            access_token: "access-token".into(),
            refresh_token: "refresh-token".into(),
            expires_at: 1,
            scopes: scopes.iter().map(|scope| (*scope).into()).collect(),
        }
    }

    #[test]
    fn authorization_uses_official_resource_scope_pkce_and_callback() {
        let authorization = ChatGptPlanAuthorization::new(
            "urn:uuid:123e4567-e89b-42d3-a456-426614174000",
            "http://127.0.0.1:43127/auth/callback",
            None,
        )
        .unwrap();
        let url = Url::parse(authorization.authorization_url()).unwrap();
        let query = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(url.host_str(), Some("auth.openai.com"));
        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("dynamic_agent_client")
        );
        assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(query.get("resource").map(String::as_str), Some(RESOURCE));
        assert_eq!(
            query.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:43127/auth/callback")
        );
        assert!(query.get("scope").is_some_and(|scope| {
            scope
                .split_ascii_whitespace()
                .any(|part| part == REQUIRED_SCOPE)
        }));
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(query.contains_key("code_challenge"));
        assert_eq!(
            query.get("state"),
            Some(&authorization.expected_state().to_string())
        );
        assert!(!query.contains_key("client_secret"));
    }

    #[test]
    fn identity_only_login_reuses_registration_and_requests_consent_only_for_plan_scope() {
        let returning = credential(&["openid", "profile", "email", "offline_access"]);
        let host_id = returning.ext_agent_host_id.clone();
        let authorization = ChatGptPlanAuthorization::new(
            &host_id,
            "http://127.0.0.1:43127/auth/callback",
            Some(returning),
        )
        .unwrap();
        let url = Url::parse(authorization.authorization_url()).unwrap();
        let query = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("issued-client")
        );
        assert_eq!(query.get("prompt").map(String::as_str), Some("consent"));
        assert!(!query.contains_key("id_token_hint"));
        assert!(!query.contains_key("login_hint"));

        let returning = credential(&["chatgpt.tokens.use.direct", "offline_access"]);
        let host_id = returning.ext_agent_host_id.clone();
        let authorization = ChatGptPlanAuthorization::new(
            &host_id,
            "http://127.0.0.1:43127/auth/callback",
            Some(returning),
        )
        .unwrap();
        let url = Url::parse(authorization.authorization_url()).unwrap();
        assert!(!url.query_pairs().any(|(name, _)| name == "prompt"));
    }

    #[test]
    fn authorization_rejects_non_loopback_redirects_and_noncanonical_host_ids() {
        assert!(
            ChatGptPlanAuthorization::new(
                "urn:uuid:123e4567-e89b-42d3-a456-426614174000",
                "http://localhost:43127/auth/callback",
                None,
            )
            .is_err()
        );
        assert!(
            ChatGptPlanAuthorization::new(
                "urn:uuid:not-a-uuid",
                "http://127.0.0.1:43127/auth/callback",
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn token_scopes_can_represent_identity_without_plan_usage_permission() {
        let token = TokenResponse {
            access_token: "token".into(),
            refresh_token: Some("refresh".into()),
            id_token: Some("identity".into()),
            token_type: "Bearer".into(),
            expires_in: 3600,
            scope: Some("openid profile offline_access".into()),
        };
        assert_eq!(
            token.scopes().unwrap().unwrap(),
            ["openid", "profile", "offline_access"]
        );
    }

    #[test]
    fn credential_debug_redacts_all_account_and_token_fields() {
        let credential = credential(&[REQUIRED_SCOPE]);
        let debug = format!("{credential:?}");
        for secret in [
            "person@example.com",
            "issued-client",
            "access-token",
            "refresh-token",
            "opaque-id-token",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}
