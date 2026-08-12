use crate::db::Database;

/// Per-session cumulative token/cost counters, persisted so a resumed or
/// reopened session can restore the token-stats display instead of resetting
/// to zero. Updated on every LLM usage emit; the row lives as long as the
/// session (ON DELETE CASCADE) and is removed with it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
    pub has_cost: bool,
}

impl Database {
    /// Upsert the cumulative token/cost counters for a session. Callers pass the
    /// full cumulative values (not per-step deltas) — the row is replaced.
    pub fn update_session_usage(
        &self,
        session_id: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
        has_cost: bool,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO session_usage
                 (session_id, prompt_tokens, completion_tokens, total_tokens, cost_usd, has_cost, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(session_id) DO UPDATE SET
                 prompt_tokens = excluded.prompt_tokens,
                 completion_tokens = excluded.completion_tokens,
                 total_tokens = excluded.total_tokens,
                 cost_usd = excluded.cost_usd,
                 has_cost = excluded.has_cost,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                session_id,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cost_usd,
                has_cost
            ],
        )?;
        Ok(())
    }

    /// Load the persisted cumulative counters for a session, if any.
    pub fn get_session_usage(&self, session_id: &str) -> anyhow::Result<Option<SessionUsage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT prompt_tokens, completion_tokens, total_tokens, cost_usd, has_cost
             FROM session_usage WHERE session_id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(SessionUsage {
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

/// One LLM API call's usage detail: the token counts, cost and model of a
/// single model response, tagged with the ReAct step it served. Rows are
/// append-only per call (unlike `session_usage`, which replaces cumulative
/// totals), so a session keeps a granular history of every call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmCallUsage {
    pub id: String,
    pub session_id: String,
    /// ReAct step number the call served (NULL when not attributable).
    pub step_number: Option<i32>,
    /// Endpoint role that produced the call (e.g. "default_model").
    pub role: String,
    pub model: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
    pub has_cost: bool,
    /// Wall-clock duration of the LLM call in milliseconds.
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

impl Database {
    /// Append one LLM-call usage row. Fire-and-forget friendly: the caller
    /// (the agent step loop) persists the detail the same way it persists
    /// the cumulative counters — on a blocking thread, errors ignored.
    /// `created_at` is stamped RFC3339 (like `messages.created_at`) so
    /// rollback's `truncate_session_after` can cut usage rows on the same
    /// timeline as messages and steps.
    #[allow(clippy::too_many_arguments)]
    pub fn record_llm_call_usage(
        &self,
        session_id: &str,
        step_number: Option<i32>,
        role: &str,
        model: Option<&str>,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
    ) -> anyhow::Result<LlmCallUsage> {
        let id = haven_common::types::new_id("usage");
        let created_at = chrono::Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO llm_usage
                 (id, session_id, step_number, role, model, prompt_tokens, completion_tokens,
                  total_tokens, cost_usd, has_cost, duration_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                id,
                session_id,
                step_number,
                role,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cost_usd,
                has_cost,
                duration_ms,
                created_at,
            ],
        )?;
        Ok(LlmCallUsage {
            id,
            session_id: session_id.into(),
            step_number,
            role: role.into(),
            model: model.map(String::from),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd,
            has_cost,
            duration_ms,
            created_at,
        })
    }

    /// All usage-detail rows for a session, oldest first. `session_usage` carries
    /// the running totals; this is the per-call history behind them.
    pub fn get_session_llm_usage(&self, session_id: &str) -> anyhow::Result<Vec<LlmCallUsage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, step_number, role, model, prompt_tokens, completion_tokens,
                    total_tokens, cost_usd, has_cost, duration_ms, created_at
             FROM llm_usage WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(LlmCallUsage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                step_number: row.get(2)?,
                role: row.get(3)?,
                model: row.get(4)?,
                prompt_tokens: row.get(5)?,
                completion_tokens: row.get(6)?,
                total_tokens: row.get(7)?,
                cost_usd: row.get(8)?,
                has_cost: row.get::<_, i32>(9)? != 0,
                duration_ms: row.get(10)?,
                created_at: row.get(11)?,
            })
        })?;
        let mut usage = Vec::new();
        for row in rows {
            usage.push(row?);
        }
        Ok(usage)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn update_and_get_session_usage_roundtrip() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        assert!(db.get_session_usage(&session.id).unwrap().is_none());
        db.update_session_usage(&session.id, 100, 50, 150, 0.25, true)
            .unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens, 150);
        assert_eq!(u.cost_usd, 0.25);
        assert!(u.has_cost);
    }

    #[test]
    fn update_replaces_cumulative_values() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.update_session_usage(&session.id, 10, 5, 15, 0.0, false)
            .unwrap();
        // Callers pass the full cumulative totals, so a later call replaces
        // (not accumulates) the stored row.
        db.update_session_usage(&session.id, 20, 10, 30, 0.5, true)
            .unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.total_tokens, 30);
        assert_eq!(u.prompt_tokens, 20);
        assert_eq!(u.cost_usd, 0.5);
        assert!(u.has_cost);
    }

    #[test]
    fn session_usage_cascades_on_session_delete() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.update_session_usage(&session.id, 10, 5, 15, 0.0, false)
            .unwrap();
        assert!(db.get_session_usage(&session.id).unwrap().is_some());
        db.delete_session(&session.id).unwrap();
        assert!(db.get_session_usage(&session.id).unwrap().is_none());
    }

    #[test]
    fn record_and_get_llm_call_usage() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        let rec = db
            .record_llm_call_usage(
                &session.id,
                Some(1),
                "default_model",
                Some("gpt-5"),
                100,
                50,
                150,
                0.25,
                true,
                Some(1234),
            )
            .unwrap();
        assert!(rec.id.starts_with("usage-"));
        assert_eq!(rec.step_number, Some(1));
        assert_eq!(rec.model.as_deref(), Some("gpt-5"));
        let usage = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].prompt_tokens, 100);
        assert_eq!(usage[0].completion_tokens, 50);
        assert_eq!(usage[0].total_tokens, 150);
        assert_eq!(usage[0].cost_usd, 0.25);
        assert!(usage[0].has_cost);
        assert_eq!(usage[0].duration_ms, Some(1234));
        assert_eq!(usage[0].role, "default_model");
    }

    #[test]
    fn llm_call_usage_is_append_only_and_ordered() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.record_llm_call_usage(
            &session.id,
            Some(1),
            "default_model",
            None,
            10,
            5,
            15,
            0.0,
            false,
            None,
        )
        .unwrap();
        db.record_llm_call_usage(
            &session.id,
            Some(2),
            "default_model",
            None,
            20,
            10,
            30,
            0.0,
            false,
            None,
        )
        .unwrap();
        // Unlike session_usage, rows accumulate (one per call) in created order.
        let usage = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].step_number, Some(1));
        assert_eq!(usage[1].step_number, Some(2));
        assert_eq!(usage[0].total_tokens, 15);
        assert_eq!(usage[1].total_tokens, 30);
    }

    #[test]
    fn llm_call_usage_unknown_session_is_empty() {
        let db = test_db();
        assert!(
            db.get_session_llm_usage("missing-session")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn llm_call_usage_cascades_on_session_delete() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.record_llm_call_usage(
            &session.id,
            Some(1),
            "default_model",
            None,
            10,
            5,
            15,
            0.0,
            false,
            None,
        )
        .unwrap();
        db.delete_session(&session.id).unwrap();
        assert!(db.get_session_llm_usage(&session.id).unwrap().is_empty());
    }
}
