//! Local OAuth loopback owner for ChatGPT plan connections.

use crate::chatgpt_plan_auth::ConfigChatGptPlanAuthPort;
use haven_common::config::ConfigService;
use haven_llm::{ChatGptPlanAuthorization, ChatGptPlanCredential};
use serde::Serialize;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const MAX_CALLBACK_REQUEST_BYTES: usize = 8 * 1024;
const MAX_PENDING_LOGINS: usize = 4;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginStart {
    pub(crate) login_id: String,
    pub(crate) authorization_url: String,
    pub(crate) account_email: Option<String>,
    pub(crate) plan_usage_enabled: bool,
}

pub(crate) struct LoginResult {
    pub(crate) credential: ChatGptPlanCredential,
    pub(crate) account_email: Option<String>,
    pub(crate) plan_usage_enabled: bool,
}

struct Callback {
    code: String,
    client_id: Option<String>,
}

struct PendingLogin {
    provider_name: String,
    expires_at: tokio::time::Instant,
    authorization: Option<ChatGptPlanAuthorization>,
    callback: Option<oneshot::Receiver<Result<Callback, String>>>,
    cancellation: CancellationToken,
    callback_task: tokio::task::AbortHandle,
}

#[derive(Default)]
pub(crate) struct ChatGptPlanLoginOwner {
    pending: Mutex<HashMap<String, PendingLogin>>,
}

impl ChatGptPlanLoginOwner {
    pub(crate) async fn start(
        &self,
        provider_name: &str,
        config: &ConfigService,
        auth: &ConfigChatGptPlanAuthPort,
    ) -> Result<LoginStart, String> {
        let provider_name = provider_name.trim();
        if provider_name.is_empty() {
            return Err("provider name is required".into());
        }
        {
            let mut pending = self.pending.lock().await;
            prune_expired_pending(&mut pending, tokio::time::Instant::now());
            if !has_pending_login_capacity(pending.len()) {
                return Err("too many pending ChatGPT sign-in attempts".into());
            }
        }
        let returning = config
            .chatgpt_plan_credential_reference(provider_name)
            .map_err(|_| "ChatGPT account credentials are unavailable")?
            .map(|reference| {
                let serialized = config
                    .read_chatgpt_plan_credential(&reference)
                    .map_err(|_| "ChatGPT account credentials are unavailable")?;
                serde_json::from_str::<ChatGptPlanCredential>(&serialized)
                    .map_err(|_| "ChatGPT account credentials are invalid")
            })
            .transpose()?;
        let account_email = returning
            .as_ref()
            .and_then(|credential| credential.email.clone());
        let plan_usage_enabled = returning.as_ref().is_some_and(|credential| {
            credential
                .scopes
                .iter()
                .any(|scope| scope == "chatgpt.tokens.use.direct")
        });
        let host_id = returning
            .as_ref()
            .map(|credential| credential.ext_agent_host_id.clone())
            .map(Ok)
            .unwrap_or_else(|| auth.host_id())?;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "could not start local ChatGPT sign-in callback")?;
        let port = listener
            .local_addr()
            .map_err(|_| "could not start local ChatGPT sign-in callback")?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
        let authorization = ChatGptPlanAuthorization::new(&host_id, &redirect_uri, returning)
            .map_err(|_| "could not prepare ChatGPT sign-in")?;
        let authorization_url = authorization.authorization_url().to_string();
        let expected_state = authorization.expected_state().to_string();
        let (sender, receiver) = oneshot::channel();
        let callback_task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                CALLBACK_TIMEOUT,
                receive_callback(listener, &expected_state),
            )
            .await
            .unwrap_or_else(|_| Err("ChatGPT sign-in timed out".into()));
            let _ = sender.send(result);
        });
        let callback_abort = callback_task.abort_handle();

        let login_id = haven_common::types::new_id("login");
        let mut pending = self.pending.lock().await;
        let now = tokio::time::Instant::now();
        prune_expired_pending(&mut pending, now);
        if !has_pending_login_capacity(pending.len()) {
            callback_abort.abort();
            return Err("too many pending ChatGPT sign-in attempts".into());
        }
        pending.insert(
            login_id.clone(),
            PendingLogin {
                provider_name: provider_name.to_string(),
                expires_at: now + CALLBACK_TIMEOUT,
                authorization: Some(authorization),
                callback: Some(receiver),
                cancellation: CancellationToken::new(),
                callback_task: callback_abort,
            },
        );
        Ok(LoginStart {
            login_id,
            authorization_url,
            account_email,
            plan_usage_enabled,
        })
    }

    pub(crate) async fn complete(
        &self,
        login_id: &str,
        provider_name: &str,
    ) -> Result<LoginResult, String> {
        let (authorization, callback_receiver, cancellation) = {
            let mut pending = self.pending.lock().await;
            let attempt = pending
                .get_mut(login_id)
                .ok_or_else(|| "ChatGPT sign-in attempt is unavailable".to_string())?;
            if attempt.provider_name != provider_name.trim() {
                return Err("ChatGPT sign-in provider did not match the request".into());
            }
            let authorization = attempt
                .authorization
                .take()
                .ok_or_else(|| "ChatGPT sign-in attempt is already completing".to_string())?;
            let callback = attempt
                .callback
                .take()
                .ok_or_else(|| "ChatGPT sign-in attempt is already completing".to_string())?;
            (authorization, callback, attempt.cancellation.clone())
        };
        let callback_result = tokio::select! {
            _ = cancellation.cancelled() => Err("ChatGPT sign-in was cancelled".to_string()),
            result = tokio::time::timeout(CALLBACK_TIMEOUT, callback_receiver) => {
                match result {
                    Err(_) => Err("ChatGPT sign-in timed out".to_string()),
                    Ok(Err(_)) => Err("ChatGPT sign-in callback ended unexpectedly".to_string()),
                    Ok(Ok(result)) => result,
                }
            }
        };
        let callback = match callback_result {
            Ok(callback) => callback,
            Err(error) => {
                self.remove_and_abort(login_id).await;
                return Err(error);
            }
        };
        let credential = tokio::select! {
            _ = cancellation.cancelled() => {
                self.remove_and_abort(login_id).await;
                return Err("ChatGPT sign-in was cancelled".into());
            }
            result = authorization.exchange(&callback.code, callback.client_id.as_deref()) => {
                match result {
                    Ok(credential) => credential,
                    Err(_) => {
                        self.remove_and_abort(login_id).await;
                        return Err("ChatGPT sign-in could not be validated".into());
                    }
                }
            }
        };
        self.remove_and_abort(login_id).await;
        let account_email = credential.email.clone();
        let plan_usage_enabled = credential
            .scopes
            .iter()
            .any(|scope| scope == "chatgpt.tokens.use.direct");
        Ok(LoginResult {
            credential,
            account_email,
            plan_usage_enabled,
        })
    }

    pub(crate) async fn cancel(&self, login_id: &str) {
        if let Some(attempt) = self.pending.lock().await.remove(login_id) {
            attempt.cancellation.cancel();
            attempt.callback_task.abort();
        }
    }

    async fn remove_and_abort(&self, login_id: &str) {
        if let Some(attempt) = self.pending.lock().await.remove(login_id) {
            attempt.cancellation.cancel();
            attempt.callback_task.abort();
        }
    }
}

