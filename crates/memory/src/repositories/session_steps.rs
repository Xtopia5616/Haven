use crate::db::Database;
use crate::repositories::messages::now_rfc3339_millis;
use haven_common::SessionStepStatus;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStep {
    pub id: String,
    pub session_id: String,
    pub step_number: i32,
    pub tool_index: i32,
    /// Raw thought text from the Reasoner (replaces old `tool_name = "thought"` hack)
    pub thought: Option<String>,
    /// Tool name when this step represents a tool call.
    pub tool_name: Option<String>,
    /// JSON-serialized tool input parameters.
    pub tool_input: Option<String>,
    pub tool_call_id: Option<String>,
    /// Tool observation / result text
    pub observation: Option<String>,
    pub status: SessionStepStatus,
    pub is_high_risk: bool,
    pub confirmed: Option<bool>,
    /// Whether the tool output was hidden from the user in the live chat
    /// (`"silent": true` in the tool input). Persisted so the history resume
    /// renders the same as the live transcript.
    pub silent: bool,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
}

/// Durable outcome of an action step. `Unknown` means execution may have
/// crossed an external side-effect boundary before cancellation/abort, so a
/// caller must not retry it automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStepOutcome {
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

/// Owned action-step identity and metadata accepted by SessionStore ports.
/// Agent owns how this data is chosen; Memory only persists it using the
/// existing session-step operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStepWrite {
    pub session_id: String,
    pub step_number: i32,
    pub tool_index: i32,
    pub tool_name: String,
    pub tool_input: String,
    pub tool_call_id: Option<String>,
    pub is_high_risk: bool,
    pub silent: bool,
    pub step_id: String,
}

#[derive(Clone, Copy)]
struct ToolStepFields<'a> {
    id: &'a str,
    session_id: &'a str,
    step_number: i32,
    tool_index: i32,
    tool_name: &'a str,
    tool_input: &'a str,
    tool_call_id: Option<&'a str>,
    is_high_risk: bool,
    silent: bool,
    confirmed: Option<bool>,
}

impl ToolStepOutcome {
    pub const fn status(self) -> SessionStepStatus {
        match self {
            Self::Completed => SessionStepStatus::Completed,
            Self::Failed => SessionStepStatus::Failed,
            Self::Cancelled => SessionStepStatus::Cancelled,
            Self::Unknown => SessionStepStatus::Unknown,
        }
    }
}

impl Database {
    fn bump_step_seq(conn: &rusqlite::Connection, session_id: &str) -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO session_step_cursors (session_id, last_step_seq)
             VALUES (?1, 1)
             ON CONFLICT(session_id) DO UPDATE SET
                last_step_seq = session_step_cursors.last_step_seq + 1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    fn insert_tool_step(
        &self,
        fields: ToolStepFields<'_>,
        ignore_existing: bool,
    ) -> anyhow::Result<String> {
        let now = now_rfc3339_millis();
        let conn = self.conn();
        let sql = if ignore_existing {
            "INSERT OR IGNORE INTO session_steps (id, session_id, step_number, tool_index, tool_name, input, tool_call_name, tool_call_input, tool_call_id, status, is_high_risk, created_at, silent, confirmed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?6, ?7, ?12, ?8, ?9, ?10, ?11)"
        } else {
            "INSERT INTO session_steps (id, session_id, step_number, tool_index, tool_name, input, tool_call_name, tool_call_input, tool_call_id, status, is_high_risk, created_at, silent, confirmed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?6, ?7, ?12, ?8, ?9, ?10, ?11)"
        };
        conn.execute(
            sql,
            rusqlite::params![
                fields.id,
                fields.session_id,
                fields.step_number,
                fields.tool_index,
                fields.tool_name,
                fields.tool_input,
                fields.tool_call_id,
                fields.is_high_risk as i32,
                now,
                fields.silent as i32,
                fields.confirmed.map(|confirmed| confirmed as i32),
                SessionStepStatus::Pending.as_str(),
            ],
        )?;
        if conn.changes() > 0 {
            Self::bump_step_seq(&conn, fields.session_id)?;
        }
        Ok(now)
    }

