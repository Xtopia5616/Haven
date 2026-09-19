use crate::db::Database;
use haven_common::ActionStatus;

/// A persisted scheduled-action row. Scheduled actions survive app restarts:
/// `due_at` is stored in RFC3339, and the app re-arms pending ones on startup
/// (or fires overdue ones immediately). `mode` selects the fire behavior:
/// - `notify`: show a notification (title/body).
/// - `tool`: call the tool in `tool_name` with `tool_args` (JSON text).
/// - `continue`: resume the session in `session_id`, delivering `prompt` as the
///   continuation message; `session_id` is the session that scheduled the action.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduledActionRow {
    pub id: String,
    pub due_at: String,
    pub title: String,
    pub body: String,
    pub mode: String,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub prompt: Option<String>,
    pub status: ActionStatus,
    pub created_at: String,
}

impl Database {
    /// Persist a new (pending) scheduled action. `mode` selects the fire behavior
    /// (see [`ScheduledActionRow`]); `session_id`/`tool_name`/`tool_args` are the
    /// mode-specific payloads, `prompt` the optional continuation text.
    #[allow(clippy::too_many_arguments)]
    pub fn save_scheduled_action(
        &self,
        id: &str,
        due_at: &str,
        title: &str,
        body: &str,
        mode: &str,
        session_id: Option<&str>,
        tool_name: Option<&str>,
        tool_args: Option<&str>,
        prompt: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO actions (id, kind, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, status, created_at)
             VALUES (?1, 'scheduled', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'waiting', datetime('now'))",
            rusqlite::params![
                id,
                due_at,
                title,
                body,
                mode,
                session_id,
                tool_name,
                tool_args,
                prompt
            ],
        )?;
        Ok(())
    }

    /// All scheduled actions that are still waiting, ordered by due time ascending.
    /// Background-action rows (`kind = 'background'`) are excluded: they carry no
    /// due time and are listed via [`Database::list_actions`].
    pub fn list_pending_scheduled_actions(&self) -> anyhow::Result<Vec<ScheduledActionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, status, created_at
             FROM actions WHERE kind = 'scheduled' AND status = 'waiting' ORDER BY due_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ScheduledActionRow {
                id: row.get(0)?,
                due_at: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                mode: row.get(4)?,
                session_id: row.get(5)?,
                tool_name: row.get(6)?,
                tool_args: row.get(7)?,
                prompt: row.get(8)?,
                status: ActionStatus::from_status_str(&row.get::<_, String>(9)?),
                created_at: row.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Claim a scheduled action's trigger. Terminal rows remain durable history
    /// and are no longer re-armed on the next startup.
    pub fn start_scheduled_action(&self, id: &str, started_at: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE actions SET status = 'running', started_at = ?2
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'waiting'",
            rusqlite::params![id, started_at],
        )?;
        Ok(changed > 0)
    }

    /// Put a scheduled action back into its durable waiting state when its
    /// trigger could not be delivered to a live consumer.
    pub fn requeue_scheduled_action(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE actions SET status = 'waiting', started_at = NULL, finished_at = NULL
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'running'",
            rusqlite::params![id],
        )?;
        Ok(changed > 0)
    }

    /// Finish a scheduled action after the actual trigger work has completed.
    /// The caller supplies the single timestamp used by both persistence and
    /// the in-memory/UI event projection.
    pub fn finish_scheduled_action(
        &self,
        id: &str,
        status: ActionStatus,
        error_reason: Option<&str>,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        if !matches!(
            status,
            ActionStatus::Completed | ActionStatus::Failed | ActionStatus::Cancelled
        ) {
            anyhow::bail!("scheduled action terminal status must be terminal");
        }
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE actions SET status = ?2, started_at = COALESCE(started_at, due_at),
                 error_reason = ?3, finished_at = ?4
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'running'",
            rusqlite::params![id, status.as_str(), error_reason, finished_at],
        )?;
        Ok(changed > 0)
    }

    /// Compatibility helper for repository callers that complete a scheduled
    /// row directly. The row must already be `running`; new runtime paths use
    /// [`Database::finish_scheduled_action`] so the terminal status records
    /// the actual trigger outcome.
    pub fn complete_scheduled_action(&self, id: &str, finished_at: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE actions SET status = 'completed', started_at = COALESCE(started_at, due_at), finished_at = ?2
             WHERE id = ?1 AND kind = 'scheduled' AND status = 'running'",
            rusqlite::params![id, finished_at],
        )?;
        Ok(changed > 0)
    }

    /// Cancel a waiting or currently-running scheduled action while retaining
    /// its terminal history.
    pub fn cancel_scheduled_action(&self, id: &str, finished_at: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE actions SET status = 'cancelled', started_at = COALESCE(started_at, due_at), finished_at = ?2
             WHERE id = ?1 AND kind = 'scheduled' AND status IN ('waiting', 'running')",
            rusqlite::params![id, finished_at],
        )?;
        Ok(changed > 0)
    }
}

