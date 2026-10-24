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

const CALLBACK_PAGE_STYLE: &str = r#"
	:root {
		color-scheme: light dark;
		font-family: "Roboto", "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif;
		--primary: #2c5090;
		--surface: #f6f8fb;
		--card: #ffffff;
		--on-surface: #202b3c;
		--on-surface-variant: #536176;
		--outline: #c7d1df;
		--container: #f0f3f7;
		--success: #006548;
		--on-success: #ffffff;
		--success-container: #e2f2ea;
		--error: #b13e42;
		--shadow: rgba(20, 30, 46, .12);
		--error-container: #f9e8e7;
	}
	* { box-sizing: border-box; }
	body {
		margin: 0;
		min-height: 100vh;
		background: var(--surface);
		color: var(--on-surface);
	}
	.shell { min-height: 100vh; display: grid; place-items: center; padding: 24px; }
	.card {
		width: min(100%, 440px);
		padding: 28px;
		border: 1px solid var(--outline);
		border-radius: 28px;
		background: var(--card);
		box-shadow: 0 2px 8px var(--shadow);
	}
	.brand { display: flex; align-items: center; gap: 9px; color: var(--primary); font-size: 15px; font-weight: 700; }
	.brand-mark { width: 30px; height: 30px; flex: 0 0 auto; }
	.status-icon {
		width: 52px; height: 52px; display: grid; place-items: center; margin: 30px auto 20px;
		border-radius: 50%; background: var(--success-container); color: var(--success);
	}
	.card--failure .status-icon { background: var(--error-container); color: var(--error); }
	.status-icon svg { width: 26px; height: 26px; }
	h1 { margin: 0; text-align: center; font-size: 24px; line-height: 1.35; letter-spacing: -.02em; }
	.description { margin: 10px auto 0; max-width: 34em; color: var(--on-surface-variant); text-align: center; font-size: 14px; line-height: 1.65; }
	.next-step {
		display: flex; align-items: flex-start; gap: 12px; margin-top: 24px; padding: 15px;
		border-radius: 16px; background: var(--container);
	}
	.step-mark { flex: 0 0 20px; height: 20px; display: grid; place-items: center; border-radius: 50%; background: color-mix(in srgb, var(--primary) 15%, transparent); color: var(--primary); font-size: 12px; font-weight: 700; }
	.next-step strong { display: block; margin: 0 0 3px; font-size: 13px; }
	.next-step p { margin: 0; color: var(--on-surface-variant); font-size: 13px; line-height: 1.5; }
	.footnote { margin: 18px 0 0; color: var(--on-surface-variant); text-align: center; font-size: 12px; line-height: 1.5; }
	@media (max-width: 480px) { .shell { padding: 16px; } .card { padding: 24px 20px 20px; border-radius: 24px; } }
	@media (prefers-color-scheme: dark) {
		:root {
			--primary: #8b9fc2;
			--surface: #171c24;
			--card: #1d232d;
			--on-surface: #e5ebf3;
			--on-surface-variant: #b8c4d4;
			--outline: #435166;
			--container: #232b37;
			--success: #83cfac;
			--on-success: #183a2d;
			--success-container: #203e33;
			--error: #f0a5a5;
			--shadow: rgba(0, 0, 0, .3);
			--error-container: #492b31;
		}
		.card { box-shadow: 0 2px 12px var(--shadow); }
	}
"#;

fn callback_page(accepted: bool) -> (&'static str, String) {
    let (status, state_class, title, description, next_title, next_detail, icon) = if accepted {
        (
            "200 OK",
            "success",
            "授权已返回 Haven",
            "请切回 Haven。应用正在验证登录并安全保存账户。",
            "回到 Haven",
            "保持应用打开，等待连接完成。",
            r#"<svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="m5 12.5 4.2 4.2L19 7" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
        )
    } else {
        (
            "400 Bad Request",
            "failure",
            "授权未完成",
            "Haven 无法验证此次授权。请关闭此页面，回到设置重新登录。",
            "重新连接 ChatGPT",
            "在 Haven 的模型设置中重新开始登录。",
            r#"<svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M12 8v5m0 3.5h.01M10.3 3.8 2.5 17.3A2 2 0 0 0 4.2 20h15.6a2 2 0 0 0 1.7-2.7L13.7 3.8a2 2 0 0 0-3.4 0Z" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
        )
    };
    let body = format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
	<meta charset="utf-8">
	<meta name="viewport" content="width=device-width, initial-scale=1">
	<meta name="color-scheme" content="light dark">
	<title>{title} · Haven</title>
	<style>{CALLBACK_PAGE_STYLE}</style>
</head>
<body>
	<main class="shell">
		<article class="card card--{state_class}" aria-labelledby="page-title">
			<div class="brand">
				<svg class="brand-mark" viewBox="0 0 64 64" fill="none" aria-hidden="true">
					<path d="M18 15h28c5.523 0 10 4.477 10 10v11c0 5.523-4.477 10-10 10H32L21 54v-8h-3c-5.523 0-10-4.477-10-10V25c0-5.523 4.477-10 10-10Z" fill="var(--primary)" />
					<g fill="var(--card)">
						<rect x="24" y="24.5" width="5" height="12" rx="2.5" />
						<rect x="29.5" y="21.5" width="5" height="18" rx="2.5" />
						<rect x="35" y="24.5" width="5" height="12" rx="2.5" />
					</g>
				</svg>
				<span>Haven</span>
			</div>
			<div class="status-icon">{icon}</div>
			<h1 id="page-title">{title}</h1>
			<p class="description">{description}</p>
			<div class="next-step">
				<span class="step-mark" aria-hidden="true">›</span>
				<div><strong>{next_title}</strong><p>{next_detail}</p></div>
			</div>
			<p class="footnote">此页面不会显示或保存您的账户凭据。</p>
		</article>
	</main>
</body>
</html>"#
    );
    (status, body)
}

async fn reply(stream: &mut TcpStream, accepted: bool) -> std::io::Result<()> {
    let (status, body) = callback_page(accepted);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

pub(crate) fn serialize_credential(credential: &ChatGptPlanCredential) -> Result<String, String> {
    serde_json::to_string(credential)
        .map_err(|_| "ChatGPT account credentials could not be saved".to_string())
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
    fn serializes_credentials_larger_than_a_single_windows_credential_blob() {
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
        let serialized = serialize_credential(&credential).unwrap();
        assert!(serialized.len() > 5 * 512);
    }

    #[test]
    fn callback_pages_explain_success_and_failure_without_external_assets() {
        let (success_status, success) = callback_page(true);
        assert_eq!(success_status, "200 OK");
        assert!(success.contains("授权已返回 Haven"));
        assert!(success.contains("回到 Haven"));
        assert!(!success.contains("<script"));
        assert!(!success.contains("https://"));

        let (failure_status, failure) = callback_page(false);
        assert_eq!(failure_status, "400 Bad Request");
        assert!(failure.contains("授权未完成"));
        assert!(failure.contains("重新连接 ChatGPT"));
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
