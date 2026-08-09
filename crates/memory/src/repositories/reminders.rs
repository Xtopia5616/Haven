use crate::db::Database;

/// A persisted reminder row. Reminders survive app restarts: `due_at` is
/// stored in RFC3339, and the app re-arms pending ones on startup (or fires
/// overdue ones immediately). `mode` selects the fire behavior:
/// - `notify`: show a notification (title/body).
/// - `tool`: call the tool in `tool_name` with `tool_args` (JSON text).
/// - `continue`: resume the task in `task_id`, delivering `prompt` (or body)
///   as the continuation message; `task_id` is the task that scheduled the
///   reminder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReminderRow {
    pub id: String,
    pub due_at: String,
    pub title: String,
    pub body: String,
    pub mode: String,
    pub task_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub prompt: Option<String>,
    pub fired: bool,
    pub created_at: String,
}

impl Database {
    /// Persist a new (pending) reminder. `mode` selects the fire behavior
    /// (see [`ReminderRow`]); `task_id`/`tool_name`/`tool_args` are the
    /// mode-specific payloads, `prompt` the optional continuation text.
    #[allow(clippy::too_many_arguments)]
    pub fn save_reminder(
        &self,
        id: &str,
        due_at: &str,
        title: &str,
        body: &str,
        mode: &str,
        task_id: Option<&str>,
        tool_name: Option<&str>,
        tool_args: Option<&str>,
        prompt: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO reminders (id, due_at, title, body, mode, task_id, tool_name, tool_args, prompt, fired, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, datetime('now'))",
            rusqlite::params![
                id,
                due_at,
                title,
                body,
                mode,
                task_id,
                tool_name,
                tool_args,
                prompt
            ],
        )?;
        Ok(())
    }

    /// All reminders that have not fired yet, ordered by due time ascending.
    /// Job rows (`kind = 'job'`) are excluded: they carry no due time and are
    /// listed via [`Database::list_activities`].
    pub fn list_pending_reminders(&self) -> anyhow::Result<Vec<ReminderRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, due_at, title, body, mode, task_id, tool_name, tool_args, prompt, fired, created_at
             FROM reminders WHERE kind = 'reminder' AND fired = 0 ORDER BY due_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ReminderRow {
                id: row.get(0)?,
                due_at: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                mode: row.get(4)?,
                task_id: row.get(5)?,
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

    /// Mark a reminder as fired (it stays in the table as history but is no
    /// longer re-armed on the next startup).
    pub fn mark_reminder_fired(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE reminders SET fired = 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    /// Remove a reminder entirely (cancelled before it fired).
    pub fn delete_reminder(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM reminders WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }
}

/// A persisted activity row (unified background jobs and reminders). Reminder
/// rows carry `kind: "reminder"` (due_at/mode/tool_name/tool_args/prompt);
/// background-job rows carry `kind: "job"` with the job lifecycle fields
/// (status/command/output/error/error_reason/log_path/exit_code/started_at/
/// finished_at). `fired` is only meaningful for reminders.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivityRow {
    pub id: String,
    pub kind: String,
    pub due_at: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub mode: Option<String>,
    pub task_id: Option<String>,
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

const ACTIVITY_COLUMNS: &str = "id, kind, due_at, title, body, mode, task_id, tool_name, tool_args, prompt, fired, status, command, output, error, error_reason, log_path, exit_code, started_at, finished_at, created_at";

fn row_to_activity(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityRow> {
    Ok(ActivityRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        due_at: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        mode: row.get(5)?,
        task_id: row.get(6)?,
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
    /// Persist a newly spawned background job (status `running`). The job is
    /// later finalized by [`Database::finish_job`]; terminal rows stay in the
    /// table as history. `due_at`/`body` are filled with safe placeholders
    /// because databases created before the job columns existed still carry
    /// `NOT NULL` on those reminder columns.
    pub fn save_job(
        &self,
        id: &str,
        task_id: Option<&str>,
        command: &str,
        started_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO reminders (id, kind, task_id, command, status, due_at, body, started_at, created_at)
             VALUES (?1, 'job', ?2, ?3, 'running', ?4, '', ?4, datetime('now'))",
            rusqlite::params![id, task_id, command, started_at],
        )?;
        Ok(())
    }

    /// Record the owning task of a background job (arrives after spawn via
    /// the tool manager's task binding).
    pub fn update_job_task(&self, id: &str, task_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE reminders SET task_id = ?2 WHERE id = ?1 AND kind = 'job'",
            rusqlite::params![id, task_id],
        )?;
        Ok(())
    }

    /// Finalize a background job with its terminal status and payload. The
    /// row stays in the table as history; `output`/`error` are bounded
    /// summaries (the full transcript lives in the `log_path` file).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_job(
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
            "UPDATE reminders
             SET status = ?2, output = ?3, error = ?4, error_reason = ?5,
                 log_path = ?6, exit_code = ?7, finished_at = ?8
             WHERE id = ?1 AND kind = 'job'",
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

    /// All persisted activities, optionally filtered by kind (`"job"` /
    /// `"reminder"`), newest first. Reminder rows include fired history;
    /// job rows include terminal history past the in-memory TTL.
    pub fn list_activities(&self, kind: Option<&str>) -> anyhow::Result<Vec<ActivityRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACTIVITY_COLUMNS} FROM reminders
             WHERE (?1 IS NULL OR kind = ?1)
             ORDER BY started_at DESC, created_at DESC"
        ))?;
        let rows = stmt.query_map([kind], row_to_activity)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One persisted activity by id (either kind).
    pub fn get_activity(&self, id: &str) -> anyhow::Result<Option<ActivityRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACTIVITY_COLUMNS} FROM reminders WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map([id], row_to_activity)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Remove a persisted activity (job or reminder) by id.
    pub fn delete_activity(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM reminders WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Mark job rows left `running` by a previous process as failed: child
    /// processes die with the app, so a `running` row after a restart is
    /// stale and must not surface as live work. Idempotent.
    pub fn mark_interrupted_jobs(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE reminders
             SET status = 'failed', error_reason = 'App restarted while the job was running',
                 finished_at = datetime('now')
             WHERE kind = 'job' AND status = 'running'",
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
        db.save_reminder(
            "r1",
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
        db.save_reminder(
            "r2",
            "2026-08-04T01:00:00+08:00",
            "Haven",
            "stand up",
            "continue",
            Some("task-7"),
            None,
            None,
            Some("check the weather"),
        )
        .unwrap();
        db.save_reminder(
            "r3",
            "2026-08-04T03:00:00+08:00",
            "Haven",
            "backup",
            "tool",
            Some("task-7"),
            Some("file"),
            Some(r#"{"operation":"read","path":"C:\\x"}"#),
            None,
        )
        .unwrap();
        let pending = db.list_pending_reminders().unwrap();
        assert_eq!(pending.len(), 3);
        // Ordered by due_at ascending.
        assert_eq!(pending[0].id, "r2");
        assert_eq!(pending[0].body, "stand up");
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].task_id.as_deref(), Some("task-7"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));
        assert_eq!(pending[1].id, "r1");
        assert_eq!(pending[1].mode, "tool");
        assert_eq!(pending[1].tool_name.as_deref(), Some("notify"));
        assert_eq!(pending[1].prompt, None);
        assert_eq!(pending[2].id, "r3");
        assert_eq!(pending[2].mode, "tool");
        assert_eq!(pending[2].tool_name.as_deref(), Some("file"));
        assert!(pending[2].tool_args.as_deref().unwrap().contains("read"));
        assert!(!pending[0].fired);
    }

    #[test]
    fn fired_reminders_are_hidden_from_pending() {
        let db = test_db();
        db.save_reminder(
            "r1",
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
        db.mark_reminder_fired("r1").unwrap();
        assert!(db.list_pending_reminders().unwrap().is_empty());
    }

    #[test]
    fn delete_removes_row() {
        let db = test_db();
        db.save_reminder(
            "r1",
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
        db.delete_reminder("r1").unwrap();
        assert!(db.list_pending_reminders().unwrap().is_empty());
    }

    #[test]
    fn job_lifecycle_persists_and_updates() {
        let db = test_db();
        db.save_job(
            "act-job-1",
            Some("task-9"),
            "echo hello",
            "2026-08-09T10:00:00Z",
        )
        .unwrap();
        db.save_job("act-job-2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();

        // A running job must not leak into the pending-reminder list.
        assert!(db.list_pending_reminders().unwrap().is_empty());

        db.update_job_task("act-job-2", "task-9").unwrap();
        db.finish_job(
            "act-job-1",
            "completed",
            Some("hello"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();
        db.finish_job(
            "act-job-2",
            "failed",
            None,
            Some("connection refused"),
            Some("connection refused"),
            Some("C:\\tmp\\job-logs\\act-job-2.log"),
            Some(1),
            "2026-08-09T10:01:03Z",
        )
        .unwrap();

        let all = db.list_activities(None).unwrap();
        assert_eq!(all.len(), 2);
        let jobs = db.list_activities(Some("job")).unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(db.list_activities(Some("reminder")).unwrap().is_empty());

        let finished = jobs.iter().find(|a| a.id == "act-job-1").unwrap();
        assert_eq!(finished.status.as_deref(), Some("completed"));
        assert_eq!(finished.output.as_deref(), Some("hello"));
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.task_id.as_deref(), Some("task-9"));
        assert!(finished.finished_at.is_some());

        let failed = jobs.iter().find(|a| a.id == "act-job-2").unwrap();
        assert_eq!(failed.status.as_deref(), Some("failed"));
        assert_eq!(failed.task_id.as_deref(), Some("task-9"));
        assert_eq!(
            failed.log_path.as_deref(),
            Some("C:\\tmp\\job-logs\\act-job-2.log")
        );
        assert_eq!(failed.error_reason.as_deref(), Some("connection refused"));

        assert!(db.get_activity("act-job-1").unwrap().is_some());
        assert!(db.get_activity("nope").unwrap().is_none());
    }

    #[test]
    fn reminder_rows_keep_kind_and_fired_history() {
        let db = test_db();
        db.save_reminder(
            "act-r1",
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
        db.mark_reminder_fired("act-r1").unwrap();
        db.save_job("act-j1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();

        // Pending list excludes both the fired reminder and the job.
        assert!(db.list_pending_reminders().unwrap().is_empty());
        // Activity listing still surfaces the fired reminder as history.
        let reminders = db.list_activities(Some("reminder")).unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].id, "act-r1");
        assert!(reminders[0].fired);
        assert_eq!(reminders[0].kind, "reminder");
    }

    #[test]
    fn interrupted_jobs_marked_failed() {
        let db = test_db();
        db.save_job("act-j1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        db.save_job("act-j2", None, "ping", "2026-08-09T10:01:00Z")
            .unwrap();
        db.finish_job(
            "act-j1",
            "completed",
            Some("ok"),
            None,
            None,
            None,
            Some(0),
            "2026-08-09T10:00:05Z",
        )
        .unwrap();

        let n = db.mark_interrupted_jobs().unwrap();
        assert_eq!(n, 1);
        let jobs = db.list_activities(Some("job")).unwrap();
        let running_left = jobs
            .iter()
            .filter(|a| a.status.as_deref() == Some("running"))
            .count();
        assert_eq!(running_left, 0);
        let j2 = jobs.iter().find(|a| a.id == "act-j2").unwrap();
        assert_eq!(j2.status.as_deref(), Some("failed"));
        assert!(j2.error_reason.as_deref().unwrap().contains("restarted"));
        // Second run is a no-op.
        assert_eq!(db.mark_interrupted_jobs().unwrap(), 0);
    }

    #[test]
    fn delete_activity_removes_any_kind() {
        let db = test_db();
        db.save_job("act-j1", None, "echo", "2026-08-09T10:00:00Z")
            .unwrap();
        db.delete_activity("act-j1").unwrap();
        assert!(db.list_activities(Some("job")).unwrap().is_empty());
    }
}
