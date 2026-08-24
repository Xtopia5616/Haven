use crate::db::Database;
use crate::repositories::messages::now_rfc3339_millis;

/// Per-session cumulative token/cost counters, persisted so a resumed or
/// reopened session can restore the token-stats display instead of resetting
/// to zero. Updated on every LLM usage emit; the row lives as long as the
/// session (ON DELETE CASCADE) and is removed with it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub cache_creation_tokens: u32,
    pub cost_usd: f64,
    pub has_cost: bool,
}

impl Database {
    /// Load the persisted cumulative counters for a session, if any.
    pub fn get_session_usage(&self, session_id: &str) -> anyhow::Result<Option<SessionUsage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT prompt_tokens, completion_tokens, total_tokens,
                    cached_tokens, cache_creation_tokens, cost_usd, has_cost
             FROM session_usage WHERE session_id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(SessionUsage {
                prompt_tokens: row.get(0)?,
                completion_tokens: row.get(1)?,
                total_tokens: row.get(2)?,
                cached_tokens: row.get(3)?,
                cache_creation_tokens: row.get(4)?,
                cost_usd: row.get(5)?,
                has_cost: row.get::<_, i32>(6)? != 0,
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
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub cache_creation_tokens: u32,
    pub cost_usd: f64,
    pub has_cost: bool,
    /// Wall-clock duration of the LLM call in milliseconds.
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

impl Database {
    /// Test-only insert without refreshing `session_usage`. Live path must use
    /// [`Self::persist_llm_call_and_refresh_session_usage`].
    #[cfg(test)]
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
        cached_tokens: u32,
        cache_creation_tokens: u32,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
    ) -> anyhow::Result<LlmCallUsage> {
        let id = haven_common::types::new_id("usage");
        let created_at = now_rfc3339_millis();
        let conn = self.conn();
        Self::insert_llm_call_usage_conn(
            &conn,
            &id,
            session_id,
            step_number,
            role,
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            cost_usd,
            has_cost,
            duration_ms,
            &created_at,
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
            cached_tokens,
            cache_creation_tokens,
            cost_usd,
            has_cost,
            duration_ms,
            created_at,
        })
    }

    /// Insert one call-detail row and rebuild `session_usage` from the
    /// remaining `llm_usage` rows in a single transaction. Using the SUM of
    /// detail rows (instead of an absolute in-memory cumulative write) keeps
    /// totals correct when fire-and-forget persists complete out of order.
    ///
    /// `created_at` uses [`now_rfc3339_millis`] (same shape as messages/steps)
    /// so string cutoffs in rollback/`truncate_session_after` compare correctly.
    #[allow(clippy::too_many_arguments)]
    pub fn persist_llm_call_and_refresh_session_usage(
        &self,
        session_id: &str,
        step_number: Option<i32>,
        role: &str,
        model: Option<&str>,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
        cache_creation_tokens: u32,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
    ) -> anyhow::Result<LlmCallUsage> {
        let id = haven_common::types::new_id("usage");
        let created_at = now_rfc3339_millis();
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<LlmCallUsage> {
            Self::insert_llm_call_usage_conn(
                &conn,
                &id,
                session_id,
                step_number,
                role,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_tokens,
                cache_creation_tokens,
                cost_usd,
                has_cost,
                duration_ms,
                &created_at,
            )?;
            Self::rebuild_session_usage_from_calls_conn(&conn, session_id)?;
            Ok(LlmCallUsage {
                id: id.clone(),
                session_id: session_id.into(),
                step_number,
                role: role.into(),
                model: model.map(String::from),
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_tokens,
                cache_creation_tokens,
                cost_usd,
                has_cost,
                duration_ms,
                created_at,
            })
        })();
        match result {
            Ok(rec) => {
                conn.execute_batch("COMMIT")?;
                Ok(rec)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Recompute `session_usage` as the SUM of remaining `llm_usage` rows
    /// (zeros when none remain). Called after rollback/truncate so discarded
    /// steps do not leave inflated totals, and resume does not fall back to
    /// estimate_session_usage.
    pub fn rebuild_session_usage_from_calls(&self, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        Self::rebuild_session_usage_from_calls_conn(&conn, session_id)
    }

    /// Delete `llm_usage` rows at-or-after `ts` (inclusive), matching
    /// user-message rollback's `delete_messages_from` cutoff.
    pub fn delete_llm_usage_from(&self, session_id: &str, created_at: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM llm_usage WHERE session_id = ?1 AND created_at >= ?2",
            rusqlite::params![session_id, created_at],
        )?;
        Ok(())
    }

    /// Remove one detail row by id (used when a detached persist lands after
    /// its session epoch was invalidated by rollback).
    pub fn delete_llm_usage_by_id(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM llm_usage WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_llm_call_usage_conn(
        conn: &rusqlite::Connection,
        id: &str,
        session_id: &str,
        step_number: Option<i32>,
        role: &str,
        model: Option<&str>,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
        cache_creation_tokens: u32,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
        created_at: &str,
    ) -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO llm_usage
                 (id, session_id, step_number, role, model, prompt_tokens, completion_tokens,
                  total_tokens, cached_tokens, cache_creation_tokens, cost_usd, has_cost,
                  duration_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                id,
                session_id,
                step_number,
                role,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_tokens,
                cache_creation_tokens,
                cost_usd,
                has_cost,
                duration_ms,
                created_at,
            ],
        )?;
        Ok(())
    }

    fn rebuild_session_usage_from_calls_conn(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let (prompt, completion, total, cached, creation, cost, has_cost): (
            i64,
            i64,
            i64,
            i64,
            i64,
            f64,
            i64,
        ) = conn.query_row(
            "SELECT COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(total_tokens), 0),
                    COALESCE(SUM(cached_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(CASE WHEN has_cost != 0 THEN cost_usd ELSE 0 END), 0),
                    COALESCE(MAX(CASE WHEN has_cost != 0 THEN 1 ELSE 0 END), 0)
             FROM llm_usage WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        // Always upsert — including zeros when no detail remains — so resume
        // does not treat a cleared row as "predates persistence" and fall
        // back to estimate_session_usage.
        conn.execute(
            "INSERT INTO session_usage
                 (session_id, prompt_tokens, completion_tokens, total_tokens,
                  cached_tokens, cache_creation_tokens, cost_usd, has_cost, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
             ON CONFLICT(session_id) DO UPDATE SET
                 prompt_tokens = excluded.prompt_tokens,
                 completion_tokens = excluded.completion_tokens,
                 total_tokens = excluded.total_tokens,
                 cached_tokens = excluded.cached_tokens,
                 cache_creation_tokens = excluded.cache_creation_tokens,
                 cost_usd = excluded.cost_usd,
                 has_cost = excluded.has_cost,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                session_id,
                prompt as u32,
                completion as u32,
                total as u32,
                cached as u32,
                creation as u32,
                cost,
                has_cost != 0
            ],
        )?;
        Ok(())
    }

    /// All usage-detail rows for a session, oldest first. `session_usage` carries
    /// the running totals; this is the per-call history behind them.
    pub fn get_session_llm_usage(&self, session_id: &str) -> anyhow::Result<Vec<LlmCallUsage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, step_number, role, model, prompt_tokens, completion_tokens,
                    total_tokens, cached_tokens, cache_creation_tokens, cost_usd, has_cost,
                    duration_ms, created_at
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
                cached_tokens: row.get(8)?,
                cache_creation_tokens: row.get(9)?,
                cost_usd: row.get(10)?,
                has_cost: row.get::<_, i32>(11)? != 0,
                duration_ms: row.get(12)?,
                created_at: row.get(13)?,
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
    fn persist_and_get_session_usage_roundtrip() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        assert!(db.get_session_usage(&session.id).unwrap().is_none());
        db.persist_llm_call_and_refresh_session_usage(
            &session.id,
            Some(1),
            "default_model",
            None,
            100,
            50,
            150,
            40,
            5,
            0.25,
            true,
            None,
        )
        .unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens, 150);
        assert_eq!(u.cached_tokens, 40);
        assert_eq!(u.cache_creation_tokens, 5);
        assert_eq!(u.cost_usd, 0.25);
        assert!(u.has_cost);
    }

    #[test]
    fn session_usage_cascades_on_session_delete() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.persist_llm_call_and_refresh_session_usage(
            &session.id,
            Some(1),
            "default_model",
            None,
            10,
            5,
            15,
            0,
            0,
            0.0,
            false,
            None,
        )
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
                40,
                0,
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
        assert_eq!(usage[0].cached_tokens, 40);
        assert_eq!(usage[0].cache_creation_tokens, 0);
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
            0,
            0,
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
            0,
            0,
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
            0,
            0,
            0.0,
            false,
            None,
        )
        .unwrap();
        db.delete_session(&session.id).unwrap();
        assert!(db.get_session_llm_usage(&session.id).unwrap().is_empty());
    }

    #[test]
    fn persist_llm_call_refreshes_session_usage_from_sum() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.persist_llm_call_and_refresh_session_usage(
            &session.id,
            Some(1),
            "default_model",
            Some("gpt-5"),
            100,
            50,
            150,
            40,
            0,
            0.25,
            true,
            Some(10),
        )
        .unwrap();
        db.persist_llm_call_and_refresh_session_usage(
            &session.id,
            Some(2),
            "default_model",
            Some("gpt-5"),
            20,
            10,
            30,
            5,
            1,
            0.05,
            true,
            Some(5),
        )
        .unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.prompt_tokens, 120);
        assert_eq!(u.completion_tokens, 60);
        assert_eq!(u.total_tokens, 180);
        assert_eq!(u.cached_tokens, 45);
        assert_eq!(u.cache_creation_tokens, 1);
        assert!((u.cost_usd - 0.30).abs() < 1e-9);
        assert!(u.has_cost);
    }

    #[test]
    fn rebuild_session_usage_zeros_row_when_no_calls_remain() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.persist_llm_call_and_refresh_session_usage(
            &session.id,
            Some(1),
            "default_model",
            None,
            10,
            5,
            15,
            0,
            0,
            0.0,
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            db.get_session_usage(&session.id).unwrap().unwrap().total_tokens,
            15
        );
        let cutoff = "1970-01-01T00:00:00.000Z";
        db.delete_llm_usage_from(&session.id, cutoff).unwrap();
        db.rebuild_session_usage_from_calls(&session.id).unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.total_tokens, 0);
        assert_eq!(u.cached_tokens, 0);
        assert_eq!(u.cache_creation_tokens, 0);
        assert_eq!(u.cost_usd, 0.0);
        assert!(!u.has_cost);
    }

    #[test]
    fn persist_stamps_created_at_like_messages() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        let rec = db
            .persist_llm_call_and_refresh_session_usage(
                &session.id,
                Some(1),
                "default_model",
                None,
                1,
                1,
                2,
                0,
                0,
                0.0,
                false,
                None,
            )
            .unwrap();
        assert!(
            rec.created_at.ends_with('Z'),
            "expected Millis+Z stamp, got {}",
            rec.created_at
        );
        assert!(
            rec.created_at.contains('.'),
            "expected fractional millis, got {}",
            rec.created_at
        );
    }

    #[test]
    fn delete_llm_usage_by_id_removes_one_row() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        let a = db
            .record_llm_call_usage(
                &session.id,
                Some(1),
                "default_model",
                None,
                10,
                5,
                15,
                0,
                0,
                0.0,
                false,
                None,
            )
            .unwrap();
        let b = db
            .record_llm_call_usage(
                &session.id,
                Some(2),
                "default_model",
                None,
                20,
                10,
                30,
                0,
                0,
                0.0,
                false,
                None,
            )
            .unwrap();
        db.delete_llm_usage_by_id(&a.id).unwrap();
        let usage = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].id, b.id);
    }
}