fn has_pending_login_capacity(pending_count: usize) -> bool {
    pending_count < MAX_PENDING_LOGINS
}

fn prune_expired_pending(pending: &mut HashMap<String, PendingLogin>, now: tokio::time::Instant) {
    let expired = pending
        .iter()
        .filter_map(|(id, attempt)| (attempt.expires_at <= now).then_some(id.clone()))
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(attempt) = pending.remove(&id) {
            attempt.cancellation.cancel();
            attempt.callback_task.abort();
        }
    }
}

async fn receive_callback(listener: TcpListener, expected_state: &str) -> Result<Callback, String> {
    let (mut stream, peer) = listener
        .accept()
        .await
        .map_err(|_| "ChatGPT sign-in callback failed")?;
    if !peer.ip().is_loopback() {
        let _ = reply(&mut stream, false).await;
        return Err("ChatGPT sign-in callback was rejected".into());
    }
    let request = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        read_request(&mut stream),
    )
    .await
    .map_err(|_| "ChatGPT sign-in callback timed out")??;
    let result = parse_callback(&request, expected_state);
    let _ = reply(&mut stream, result.is_ok()).await;
    result
}

async fn read_request(stream: &mut TcpStream) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        if bytes.len() >= MAX_CALLBACK_REQUEST_BYTES {
            return Err("ChatGPT sign-in callback request was too large".into());
        }
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| "ChatGPT sign-in callback request failed")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| "ChatGPT sign-in callback request was invalid".into())
}

fn parse_callback(request: &str, expected_state: &str) -> Result<Callback, String> {
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.split_ascii_whitespace().next())
        .ok_or_else(|| "ChatGPT sign-in callback request was invalid".to_string())?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| "ChatGPT sign-in callback request was invalid")?;
    if url.path() != "/auth/callback" {
        return Err("ChatGPT sign-in callback path was invalid".into());
    }
    let mut state = None;
    let mut code = None;
    let mut client_id = None;
    let mut oauth_error = false;
    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "state" => state = Some(value.into_owned()),
            "code" => code = Some(value.into_owned()),
            "client_id" => client_id = Some(value.into_owned()),
            "error" => oauth_error = true,
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err("ChatGPT sign-in callback state did not match".into());
    }
    if oauth_error {
        return Err("ChatGPT sign-in was not authorized".into());
    }
    let code = code.filter(|value| !value.is_empty()).ok_or_else(|| {
        "ChatGPT sign-in callback did not contain an authorization code".to_string()
    })?;
    Ok(Callback { code, client_id })
}

