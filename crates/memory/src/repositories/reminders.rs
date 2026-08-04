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
    pub fn list_pending_reminders(&self) -> anyhow::Result<Vec<ReminderRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, due_at, title, body, mode, task_id, tool_name, tool_args, prompt, fired, created_at
             FROM reminders WHERE fired = 0 ORDER BY due_at ASC",
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
            "notify",
            None,
            None,
            None,
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
        assert_eq!(pending[1].mode, "notify");
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
            "notify",
            None,
            None,
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
            "notify",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.delete_reminder("r1").unwrap();
        assert!(db.list_pending_reminders().unwrap().is_empty());
    }
}
