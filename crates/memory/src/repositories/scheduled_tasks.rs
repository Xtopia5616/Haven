use crate::db::Database;

/// A persisted scheduled-task row. Scheduled tasks survive app restarts:
/// `due_at` is stored in RFC3339, and the app re-arms pending ones on startup
/// (or fires overdue ones immediately). `mode` selects the fire behavior:
/// - `notify`: show a notification (title/body).
/// - `tool`: call the tool in `tool_name` with `tool_args` (JSON text).
/// - `continue`: resume the session in `session_id`, delivering `prompt` (or body)
///   as the continuation message; `session_id` is the session that scheduled the
///   task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTaskRow {
    pub id: String,
    pub due_at: String,
    pub title: String,
    pub body: String,
    pub mode: String,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub prompt: Option<String>,
    pub fired: bool,
    pub created_at: String,
}

impl Database {
    /// Persist a new (pending) scheduled task. `mode` selects the fire behavior
    /// (see [`ScheduledTaskRow`]); `session_id`/`tool_name`/`tool_args` are the
    /// mode-specific payloads, `prompt` the optional continuation text.
    #[allow(clippy::too_many_arguments)]
    pub fn save_scheduled_task(
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
            "INSERT INTO tasks (id, kind, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, fired, created_at)
             VALUES (?1, 'scheduled', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, datetime('now'))",
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

    /// All scheduled tasks that have not fired yet, ordered by due time ascending.
    /// Background-task rows (`kind = 'background'`) are excluded: they carry no
    /// due time and are listed via [`Database::list_tasks`].
    pub fn list_pending_scheduled_tasks(&self) -> anyhow::Result<Vec<ScheduledTaskRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, fired, created_at
             FROM tasks WHERE kind = 'scheduled' AND fired = 0 ORDER BY due_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ScheduledTaskRow {
                id: row.get(0)?,
                due_at: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                mode: row.get(4)?,
                session_id: row.get(5)?,
                tool_name: row.get(6)?,
                tool_args: row.get(7)?,
                prompt: row.get(8)?,
                fired: row.get::<_, i32>(9)? != 0,
                created_at: row.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark a scheduled task as fired (it stays in the table as history but is
    /// no longer re-armed on the next startup).
    pub fn mark_scheduled_task_fired(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET fired = 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    /// Remove a scheduled task entirely (cancelled before it fired).
    pub fn delete_scheduled_task(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }
}

/// A persisted task row (unified background tasks and scheduled tasks).
/// Scheduled-task rows carry `kind: "scheduled"` (due_at/mode/tool_name/
/// tool_args/prompt); background-task rows carry `kind: "background"` with the
/// task lifecycle fields (status/command/output/error/error_reason/log_path/
/// exit_code/started_at/finished_at). `fired` is only meaningful for scheduled
/// tasks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskRow {
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
    pub fired: bool,
    pub status: Option<String>,
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

const TASK_COLUMNS: &str = "id, kind, due_at, title, body, mode, session_id, tool_name, tool_args, prompt, fired, status, command, output, error, error_reason, log_path, exit_code, started_at, finished_at, created_at";

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
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
        fired: row.get::<_, i32>(10)? != 0,
        status: row.get(11)?,
        command: row.get(12)?,
        output: row.get(13)?,
        error: row.get(14)?,
        error_reason: row.get(15)?,
        log_path: row.get(16)?,
        exit_code: row.get(17)?,
        started_at: row.get(18)?,
        finished_at: row.get(19)?,
        created_at: row.get(20)?,
    })
}

impl Database {
    /// Persist a newly spawned background task (status `running`). The task is
    /// later finalized by [`Database::finish_task`]; terminal rows stay in the
    /// table as history. `due_at`/`body` are filled with safe placeholders
    /// because databases created before the task columns existed still carry
    /// `NOT NULL` on those scheduled-task columns.
    pub fn save_task(
        &self,
        id: &str,
        session_id: Option<&str>,
        command: &str,
        started_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO tasks (id, kind, session_id, command, status, due_at, body, started_at, created_at)
             VALUES (?1, 'background', ?2, ?3, 'running', ?4, '', ?4, datetime('now'))",
            rusqlite::params![id, session_id, command, started_at],
        )?;
        Ok(())
    }