    fn tool_step_from_fields(fields: ToolStepFields<'_>, created_at: String) -> SessionStep {
        SessionStep {
            id: fields.id.into(),
            session_id: fields.session_id.into(),
            step_number: fields.step_number,
            tool_index: fields.tool_index,
            thought: None,
            tool_name: Some(fields.tool_name.into()),
            tool_input: Some(fields.tool_input.into()),
            tool_call_id: fields.tool_call_id.map(String::from),
            observation: None,
            status: SessionStepStatus::Pending,
            is_high_risk: fields.is_high_risk,
            confirmed: fields.confirmed,
            silent: fields.silent,
            started_at: None,
            completed_at: None,
            created_at,
        }
    }

    fn update_pending_confirmation(
        conn: &rusqlite::Connection,
        id: &str,
        confirmed: Option<bool>,
    ) -> anyhow::Result<()> {
        if let Some(confirmed) = confirmed {
            conn.execute(
                "UPDATE session_steps SET confirmed = ?1 WHERE id = ?2 AND status = ?3",
                rusqlite::params![confirmed as i32, id, SessionStepStatus::Pending.as_str()],
            )?;
        }
        Ok(())
    }

    fn ensure_tool_step_record(
        &self,
        fields: ToolStepFields<'_>,
        refresh_identity: bool,
    ) -> anyhow::Result<()> {
        self.insert_tool_step(fields, true)?;
        let conn = self.conn();
        if refresh_identity {
            conn.execute(
                "UPDATE session_steps SET tool_index = COALESCE(tool_index, ?1), tool_call_id = COALESCE(tool_call_id, ?2) WHERE id = ?3 AND status = ?4",
                rusqlite::params![
                    fields.tool_index,
                    fields.tool_call_id,
                    fields.id,
                    SessionStepStatus::Pending.as_str()
                ],
            )?;
        }
        Self::update_pending_confirmation(&conn, fields.id, fields.confirmed)
    }

