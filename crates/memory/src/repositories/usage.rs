use crate::db::Database;

/// Per-task cumulative token/cost counters, persisted so a resumed or
/// reopened session can restore the token-stats display instead of resetting
/// to zero. Updated on every LLM usage emit; the row lives as long as the
/// task (ON DELETE CASCADE) and is removed with it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TaskUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
    pub has_cost: bool,
}

impl Database {
    /// Upsert the cumulative token/cost counters for a task. Callers pass the
    /// full cumulative values (not per-step deltas) — the row is replaced.
    pub fn update_task_usage(
        &self,
        task_id: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
        has_cost: bool,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO task_usage
                 (task_id, prompt_tokens, completion_tokens, total_tokens, cost_usd, has_cost, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(task_id) DO UPDATE SET
                 prompt_tokens = excluded.prompt_tokens,
                 completion_tokens = excluded.completion_tokens,
                 total_tokens = excluded.total_tokens,
                 cost_usd = excluded.cost_usd,
                 has_cost = excluded.has_cost,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                task_id,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cost_usd,
                has_cost
            ],
        )?;
        Ok(())
    }

    /// Load the persisted cumulative counters for a task, if any.
    pub fn get_task_usage(&self, task_id: &str) -> anyhow::Result<Option<TaskUsage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT prompt_tokens, completion_tokens, total_tokens, cost_usd, has_cost
             FROM task_usage WHERE task_id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![task_id], |row| {
            Ok(TaskUsage {
                prompt_tokens: row.get(0)?,
                completion_tokens: row.get(1)?,
                total_tokens: row.get(2)?,
                cost_usd: row.get(3)?,
                has_cost: row.get::<_, i32>(4)? != 0,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn update_and_get_task_usage_roundtrip() {
        let db = test_db();
        let task = db.create_task("hello", "").unwrap();
        assert!(db.get_task_usage(&task.id).unwrap().is_none());
        db.update_task_usage(&task.id, 100, 50, 150, 0.25, true)
            .unwrap();
        let u = db.get_task_usage(&task.id).unwrap().unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens, 150);
        assert_eq!(u.cost_usd, 0.25);
        assert!(u.has_cost);
    }

    #[test]
    fn update_replaces_cumulative_values() {
        let db = test_db();
        let task = db.create_task("hello", "").unwrap();
        db.update_task_usage(&task.id, 10, 5, 15, 0.0, false)
            .unwrap();
        // Callers pass the full cumulative totals, so a later call replaces
        // (not accumulates) the stored row.
        db.update_task_usage(&task.id, 20, 10, 30, 0.5, true)
            .unwrap();
        let u = db.get_task_usage(&task.id).unwrap().unwrap();
        assert_eq!(u.total_tokens, 30);
        assert_eq!(u.prompt_tokens, 20);
        assert_eq!(u.cost_usd, 0.5);
        assert!(u.has_cost);
    }

    #[test]
    fn task_usage_cascades_on_task_delete() {
        let db = test_db();
        let task = db.create_task("hello", "").unwrap();
        db.update_task_usage(&task.id, 10, 5, 15, 0.0, false)
            .unwrap();
        assert!(db.get_task_usage(&task.id).unwrap().is_some());
        db.delete_task(&task.id).unwrap();
        assert!(db.get_task_usage(&task.id).unwrap().is_none());
    }
}