    /// Record the owning session of a background task (arrives after spawn via
    /// the tool manager's session binding).
    pub fn update_task_session(&self, id: &str, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET session_id = ?2 WHERE id = ?1 AND kind = 'background'",
            rusqlite::params![id, session_id],
        )?;
        Ok(())
    }

    /// Finalize a background task with its terminal status and payload. The
    /// row stays in the table as history; `output`/`error` are bounded
    /// summaries (the full transcript lives in the `log_path` file).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_task(
        &self,
        id: &str,
        status: &str,
        output: Option<&str>,
        error: Option<&str>,
        error_reason: Option<&str>,
        log_path: Option<&str>,
        exit_code: Option<i32>,
        finished_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks
             SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                 log_path = ?6, exit_code = ?7, finished_at = ?8
             WHERE id = ?1 AND kind = 'background'",
            rusqlite::params![
                id,
                status,
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

    /// All persisted tasks, optionally filtered by kind (`"background"` /
    /// `"scheduled"`), newest first. Scheduled-task rows include fired history;
    /// background-task rows include terminal history past the in-memory TTL.
    pub fn list_tasks(&self, kind: Option<&str>) -> anyhow::Result<Vec<TaskRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks
             WHERE (?1 IS NULL OR kind = ?1)
             ORDER BY started_at DESC, created_at DESC"
        ))?;
        let rows = stmt.query_map([kind], row_to_task)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One persisted task by id (either kind).
    pub fn get_task(&self, id: &str) -> anyhow::Result<Option<TaskRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"))?;
        let mut rows = stmt.query_map([id], row_to_task)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Remove a persisted task (background or scheduled) by id.
    pub fn delete_task(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Mark background-task rows left `running` by a previous process as
    /// failed: child processes die with the app, so a `running` row after a
    /// restart is stale and must not surface as live work. Idempotent.
    pub fn mark_interrupted_tasks(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE tasks
             SET status = 'failed', error_reason = 'App restarted while the task was running',
                 finished_at = datetime('now')
             WHERE kind = 'background' AND status = 'running'",
            [],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn save_and_list_pending() {
        let db = test_db();
        db.save_scheduled_task(
            "task-1",
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
        db.save_scheduled_task(
            "task-2",
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
        db.save_scheduled_task(
            "task-3",
            "2026-08-04T03:00:00+08:00",
            "Haven",
            "backup",
            "tool",
            Some("ses-7"),
            Some("file"),
            Some(r#"{"operation":"read","path":"C:\\x"}"#),
            None,
        )
        .unwrap();
        let pending = db.list_pending_scheduled_tasks().unwrap();
        assert_eq!(pending.len(), 3);
        // Ordered by due_at ascending.
        assert_eq!(pending[0].id, "task-2");
        assert_eq!(pending[0].body, "stand up");
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].session_id.as_deref(), Some("ses-7"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));
        assert_eq!(pending[1].id, "task-1");
        assert_eq!(pending[1].mode, "tool");
        assert_eq!(pending[1].tool_name.as_deref(), Some("notify"));
        assert_eq!(pending[1].prompt, None);
        assert_eq!(pending[2].id, "task-3");
        assert_eq!(pending[2].mode, "tool");
        assert_eq!(pending[2].tool_name.as_deref(), Some("file"));
        assert!(pending[2].tool_args.as_deref().unwrap().contains("read"));
        assert!(!pending[0].fired);
    }

    #[test]
    fn fired_scheduled_tasks_are_hidden_from_pending() {
        let db = test_db();
        db.save_scheduled_task(
            "task-1",
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
        db.mark_scheduled_task_fired("task-1").unwrap();
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());
    }

    #[test]
    fn delete_removes_row() {
        let db = test_db();
        db.save_scheduled_task(
            "task-1",
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
        db.delete_scheduled_task("task-1").unwrap();
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());
    }

    #[test]
    fn task_lifecycle_persists_and_updates() {
        let db = test_db();
        db.save_task(
            "task-1",
            Some("ses-9"),
            "echo hello",
            "2026-08-09T10:00:00Z",
        )
        .unwrap();
        db.save_task("task-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();

        // A running background task must not leak into the pending list.
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());

        db.update_task_session("task-2", "ses-9").unwrap();
        db.finish_task(
            "task-1",
            "completed",
            Some("hello"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();
        db.finish_task(
            "task-2",
            "failed",
            None,
            Some("connection refused"),
            Some("connection refused"),
            Some("C:\\tmp\\task-logs\\task-2.log"),
            Some(1),
            "2026-08-09T10:01:03Z",
        )
        .unwrap();

        let all = db.list_tasks(None).unwrap();
        assert_eq!(all.len(), 2);
        let backgrounds = db.list_tasks(Some("background")).unwrap();
        assert_eq!(backgrounds.len(), 2);
        assert!(db.list_tasks(Some("scheduled")).unwrap().is_empty());

        let finished = backgrounds.iter().find(|a| a.id == "task-1").unwrap();
        assert_eq!(finished.status.as_deref(), Some("completed"));
        assert_eq!(finished.output.as_deref(), Some("hello"));
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.session_id.as_deref(), Some("ses-9"));
        assert!(finished.finished_at.is_some());

        let failed = backgrounds.iter().find(|a| a.id == "task-2").unwrap();
        assert_eq!(failed.status.as_deref(), Some("failed"));
        assert_eq!(failed.session_id.as_deref(), Some("ses-9"));
        assert_eq!(
            failed.log_path.as_deref(),
            Some("C:\\tmp\\task-logs\\task-2.log")
        );
        assert_eq!(failed.error_reason.as_deref(), Some("connection refused"));

        assert!(db.get_task("task-1").unwrap().is_some());
        assert!(db.get_task("nope").unwrap().is_none());
    }

    #[test]
    fn scheduled_rows_keep_kind_and_fired_history() {
        let db = test_db();
        db.save_scheduled_task(
            "task-1",
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
        db.mark_scheduled_task_fired("task-1").unwrap();
        db.save_task("task-2", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();

        // Pending list excludes both the fired scheduled task and the task.
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());
        // Task listing still surfaces the fired scheduled task as history.
        let scheduled = db.list_tasks(Some("scheduled")).unwrap();
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].id, "task-1");
        assert!(scheduled[0].fired);
        assert_eq!(scheduled[0].kind, "scheduled");
    }

    #[test]
    fn interrupted_tasks_marked_failed() {
        let db = test_db();
        db.save_task("task-1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        db.save_task("task-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();
        db.finish_task(
            "task-1",
            "completed",
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();

        let n = db.mark_interrupted_tasks().unwrap();
        assert_eq!(n, 1);
        let backgrounds = db.list_tasks(Some("background")).unwrap();
        let running_left = backgrounds
            .iter()
            .filter(|a| a.status.as_deref() == Some("running"))
            .count();
        assert_eq!(running_left, 0);
        let j2 = backgrounds.iter().find(|a| a.id == "task-2").unwrap();
        assert_eq!(j2.status.as_deref(), Some("failed"));
        assert!(j2.error_reason.as_deref().unwrap().contains("restarted"));
        // Second run is a no-op.
        assert_eq!(db.mark_interrupted_tasks().unwrap(), 0);
    }

    #[test]
    fn delete_removes_any_kind() {
        let db = test_db();
        db.save_task("task-1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        db.delete_task("task-1").unwrap();
        assert!(db.list_tasks(Some("background")).unwrap().is_empty());
    }
}