async fn reply(stream: &mut TcpStream, accepted: bool) -> std::io::Result<()> {
    let (status, title, detail) = if accepted {
        (
            "200 OK",
            "Sign-in received",
            "Return to Haven to finish connecting ChatGPT.",
        )
    } else {
        (
            "400 Bad Request",
            "Sign-in not completed",
            "Return to Haven and try again.",
        )
    };
    let body = format!(
        "<!doctype html><html><meta charset=\"utf-8\"><title>{title}</title><body><h1>{title}</h1><p>{detail}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

pub(crate) fn serialize_credential(credential: &ChatGptPlanCredential) -> Result<String, String> {
    const CREDENTIAL_BLOB_MAX_BYTES: usize = 5 * 512;
    let serialized = serde_json::to_string(credential)
        .map_err(|_| "ChatGPT account credentials could not be saved".to_string())?;
    if serialized.len() > CREDENTIAL_BLOB_MAX_BYTES {
        return Err("ChatGPT account credentials exceed the Windows secure-storage limit".into());
    }
    Ok(serialized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loopback_callback_and_requires_matching_state() {
        let callback = parse_callback(
            "GET /auth/callback?code=auth-code&state=state-1&client_id=issued-client HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "state-1",
        )
        .unwrap();
        assert_eq!(callback.code, "auth-code");
        assert_eq!(callback.client_id.as_deref(), Some("issued-client"));

        assert!(
            parse_callback(
                "GET /auth/callback?code=auth-code&state=attacker HTTP/1.1\r\n\r\n",
                "state-1"
            )
            .is_err()
        );
        assert!(
            parse_callback(
                "GET /other?code=auth-code&state=state-1 HTTP/1.1\r\n\r\n",
                "state-1"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_callback_errors_and_missing_codes() {
        assert!(
            parse_callback(
                "GET /auth/callback?error=access_denied&state=state-1 HTTP/1.1\r\n\r\n",
                "state-1"
            )
            .is_err()
        );
        assert!(
            parse_callback(
                "GET /auth/callback?state=state-1 HTTP/1.1\r\n\r\n",
                "state-1"
            )
            .is_err()
        );
    }

    #[test]
    fn pending_login_capacity_is_bounded() {
        assert!(has_pending_login_capacity(0));
        assert!(has_pending_login_capacity(MAX_PENDING_LOGINS - 1));
        assert!(!has_pending_login_capacity(MAX_PENDING_LOGINS));
        assert!(!has_pending_login_capacity(MAX_PENDING_LOGINS + 1));
    }

    #[test]
    fn refuses_credentials_larger_than_windows_secure_storage_limit() {
        let credential = ChatGptPlanCredential {
            client_id: "client".into(),
            ext_agent_host_id: "urn:uuid:123e4567-e89b-42d3-a456-426614174000".into(),
            subject: "subject".into(),
            email: Some("person@example.com".into()),
            access_token: "x".repeat(3_000),
            refresh_token: "refresh".into(),
            expires_at: 1,
            scopes: vec!["chatgpt.tokens.use.direct".into()],
        };
        assert!(
            serialize_credential(&credential)
                .unwrap_err()
                .contains("secure-storage limit")
        );
    }

    #[tokio::test]
    async fn cancel_removes_the_attempt_and_signals_its_waiter() {
        let owner = ChatGptPlanLoginOwner::default();
        let cancellation = CancellationToken::new();
        let waiter = cancellation.clone();
        let callback_task = tokio::spawn(async move { waiter.cancelled().await });
        let callback_abort = callback_task.abort_handle();
        let (_, callback) = oneshot::channel();
        let authorization = ChatGptPlanAuthorization::new(
            "urn:uuid:123e4567-e89b-42d3-a456-426614174000",
            "http://127.0.0.1:43127/auth/callback",
            None,
        )
        .unwrap();
        owner.pending.lock().await.insert(
            "attempt-1".into(),
            PendingLogin {
                provider_name: "chatgpt".into(),
                expires_at: tokio::time::Instant::now() + CALLBACK_TIMEOUT,
                authorization: Some(authorization),
                callback: Some(callback),
                cancellation: cancellation.clone(),
                callback_task: callback_abort,
            },
        );

        owner.cancel("attempt-1").await;
        assert!(cancellation.is_cancelled());
        assert!(owner.pending.lock().await.is_empty());
        assert!(callback_task.await.unwrap_err().is_cancelled());
    }
}