/// A persisted action row (unified background actions and scheduled actions).
/// Scheduled-action rows carry `kind: "scheduled"` (due_at/mode/tool_name/
/// tool_args/prompt); background-action rows carry `kind: "background"` with the
/// action lifecycle fields (status/command/output/error/error_reason/log_path/
/// exit_code/started_at/finished_at). `status` is authoritative for both kinds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionRow {
    pub id: String,
    pub kind: String,
    pub due_at: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub mode: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub prompt: Option<String>,
    pub status: ActionStatus,
    pub command: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub log_path: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

const ACTION_COLUMNS: &str = "id, kind, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, status, command, output, error, error_reason, log_path, exit_code, started_at, finished_at, created_at";

fn row_to_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActionRow> {
    Ok(ActionRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        due_at: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        mode: row.get(5)?,
        session_id: row.get(6)?,
        tool_name: row.get(7)?,
        tool_args: row.get(8)?,
        prompt: row.get(9)?,
        status: ActionStatus::from_status_str(&row.get::<_, String>(10)?),
        command: row.get(11)?,
        output: row.get(12)?,
        error: row.get(13)?,
        error_reason: row.get(14)?,
        log_path: row.get(15)?,
        exit_code: row.get(16)?,
        started_at: row.get(17)?,
        finished_at: row.get(18)?,
        created_at: row.get(19)?,
    })
}

