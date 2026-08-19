use crate::app_state::AppState;
use crate::commands::SessionListResponse;
use crate::commands::log_err;
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::session_steps::SessionStep;
use haven_memory::repositories::sessions::Session;
use serde::Serialize;
use std::sync::Arc;
use tauri::Emitter;
use tauri::State;

#[tauri::command]
pub async fn reopen_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    tracing::debug!("reopen_session called: session_id={}", session_id);
    state
        .agent
        .reopen_session(&session_id)
        .await
        .map_err(|e| log_err("reopen_session", e))?;
    tracing::debug!("reopen_session done");
    Ok(())
}

#[tauri::command]
pub async fn get_sessions(state: State<'_, Arc<AppState>>) -> Result<SessionListResponse, String> {
    let sessions = state.executor.list_sessions().await;
    Ok(SessionListResponse { sessions })
}

#[tauri::command]
pub async fn end_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    _app: tauri::AppHandle,
) -> Result<(), String> {
    // L3: capture the title BEFORE end_session removes the session from the
    // in-memory list; reading afterwards would fall back to the DB and lose
    // the generated title (end_session clears the working set).
    let title = state
        .executor
        .get_session(&session_id)
        .await
        .map(|t| t.title.clone().unwrap_or(t.input))
        .or_else(|| {
            state
                .db
                .get_session(&session_id)
                .ok()
                .flatten()
                .map(|t| t.title.unwrap_or(t.input_text))
        })
        .unwrap_or_default();

    let _ = state
        .executor
        .end_session(&session_id)
        .await
        .map_err(|e| log_err("end_session", e))?;
    // end_session always ends as Completed — the user explicitly finished the
    // session, so it is reported as completed (with notification), never error.
    state
        .agent
        .emit_session_completed(&session_id, &title)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn resolve_confirmation(
    state: State<'_, Arc<AppState>>,
    step_id: String,
    confirmed: bool,
    trust_session: Option<bool>,
) -> Result<(), String> {
    // Resolve the confirmation and capture the step's risk level atomically
    // (under the executor's sessions lock). This avoids the previous race where
    // the resolution and a separate `list_sessions()` lookup could observe a
    // step that a concurrent `end_session`/rollback had already removed.
    let resolution = state
        .executor
        .resolve_confirmation(&step_id.into(), confirmed)
        .await
        .map_err(|e| log_err("resolve_confirmation", e))?;
    if trust_session.unwrap_or(false)
        && confirmed
        && let Some((level, session_id)) = resolution
    {
        // Trust is recorded per conversation: it is scoped to the session that
        // actually owns this confirmation (from the wait, not the caller), so
        // an approval can never leak into other conversations. A None session
        // (background action without a conversation) records nothing.
        state
            .tools
            .safety_gateway
            .trust_risk_level(session_id.as_deref(), level)
            .await;
    }
    Ok(())
}

/// Manually update a session's display title.
#[tauri::command]
pub async fn update_session_title(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Title cannot be empty".into());
    }
    state
        .db
        .update_session_title(&session_id, &title)
        .map_err(|e| log_err("update_session_title", e))?;
    state
        .executor
        .update_session_title(&session_id, &title)
        .await;
    let _ = app.emit(
        "session:title-updated",
        serde_json::json!({
            "session_id": session_id,
            "title": title,
        }),
    );
    Ok(())
}

