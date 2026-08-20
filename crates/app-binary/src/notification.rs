//! Windows desktop notifications for AgentEvent lifecycle + agent `notify`.
//!
//! In-app toasts stay on the frontend (`addNotification`); this module only
//! drives the Windows channel via `tauri_plugin_notification`.
//! See `docs/conventions.md` §2.

use crate::app_state::AppState;
use haven_agent::AgentEvent;
use haven_common::config::NotificationConfig;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

/// Desktop (Windows) notification sink keyed off `AgentEvent`.
pub(crate) struct DesktopNotifications {
    handle: tauri::AppHandle,
    /// Last observed session status per id — used so "会话已恢复" only fires on
    /// paused/error → pending, not Running→Pending (ask-answer same turn).
    last_session_status: Mutex<HashMap<String, String>>,
    /// Cached display titles so `SessionUpdated` / toast paths do not sync
    /// `get_session` on every status churn. Seeded from `SessionCreated` /
    /// `TitleUpdated` / `SessionCompleted`; DB is a miss-only fallback.
    session_titles: Mutex<HashMap<String, String>>,
}

impl DesktopNotifications {
    pub(crate) fn new(handle: tauri::AppHandle) -> Self {
        Self {
            handle,
            last_session_status: Mutex::new(HashMap::new()),
            session_titles: Mutex::new(HashMap::new()),
        }
    }

    /// 读 `config.notification.*.windows`；锁失败时回退到 `default`。
    fn windows_enabled(
        &self,
        pick: impl FnOnce(&NotificationConfig) -> bool,
        default: bool,
    ) -> bool {
        self.handle
            .state::<Arc<AppState>>()
            .config_loader
            .lock()
            .map(|c| pick(&c.config().notification))
            .unwrap_or(default)
    }

    fn show_windows_toast(&self, title: &str, body: impl AsRef<str>) {
        let _ = self
            .handle
            .notification()
            .builder()
            .title(title)
            .body(body.as_ref())
            .show();
    }

    fn cache_title(&self, session_id: &str, title: impl Into<String>) {
        if let Ok(mut map) = self.session_titles.lock() {
            map.insert(session_id.to_string(), title.into());
        }
    }

    /// 会话展示名：cache → DB title → input_text → session_id（绝不把 raw input
    /// 当默认首选给 `SessionCreated`；该路径只用 title||id）。
    pub(crate) fn session_display_title(&self, session_id: &str) -> String {
        if let Ok(map) = self.session_titles.lock()
            && let Some(title) = map.get(session_id)
            && !title.is_empty()
        {
            return title.clone();
        }
        let resolved = self
            .handle
            .state::<Arc<AppState>>()
            .db
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|t| {
                t.title
                    .filter(|s| !s.is_empty())
                    .or_else(|| (!t.input_text.is_empty()).then_some(t.input_text))
            })
            .unwrap_or_else(|| session_id.to_string());
        self.cache_title(session_id, resolved.clone());
        resolved
    }

    pub(crate) fn remember_session_status(&self, event: &AgentEvent) {
        match event {
            AgentEvent::SessionCreated(session) => {
                if let Ok(mut map) = self.last_session_status.lock() {
                    map.insert(session.id.clone(), session.status.as_str().to_string());
                }
                let display = session
                    .title
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(session.id.as_str());
                self.cache_title(&session.id, display.to_string());
            }
            AgentEvent::SessionUpdated { session_id, status } => {
                if let Ok(mut map) = self.last_session_status.lock() {
                    map.insert(session_id.clone(), status.clone());
                }
            }
            AgentEvent::SessionCompleted { session_id, title } => {
                if let Ok(mut map) = self.last_session_status.lock() {
                    map.insert(session_id.clone(), "completed".into());
                }
                if !title.is_empty() {
                    self.cache_title(session_id, title.clone());
                }
            }
            AgentEvent::SessionError { session_id, .. } => {
                if let Ok(mut map) = self.last_session_status.lock() {
                    map.insert(session_id.clone(), "error".into());
                }
            }
            AgentEvent::TitleUpdated { session_id, title } => {
                if !title.is_empty() {
                    self.cache_title(session_id, title.clone());
                }
            }
            _ => {}
        }
    }

    fn previous_session_status(&self, session_id: &str) -> Option<String> {
        self.last_session_status
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned())
    }

    /// 会话生命周期与 agent `notify` 的 Windows 桌面通知（其余变体直接返回）。
    /// 文案与应用内 toast 对齐，使用中文。
    pub(crate) fn maybe_show_toast(&self, event: &AgentEvent) {
        match event {
            AgentEvent::SessionCreated(session) => {
                if !self.windows_enabled(|n| n.session_created.windows, false) {
                    return;
                }
                // Never use session.input — wire/in-app use title||id only, and
                // peer-spawn briefs can contain delegated secrets.
                let display = session
                    .title
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(session.id.as_str());
                self.show_windows_toast("Haven", format!("新会话: {}", display));
            }
            AgentEvent::SessionCompleted {
                session_id: _,
                title,
            } => {
                if !self.windows_enabled(|n| n.session_completed.windows, true) {
                    return;
                }
                self.show_windows_toast("Haven", format!("会话已完成: {}", title));
            }
            AgentEvent::SessionError {
                session_id: _,
                error,
            } => {
                if !self.windows_enabled(|n| n.session_error.windows, true) {
                    return;
                }
                self.show_windows_toast("Haven", format!("会话出错: {}", error));
            }
            AgentEvent::SessionUpdated { session_id, status }
                if status == "paused" || status == "paused_awaiting_answer" =>
            {
                if !self.windows_enabled(|n| n.session_paused.windows, false) {
                    return;
                }
                let display = self.session_display_title(session_id);
                self.show_windows_toast("Haven", format!("会话已暂停: {}", display));
            }
            AgentEvent::SessionUpdated { session_id, status } if status == "pending" => {
                // Only paused*/error → pending counts as resume. Running→Pending
                // (ask answered in-turn) must not toast.
                let prev = self.previous_session_status(session_id);
                if !matches!(
                    prev.as_deref(),
                    Some("paused") | Some("paused_awaiting_answer") | Some("error")
                ) {
                    return;
                }
                if !self.windows_enabled(|n| n.session_resumed.windows, false) {
                    return;
                }
                let display = self.session_display_title(session_id);
                self.show_windows_toast("Haven", format!("会话已恢复: {}", display));
            }
            AgentEvent::Notification {
                session_id: _,
                title,
                body,
            } => {
                // Agent 显式 `notify`：双通道默认全开，不读 NotificationConfig
                //（设置页注明「Agent 通知始终开启」）。
                self.show_windows_toast(
                    if title.is_empty() { "Haven" } else { title },
                    body,
                );
            }
            _ => {}
        }
    }
}
