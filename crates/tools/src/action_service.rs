//! Unified task facade for background and scheduled actions.
//!
//! The two runtimes intentionally keep their own execution mechanics, but
//! callers must not need to know which state machine owns an `act-*` id.  This
//! facade is the single model-facing query/control boundary.

use crate::BackgroundActions;
use crate::builtin::ScheduledActionCenter;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub struct ActionService {
    background: Arc<BackgroundActions>,
    scheduled: Arc<ScheduledActionCenter>,
}

impl ActionService {
    pub fn new(background: Arc<BackgroundActions>, scheduled: Arc<ScheduledActionCenter>) -> Self {
        Self {
            background,
            scheduled,
        }
    }

    pub fn background(&self) -> &Arc<BackgroundActions> {
        &self.background
    }

    pub fn scheduled(&self) -> &Arc<ScheduledActionCenter> {
        &self.scheduled
    }

    /// Return the normalized task board used by the app shell.
    ///
    /// This is intentionally broader than `list_for_session`: the desktop
    /// task panel is allowed to see the complete local board, while model
    /// callers must use the session-scoped method below.
    pub async fn board(&self) -> Vec<Value> {
        let mut rows = self.background.board().await;
        for row in &mut rows {
            let action_id = row
                .get("action_id")
                .and_then(Value::as_str)
                .or_else(|| row.get("id").and_then(Value::as_str))
                .map(str::to_owned);
            row["kind"] = json!("background");
            if let Some(action_id) = action_id {
                row["action_id"] = json!(action_id);
            }
        }

        for mut row in self.scheduled.list().await {
            let action_id = row
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            row["action_id"] = json!(action_id);
            row["kind"] = json!("scheduled");
            row["status"] = json!("scheduled");
            rows.push(row);
        }

        rows.sort_by(|left, right| {
            let left_time = left
                .get("started_at")
                .or_else(|| left.get("due_at"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let right_time = right
                .get("started_at")
                .or_else(|| right.get("due_at"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            left_time.cmp(right_time)
        });
        rows
    }

    /// Return one normalized task row for every task owned by a session.
    pub async fn list_for_session(&self, session_id: &str) -> Vec<Value> {
        let mut rows = self.background.list_for_session(session_id).await;
        for row in &mut rows {
            let action_id = row
                .get("action_id")
                .and_then(Value::as_str)
                .or_else(|| row.get("id").and_then(Value::as_str))
                .map(str::to_owned);
            row["kind"] = json!("background");
            if let Some(action_id) = action_id {
                row["action_id"] = json!(action_id);
            }
        }

        for mut row in self.scheduled.list_for_session(session_id).await {
            let action_id = row
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            row["action_id"] = json!(action_id);
            row["kind"] = json!("scheduled");
            row["status"] = json!("scheduled");
            rows.push(row);
        }

        rows.sort_by(|left, right| {
            let left_time = left
                .get("started_at")
                .or_else(|| left.get("due_at"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let right_time = right
                .get("started_at")
                .or_else(|| right.get("due_at"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            left_time.cmp(right_time)
        });
        rows
    }

    /// Look up an action without leaking another session's output or schedule.
    pub async fn status_for_session(&self, action_id: &str, session_id: &str) -> Value {
        let background = self
            .background
            .status_for_session(action_id, session_id)
            .await;
        if background.get("status").and_then(Value::as_str) != Some("not_found") {
            let mut result = background;
            result["kind"] = json!("background");
            result["action_id"] = json!(action_id);
            return result;
        }

        if let Some(row) = self
            .scheduled
            .list_for_session(session_id)
            .await
            .into_iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(action_id))
        {
            let mut result = row;
            result["action_id"] = json!(action_id);
            result["kind"] = json!("scheduled");
            result["status"] = json!("scheduled");
            return result;
        }

        json!({ "action_id": action_id, "status": "not_found" })
    }

    /// Cancel only an action owned by the requesting session.
    pub async fn cancel_for_session(&self, action_id: &str, session_id: &str) -> bool {
        if self
            .background
            .cancel_for_session(action_id, session_id)
            .await
        {
            return true;
        }
        self.scheduled
            .cancel_for_session(action_id, session_id)
            .await
    }
}