#[tauri::command]
pub async fn delete_session(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<(), String> {
    state
        .db
        .delete_session(&session_id)
        .map_err(|e| log_err("delete_session", e))?;
    state.executor.remove_session(&session_id).await;
    // The session is gone, so no `session:updated` terminal transition will ever
    // fire for it; a dedicated `session:deleted` lets listeners (busy-session
    // tracking, per-session state) release the id immediately.
    let _ = app.emit(
        "session:deleted",
        serde_json::json!({ "session_id": session_id }),
    );
    Ok(())
}

#[tauri::command]
pub async fn clear_history(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<u64, String> {
    let count = state
        .db
        .clear_sessions()
        .map(|n| n as u64)
        .map_err(|e| log_err("clear_history", e))?;
    state.executor.clear_all_sessions().await;
    // `session_id: null` signals "every session was removed" so listeners clear
    // per-session state (e.g. the busy set) in one shot instead of one event
    // per deleted session.
    let _ = app.emit("session:deleted", serde_json::json!({ "session_id": null }));
    Ok(count)
}

/// Roll back a session to a specific branch point. The session is rewound to
/// the saved state at that step. When `pause` is true the session is set to
/// Paused (user wants to edit the message before re-sending); otherwise it
/// is set to Pending for immediate re-execution. `target_message_id` is the
/// id of the exact message being rolled back; it lets the backend detect an
/// orphan rollback (a user message that was never processed into the
/// ReAct context). The id must resolve to a persisted session message when
/// `pause` is true — an unresolvable id is an error, not a content-based
/// guess.
#[tauri::command]
pub async fn rollback_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    target_step: u32,
    pause: Option<bool>,
    target_message_id: Option<String>,
) -> Result<(), String> {
    state
        .agent
        .rollback_session(
            &session_id,
            target_step,
            pause.unwrap_or(false),
            target_message_id.as_deref(),
        )
        .await
        .map_err(|e| log_err("rollback_session", e))
}

/// Resume a session that errored mid-step. Removes partial output persisted on
/// error and sets the session to Pending so the dispatcher retries the failed
/// step from the saved snapshot.
#[tauri::command]
pub async fn continue_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state
        .agent
        .continue_session(&session_id)
        .await
        .map_err(|e| log_err("continue_session", e))
}

#[derive(Serialize)]
pub struct SessionReviewResponse {
    pub session: Session,
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
    /// Persisted cumulative token/cost counters for the session, so a resumed
    /// or auto-restored conversation can restore the token-stats display.
    /// When the session predates usage persistence (no `session_usage` row) this
    /// falls back to a rough estimate derived from the persisted message
    /// and step text, flagged by `usage_estimated`.
    pub usage: Option<haven_memory::repositories::usage::SessionUsage>,
    /// True when `usage` is an estimate (session created before per-session usage
    /// counters were persisted) rather than the real recorded totals.
    pub usage_estimated: bool,
    /// Per-LLM-call usage detail (one row per model response: step, role,
    /// model, tokens, cost, duration), oldest first. Empty for sessions that
    /// predate per-call usage persistence.
    pub llm_usage: Vec<haven_memory::repositories::usage::LlmCallUsage>,
}

/// Rough token-count estimate for sessions that predate usage persistence.
/// Counts CJK characters as ~1 token and other characters as ~1/4 token
/// across persisted messages and tool steps, adds a flat prompt/tool
/// definition overhead, and charges 800 tokens per image attachment.
/// Cost is unknown, so `has_cost` stays false. Estimates are computed on
/// read and never written to `session_usage`, so a resumed conversation's
/// real counters can never be contaminated by them.
fn estimate_session_usage(
    messages: &[Message],
    steps: &[SessionStep],
) -> haven_memory::repositories::usage::SessionUsage {
    use haven_memory::repositories::usage::SessionUsage;

    fn estimate_text(text: &str) -> u32 {
        let mut cjk: u32 = 0;
        let mut other: u32 = 0;
        for ch in text.chars() {
            let cp = ch as u32;
            if (0x4E00..=0x9FFF).contains(&cp)
                || (0x3000..=0x303F).contains(&cp)
                || (0x3040..=0x30FF).contains(&cp)
            {
                cjk += 1;
            } else {
                other += 1;
            }
        }
        cjk + other / 4
    }

    let mut total: u32 = 0;
    for m in messages {
        total += estimate_text(&m.content);
        for att in &m.attachments {
            if att.media_type.starts_with("image/") {
                total += 800;
            } else {
                total += 300;
            }
        }
    }
    // Tool input/observation are stored only in steps (never in messages);
    // thought text lives in the message stream, so it is already counted
    // above and must not be re-counted here.
    for s in steps {
        if let Some(i) = &s.action_input {
            total += estimate_text(i);
        }
        if let Some(o) = &s.observation {
            total += estimate_text(o);
        }
    }
    // Fixed system prompt + tool definition overhead (~6.5K chars prompt).
    total += 3000;
    let prompt = total * 2 / 3;
    let completion = total - prompt;
    SessionUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cost_usd: 0.0,
        has_cost: false,
    }
}