impl Database {
    /// Persist a newly spawned background action (status `running`). The action is
    /// later finalized by [`Database::finish_action`]; terminal rows stay in the
    /// table as history. Scheduled-only columns remain NULL for background
    /// actions because each action kind owns its own payload fields.
    pub fn save_action(
        &self,
        id: &str,
        session_id: Option<&str>,
        command: &str,
        started_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO actions (id, kind, session_id, command, status, started_at, created_at)
             VALUES (?1, 'background', ?2, ?3, 'running', ?4, datetime('now'))",
            rusqlite::params![id, session_id, command, started_at],
        )?;
        Ok(())
    }

    /// Record the owning session of a background action (arrives after spawn via
    /// the tool manager's session binding).
    pub fn update_action_session(&self, id: &str, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE actions SET session_id = ?2 WHERE id = ?1 AND kind = 'background'",
            rusqlite::params![id, session_id],
        )?;
        Ok(())
    }

    /// Finalize a background action with its terminal status and payload. The
    /// row stays in the table as history; `output`/`error` are bounded
    /// summaries (the full transcript lives in the `log_path` file).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_action(
        &self,
        id: &str,
        status: ActionStatus,
        output: Option<&str>,
        error: Option<&str>,
        error_reason: Option<&str>,
        log_path: Option<&str>,
        exit_code: Option<i32>,
        finished_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE actions
             SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                 log_path = ?6, exit_code = ?7, finished_at = ?8
             WHERE id = ?1 AND kind = 'background'",
            rusqlite::params![
                id,
                status.as_str(),
                output,
                error,
                error_reason,
                log_path,
                exit_code,
                finished_at
            ],
        )?;
        Ok(())
    }

    /// All persisted actions, optionally filtered by kind (`"background"` /
    /// `"scheduled"`), newest first. Waiting rows are returned for board
    /// hydration; terminal rows remain available as history.
    pub fn list_actions(&self, kind: Option<&str>) -> anyhow::Result<Vec<ActionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACTION_COLUMNS} FROM actions
             WHERE (?1 IS NULL OR kind = ?1)
             ORDER BY started_at DESC, created_at DESC"
        ))?;
        let rows = stmt.query_map([kind], row_to_action)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One persisted action by id (either kind).
    pub fn get_action(&self, id: &str) -> anyhow::Result<Option<ActionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACTION_COLUMNS} FROM actions WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map([id], row_to_action)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Remove a persisted action (background or scheduled) by id.
    pub fn delete_action(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute("DELETE FROM actions WHERE id = ?1", rusqlite::params![id])?;
        Ok(changed > 0)
    }

    /// Mark background-action rows left `running` by a previous process as
    /// failed: child processes die with the app, so a `running` row after a
    /// restart is stale and must not surface as live work. Idempotent.
    pub fn mark_interrupted_actions(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE actions
             SET status = 'failed', error_reason = 'App restarted while the action was running',
                 finished_at = datetime('now')
             WHERE kind IN ('background', 'scheduled') AND status = 'running'",
            [],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use haven_common::ActionStatus;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn save_and_list_pending() {
        let db = test_db();
        db.save_scheduled_action(
            "action-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "drink water",
            "tool",
            None,
            Some("notify"),
            Some(r#"{"title":"Haven","body":"drink water"}"#),
            None,
        )
        .unwrap();
        db.save_scheduled_action(
            "action-2",
            "2026-08-04T01:00:00+08:00",
            "Haven",
            "stand up",
            "continue",
            Some("ses-7"),
            None,
            None,
            Some("check the weather"),
        )
        .unwrap();
        db.save_scheduled_action(
            "action-3",
            "2026-08-04T03:00:00+08:00",
            "Haven",
            "backup",
            "tool",
            Some("ses-7"),
            Some("files"),
            Some(r#"{"operation":"read","path":"C:\\x"}"#),
            None,
        )
        .unwrap();
        let pending = db.list_pending_scheduled_actions().unwrap();
        assert_eq!(pending.len(), 3);
        // Ordered by due_at ascending.
        assert_eq!(pending[0].id, "action-2");
        assert_eq!(pending[0].body, "stand up");
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].session_id.as_deref(), Some("ses-7"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));
        assert_eq!(pending[1].id, "action-1");
        assert_eq!(pending[1].mode, "tool");
        assert_eq!(pending[1].tool_name.as_deref(), Some("notify"));
        assert_eq!(pending[1].prompt, None);
        assert_eq!(pending[2].id, "action-3");
        assert_eq!(pending[2].mode, "tool");
        assert_eq!(pending[2].tool_name.as_deref(), Some("files"));
        assert!(pending[2].tool_args.as_deref().unwrap().contains("read"));
        assert_eq!(pending[0].status, ActionStatus::Waiting);
    }

    #[test]
    fn completed_scheduled_actions_are_hidden_from_pending() {
        let db = test_db();
        db.save_scheduled_action(
            "action-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "x",
            "tool",
            None,
            Some("notify"),
            None,
            None,
        )
        .unwrap();
        db.start_scheduled_action("action-1", "2026-08-04T02:00:00Z")
            .unwrap();
        db.complete_scheduled_action("action-1", "2026-08-04T02:00:01Z")
            .unwrap();
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
    }

    #[test]
    fn cancelled_scheduled_actions_are_hidden_from_pending() {
        let db = test_db();
        db.save_scheduled_action(
            "action-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "x",
            "tool",
            None,
            Some("notify"),
            None,
            None,
        )
        .unwrap();
        db.cancel_scheduled_action("action-1", "2026-08-04T02:00:01Z")
            .unwrap();
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
    }

    #[test]
    fn action_lifecycle_persists_and_updates() {
        let db = test_db();
        db.save_action(
            "action-1",
            Some("ses-9"),
            "echo hello",
            "2026-08-09T10:00:00Z",
        )
        .unwrap();
        db.save_action("action-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();

        // A running background action must not leak into the pending list.
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());

        db.update_action_session("action-2", "ses-9").unwrap();
        db.finish_action(
            "action-1",
            ActionStatus::Completed,
            Some("hello"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();
        db.finish_action(
            "action-2",
            ActionStatus::Failed,
            None,
            Some("connection refused"),
            Some("connection refused"),
            Some("C:\\tmp\\action-logs\\action-2.log"),
            Some(1),
            "2026-08-09T10:01:03Z",
        )
        .unwrap();

        let all = db.list_actions(None).unwrap();
        assert_eq!(all.len(), 2);
        let backgrounds = db.list_actions(Some("background")).unwrap();
        assert_eq!(backgrounds.len(), 2);
        assert!(db.list_actions(Some("scheduled")).unwrap().is_empty());

        let finished = backgrounds.iter().find(|a| a.id == "action-1").unwrap();
        assert_eq!(finished.status, ActionStatus::Completed);
        assert_eq!(finished.output.as_deref(), Some("hello"));
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.session_id.as_deref(), Some("ses-9"));
        assert!(finished.finished_at.is_some());

        let failed = backgrounds.iter().find(|a| a.id == "action-2").unwrap();
        assert_eq!(failed.status, ActionStatus::Failed);
        assert_eq!(failed.session_id.as_deref(), Some("ses-9"));
        assert_eq!(
            failed.log_path.as_deref(),
            Some("C:\\tmp\\action-logs\\action-2.log")
        );
        assert_eq!(failed.error_reason.as_deref(), Some("connection refused"));

        assert!(db.get_action("action-1").unwrap().is_some());
        assert!(db.get_action("nope").unwrap().is_none());
    }

    #[test]
    fn scheduled_rows_keep_kind_and_terminal_history() {
        let db = test_db();
        db.save_scheduled_action(
            "action-1",
            "2026-08-04T02:00:00+08:00",
            "Haven",
            "drink water",
            "tool",
            None,
            Some("notify"),
            None,
            None,
        )
        .unwrap();
        db.start_scheduled_action("action-1", "2026-08-04T02:00:00Z")
            .unwrap();
        db.complete_scheduled_action("action-1", "2026-08-04T02:00:01Z")
            .unwrap();
        db.save_action("action-2", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();

        // Pending list excludes both the completed scheduled action and the action.
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
        // Action listing still surfaces the completed scheduled action as history.
        let scheduled = db.list_actions(Some("scheduled")).unwrap();
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].id, "action-1");
        assert_eq!(scheduled[0].status, ActionStatus::Completed);
        assert_eq!(scheduled[0].kind, "scheduled");
    }

    #[test]
    fn interrupted_actions_marked_failed() {
        let db = test_db();
        db.save_action("action-1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        db.save_action("action-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();
        db.save_scheduled_action(
            "action-3",
            "2026-08-09T10:02:00Z",
            "Scheduled",
            "resume",
            "tool",
            None,
            Some("notify"),
            None,
            None,
        )
        .unwrap();
        db.start_scheduled_action("action-3", "2026-08-09T10:02:00Z")
            .unwrap();
        db.finish_action(
            "action-1",
            ActionStatus::Completed,
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();

        let n = db.mark_interrupted_actions().unwrap();
        assert_eq!(n, 2);
        let backgrounds = db.list_actions(Some("background")).unwrap();
        let running_left = backgrounds
            .iter()
            .filter(|a| a.status == ActionStatus::Running)
            .count();
        assert_eq!(running_left, 0);
        let j2 = backgrounds.iter().find(|a| a.id == "action-2").unwrap();
        assert_eq!(j2.status, ActionStatus::Failed);
        assert!(j2.error_reason.as_deref().unwrap().contains("restarted"));
        let scheduled = db.get_action("action-3").unwrap().unwrap();
        assert_eq!(scheduled.status, ActionStatus::Failed);
        assert!(
            scheduled
                .error_reason
                .as_deref()
                .unwrap()
                .contains("restarted")
        );
        // Second run is a no-op.
        assert_eq!(db.mark_interrupted_actions().unwrap(), 0);
    }

    #[test]
    fn delete_removes_any_kind() {
        let db = test_db();
        db.save_action("action-1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        assert!(db.delete_action("action-1").unwrap());
        assert!(!db.delete_action("action-1").unwrap());
        assert!(db.list_actions(Some("background")).unwrap().is_empty());
    }
}
