use crate::db::Database;
use crate::repositories::messages::now_rfc3339_millis;
use chrono::Utc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionStep {
    pub id: String,
    pub session_id: String,
    pub step_number: i32,
    /// Raw thought text from the Reasoner (replaces old `tool_name = "thought"` hack)
    pub thought: Option<String>,
    /// Tool name when this step represents a tool call action
    pub action_tool: Option<String>,
    /// JSON-serialized tool input parameters
    pub action_input: Option<String>,
    /// Tool observation / result text
    pub observation: Option<String>,
    pub status: String,
    pub is_high_risk: bool,
    pub confirmed: Option<bool>,
    /// Whether the tool output was hidden from the user in the live chat
    /// (`"silent": true` in the tool input). Persisted so the history review
    /// renders the same as the live conversation.
    pub silent: bool,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
}

impl Database {
    /// Create a thought-only step row under a PRE-MINTED id.
    ///
    /// The row is the execution-state anchor of a streamed thought (or a user
    /// supplement/steering input): its id is the SAME id its content message
    /// row is persisted under in the `messages` table, so the review builder
    /// links the two without content matching. The `thought` column is
    /// intentionally NOT written — the text lives exclusively in the
    /// `messages` table (single content authority). Legacy rows keep their
    /// text.
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
             VALUES (?1, ?2, ?3, 'thought', ?1, NULL, 'completed', 0, ?4)",
            rusqlite::params![id, session_id, step_number, now],
        )?;
        Ok(SessionStep {
            id: id.into(),
            session_id: session_id.into(),
            step_number,
            thought: None,
            action_tool: None,
            action_input: None,
            observation: None,
            status: "completed".into(),
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
    pub fn create_action_step(
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
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO session_steps (id, session_id, step_number, tool_name, input, action_tool, action_input, status, is_high_risk, created_at, silent, confirmed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?5, 'pending', ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id,
                session_id,
                step_number,
                tool_name,
                tool_input,
                is_high_risk as i32,
                now,
                silent as i32,
                confirmed.map(|c| c as i32)
            ],
        )?;
        Ok(SessionStep {
            id,
            session_id: session_id.into(),
            step_number,
            thought: None,
            action_tool: Some(tool_name.into()),
            action_input: Some(tool_input.into()),
            observation: None,
            status: "pending".into(),
            is_high_risk,
            confirmed,
            silent,
            started_at: None,
            completed_at: None,
            created_at: now,
        })
    }

    /// Complete an action step by recording its observation.
    pub fn complete_action_step(
        &self,
        id: &str,
        observation: &str,
        success: bool,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let status = if success { "completed" } else { "failed" };
        let conn = self.conn();
        conn.execute(
            "UPDATE session_steps SET status = ?1, observation = ?2, completed_at = ?3 WHERE id = ?4",
            rusqlite::params![status, observation, now, id],
        )?;
        Ok(())
    }

    pub fn get_session_steps(&self, session_id: &str) -> anyhow::Result<Vec<SessionStep>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, step_number, tool_name, input, output, thought, action_tool, action_input, observation,
                    status, is_high_risk, confirmed, started_at, completed_at, created_at, silent
             FROM session_steps WHERE session_id = ?1 ORDER BY step_number ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            let output: Option<String> = row.get(5)?;
            let obs: Option<String> = row.get(9)?;
            Ok(SessionStep {
                id: row.get(0)?,
                session_id: row.get(1)?,
                step_number: row.get(2)?,
                thought: row.get(6)?,
                action_tool: row.get(7)?,
                action_input: row.get(8)?,
                observation: obs.or(output),
                status: row.get(10)?,
                is_high_risk: row.get::<_, i32>(11)? != 0,
                confirmed: row.get(12)?,
                started_at: row.get(13)?,
                completed_at: row.get(14)?,
                created_at: row.get(15)?,
                silent: row.get::<_, i32>(16)? != 0,
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
    /// recorded steps instead of appending to them, so the review history
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
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn seed_session(db: &Database, session_id: &str) {
        db.create_session("test", "test").unwrap();
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
        assert!(step.action_tool.is_none());
        assert!(step.action_input.is_none());
        let steps = db.get_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "step-thought-1");
    }

    #[test]
    fn create_and_get_action_step() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let step = db
            .create_action_step(
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
        assert_eq!(step.action_tool.as_deref(), Some("read_file"));
        assert_eq!(
            step.action_input.as_deref(),
            Some(r#"{"path": "test.txt"}"#)
        );
        assert!(step.thought.is_none());
        let steps = db.get_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn complete_action_step_sets_observation() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let step = db
            .create_action_step("ses-1", 0, "read_file", "{}", false, false, None, None)
            .unwrap();
        db.complete_action_step(&step.id, "file content here", true)
            .unwrap();
        let steps = db.get_session_steps("ses-1").unwrap();
        assert_eq!(steps[0].observation.as_deref(), Some("file content here"));
        assert_eq!(steps[0].status, "completed");
    }

    #[test]
    fn create_action_step_persists_silent_flag() {
        let db = test_db();
        seed_session(&db, "ses-1");
        let visible = db
            .create_action_step("ses-1", 0, "shell", "{}", false, false, None, None)
            .unwrap();
        assert!(!visible.silent);
        let silent = db
            .create_action_step(
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
        let steps = db.get_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(!steps[0].silent);
        assert!(steps[1].silent);
    }

    #[test]
    fn get_session_steps_returns_empty_for_unknown_session() {
        let db = test_db();
        let steps = db.get_session_steps("missing-session").unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn get_session_steps_preserves_order_by_index() {
        let db = test_db();
        seed_session(&db, "ses-1");
        db.create_action_step("ses-1", 2, "c", "{}", false, false, None, None)
            .unwrap();
        db.create_action_step("ses-1", 0, "a", "{}", false, false, None, None)
            .unwrap();
        db.create_action_step("ses-1", 1, "b", "{}", false, false, None, None)
            .unwrap();
        let steps = db.get_session_steps("ses-1").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_number, 0);
        assert_eq!(steps[1].step_number, 1);
        assert_eq!(steps[2].step_number, 2);
        assert_eq!(steps[0].action_tool.as_deref(), Some("a"));
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
        let steps = db.get_session_steps("ses-1").unwrap();
        assert_eq!(
            steps.len(),
            1,
            "only the row created before the cutoff survives"
        );
        assert_eq!(steps[0].id, first.id);
        assert_ne!(steps[0].id, second.id);
    }
}
