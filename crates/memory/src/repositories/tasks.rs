use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub id: String,
    pub session_id: Option<String>,
    pub input_text: String,
    pub title: Option<String>,
    pub status: String,
    pub classification: String,
    pub created_at: String,
    pub updated_at: String,
    pub transcript: String,
    pub react_state: Option<String>,
}

impl Database {
    pub fn create_task(
        &self,
        session_id: Option<&str>,
        input_text: &str,
        classification: &str,
        transcript: &str,
    ) -> anyhow::Result<Task> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO tasks (id, session_id, input_text, status, classification, created_at, updated_at, transcript)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?7)",
            rusqlite::params![id, session_id, input_text, classification, now, now, transcript],
        )?;
        self.cache_invalidate_tasks();
        Ok(Task {
            id,
            session_id: session_id.map(String::from),
            input_text: input_text.into(),
            title: None,
            status: "pending".into(),
            classification: classification.into(),
            created_at: now.clone(),
            updated_at: now,
            transcript: transcript.into(),
            react_state: None,
        })
    }

    pub fn get_task(&self, id: &str) -> anyhow::Result<Option<Task>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, input_text, title, status, classification, created_at, updated_at, transcript, react_state
             FROM tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Task {
                id: row.get(0)?,
                session_id: row.get(1)?,
                input_text: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                classification: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                transcript: row.get(8)?,
                react_state: row.get(9)?,
            })),
            None => Ok(None),
        }
    }

    pub fn update_task_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status, now, id],
        )?;
        self.cache_invalidate_tasks();
        Ok(())
    }

    pub fn update_task_title(&self, id: &str, title: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, now, id],
        )?;
        self.cache_invalidate_tasks();
        Ok(())
    }

    pub fn list_tasks(&self, limit: i64, offset: i64) -> anyhow::Result<Vec<Task>> {
        if offset == 0 && limit == 50
            && let Some(cached) = self.cache_get_tasks()
        {
            return Ok(cached);
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, input_text, title, status, classification, created_at, updated_at, transcript, react_state
             FROM tasks ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], |row| {
            Ok(Task {
                id: row.get(0)?,
                session_id: row.get(1)?,
                input_text: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                classification: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                transcript: row.get(8)?,
                react_state: row.get(9)?,
            })
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        if offset == 0 && limit == 50 {
            self.cache_put_tasks(tasks.clone(), 10);
        }
        Ok(tasks)
    }

    pub fn search_tasks(&self, query: &str) -> anyhow::Result<Vec<Task>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, input_text, title, status, classification, created_at, updated_at, transcript, react_state
             FROM tasks WHERE input_text LIKE ?1 OR transcript LIKE ?1 OR title LIKE ?1
             ORDER BY created_at DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(Task {
                id: row.get(0)?,
                session_id: row.get(1)?,
                input_text: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                classification: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                transcript: row.get(8)?,
                react_state: row.get(9)?,
            })
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }
    pub fn count_tasks(&self) -> anyhow::Result<i64> {
        let conn = self.conn();
        conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .map_err(Into::into)
    }

    pub fn count_tasks_search(&self, query: &str) -> anyhow::Result<i64> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE input_text LIKE ?1 OR transcript LIKE ?1 OR title LIKE ?1",
            rusqlite::params![pattern],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }

    pub fn search_tasks_paginated(&self, query: &str, limit: i64, offset: i64) -> anyhow::Result<Vec<Task>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, input_text, title, status, classification, created_at, updated_at, transcript, react_state
             FROM tasks WHERE input_text LIKE ?1 OR transcript LIKE ?1 OR title LIKE ?1
             ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit, offset], |row| {
            Ok(Task {
                id: row.get(0)?,
                session_id: row.get(1)?,
                input_text: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                classification: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                transcript: row.get(8)?,
                react_state: row.get(9)?,
            })
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    pub fn delete_task(&self, id: &str) -> anyhow::Result<()> {
        // Delete session messages associated with this task's session
        // so the conversation history does not linger after task deletion.
        if let Ok(Some(task)) = self.get_task(id)
            && let Some(ref session_id) = task.session_id
        {
            self.delete_session_messages(session_id).ok();
        }
        let conn = self.conn();
        let affected = conn.execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
        if affected == 0 {
            anyhow::bail!("task '{}' not found in database", id);
        }
        self.cache_invalidate_tasks();
        Ok(())
    }

    pub fn clear_tasks(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        // Delete all messages (session-level data) first.
        conn.execute("DELETE FROM messages", [])?;
        // CASCADE handles task_steps.
        let count = conn.execute("DELETE FROM tasks", [])?;
        self.cache_invalidate_tasks();
        Ok(count)
    }

    pub fn finalize_stale_tasks(&self, stale_minutes: i64) -> anyhow::Result<usize> {
        let cutoff = chrono::Duration::minutes(stale_minutes);
        let threshold = (Utc::now() - cutoff).to_rfc3339();
        let conn = self.conn();
        let count = conn.execute(
            "UPDATE tasks SET status = 'error', updated_at = ?1
             WHERE (status = 'running' OR status = 'pending' OR status = 'paused')
               AND updated_at < ?2",
            rusqlite::params![Utc::now().to_rfc3339(), threshold],
        )?;
        self.cache_invalidate_tasks();
        Ok(count)
    }

    pub fn delete_old_tasks(&self, retention_days: u32) -> anyhow::Result<usize> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
        let conn = self.conn();
        let count = conn.execute(
            "DELETE FROM tasks WHERE created_at < ?1",
            rusqlite::params![cutoff],
        )?;
        if count > 0 {
            self.cache_invalidate_tasks();
        }
        Ok(count)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn search_tasks_filtered(
        &self,
        query: Option<&str>,
        status: Option<&str>,
        classification: Option<&str>,
        start_date: Option<&str>,
        end_date: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Task>> {
        let mut wheres: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(q) = query.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            let p = format!("%{q}%");
            wheres.push("(input_text LIKE ? OR transcript LIKE ? OR title LIKE ?)".into());
            params.push(Box::new(p.clone()));
            params.push(Box::new(p.clone()));
            params.push(Box::new(p));
        }
        if let Some(s) = status.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            wheres.push("status = ?".into());
            params.push(Box::new(s.to_owned()));
        }
        if let Some(c) = classification.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            wheres.push("classification = ?".into());
            params.push(Box::new(c.to_owned()));
        }
        if let Some(d) = start_date.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            wheres.push("created_at >= ?".into());
            params.push(Box::new(d.to_owned()));
        }
        if let Some(d) = end_date.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            wheres.push("created_at <= ?".into());
            params.push(Box::new(d.to_owned()));
        }

        let where_clause = if wheres.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", wheres.join(" AND "))
        };

        let sql = format!(
            "SELECT id, session_id, input_text, title, status, classification, created_at, updated_at, transcript, react_state \
             FROM tasks {where_clause} ORDER BY created_at DESC LIMIT ? OFFSET ?"
        );

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;

        let mut param_refs: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
        for p in &params {
            param_refs.push(p.as_ref());
        }
        let limit_param: i64 = limit;
        let offset_param: i64 = offset;
        param_refs.push(&limit_param);
        param_refs.push(&offset_param);

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(Task {
                id: row.get(0)?,
                session_id: row.get(1)?,
                input_text: row.get(2)?,
                title: row.get(3)?,
                status: row.get(4)?,
                classification: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                transcript: row.get(8)?,
                react_state: row.get(9)?,
            })
        })?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    /// Save serialized ReAct state (canonical messages + history) for pause/resume.
    pub fn save_react_state(&self, task_id: &str, state_json: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET react_state = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![state_json, now, task_id],
        )?;
        Ok(())
    }

    /// Load serialized ReAct state for a paused task.
    pub fn get_react_state(&self, task_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT react_state FROM tasks WHERE id = ?1",
            rusqlite::params![task_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_task() {
        let db = create_db();
        let task = db
            .create_task(None, "input text", "NEW_TASK", "transcript")
            .unwrap();
        assert!(!task.id.is_empty());
        assert_eq!(task.session_id.as_deref(), None);
        assert_eq!(task.input_text, "input text");
        assert_eq!(task.title, None);
        assert_eq!(task.status, "pending");
        assert_eq!(task.classification, "NEW_TASK");
        assert!(!task.created_at.is_empty());
        assert!(!task.updated_at.is_empty());
        assert_eq!(task.transcript, "transcript");
        assert!(task.react_state.is_none());
    }

    #[test]
    fn test_create_task_no_session() {
        let db = create_db();
        let task = db
            .create_task(None, "input", "NEW_TASK", "")
            .unwrap();
        assert!(task.session_id.is_none());
    }

    #[test]
    fn test_get_task_found() {
        let db = create_db();
        let created = db
            .create_task(None, "input", "NEW_TASK", "")
            .unwrap();
        let found = db.get_task(&created.id).unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.input_text, "input");
    }

    #[test]
    fn test_get_task_not_found() {
        let db = create_db();
        let result = db.get_task("non-existent-id").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_task_status() {
        let db = create_db();
        let task = db
            .create_task(None, "input", "NEW_TASK", "")
            .unwrap();
        db.update_task_status(&task.id, "running").unwrap();
        let updated = db.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.status, "running");
    }

    #[test]
    fn test_list_tasks_default() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();
        db.create_task(None, "c", "NEW_TASK", "").unwrap();

        let tasks = db.list_tasks(50, 0).unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn test_list_tasks_limit_offset() {
        let db = create_db();
        for i in 0..5 {
            db.create_task(None, &format!("task-{}", i), "NEW_TASK", "")
                .unwrap();
        }
        let tasks = db.list_tasks(2, 0).unwrap();
        assert_eq!(tasks.len(), 2);

        let tasks = db.list_tasks(2, 2).unwrap();
        assert_eq!(tasks.len(), 2);

        let tasks = db.list_tasks(10, 5).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_list_tasks_caching() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        let first = db.list_tasks(50, 0).unwrap();
        assert_eq!(first.len(), 1);

        db.create_task(None, "b", "NEW_TASK", "").unwrap();
        let second = db.list_tasks(50, 0).unwrap();
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn test_search_tasks() {
        let db = create_db();
        db.create_task(None, "rust compiler", "NEW_TASK", "")
            .unwrap();
        db.create_task(None, "python script", "NEW_TASK", "")
            .unwrap();
        db.create_task(None, "rust debugging", "NEW_TASK", "")
            .unwrap();

        let results = db.search_tasks("rust").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.search_tasks("python").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_tasks("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_tasks_in_transcript() {
        let db = create_db();
        db.create_task(None, "task", "NEW_TASK", "transcript about rust")
            .unwrap();
        let results = db.search_tasks("rust").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_tasks_paginated() {
        let db = create_db();
        for i in 0..5 {
            db.create_task(None, &format!("rust task {}", i), "NEW_TASK", "")
                .unwrap();
        }

        let page1 = db.search_tasks_paginated("rust", 2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = db.search_tasks_paginated("rust", 2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = db.search_tasks_paginated("rust", 2, 4).unwrap();
        assert_eq!(page3.len(), 1);

        let empty = db.search_tasks_paginated("rust", 2, 10).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_count_tasks() {
        let db = create_db();
        assert_eq!(db.count_tasks().unwrap(), 0);
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();
        assert_eq!(db.count_tasks().unwrap(), 2);
    }

    #[test]
    fn test_count_tasks_search() {
        let db = create_db();
        db.create_task(None, "hello world", "NEW_TASK", "")
            .unwrap();
        db.create_task(None, "goodbye", "NEW_TASK", "").unwrap();
        assert_eq!(db.count_tasks_search("hello").unwrap(), 1);
        assert_eq!(db.count_tasks_search("good").unwrap(), 1);
        assert_eq!(db.count_tasks_search("xyz").unwrap(), 0);
    }

    #[test]
    fn test_delete_task() {
        let db = create_db();
        let task = db
            .create_task(None, "input", "NEW_TASK", "")
            .unwrap();
        db.delete_task(&task.id).unwrap();
        assert!(db.get_task(&task.id).unwrap().is_none());
        assert_eq!(db.count_tasks().unwrap(), 0);
    }

    #[test]
    fn test_delete_task_nonexistent() {
        let db = create_db();
        let result = db.delete_task("non-existent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_clear_tasks() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();
        db.create_task(None, "c", "NEW_TASK", "").unwrap();

        let count = db.clear_tasks().unwrap();
        assert_eq!(count, 3);
        assert_eq!(db.count_tasks().unwrap(), 0);
    }

    #[test]
    fn test_clear_tasks_empty() {
        let db = create_db();
        let count = db.clear_tasks().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_finalize_stale_tasks() {
        let db = create_db();
        let task_a = db.create_task(None, "a", "NEW_TASK", "").unwrap();
        let task_b = db.create_task(None, "b", "NEW_TASK", "").unwrap();

        // stale_minutes=0: threshold = now. SQLite's datetime('now') is
        // slightly behind Rust's Utc::now(), so newly created tasks are
        // considered stale and finalized to 'error'.
        let count = db.finalize_stale_tasks(0).unwrap();
        assert_eq!(count, 2);

        let found = db.get_task(&task_a.id).unwrap().unwrap();
        assert_eq!(found.status, "error");
        let found = db.get_task(&task_b.id).unwrap().unwrap();
        assert_eq!(found.status, "error");
    }

    #[test]
    fn test_finalize_stale_tasks_leaves_completed() {
        let db = create_db();
        let task = db
            .create_task(None, "a", "NEW_TASK", "")
            .unwrap();
        db.update_task_status(&task.id, "completed").unwrap();

        let count = db.finalize_stale_tasks(0).unwrap();
        assert_eq!(count, 0);

        let found = db.get_task(&task.id).unwrap().unwrap();
        assert_eq!(found.status, "completed");
    }

    #[test]
    fn test_delete_old_tasks() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();

        let count = db.delete_old_tasks(0).unwrap();
        assert_eq!(count, 2);
        assert_eq!(db.count_tasks().unwrap(), 0);
    }

    #[test]
    fn test_delete_old_tasks_keeps_recent() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();

        let count = db.delete_old_tasks(365).unwrap();
        assert_eq!(count, 0);
        assert_eq!(db.count_tasks().unwrap(), 1);
    }

    #[test]
    fn test_search_tasks_filtered_query_only() {
        let db = create_db();
        db.create_task(None, "rust compile", "NEW_TASK", "")
            .unwrap();
        db.create_task(None, "python run", "NEW_TASK", "")
            .unwrap();

        let results = db
            .search_tasks_filtered(Some("rust"), None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_tasks_filtered_status() {
        let db = create_db();
        let t1 = db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();
        db.update_task_status(&t1.id, "completed").unwrap();

        let results = db
            .search_tasks_filtered(None, Some("completed"), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "completed");
    }

    #[test]
    fn test_search_tasks_filtered_classification() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "SUPPLEMENT", "").unwrap();
        db.create_task(None, "c", "SUPPLEMENT", "").unwrap();

        let results = db
            .search_tasks_filtered(None, None, Some("SUPPLEMENT"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_tasks_filtered_date_range() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();

        let results = db
            .search_tasks_filtered(None, None, None, Some("2000-01-01"), Some("2099-12-31"), 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = db
            .search_tasks_filtered(None, None, None, Some("2099-01-01"), None, 50, 0)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_tasks_filtered_combined() {
        let db = create_db();
        let t1 = db
            .create_task(None, "rust compiler bug", "NEW_TASK", "")
            .unwrap();
        db.create_task(None, "python script", "NEW_TASK", "")
            .unwrap();
        db.update_task_status(&t1.id, "completed").unwrap();

        let results = db
            .search_tasks_filtered(
                Some("rust"),
                Some("completed"),
                Some("NEW_TASK"),
                None,
                None,
                50,
                0,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, t1.id);
    }

    #[test]
    fn test_search_tasks_filtered_no_filters() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();
        db.create_task(None, "b", "NEW_TASK", "").unwrap();

        let results = db
            .search_tasks_filtered(None, None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_tasks_filtered_empty_query_ignored() {
        let db = create_db();
        db.create_task(None, "a", "NEW_TASK", "").unwrap();

        let results = db
            .search_tasks_filtered(Some(""), None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_save_and_get_react_state() {
        let db = create_db();
        let task = db
            .create_task(None, "input", "NEW_TASK", "")
            .unwrap();

        let result = db.get_react_state(&task.id).unwrap();
        assert!(result.is_none());

        let state = r#"{"step":0,"messages":[]}"#;
        db.save_react_state(&task.id, state).unwrap();

        let loaded = db.get_react_state(&task.id).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap(), state);
    }

    #[test]
    fn test_save_react_state_overwrites() {
        let db = create_db();
        let task = db
            .create_task(None, "input", "NEW_TASK", "")
            .unwrap();

        db.save_react_state(&task.id, r#"{"v":1}"#).unwrap();
        db.save_react_state(&task.id, r#"{"v":2}"#).unwrap();

        let loaded = db.get_react_state(&task.id).unwrap().unwrap();
        assert_eq!(loaded, r#"{"v":2}"#);
    }
}
