use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskStep {
    pub id: String,
    pub task_id: String,
    pub step_index: i32,
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
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
}

impl Database {
    /// Create a thought-only step using the new schema fields directly.
    pub fn create_thought_step(
        &self,
        task_id: &str,
        step_index: i32,
        thought: &str,
    ) -> anyhow::Result<TaskStep> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO task_steps (id, task_id, step_index, tool_name, input, thought, status, is_high_risk, created_at)
             VALUES (?1, ?2, ?3, 'thought', ?4, ?4, 'completed', 0, ?5)",
            rusqlite::params![id, task_id, step_index, thought, now],
        )?;
        Ok(TaskStep {
            id,
            task_id: task_id.into(),
            step_index,
            thought: Some(thought.into()),
            action_tool: None,
            action_input: None,
            observation: None,
            status: "completed".into(),
            is_high_risk: false,
            confirmed: None,
            started_at: None,
            completed_at: None,
            created_at: now,
        })
    }

    /// Create an action step with the new schema fields directly.
    pub fn create_action_step(
        &self,
        task_id: &str,
        step_index: i32,
        tool_name: &str,
        tool_input: &str,
        is_high_risk: bool,
    ) -> anyhow::Result<TaskStep> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO task_steps (id, task_id, step_index, tool_name, input, action_tool, action_input, status, is_high_risk, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?5, 'pending', ?6, ?7)",
            rusqlite::params![id, task_id, step_index, tool_name, tool_input, is_high_risk as i32, now],
        )?;
        Ok(TaskStep {
            id,
            task_id: task_id.into(),
            step_index,
            thought: None,
            action_tool: Some(tool_name.into()),
            action_input: Some(tool_input.into()),
            observation: None,
            status: "pending".into(),
            is_high_risk,
            confirmed: None,
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
            "UPDATE task_steps SET status = ?1, observation = ?2, completed_at = ?3 WHERE id = ?4",
            rusqlite::params![status, observation, now, id],
        )?;
        Ok(())
    }

    pub fn confirm_step(&self, id: &str, confirmed: bool) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE task_steps SET confirmed = ?1 WHERE id = ?2",
            rusqlite::params![confirmed as i32, id],
        )?;
        Ok(())
    }

    pub fn get_task_steps(&self, task_id: &str) -> anyhow::Result<Vec<TaskStep>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, step_index, tool_name, input, output, thought, action_tool, action_input, observation,
                    status, is_high_risk, confirmed, started_at, completed_at, created_at
             FROM task_steps WHERE task_id = ?1 ORDER BY step_index ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], |row| {
            let output: Option<String> = row.get(5)?;
            let obs: Option<String> = row.get(9)?;
            Ok(TaskStep {
                id: row.get(0)?,
                task_id: row.get(1)?,
                step_index: row.get(2)?,
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
            })
        })?;
        let mut steps = Vec::new();
        for row in rows {
            steps.push(row?);
        }
        Ok(steps)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn seed_task(db: &Database, task_id: &str) {
        db.create_task(None, "test", "test").unwrap();
        // Override the id to match test expectations
        let conn = db.conn();
        let _ = conn.execute(
            "UPDATE tasks SET id = ?1 WHERE id IN (SELECT id FROM tasks ORDER BY created_at DESC LIMIT 1)",
            rusqlite::params![task_id],
        );
    }

    #[test]
    fn create_and_get_thought_step() {
        let db = test_db();
        seed_task(&db, "task-1");
        let step = db.create_thought_step("task-1", 0, "I should check the file").unwrap();
        assert_eq!(step.thought.as_deref(), Some("I should check the file"));
        assert!(step.action_tool.is_none());
        assert!(step.action_input.is_none());
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn create_and_get_action_step() {
        let db = test_db();
        seed_task(&db, "task-1");
        let step = db.create_action_step("task-1", 0, "read_file", r#"{"path": "test.txt"}"#, false).unwrap();
        assert_eq!(step.action_tool.as_deref(), Some("read_file"));
        assert_eq!(step.action_input.as_deref(), Some(r#"{"path": "test.txt"}"#));
        assert!(step.thought.is_none());
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn complete_action_step_sets_observation() {
        let db = test_db();
        seed_task(&db, "task-1");
        let step = db.create_action_step("task-1", 0, "read_file", "{}", false).unwrap();
        db.complete_action_step(&step.id, "file content here", true).unwrap();
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps[0].observation.as_deref(), Some("file content here"));
        assert_eq!(steps[0].status, "completed");
    }

    #[test]
    fn confirm_step_sets_confirmed_flag() {
        let db = test_db();
        seed_task(&db, "task-1");
        let step = db.create_action_step("task-1", 0, "rm_file", "{}", true).unwrap();
        // initially unconfirmed
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps[0].confirmed, None);
        // confirm true
        db.confirm_step(&step.id, true).unwrap();
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps[0].confirmed, Some(true));
        // confirm false
        db.confirm_step(&step.id, false).unwrap();
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps[0].confirmed, Some(false));
    }

    #[test]
    fn confirm_step_nonexistent_is_noop() {
        let db = test_db();
        // confirm_step on a non-existent step does not error (UPDATE affects 0 rows).
        let result = db.confirm_step("no-such-step", true);
        assert!(result.is_ok());
        // Confirmed flag stays absent for any real step.
        seed_task(&db, "task-1");
        let steps = db.get_task_steps("task-1").unwrap();
        assert!(steps.is_empty() || steps.iter().all(|s| s.confirmed.is_none()));
    }

    #[test]
    fn get_task_steps_returns_empty_for_unknown_task() {
        let db = test_db();
        let steps = db.get_task_steps("missing-task").unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn get_task_steps_preserves_order_by_index() {
        let db = test_db();
        seed_task(&db, "task-1");
        db.create_action_step("task-1", 2, "c", "{}", false).unwrap();
        db.create_action_step("task-1", 0, "a", "{}", false).unwrap();
        db.create_action_step("task-1", 1, "b", "{}", false).unwrap();
        let steps = db.get_task_steps("task-1").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_index, 0);
        assert_eq!(steps[1].step_index, 1);
        assert_eq!(steps[2].step_index, 2);
        assert_eq!(steps[0].action_tool.as_deref(), Some("a"));
    }
}
