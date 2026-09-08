use crate::db::Database;
use crate::repositories::messages::now_rfc3339_millis;
use rusqlite::OptionalExtension;

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
    #[serde(default)]
    pub cache_miss_tokens: u32,
    #[serde(default)]
    pub context_tokens: u32,
    #[serde(default)]
    pub context_window: Option<u32>,
    pub cost_usd: f64,
    pub has_cost: bool,
}

impl SessionUsage {
    /// Providers sometimes omit `total_tokens`. Session aggregates cannot
    /// encode a single provider mode, so only a recorded total is authoritative.
    pub fn coalesce_total(&mut self) {
        self.total_tokens = coalesced_total(
            self.prompt_tokens,
            self.completion_tokens,
            self.total_tokens,
            self.cached_tokens,
            self.cache_creation_tokens,
            "unknown",
        );
    }
}

fn coalesced_total(
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cached_tokens: u32,
    cache_creation_tokens: u32,
    cache_accounting: &str,
) -> u32 {
    if total_tokens != 0 {
        return total_tokens;
    }
    let extra = if cache_accounting == "exclusive" {
        cached_tokens.saturating_add(cache_creation_tokens)
    } else {
        0
    };
    prompt_tokens
        .saturating_add(completion_tokens)
        .saturating_add(extra)
}