    /// Create a thought-only step row under a PRE-MINTED id.
    ///
    /// The row is the execution-state anchor of a streamed thought (or a user
    /// supplement/steering input): its id is the SAME id its content message
    /// row is persisted under in the `messages` table, so the resume builder
    /// links the two without content matching. The `thought` column is
    /// intentionally NOT written — the text lives exclusively in the
    /// `messages` table (single content authority).
    pub fn create_thought_step(
        &self,
        session_id: &str,
        step_number: i32,
        id: &str,
    ) -> anyhow::Result<SessionStep> {
        let now = now_rfc3339_millis();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO session_steps (id, session_id, step_number, tool_name, input, thought, status, is_high_risk, created_at)
             VALUES (?1, ?2, ?3, 'thought', ?1, NULL, ?4, 0, ?5)",
            rusqlite::params![
                id,
                session_id,
                step_number,
                SessionStepStatus::Completed.as_str(),
                now
            ],
        )?;
        Self::bump_step_seq(&conn, session_id)?;
        Ok(SessionStep {
            id: id.into(),
            session_id: session_id.into(),
            step_number,
            tool_index: 0,
            thought: None,
            tool_name: None,
            tool_input: None,
            tool_call_id: None,
            observation: None,
            status: SessionStepStatus::Completed,
            is_high_risk: false,
            confirmed: None,
            silent: false,
            started_at: None,
            completed_at: None,
            created_at: now,
        })
    }

    /// Create an action step with the new schema fields directly.
    /// `confirmed` records whether the operation passed the safety gateway
    /// (Some(true)=approved, Some(false)=rejected, None=not gated) so the
    /// decision is persisted on the actual step row at creation time.
    /// `id` is the pre-minted `step-*` id the live tool card already uses
    /// (`None` mints a fresh one); passing the same id lets execute_step
    /// persist the row the frontend's streamed card references.
    #[allow(clippy::too_many_arguments)]
    pub fn create_tool_step(
        &self,
        session_id: &str,
        step_number: i32,
        tool_name: &str,
        tool_input: &str,
        is_high_risk: bool,
        silent: bool,
        confirmed: Option<bool>,
        id: Option<&str>,
    ) -> anyhow::Result<SessionStep> {
        let id = id
            .map(String::from)
            .unwrap_or_else(|| haven_common::types::new_id("step"));
        let fields = ToolStepFields {
            id: &id,
            session_id,
            step_number,
            tool_index: 0,
            tool_name,
            tool_input,
            tool_call_id: None,
            is_high_risk,
            silent,
            confirmed,
        };
        let created_at = self.insert_tool_step(fields, false)?;
        Ok(Self::tool_step_from_fields(fields, created_at))
    }

    /// Ensure an action step exists while retaining its stable invocation
    /// identity. Existing rows are never rewritten after completion.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_tool_step_with_identity(
        &self,
        session_id: &str,
        step_number: i32,
        tool_index: i32,
        tool_name: &str,
        tool_input: &str,
        tool_call_id: Option<&str>,
        is_high_risk: bool,
        silent: bool,
        confirmed: Option<bool>,
        id: &str,
    ) -> anyhow::Result<()> {
        self.ensure_tool_step_record(
            ToolStepFields {
                id,
                session_id,
                step_number,
                tool_index,
                tool_name,
                tool_input,
                tool_call_id,
                is_high_risk,
                silent,
                confirmed,
            },
            true,
        )
    }

    /// Complete an action step by recording its observation.
    pub fn complete_tool_step(
        &self,
        id: &str,
        observation: &str,
        success: bool,
    ) -> anyhow::Result<()> {
        self.finish_tool_step(
            id,
            observation,
            if success {
                ToolStepOutcome::Completed
            } else {
                ToolStepOutcome::Failed
            },
        )?;
        Ok(())
    }

    /// Mark a pending action as running. The update is idempotent for an
    /// already-running row and refuses to revive a terminal row.
    pub fn start_tool_step(&self, id: &str) -> anyhow::Result<bool> {
        let now = now_rfc3339_millis();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE session_steps SET status = ?1, started_at = COALESCE(started_at, ?2) \
             WHERE id = ?3 AND status IN (?4, ?1)",
            rusqlite::params![
                SessionStepStatus::Running.as_str(),
                now,
                id,
                SessionStepStatus::Pending.as_str()
            ],
        )?;
        Ok(changed > 0)
    }

    /// Finish an ToolRun with an explicit durable outcome. Only pending or
    /// running rows can transition, making late tool completions harmless
    /// after rollback/cancellation has already finalized the row.
    pub fn finish_tool_step(
        &self,
        id: &str,
        observation: &str,
        outcome: ToolStepOutcome,
    ) -> anyhow::Result<bool> {
        let now = now_rfc3339_millis();
        let conn = self.conn();
        let status = outcome.status();
        let changed = conn.execute(
            "UPDATE session_steps SET status = ?1, observation = ?2, completed_at = ?3 \
             WHERE id = ?4 AND status IN (?5, ?6)",
            rusqlite::params![
                status.as_str(),
                observation,
                now,
                id,
                SessionStepStatus::Pending.as_str(),
                SessionStepStatus::Running.as_str()
            ],
        )?;
        Ok(changed > 0)
    }

    /// Finalize every still-pending/running action step as `unknown` after a
    /// handler panic/abort. The tool may have crossed an external side-effect
    /// boundary, so recovery must not present it as a deterministic failure.
    pub fn fail_pending_tool_run_steps(
        &self,
        session_id: &str,
        observation: &str,
    ) -> anyhow::Result<usize> {
        let now = now_rfc3339_millis();
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE session_steps SET status = ?1, observation = ?2, completed_at = ?3 \
             WHERE session_id = ?4 AND status IN (?5, ?6)",
            rusqlite::params![
                SessionStepStatus::Unknown.as_str(),
                observation,
                now,
                session_id,
                SessionStepStatus::Pending.as_str(),
                SessionStepStatus::Running.as_str()
            ],
        )?;
        Ok(n)
    }

    pub fn list_session_steps(&self, session_id: &str) -> anyhow::Result<Vec<SessionStep>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, step_number, tool_index, tool_name, input, output, thought, tool_call_name, tool_call_input, tool_call_id, observation,
                    status, is_high_risk, confirmed, started_at, completed_at, created_at, silent
             FROM session_steps WHERE session_id = ?1 ORDER BY step_number ASC, tool_index ASC, created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            let output: Option<String> = row.get(6)?;
            let obs: Option<String> = row.get(11)?;
            let status_text: String = row.get(12)?;
            let status = SessionStepStatus::parse(&status_text).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    format!("invalid SessionStepStatus in session_steps.status: {status_text}")
                        .into(),
                )
            })?;
            Ok(SessionStep {
                id: row.get(0)?,
                session_id: row.get(1)?,
                step_number: row.get(2)?,
                thought: row.get(7)?,
                tool_index: row.get(3)?,
                tool_name: row.get(8)?,
                tool_input: row.get(9)?,
                tool_call_id: row.get(10)?,
                observation: obs.or(output),
                status,
                is_high_risk: row.get::<_, i32>(13)? != 0,
                confirmed: row.get(14)?,
                started_at: row.get(15)?,
                completed_at: row.get(16)?,
                created_at: row.get(17)?,
                silent: row.get::<_, i32>(18)? != 0,
            })
        })?;
        let mut steps = Vec::new();
        for row in rows {
            steps.push(row?);
        }
        Ok(steps)
    }

    /// Delete every step row created strictly after the given timestamp.
    /// Used by retry/rollback: the re-run OVERWRITES the previous attempt's
    /// recorded steps instead of appending to them, so the resume history
    /// stays linear — only branching creates separate timelines.
    pub fn delete_session_steps_after(
        &self,
        session_id: &str,
        created_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM session_steps WHERE session_id = ?1 AND created_at > ?2",
            rusqlite::params![session_id, created_at],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionStepStatus, ToolStepOutcome};
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn seed_session(db: &Database, session_id: &str) {
        db.create_session("test").unwrap();
        // Override the id to match test expectations
        let conn = db.conn();
        let _ = conn.execute(
            "UPDATE sessions SET id = ?1 WHERE id IN (SELECT id FROM sessions ORDER BY created_at DESC LIMIT 1)",
            rusqlite::params![session_id],
        );
    }

    #[test]
    fn create_and_get_thought_step() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let step = db
            .create_thought_step("ses-1", 0, "step-thought-1")
            .unwrap();
        // The id is pre-minted (shared with the content message row) and the
        // thought column stays empty: the text lives in `messages`.
        assert_eq!(step.id, "step-thought-1");
        assert!(step.thought.is_none());
        assert!(step.tool_name.is_none());
        assert!(step.tool_input.is_none());
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "step-thought-1");
    }

    #[test]
    fn create_and_get_tool_step() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let step = db
            .create_tool_step(
                "ses-1",
                0,
                "read_file",
                r#"{"path": "test.txt"}"#,
                false,
                false,
                None,
                None,
            )
            .unwrap();
        assert_eq!(step.tool_name.as_deref(), Some("read_file"));
        assert_eq!(step.tool_input.as_deref(), Some(r#"{"path": "test.txt"}"#));
        assert!(step.thought.is_none());
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn complete_tool_step_sets_observation() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let step = db
            .create_tool_step("ses-1", 0, "read_file", "{}", false, false, None, None)
            .unwrap();
        db.complete_tool_step(&step.id, "file content here", true)
            .unwrap();
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(steps[0].observation.as_deref(), Some("file content here"));
        assert_eq!(steps[0].status, SessionStepStatus::Completed);
    }

    #[test]
    fn tool_step_lifecycle_records_running_and_unknown() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let step = db
            .create_tool_step("ses-1", 0, "shell", "{}", false, false, None, None)
            .unwrap();
        assert!(db.start_tool_step(&step.id).unwrap());
        let running = db.list_session_steps("ses-1").unwrap();
        assert_eq!(running[0].status, SessionStepStatus::Running);
        assert!(running[0].started_at.is_some());
        assert!(
            db.finish_tool_step(
                &step.id,
                "cancelled while in flight",
                ToolStepOutcome::Unknown
            )
            .unwrap()
        );
        let finished = db.list_session_steps("ses-1").unwrap();
        assert_eq!(finished[0].status, SessionStepStatus::Unknown);
        assert!(finished[0].completed_at.is_some());
        assert!(!db.start_tool_step(&step.id).unwrap());

        let cancelled = db
            .create_tool_step("ses-1", 1, "shell", "{}", false, false, None, None)
            .unwrap();
        db.finish_tool_step(
            &cancelled.id,
            "cancelled before execution",
            ToolStepOutcome::Cancelled,
        )
        .unwrap();
        assert_eq!(
            db.list_session_steps("ses-1").unwrap()[1].status,
            SessionStepStatus::Cancelled
        );
    }

    #[test]
    fn ensure_tool_step_is_idempotent_and_updates_confirmed() {
        let db = test_db();
        seed_session(&db, "ses-1");
        db.ensure_tool_step_with_identity(
            "ses-1",
            0,
            0,
            "shell",
            "{}",
            None,
            false,
            false,
            None,
            "step-ensure-1",
        )
        .unwrap();
        db.ensure_tool_step_with_identity(
            "ses-1",
            0,
            0,
            "shell",
            "{}",
            None,
            false,
            false,
            Some(true),
            "step-ensure-1",
        )
        .unwrap();
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "step-ensure-1");
        assert_eq!(steps[0].status, SessionStepStatus::Pending);
        assert_eq!(steps[0].confirmed, Some(true));
    }

    #[test]
    fn fail_pending_tool_run_steps_finalizes_unfinished_only() {
        let db = test_db();
        seed_session(&db, "ses-1");
        db.ensure_tool_step_with_identity(
            "ses-1", 0, 0, "shell", "{}", None, false, false, None, "step-p1",
        )
        .unwrap();
        let done = db
            .create_tool_step("ses-1", 1, "shell", "{}", false, false, None, None)
            .unwrap();
        db.complete_tool_step(&done.id, "ok", true).unwrap();
        let n = db
            .fail_pending_tool_run_steps("ses-1", "Session ended before tool finished")
            .unwrap();
        assert_eq!(n, 1);
        let steps = db.list_session_steps("ses-1").unwrap();
        let pending = steps.iter().find(|s| s.id == "step-p1").unwrap();
        assert_eq!(pending.status, SessionStepStatus::Unknown);
        assert_eq!(
            pending.observation.as_deref(),
            Some("Session ended before tool finished")
        );
        let completed = steps.iter().find(|s| s.id == done.id).unwrap();
        assert_eq!(completed.status, SessionStepStatus::Completed);
        assert_eq!(completed.observation.as_deref(), Some("ok"));
    }

    #[test]
    fn create_tool_step_persists_silent_flag() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let visible = db
            .create_tool_step("ses-1", 0, "shell", "{}", false, false, None, None)
            .unwrap();
        assert!(!visible.silent);
        let silent = db
            .create_tool_step(
                "ses-1",
                1,
                "shell",
                r#"{"silent": true}"#,
                false,
                true,
                None,
                None,
            )
            .unwrap();
        assert!(silent.silent);
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(!steps[0].silent);
        assert!(steps[1].silent);
    }

    #[test]
    fn list_session_steps_returns_empty_for_unknown_session() {
        let db = test_db();
        let steps = db.list_session_steps("missing-session").unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn list_session_steps_rejects_unknown_status_from_storage() {
        let db = test_db();
        seed_session(&db, "ses-1");
        db.create_tool_step("ses-1", 0, "shell", "{}", false, false, None, None)
            .unwrap();

        {
            let conn = db.conn();
            conn.execute_batch("PRAGMA ignore_check_constraints = ON")
                .unwrap();
            conn.execute(
                "UPDATE session_steps SET status = 'future' WHERE session_id = ?1",
                ["ses-1"],
            )
            .unwrap();
            conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
                .unwrap();
        }

        let error = db.list_session_steps("ses-1").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid SessionStepStatus in session_steps.status")
        );
    }

    #[test]
    fn list_session_steps_preserves_order_by_index() {
        let db = test_db();
        seed_session(&db, "ses-1");
        db.create_tool_step("ses-1", 2, "c", "{}", false, false, None, None)
            .unwrap();
        db.create_tool_step("ses-1", 0, "a", "{}", false, false, None, None)
            .unwrap();
        db.create_tool_step("ses-1", 1, "b", "{}", false, false, None, None)
            .unwrap();
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_number, 0);
        assert_eq!(steps[1].step_number, 1);
        assert_eq!(steps[2].step_number, 2);
        assert_eq!(steps[0].tool_name.as_deref(), Some("a"));
    }

    #[test]
    fn delete_session_steps_after_removes_only_newer_rows() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let first = db.create_thought_step("ses-1", 1, "step-first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let cutoff = chrono::Utc::now().to_rfc3339();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = db.create_thought_step("ses-1", 2, "step-second").unwrap();

        db.delete_session_steps_after("ses-1", &cutoff).unwrap();
        let steps = db.list_session_steps("ses-1").unwrap();
        assert_eq!(
            steps.len(),
            1,
            "only the row created before the cutoff survives"
        );
        assert_eq!(steps[0].id, first.id);
        assert_ne!(steps[0].id, second.id);
    }
}