/// Load the session's messages and steps into a review response.
/// Shared by `get_session_for_review` and `get_last_conversation`.
fn review_response_for_session(
    db: &haven_memory::Database,
    session: Session,
) -> Result<SessionReviewResponse, String> {
    let messages = db
        .get_session_messages(&session.id)
        .map_err(|e| log_err("review_response_for_session", e))?;
    let steps = db
        .get_session_steps(&session.id)
        .map_err(|e| log_err("review_response_for_session", e))?;
    let (usage, usage_estimated) = match db
        .get_session_usage(&session.id)
        .map_err(|e| log_err("review_response_for_session", e))?
    {
        Some(u) => (Some(u), false),
        None => (Some(estimate_session_usage(&messages, &steps)), true),
    };
    let llm_usage = db
        .get_session_llm_usage(&session.id)
        .map_err(|e| log_err("review_response_for_session", e))?;
    Ok(SessionReviewResponse {
        session,
        messages,
        steps,
        usage,
        usage_estimated,
        llm_usage,
    })
}

#[tauri::command]
pub async fn get_session_for_review(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<SessionReviewResponse, String> {
    let session = state
        .db
        .get_session(&session_id)
        .map_err(|e| log_err("get_session_for_review", e))?
        .ok_or_else(|| format!("Session not found: {}", session_id))?;
    review_response_for_session(&state.db, session)
}

/// Return the most recent persisted session with its session messages and
/// steps, for the chat page to auto-restore the last conversation on app
/// start. Returns `None` when no session exists yet.
#[tauri::command]
pub async fn get_last_conversation(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<SessionReviewResponse>, String> {
    let sessions = state
        .db
        .list_sessions(1, 0)
        .map_err(|e| log_err("get_last_conversation", e))?;
    match sessions.into_iter().next() {
        Some(session) => review_response_for_session(&state.db, session).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SessionListResponse;
    use haven_common::types::MessageAttachment;

    #[test]
    fn test_session_list_response_serde() {
        let resp = SessionListResponse { sessions: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"sessions":[]}"#);
    }

    #[test]
    fn test_estimate_session_usage_has_no_cost() {
        let msg = Message {
            id: "m1".into(),
            session_id: "t1".into(),
            role: "user".into(),
            content: "你好 world".into(),
            message_type: None,
            created_at: String::new(),
            tool_call_id: None,
            attachments: vec![MessageAttachment::new("image/png", "aGVsbG8=")],
            voice: false,
        };
        let step = SessionStep {
            id: "s1".into(),
            session_id: "t1".into(),
            step_number: 0,
            thought: Some("查找文件".into()),
            action_tool: Some("file".into()),
            action_input: Some("{\"path\":\"C:/tmp\"}".into()),
            observation: Some("found 3 files".into()),
            status: "completed".into(),
            is_high_risk: false,
            confirmed: Some(true),
            silent: false,
            started_at: None,
            completed_at: None,
            created_at: String::new(),
        };
        let u = estimate_session_usage(&[msg], &[step]);
        assert!(!u.has_cost);
        assert_eq!(u.cost_usd, 0.0);
        // CJK chars count 1 token, latin chars count 1/4, image +800,
        // plus the 3000 flat prompt overhead.
        assert_eq!(u.prompt_tokens + u.completion_tokens, u.total_tokens);
        assert!(u.total_tokens > 3000);
    }

    #[test]
    fn test_estimate_session_usage_cjk_weighting() {
        let cjk = Message {
            id: "m1".into(),
            session_id: "t1".into(),
            role: "user".into(),
            content: "你好世界".into(),
            message_type: None,
            created_at: String::new(),
            tool_call_id: None,
            attachments: vec![],
            voice: false,
        };
        let latin = Message {
            id: "m2".into(),
            session_id: "t1".into(),
            role: "user".into(),
            content: "hello".into(),
            message_type: None,
            created_at: String::new(),
            tool_call_id: None,
            attachments: vec![],
            voice: false,
        };
        let u1 = estimate_session_usage(&[cjk], &[]);
        let u2 = estimate_session_usage(&[latin], &[]);
        // 4 CJK chars = 4 tokens; 5 latin chars = 1 token.
        assert_eq!(u1.total_tokens - u2.total_tokens, 3);
    }
}