impl Database {
    /// Load the persisted cumulative counters for a session, if any.
    pub fn get_session_usage(&self, session_id: &str) -> anyhow::Result<Option<SessionUsage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT prompt_tokens, completion_tokens, total_tokens,
                    cached_tokens, cache_creation_tokens, cache_miss_tokens,
                    context_tokens, context_window, cost_usd, has_cost
             FROM session_usage WHERE session_id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(SessionUsage {
                prompt_tokens: row.get(0)?,
                completion_tokens: row.get(1)?,
                total_tokens: row.get(2)?,
                cached_tokens: row.get(3)?,
                cache_creation_tokens: row.get(4)?,
                cache_miss_tokens: row.get(5)?,
                context_tokens: row.get(6)?,
                context_window: row.get(7)?,
                cost_usd: row.get(8)?,
                has_cost: row.get::<_, i32>(9)? != 0,
            })
        })?;
        match rows.next() {
            Some(row) => {
                let mut usage = row?;
                usage.coalesce_total();
                Ok(Some(usage))
            }
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
    #[serde(default)]
    pub cache_miss_tokens: u32,
    /// `inclusive`, `exclusive`, or `unknown` for legacy rows. Only per-call
    /// rows can safely express this when a session switches providers.
    #[serde(default)]
    pub cache_accounting: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_diagnostics: Option<serde_json::Value>,
    /// Tokens occupying the provider context window for this call.
    #[serde(default)]
    pub context_tokens: u32,
    /// Configured provider context window for this call, when known.
    #[serde(default)]
    pub context_window: Option<u32>,
    pub cost_usd: f64,
    pub has_cost: bool,
    /// Wall-clock duration of the LLM call in milliseconds.
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

impl LlmCallUsage {
    pub fn coalesce_total(&mut self) {
        self.total_tokens = coalesced_total(
            self.prompt_tokens,
            self.completion_tokens,
            self.total_tokens,
            self.cached_tokens,
            self.cache_creation_tokens,
            &self.cache_accounting,
        );
    }
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
        self.record_llm_call_usage_with_cache_accounting(
            session_id,
            step_number,
            role,
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            0,
            "unknown",
            None,
            cost_usd,
            has_cost,
            duration_ms,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn record_llm_call_usage_with_cache_accounting(
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
        cache_miss_tokens: u32,
        cache_accounting: &str,
        cache_diagnostics: Option<&str>,
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
            cache_miss_tokens,
            cache_accounting,
            cache_diagnostics,
            0,
            None,
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
            cache_miss_tokens,
            cache_accounting: cache_accounting.into(),
            cache_diagnostics: cache_diagnostics.and_then(|value| serde_json::from_str(value).ok()),
            context_tokens: 0,
            context_window: None,
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
        self.persist_llm_call_and_refresh_session_usage_with_cache_accounting(
            session_id,
            step_number,
            role,
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            0,
            "unknown",
            None,
            cost_usd,
            has_cost,
            duration_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn persist_llm_call_and_refresh_session_usage_with_cache_accounting(
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
        cache_miss_tokens: u32,
        cache_accounting: &str,
        cache_diagnostics: Option<&str>,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
    ) -> anyhow::Result<LlmCallUsage> {
        self.persist_llm_call_and_refresh_session_usage_with_cache_accounting_and_context(
            session_id,
            step_number,
            role,
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            cache_miss_tokens,
            cache_accounting,
            cache_diagnostics,
            cost_usd,
            has_cost,
            duration_ms,
            0,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn persist_llm_call_and_refresh_session_usage_with_cache_accounting_and_context(
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
        cache_miss_tokens: u32,
        cache_accounting: &str,
        cache_diagnostics: Option<&str>,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
        context_tokens: u32,
        context_window: Option<u32>,
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
                cache_miss_tokens,
                cache_accounting,
                cache_diagnostics,
                context_tokens,
                context_window,
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
                cache_miss_tokens,
                cache_accounting: cache_accounting.into(),
                cache_diagnostics: cache_diagnostics
                    .and_then(|value| serde_json::from_str(value).ok()),
                context_tokens,
                context_window,
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
        cache_miss_tokens: u32,
        cache_accounting: &str,
        cache_diagnostics: Option<&str>,
        context_tokens: u32,
        context_window: Option<u32>,
        cost_usd: f64,
        has_cost: bool,
        duration_ms: Option<u64>,
        created_at: &str,
    ) -> anyhow::Result<()> {
        let total_tokens = coalesced_total(
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            cache_accounting,
        );
        conn.execute(
            "INSERT INTO llm_usage
                 (id, session_id, step_number, role, model, prompt_tokens, completion_tokens,
                   total_tokens, cached_tokens, cache_creation_tokens, cache_miss_tokens, cache_accounting,
                   cache_diagnostics, context_tokens, context_window, cost_usd, has_cost, duration_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
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
                cache_miss_tokens,
                cache_accounting,
                cache_diagnostics,
                context_tokens,
                context_window,
                cost_usd,
                has_cost,
                duration_ms,
                created_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn rebuild_session_usage_from_calls_conn(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let (prompt, completion, total, cached, creation, miss, cost, has_cost): (
            i64,
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
                    COALESCE(SUM(CASE WHEN total_tokens > 0 THEN total_tokens ELSE prompt_tokens + completion_tokens + CASE WHEN cache_accounting = 'exclusive' THEN cached_tokens + cache_creation_tokens ELSE 0 END END), 0),
                    COALESCE(SUM(cached_tokens), 0),
                    COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(cache_miss_tokens), 0),
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
                    row.get(7)?,
                ))
            },
        )?;
        let (context_tokens, context_window): (u32, Option<u32>) = conn
            .query_row(
                "SELECT context_tokens, context_window
                   FROM llm_usage
                  WHERE session_id = ?1
                  ORDER BY created_at DESC, rowid DESC
                  LIMIT 1",
                rusqlite::params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, None));
        // Always upsert — including zeros when no detail remains — so resume
        // does not treat a cleared row as "predates persistence" and fall
        // back to estimate_session_usage.
        conn.execute(
            "INSERT INTO session_usage
                 (session_id, prompt_tokens, completion_tokens, total_tokens,
                   cached_tokens, cache_creation_tokens, cache_miss_tokens,
                   context_tokens, context_window, cost_usd, has_cost, updated_at)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, datetime('now'))
             ON CONFLICT(session_id) DO UPDATE SET
                 prompt_tokens = excluded.prompt_tokens,
                 completion_tokens = excluded.completion_tokens,
                 total_tokens = excluded.total_tokens,
                 cached_tokens = excluded.cached_tokens,
                 cache_creation_tokens = excluded.cache_creation_tokens,
                 cache_miss_tokens = excluded.cache_miss_tokens,
                 context_tokens = excluded.context_tokens,
                 context_window = excluded.context_window,
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
                miss as u32,
                context_tokens,
                context_window,
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
                    total_tokens, cached_tokens, cache_creation_tokens, cache_miss_tokens, cache_accounting,
                    cache_diagnostics, context_tokens, context_window, cost_usd, has_cost, duration_ms, created_at
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
                cache_miss_tokens: row.get(10)?,
                cache_accounting: row.get(11)?,
                cache_diagnostics: row
                    .get::<_, Option<String>>(12)?
                    .and_then(|value| serde_json::from_str(&value).ok()),
                context_tokens: row.get(13)?,
                context_window: row.get(14)?,
                cost_usd: row.get(15)?,
                has_cost: row.get::<_, i32>(16)? != 0,
                duration_ms: row.get(17)?,
                created_at: row.get(18)?,
            })
        })?;
        let mut usage = Vec::new();
        for row in rows {
            let mut rec = row?;
            rec.coalesce_total();
            usage.push(rec);
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
    fn persist_and_restore_context_snapshot() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        let rec = db
            .persist_llm_call_and_refresh_session_usage_with_cache_accounting_and_context(
                &session.id,
                Some(1),
                "default_model",
                Some("model-a"),
                900,
                120,
                1020,
                300,
                0,
                600,
                "inclusive",
                None,
                0.0,
                false,
                Some(42),
                900,
                Some(4096),
            )
            .unwrap();

        assert_eq!(rec.context_tokens, 900);
        assert_eq!(rec.context_window, Some(4096));
        let summary = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(summary.context_tokens, 900);
        assert_eq!(summary.context_window, Some(4096));
        let calls = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(calls[0].context_tokens, 900);
        assert_eq!(calls[0].context_window, Some(4096));
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
    fn persist_keeps_cache_accounting_per_call_for_mixed_providers() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.persist_llm_call_and_refresh_session_usage_with_cache_accounting(
            &session.id,
            Some(1),
            "default_model",
            Some("gpt-test"),
            100,
            5,
            105,
            100,
            0,
            0,
            "inclusive",
            None,
            0.0,
            false,
            None,
        )
        .unwrap();
        db.persist_llm_call_and_refresh_session_usage_with_cache_accounting(
            &session.id,
            Some(2),
            "default_model",
            Some("claude-test"),
            100,
            5,
            505,
            400,
            0,
            0,
            "exclusive",
            None,
            0.0,
            false,
            None,
        )
        .unwrap();

        let calls = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].cache_accounting, "inclusive");
        assert_eq!(calls[1].cache_accounting, "exclusive");
        let total = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(total.prompt_tokens, 200);
        assert_eq!(total.cached_tokens, 500);
        assert_eq!(total.total_tokens, 610);
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
            db.get_session_usage(&session.id)
                .unwrap()
                .unwrap()
                .total_tokens,
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
    fn persist_coalesces_omitted_total_into_session_usage() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.persist_llm_call_and_refresh_session_usage(
            &session.id,
            Some(1),
            "default_model",
            None,
            10,
            5,
            0,
            0,
            0,
            0.0,
            false,
            None,
        )
        .unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.completion_tokens, 5);
        assert_eq!(u.total_tokens, 15);
        let calls = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(calls[0].total_tokens, 15);
    }

    #[test]
    fn persist_coalesces_exclusive_cache_into_omitted_total() {
        let db = test_db();
        let session = db.create_session("hello", "").unwrap();
        db.persist_llm_call_and_refresh_session_usage_with_cache_accounting(
            &session.id,
            Some(1),
            "default_model",
            None,
            100,
            20,
            0,
            400,
            50,
            0,
            "exclusive",
            None,
            0.0,
            false,
            None,
        )
        .unwrap();
        let u = db.get_session_usage(&session.id).unwrap().unwrap();
        assert_eq!(u.total_tokens, 570);
        let calls = db.get_session_llm_usage(&session.id).unwrap();
        assert_eq!(calls[0].total_tokens, 570);
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
