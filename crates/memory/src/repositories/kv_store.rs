use crate::db::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;

pub const MAX_MEMORY_OUTBOX_PAGE_SIZE: usize = 64;
pub const MAX_MEMORY_SESSION_ID_PAGE_SIZE: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactExtractionMarkerState {
    pub event_sequence: i64,
    pub bypass_throttle: bool,
    pub attempt: u32,
    pub next_attempt_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactExtractionMarker {
    pub key: String,
    pub value: String,
    pub session_id: String,
    pub state: Result<FactExtractionMarkerState, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryExtractionMarkerState {
    pub attempt: u32,
    pub next_attempt_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryExtractionMarker {
    pub key: String,
    pub value: String,
    pub session_id: String,
    pub episode_id: String,
    pub state: Result<SummaryExtractionMarkerState, String>,
}

fn encode_fact_extraction_marker(state: &FactExtractionMarkerState) -> String {
    format!(
        "{}:{}:{}:{}",
        state.event_sequence,
        u8::from(state.bypass_throttle),
        state.attempt,
        state.next_attempt_at_ms
    )
}

fn decode_fact_extraction_marker(value: &str) -> anyhow::Result<FactExtractionMarkerState> {
    let parts = value.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        ["0"] => Ok(FactExtractionMarkerState {
            event_sequence: 0,
            bypass_throttle: false,
            attempt: 0,
            next_attempt_at_ms: 0,
        }),
        ["1"] => Ok(FactExtractionMarkerState {
            event_sequence: 0,
            bypass_throttle: true,
            attempt: 0,
            next_attempt_at_ms: 0,
        }),
        [sequence, bypass] => decode_fact_marker_parts(sequence, bypass, "0", "0"),
        [sequence, bypass, attempt, due] => {
            decode_fact_marker_parts(sequence, bypass, attempt, due)
        }
        _ => anyhow::bail!("invalid fact extraction marker"),
    }
}

fn decode_fact_marker_parts(
    sequence: &str,
    bypass: &str,
    attempt: &str,
    due: &str,
) -> anyhow::Result<FactExtractionMarkerState> {
    let event_sequence = sequence.parse::<i64>()?;
    anyhow::ensure!(
        event_sequence >= 0,
        "invalid fact extraction marker sequence"
    );
    let bypass_throttle = match bypass {
        "0" => false,
        "1" => true,
        _ => anyhow::bail!("invalid fact extraction marker bypass flag"),
    };
    let attempt = attempt.parse::<u32>()?;
    let next_attempt_at_ms = due.parse::<i64>()?;
    anyhow::ensure!(
        next_attempt_at_ms >= 0,
        "invalid fact extraction retry deadline"
    );
    Ok(FactExtractionMarkerState {
        event_sequence,
        bypass_throttle,
        attempt,
        next_attempt_at_ms,
    })
}

fn encode_summary_extraction_marker(
    session_id: &str,
    attempt: u32,
    next_attempt_at_ms: i64,
) -> String {
    format!("{session_id}:{attempt}:{next_attempt_at_ms}")
}

fn decode_summary_extraction_marker(value: &str) -> anyhow::Result<(String, u32, i64)> {
    if let Some((session_id, retry)) = value.rsplit_once(':')
        && let Some((session_id, attempt)) = session_id.rsplit_once(':')
    {
        let attempt = attempt.parse::<u32>()?;
        let next_attempt_at_ms = retry.parse::<i64>()?;
        anyhow::ensure!(
            !session_id.trim().is_empty() && next_attempt_at_ms >= 0,
            "invalid summary extraction marker"
        );
        return Ok((session_id.to_owned(), attempt, next_attempt_at_ms));
    }
    anyhow::ensure!(
        !value.trim().is_empty(),
        "invalid summary extraction marker"
    );
    Ok((value.to_owned(), 0, 0))
}

fn move_marker_to_quarantine_on(
    conn: &rusqlite::Connection,
    key: &str,
    quarantine_key: &str,
    expected_value: &str,
) -> anyhow::Result<bool> {
    let current: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_store WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .optional()?;
    if current.as_deref() != Some(expected_value) {
        return Ok(false);
    }
    // Retain only the newest malformed value for each logical job. Renaming
    // the row preserves the exact raw value without copying it into memory.
    conn.execute(
        "DELETE FROM kv_store WHERE key = ?1",
        rusqlite::params![quarantine_key],
    )?;
    let changed = conn.execute(
        "UPDATE kv_store SET key = ?2 WHERE key = ?1 AND value = ?3",
        rusqlite::params![key, quarantine_key, expected_value],
    )?;
    Ok(changed > 0)
}

/// Internal key-value store for agent bookkeeping that is not user memory.
///
/// User-facing preferences live in the `facts` table (tag `preference`);
/// this table holds only internal state such as the fact-extraction cursor
/// (`fact_extraction.<session_id>`), the durable extraction outbox
/// (`fact_extraction_pending.<session_id>` and
/// `fact_extraction_episode_pending.<session_id>.<episode_id>`), per-episode
/// completion markers (`fact_extraction_episode_done.<session_id>.<episode_id>`),
/// the committed-event cursor (`memory_event_cursor.<session_id>`), and the
/// runtime's scheduled-confirmation execution claim
/// (`scheduled_execution_claim.<tool_run_id>`). Exposed as `kv_store` in the
/// schema.
impl Database {
    /// Baseline every currently persisted session that lacks a memory event
    /// cursor in one SQLite statement snapshot. The caller subscribes to live
    /// events after this operation and then replays durable session events.
    pub fn baseline_missing_memory_event_cursors_to_latest(&self) -> anyhow::Result<usize> {
        let now = Utc::now().to_rfc3339();
        Ok(self.conn().execute(
            "INSERT OR IGNORE INTO kv_store (key, value, updated_at)
             SELECT 'memory_event_cursor.' || s.id,
                    CAST(COALESCE(MAX(e.sequence), 0) AS TEXT),
                    ?1
             FROM sessions s
             LEFT JOIN session_events e ON e.session_id = s.id
             WHERE NOT EXISTS (
                 SELECT 1 FROM kv_store cursor
                 WHERE cursor.key = 'memory_event_cursor.' || s.id
             )
             GROUP BY s.id",
            rusqlite::params![now],
        )?)
    }

    pub fn set_kv(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO kv_store (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![key, value, now],
        )?;
        Ok(())
    }

    pub fn get_kv(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT value FROM kv_store WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Read the last committed session event consumed by the memory runtime.
    /// An absent key is the initial event sequence zero. This clock is separate
    /// from the fact-extraction message-id cursor.
    pub fn memory_event_cursor(&self, session_id: &str) -> anyhow::Result<i64> {
        Ok(self.memory_event_cursor_optional(session_id)?.unwrap_or(0))
    }

    /// Read the memory event cursor while preserving whether its key exists.
    /// An explicit zero is meaningful during startup baseline selection.
    pub fn memory_event_cursor_optional(&self, session_id: &str) -> anyhow::Result<Option<i64>> {
        let key = memory_event_cursor_key(session_id)?;
        let value = self.get_kv(&key)?;
        value
            .as_deref()
            .map(|value| parse_memory_event_cursor(Some(value)))
            .transpose()
    }

    /// Initialize the memory event cursor only when no durable key exists.
    /// Returns true when this call installed `sequence`; an existing zero is
    /// preserved just like any other checkpoint.
    pub fn initialize_memory_event_cursor_if_absent(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(sequence >= 0, "memory event cursor cannot be negative");
        let key = memory_event_cursor_key(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO kv_store (key, value, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![key, sequence.to_string(), now],
        )?;
        if inserted == 0 {
            // Validate an existing value instead of treating a malformed key
            // as a successful no-op.
            let stored: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get(0),
                )
                .optional()?;
            parse_memory_event_cursor(stored.as_deref())?;
        }
        Ok(inserted != 0)
    }

    /// Persist a memory event checkpoint without allowing it to move
    /// backwards. An equal checkpoint is a no-op; writing zero to a missing
    /// key materializes the initial checkpoint before the first event.
    pub fn checkpoint_memory_event_cursor(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(sequence >= 0, "memory event cursor cannot be negative");
        let key = memory_event_cursor_key(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            let stored_value: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get(0),
                )
                .optional()?;
            let current = parse_memory_event_cursor(stored_value.as_deref())?;
            anyhow::ensure!(
                sequence >= current,
                "memory event cursor cannot move backwards from {current} to {sequence}"
            );
            if stored_value.is_none() || sequence > current {
                conn.execute(
                    "INSERT INTO kv_store (key, value, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                         updated_at = excluded.updated_at",
                    rusqlite::params![key, sequence.to_string(), now],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT").map_err(Into::into),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Remove one session's memory event checkpoint.
    pub fn clear_memory_event_cursor(&self, session_id: &str) -> anyhow::Result<()> {
        let key = memory_event_cursor_key(session_id)?;
        self.conn().execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    /// Coalesce one committed memory trigger into durable internal state.
    /// The event sequence identifies the generation: while a marker is pending,
    /// replaying the same event preserves its generation and a newer event
    /// cannot be cleared by an older job. If a replay follows an ack but comes
    /// before the event cursor checkpoint, it may recreate that generation;
    /// the message cursor makes the resulting empty extraction safe to ack.
    /// `bypass_throttle` may only be upgraded while the marker is pending.
    pub fn enqueue_fact_extraction(
        &self,
        session_id: &str,
        bypass_throttle: bool,
        event_sequence: i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(event_sequence > 0, "event sequence must be positive");
        let key = format!("fact_extraction_pending.{session_id}");
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            let previous_value: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = ?1",
                    rusqlite::params![&key],
                    |row| row.get(0),
                )
                .optional()?;
            let previous_state = match previous_value.as_deref() {
                Some(value) => match decode_fact_extraction_marker(value) {
                    Ok(state) => state,
                    Err(_) => {
                        let quarantine_key = format!("fact_extraction_pending_poison.{session_id}");
                        anyhow::ensure!(
                            move_marker_to_quarantine_on(&conn, &key, &quarantine_key, value)?,
                            "fact extraction marker changed during enqueue"
                        );
                        FactExtractionMarkerState {
                            event_sequence: 0,
                            bypass_throttle: false,
                            attempt: 0,
                            next_attempt_at_ms: 0,
                        }
                    }
                },
                None => FactExtractionMarkerState {
                    event_sequence: 0,
                    bypass_throttle: false,
                    attempt: 0,
                    next_attempt_at_ms: 0,
                },
            };
            let is_new_generation = event_sequence > previous_state.event_sequence;
            let bypass_upgraded = bypass_throttle && !previous_state.bypass_throttle;
            let state = FactExtractionMarkerState {
                event_sequence: previous_state.event_sequence.max(event_sequence),
                bypass_throttle: previous_state.bypass_throttle || bypass_throttle,
                attempt: if is_new_generation || bypass_upgraded {
                    0
                } else {
                    previous_state.attempt
                },
                next_attempt_at_ms: if is_new_generation || bypass_upgraded {
                    0
                } else {
                    previous_state.next_attempt_at_ms
                },
            };
            let value = encode_fact_extraction_marker(&state);
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO kv_store (key, value, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at",
                rusqlite::params![key, value, now],
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn pending_fact_extraction_high_water(&self) -> anyhow::Result<Option<String>> {
        let prefix = "fact_extraction_pending.";
        Ok(self.conn().query_row(
            "SELECT MAX(key) FROM kv_store WHERE key >= ?1 AND key < ?2",
            rusqlite::params![prefix, "fact_extraction_pending/"],
            |row| row.get(0),
        )?)
    }

    pub fn pending_fact_extractions_page(
        &self,
        after_key: Option<&str>,
        high_water: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<FactExtractionMarker>> {
        anyhow::ensure!(
            (1..=MAX_MEMORY_OUTBOX_PAGE_SIZE).contains(&limit),
            "fact outbox page limit must be between 1 and {MAX_MEMORY_OUTBOX_PAGE_SIZE}"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT k.key, k.value, substr(k.key, 25)
             FROM kv_store k
             INNER JOIN sessions s ON s.id = substr(k.key, 25)
             WHERE k.key >= 'fact_extraction_pending.'
               AND k.key < 'fact_extraction_pending/'
               AND (?1 IS NULL OR k.key > ?1)
               AND k.key <= ?2
             ORDER BY k.key COLLATE BINARY ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![after_key, high_water, limit as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (key, value, session_id) = row?;
            let state = decode_fact_extraction_marker(&value).map_err(|error| error.to_string());
            Ok(FactExtractionMarker {
                key,
                value,
                session_id,
                state,
            })
        })
        .collect()
    }

    /// Full diagnostic helper retained for repository tests and one-off
    /// inspection. Runtime consumers must use the bounded page API.
    pub fn pending_fact_extractions(&self) -> anyhow::Result<Vec<(String, bool, i64)>> {
        let Some(high_water) = self.pending_fact_extraction_high_water()? else {
            return Ok(Vec::new());
        };
        let mut after_key = None;
        let mut pending = Vec::new();
        loop {
            let page = self.pending_fact_extractions_page(
                after_key.as_deref(),
                &high_water,
                MAX_MEMORY_OUTBOX_PAGE_SIZE,
            )?;
            if page.is_empty() {
                break;
            }
            after_key = page.last().map(|marker| marker.key.clone());
            for marker in page {
                let state = marker
                    .state
                    .map_err(|error| anyhow::anyhow!("invalid marker {}: {error}", marker.key))?;
                pending.push((
                    marker.session_id,
                    state.bypass_throttle,
                    state.event_sequence,
                ));
            }
        }
        Ok(pending)
    }

    pub fn update_pending_fact_extraction_retry_if_current(
        &self,
        key: &str,
        expected_value: &str,
        attempt: u32,
        next_attempt_at_ms: i64,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(next_attempt_at_ms >= 0, "retry deadline cannot be negative");
        let mut state = decode_fact_extraction_marker(expected_value)?;
        state.attempt = attempt;
        state.next_attempt_at_ms = next_attempt_at_ms;
        let changed = self.conn().execute(
            "UPDATE kv_store SET value = ?3, updated_at = ?4
             WHERE key = ?1 AND value = ?2",
            rusqlite::params![
                key,
                expected_value,
                encode_fact_extraction_marker(&state),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn clear_pending_fact_extraction_marker_if_current(
        &self,
        key: &str,
        expected_value: &str,
    ) -> anyhow::Result<bool> {
        let changed = self.conn().execute(
            "DELETE FROM kv_store WHERE key = ?1 AND value = ?2",
            rusqlite::params![key, expected_value],
        )?;
        Ok(changed > 0)
    }

    /// Acknowledge only the durable marker generation captured by this job.
    /// A newer committed trigger remains pending even when its bypass flag is
    /// identical to the in-flight job's flag.
    pub fn clear_pending_fact_extraction_if_current(
        &self,
        session_id: &str,
        event_sequence: i64,
        bypass_throttle: bool,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(event_sequence >= 0, "event sequence must not be negative");
        let key = format!("fact_extraction_pending.{session_id}");
        let conn = self.conn();
        let current: Option<String> = conn
            .query_row("SELECT value FROM kv_store WHERE key = ?1", [&key], |row| {
                row.get(0)
            })
            .optional()?;
        drop(conn);
        let Some(current) = current else {
            return Ok(false);
        };
        let state = decode_fact_extraction_marker(&current)?;
        if state.event_sequence != event_sequence || state.bypass_throttle != bypass_throttle {
            return Ok(false);
        }
        self.clear_pending_fact_extraction_marker_if_current(&key, &current)
    }

    /// Load durable compaction-summary extraction jobs. The episode id is
    /// encoded in the key while the value keeps session cleanup inexpensive.
    pub fn pending_summary_extraction_high_water(&self) -> anyhow::Result<Option<String>> {
        Ok(self.conn().query_row(
            "SELECT MAX(key) FROM kv_store WHERE key >= ?1 AND key < ?2",
            rusqlite::params![
                "fact_extraction_episode_pending.",
                "fact_extraction_episode_pending/"
            ],
            |row| row.get(0),
        )?)
    }

    pub fn pending_summary_extractions_page(
        &self,
        after_key: Option<&str>,
        high_water: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SummaryExtractionMarker>> {
        anyhow::ensure!(
            (1..=MAX_MEMORY_OUTBOX_PAGE_SIZE).contains(&limit),
            "summary outbox page limit must be between 1 and {MAX_MEMORY_OUTBOX_PAGE_SIZE}"
        );
        let prefix = "fact_extraction_episode_pending.";
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT key, value FROM kv_store
             WHERE key >= 'fact_extraction_episode_pending.'
               AND key < 'fact_extraction_episode_pending/'
               AND (?1 IS NULL OR key > ?1)
               AND key <= ?2
             ORDER BY key COLLATE BINARY ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![after_key, high_water, limit as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        rows.map(|row| {
            let (key, value) = row?;
            let suffix = key.strip_prefix(prefix);
            let key_parts = suffix.and_then(|suffix| suffix.split_once('.'));
            let (key_session_id, episode_id) = key_parts
                .map(|(session_id, episode_id)| (session_id.to_owned(), episode_id.to_owned()))
                .unwrap_or_default();
            let state = decode_summary_extraction_marker(&value)
                .and_then(|(session_id, attempt, next_attempt_at_ms)| {
                    anyhow::ensure!(
                        session_id == key_session_id && !episode_id.is_empty(),
                        "summary marker key/value identity mismatch"
                    );
                    Ok((session_id, attempt, next_attempt_at_ms))
                })
                .map(
                    |(_, attempt, next_attempt_at_ms)| SummaryExtractionMarkerState {
                        attempt,
                        next_attempt_at_ms,
                    },
                )
                .map_err(|error| error.to_string());
            Ok(SummaryExtractionMarker {
                key,
                value,
                session_id: key_session_id,
                episode_id,
                state,
            })
        })
        .collect()
    }

    /// Full diagnostic helper retained for repository tests and one-off
    /// inspection. Runtime consumers must use the bounded page API.
    pub fn pending_summary_extractions(&self) -> anyhow::Result<Vec<(String, String)>> {
        let Some(high_water) = self.pending_summary_extraction_high_water()? else {
            return Ok(Vec::new());
        };
        let mut after_key = None;
        let mut pending = Vec::new();
        loop {
            let page = self.pending_summary_extractions_page(
                after_key.as_deref(),
                &high_water,
                MAX_MEMORY_OUTBOX_PAGE_SIZE,
            )?;
            if page.is_empty() {
                break;
            }
            after_key = page.last().map(|marker| marker.key.clone());
            for marker in page {
                marker
                    .state
                    .map_err(|error| anyhow::anyhow!("invalid marker {}: {error}", marker.key))?;
                pending.push((marker.session_id, marker.episode_id));
            }
        }
        Ok(pending)
    }

    pub fn update_summary_extraction_retry_if_current(
        &self,
        key: &str,
        expected_value: &str,
        attempt: u32,
        next_attempt_at_ms: i64,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(next_attempt_at_ms >= 0, "retry deadline cannot be negative");
        let (session_id, _, _) = decode_summary_extraction_marker(expected_value)?;
        let changed = self.conn().execute(
            "UPDATE kv_store SET value = ?3, updated_at = ?4
             WHERE key = ?1 AND value = ?2",
            rusqlite::params![
                key,
                expected_value,
                encode_summary_extraction_marker(&session_id, attempt, next_attempt_at_ms),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn clear_summary_extraction_if_current(
        &self,
        key: &str,
        expected_value: &str,
    ) -> anyhow::Result<bool> {
        let changed = self.conn().execute(
            "DELETE FROM kv_store WHERE key = ?1 AND value = ?2",
            rusqlite::params![key, expected_value],
        )?;
        Ok(changed > 0)
    }

    /// Repair malformed retry metadata for a summary marker whose composite
    /// key still matches a durable episode row. The original raw value is
    /// retained under a session-scoped poison key; the job is made runnable.
    pub fn repair_summary_extraction_marker_if_current(
        &self,
        key: &str,
        expected_value: &str,
    ) -> anyhow::Result<bool> {
        let Some(suffix) = key.strip_prefix("fact_extraction_episode_pending.") else {
            return Ok(false);
        };
        let Some((session_id, episode_id)) = suffix.split_once('.') else {
            return Ok(false);
        };
        if session_id.is_empty() || episode_id.is_empty() {
            return Ok(false);
        }

        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT session_id FROM memory_items
                     WHERE id = ?1 AND kind = 'episode_summary'",
                    rusqlite::params![episode_id],
                    |row| row.get(0),
                )
                .optional()?;
            if owner.as_deref() != Some(session_id) {
                return Ok(false);
            }

            let quarantine_key =
                format!("fact_extraction_episode_pending_poison.{session_id}.{episode_id}");
            if !move_marker_to_quarantine_on(&conn, key, &quarantine_key, expected_value)? {
                return Ok(false);
            }
            conn.execute(
                "INSERT INTO kv_store (key, value, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at",
                rusqlite::params![
                    key,
                    encode_summary_extraction_marker(session_id, 0, 0),
                    Utc::now().to_rfc3339()
                ],
            )?;
            Ok(true)
        })();
        match result {
            Ok(true) => {
                conn.execute_batch("COMMIT")?;
                Ok(true)
            }
            Ok(false) => {
                conn.execute_batch("ROLLBACK")?;
                Ok(false)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Acknowledge one completed compaction-summary extraction.
    pub fn clear_summary_extraction(
        &self,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(!episode_id.trim().is_empty(), "episode id is required");
        self.conn().execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![format!(
                "fact_extraction_episode_pending.{session_id}.{episode_id}"
            )],
        )?;
        Ok(())
    }

    /// Remove session-scoped internal cursors whose session no longer exists
    /// (session rows are deleted without going through `delete_session`, e.g.
    /// history purge or older deletions before cursor cleanup was added). This
    /// also purges extraction throttle stamps, per-episode completion
    /// markers, pending markers, and
    /// `memory_event_cursor.<session_id>` checkpoints of dead sessions.
    /// Called during memory maintenance so the kv table does not grow without
    /// bound. Session retention uses the same connection-level cleanup rule.
    pub fn cleanup_orphan_extraction_cursors(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        cleanup_orphan_session_scoped_state_on(&conn)
    }
}

/// Delete session-scoped extraction and event-consumer markers whose owning
/// session no longer exists. Callers that already hold a connection can reuse
/// this predicate without checking out another pooled connection.
pub(super) fn cleanup_orphan_session_scoped_state_on(
    conn: &rusqlite::Connection,
) -> anyhow::Result<u64> {
    let deleted = conn.execute(
        "DELETE FROM kv_store
         WHERE (key LIKE 'fact_extraction.%'
                OR key LIKE 'fact_extraction_last_run.%'
                OR key LIKE 'fact_extraction_episode_done.%'
                OR key LIKE 'fact_extraction_pending.%'
                OR key GLOB 'fact_extraction_pending_poison.*'
                OR key LIKE 'fact_extraction_episode_pending.%'
                OR key GLOB 'fact_extraction_episode_pending_poison.*'
                OR key GLOB 'memory_event_cursor.*')
           AND NOT EXISTS (SELECT 1 FROM sessions
                           WHERE id = CASE
                               WHEN key GLOB 'memory_event_cursor.*'
                               THEN substr(key, 21)
                               WHEN key LIKE 'fact_extraction_last_run.%'
                               THEN substr(key, 26)
                               WHEN key GLOB 'fact_extraction_pending_poison.*'
                               THEN substr(key, instr(key, '.') + 1)
                               WHEN key LIKE 'fact_extraction_pending.%'
                               THEN substr(key, 25)
                               WHEN key GLOB 'fact_extraction_episode_pending_poison.*'
                               THEN substr(substr(key, length('fact_extraction_episode_pending_poison.') + 1),
                                           1,
                                           instr(substr(key, length('fact_extraction_episode_pending_poison.') + 1), '.') - 1)
                               WHEN key LIKE 'fact_extraction_episode_pending.%'
                               THEN value
                               WHEN key LIKE 'fact_extraction_episode_done.%'
                               THEN value
                               ELSE substr(key, 17)
                           END)",
        [],
    )?;
    Ok(deleted as u64)
}

fn memory_event_cursor_key(session_id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
    Ok(format!("memory_event_cursor.{session_id}"))
}

fn parse_memory_event_cursor(value: Option<&str>) -> anyhow::Result<i64> {
    let Some(value) = value else {
        return Ok(0);
    };
    let sequence = value
        .parse::<i64>()
        .map_err(|error| anyhow::anyhow!("invalid memory event cursor '{value}': {error}"))?;
    anyhow::ensure!(sequence >= 0, "memory event cursor cannot be negative");
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use super::MAX_MEMORY_OUTBOX_PAGE_SIZE;
    use crate::db::Database;
    use haven_common::types::new_id;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn memory_event_cursor_optional_distinguishes_missing_from_zero() {
        let db = test_db();
        let session = db.create_session("cursor").unwrap();

        assert_eq!(db.memory_event_cursor_optional(&session.id).unwrap(), None);
        assert!(
            db.initialize_memory_event_cursor_if_absent(&session.id, 0)
                .unwrap()
        );
        assert_eq!(
            db.memory_event_cursor_optional(&session.id).unwrap(),
            Some(0)
        );
        assert!(
            !db.initialize_memory_event_cursor_if_absent(&session.id, 8)
                .unwrap()
        );
        assert_eq!(
            db.memory_event_cursor_optional(&session.id).unwrap(),
            Some(0)
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 0);
    }

    #[test]
    fn memory_event_cursor_optional_rejects_invalid_stored_values() {
        let db = test_db();
        let session = db.create_session("cursor").unwrap();
        db.set_kv(&format!("memory_event_cursor.{}", session.id), "bad")
            .unwrap();

        assert!(db.memory_event_cursor_optional(&session.id).is_err());
        assert!(
            db.initialize_memory_event_cursor_if_absent(&session.id, 1)
                .is_err()
        );
    }

    #[test]
    fn set_and_get_kv() {
        let db = test_db();
        db.set_kv("fact_extraction.t1", "msg-1").unwrap();
        assert_eq!(
            db.get_kv("fact_extraction.t1").unwrap(),
            Some("msg-1".into())
        );
        assert_eq!(db.get_kv("nonexistent").unwrap(), None);
    }

    #[test]
    fn set_kv_updates_existing() {
        let db = test_db();
        db.set_kv("cursor", "a").unwrap();
        db.set_kv("cursor", "b").unwrap();
        assert_eq!(db.get_kv("cursor").unwrap(), Some("b".into()));
    }

    #[test]
    fn summary_extraction_jobs_keep_each_episode_and_ack_independently() {
        let db = test_db();
        let session = db.create_session("summary jobs").unwrap();
        let first_episode_id = new_id("msg");
        let second_episode_id = new_id("msg");
        db.add_episode_with_pending_extraction(
            &session.id,
            "The first durable summary contains enough extraction context.",
            &first_episode_id,
            true,
        )
        .unwrap();
        db.add_episode_with_pending_extraction(
            &session.id,
            "The second durable summary contains enough extraction context.",
            &second_episode_id,
            true,
        )
        .unwrap();

        let mut pending = db.pending_summary_extractions().unwrap();
        pending.sort();
        let mut expected = vec![
            (session.id.clone(), first_episode_id.clone()),
            (session.id.clone(), second_episode_id.clone()),
        ];
        expected.sort();
        assert_eq!(pending, expected);
        db.clear_summary_extraction(&session.id, &first_episode_id)
            .unwrap();
        assert_eq!(
            db.pending_summary_extractions().unwrap(),
            vec![(session.id, second_episode_id)]
        );
    }

    #[test]
    fn cleanup_orphan_extraction_cursors_removes_stale_keys() {
        let db = test_db();
        let session = db.create_session("").unwrap();
        // Cursor + throttle stamp for an existing session: kept.
        db.set_kv(&format!("fact_extraction.{}", session.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", session.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.checkpoint_memory_event_cursor(&session.id, 9).unwrap();
        // Orphan cursor / orphan throttle stamp (no session row) and
        // non-cursor keys: the orphans are removed, unrelated kv keys survive.
        db.set_kv("fact_extraction.gone", "msg-9").unwrap();
        db.set_kv("fact_extraction_last_run.gone", "2026-08-15T00:00:00Z")
            .unwrap();
        db.set_kv("fact_extraction_episode_done.gone.msg-11", "gone")
            .unwrap();
        db.set_kv("fact_extraction_pending.gone", "1").unwrap();
        db.set_kv("fact_extraction_pending_poison.gone", "bad fact marker")
            .unwrap();
        db.set_kv(
            "fact_extraction_episode_pending_poison.gone.msg-11",
            "bad summary marker",
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_pending_poison.{}", session.id),
            "keep for existing session",
        )
        .unwrap();
        db.set_kv("memory_event_cursor.gone", "8").unwrap();
        db.set_kv("memoryXeventYcursor.gone", "keep").unwrap();
        db.set_kv("other.state", "keep").unwrap();

        let removed = db.cleanup_orphan_extraction_cursors().unwrap();
        assert_eq!(removed, 7);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", session.id))
                .unwrap()
                .is_some()
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 9);
        assert!(db.get_kv("fact_extraction.gone").unwrap().is_none());
        assert!(
            db.get_kv("fact_extraction_last_run.gone")
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv("fact_extraction_episode_done.gone.msg-11")
                .unwrap()
                .is_none()
        );
        assert!(db.get_kv("fact_extraction_pending.gone").unwrap().is_none());
        assert!(
            db.get_kv("fact_extraction_pending_poison.gone")
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv("fact_extraction_episode_pending_poison.gone.msg-11")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.get_kv(&format!("fact_extraction_pending_poison.{}", session.id))
                .unwrap()
                .as_deref(),
            Some("keep for existing session")
        );
        assert!(db.get_kv("memory_event_cursor.gone").unwrap().is_none());
        assert_eq!(
            db.get_kv("memoryXeventYcursor.gone").unwrap(),
            Some("keep".into())
        );
        assert_eq!(db.get_kv("other.state").unwrap(), Some("keep".into()));
    }

    #[test]
    fn memory_event_cursor_defaults_is_monotonic_and_session_scoped() {
        let db = test_db();
        let first = db.create_session("first").unwrap();
        let second = db.create_session("second").unwrap();
        db.set_kv(&format!("fact_extraction.{}", first.id), "msg-7")
            .unwrap();

        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 0);
        db.checkpoint_memory_event_cursor(&first.id, 0).unwrap();
        db.checkpoint_memory_event_cursor(&first.id, 4).unwrap();
        db.checkpoint_memory_event_cursor(&first.id, 4).unwrap();
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 4);
        assert_eq!(db.memory_event_cursor(&second.id).unwrap(), 0);
        assert!(db.checkpoint_memory_event_cursor(&first.id, 3).is_err());
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 4);

        db.checkpoint_memory_event_cursor(&second.id, 2).unwrap();
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 4);
        assert_eq!(db.memory_event_cursor(&second.id).unwrap(), 2);
        assert_eq!(
            db.get_kv(&format!("memory_event_cursor.{}", first.id))
                .unwrap(),
            Some("4".into())
        );
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", first.id)).unwrap(),
            Some("msg-7".into())
        );
        assert!(db.checkpoint_memory_event_cursor(&first.id, -1).is_err());

        db.clear_memory_event_cursor(&first.id).unwrap();
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 0);
        assert_eq!(db.memory_event_cursor(&second.id).unwrap(), 2);
    }

    #[test]
    fn delete_session_removes_extraction_cursor() {
        let db = test_db();
        let session = db.create_session("t-cursor").unwrap();
        let episode_id = new_id("msg");
        db.set_kv(&format!("fact_extraction.{}", session.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", session.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_episode_done.{}.{}", session.id, episode_id),
            &session.id,
        )
        .unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", session.id), "1")
            .unwrap();
        db.add_episode_with_pending_extraction(
            &session.id,
            "A durable summary with an extraction marker for session cleanup.",
            &episode_id,
            true,
        )
        .unwrap();
        db.checkpoint_memory_event_cursor(&session.id, 6).unwrap();
        db.delete_session(&session.id).unwrap();
        assert!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!(
                "fact_extraction_episode_pending.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .is_none()
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 0);
    }

    #[test]
    fn pending_extraction_markers_keep_event_generation_and_bypass() {
        let db = test_db();
        let session = db.create_session("t-pending").unwrap();
        db.enqueue_fact_extraction(&session.id, false, 1).unwrap();
        db.enqueue_fact_extraction(&session.id, true, 2).unwrap();
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 2)]
        );

        assert!(
            !db.clear_pending_fact_extraction_if_current(&session.id, 1, false)
                .unwrap()
        );
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 2)]
        );
        assert!(
            db.clear_pending_fact_extraction_if_current(&session.id, 2, true)
                .unwrap()
        );
        assert!(db.pending_fact_extractions().unwrap().is_empty());
    }

    #[test]
    fn same_value_retrigger_survives_an_older_job_ack() {
        let db = test_db();
        let session = db.create_session("t-pending-retrigger").unwrap();
        db.enqueue_fact_extraction(&session.id, true, 4).unwrap();
        db.enqueue_fact_extraction(&session.id, true, 5).unwrap();

        assert!(
            !db.clear_pending_fact_extraction_if_current(&session.id, 4, true)
                .unwrap()
        );
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 5)]
        );

        assert!(
            db.clear_pending_fact_extraction_if_current(&session.id, 5, true)
                .unwrap()
        );
        assert!(db.pending_fact_extractions().unwrap().is_empty());
    }

    #[test]
    fn legacy_boolean_fact_marker_is_readable_and_upgraded_on_enqueue() {
        let db = test_db();
        let session = db.create_session("t-legacy-pending").unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", session.id), "1")
            .unwrap();

        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 0)]
        );
        db.enqueue_fact_extraction(&session.id, false, 8).unwrap();
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 8)]
        );
        assert!(
            !db.clear_pending_fact_extraction_if_current(&session.id, 0, true)
                .unwrap()
        );
        assert!(
            db.clear_pending_fact_extraction_if_current(&session.id, 8, true)
                .unwrap()
        );
    }

    #[test]
    fn fact_outbox_pages_are_bounded_and_resume_after_delete_or_behind_cursor_update() {
        let db = test_db();
        let mut sessions = (0..130)
            .map(|index| db.create_session(&format!("page-{index}")).unwrap().id)
            .collect::<Vec<_>>();
        sessions.sort();
        for session_id in &sessions {
            db.enqueue_fact_extraction(session_id, false, 1).unwrap();
        }
        db.set_kv(
            &format!("fact_extraction_pending.{}", sessions[1]),
            "malformed marker",
        )
        .unwrap();

        let high_water = db.pending_fact_extraction_high_water().unwrap().unwrap();
        let first = db
            .pending_fact_extractions_page(None, &high_water, MAX_MEMORY_OUTBOX_PAGE_SIZE)
            .unwrap();
        assert_eq!(first.len(), MAX_MEMORY_OUTBOX_PAGE_SIZE);
        assert!(
            first.iter().any(|marker| marker.state.is_err()),
            "poison marker must remain scannable"
        );
        let cursor = first.last().unwrap().key.clone();

        // Deleting a row after the cursor does not shift later rows as OFFSET
        // pagination would, and an update behind the cursor is picked up by
        // the next pass from its first key.
        assert!(
            db.clear_pending_fact_extraction_if_current(&sessions[80], 1, false)
                .unwrap()
        );
        db.enqueue_fact_extraction(&sessions[0], true, 2).unwrap();
        let second = db
            .pending_fact_extractions_page(Some(&cursor), &high_water, MAX_MEMORY_OUTBOX_PAGE_SIZE)
            .unwrap();
        let second_cursor = second.last().unwrap().key.clone();
        let third = db
            .pending_fact_extractions_page(
                Some(&second_cursor),
                &high_water,
                MAX_MEMORY_OUTBOX_PAGE_SIZE,
            )
            .unwrap();
        assert_eq!(second.len() + third.len(), 65);
        assert!(third.is_empty() || third.len() <= MAX_MEMORY_OUTBOX_PAGE_SIZE);

        let next_pass = db
            .pending_fact_extractions_page(None, &high_water, MAX_MEMORY_OUTBOX_PAGE_SIZE)
            .unwrap();
        assert_eq!(
            next_pass[0].state.as_ref().unwrap().event_sequence,
            2,
            "a changed marker behind the prior cursor is visible from the next pass"
        );
        assert!(
            db.pending_fact_extractions_page(None, &high_water, 0)
                .is_err()
        );
        assert!(
            db.pending_fact_extractions_page(None, &high_water, MAX_MEMORY_OUTBOX_PAGE_SIZE + 1)
                .is_err()
        );
    }

    #[test]
    fn fact_retry_metadata_survives_same_generation_and_resets_for_new_event() {
        let db = test_db();
        let session = db.create_session("retry metadata").unwrap();
        db.enqueue_fact_extraction(&session.id, false, 1).unwrap();
        let high_water = db.pending_fact_extraction_high_water().unwrap().unwrap();
        let original = db
            .pending_fact_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);
        assert!(
            db.update_pending_fact_extraction_retry_if_current(
                &original.key,
                &original.value,
                3,
                50_000,
            )
            .unwrap()
        );
        db.enqueue_fact_extraction(&session.id, false, 1).unwrap();
        let retried = db
            .pending_fact_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);
        assert_eq!(retried.state.as_ref().unwrap().attempt, 3);
        assert_eq!(retried.state.as_ref().unwrap().next_attempt_at_ms, 50_000);
        assert!(
            !db.clear_pending_fact_extraction_marker_if_current(&retried.key, &original.value)
                .unwrap()
        );

        db.enqueue_fact_extraction(&session.id, true, 2).unwrap();
        let next_generation = db
            .pending_fact_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);
        assert_eq!(next_generation.state.as_ref().unwrap().attempt, 0);
        assert_eq!(
            next_generation.state.as_ref().unwrap().next_attempt_at_ms,
            0
        );
        assert!(next_generation.state.as_ref().unwrap().bypass_throttle);
    }

    #[test]
    fn summary_retry_metadata_survives_idempotent_episode_replay_and_high_water() {
        let db = test_db();
        let session = db.create_session("summary retry metadata").unwrap();
        let episode_id = new_id("msg");
        let content = "A summary long enough to create a durable episode row.";
        db.add_episode_with_pending_extraction(&session.id, content, &episode_id, true)
            .unwrap();
        let high_water = db.pending_summary_extraction_high_water().unwrap().unwrap();
        let original = db
            .pending_summary_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);
        assert!(
            db.update_summary_extraction_retry_if_current(
                &original.key,
                &original.value,
                2,
                70_000,
            )
            .unwrap()
        );
        db.add_episode_with_pending_extraction(&session.id, content, &episode_id, true)
            .unwrap();
        let current = db
            .pending_summary_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);
        assert_eq!(current.state.as_ref().unwrap().attempt, 2);
        assert_eq!(current.state.as_ref().unwrap().next_attempt_at_ms, 70_000);
        assert!(
            !db.clear_summary_extraction_if_current(&current.key, &original.value)
                .unwrap()
        );

        db.set_kv("fact_extraction_episode_pending.zzzz.zzzz", &session.id)
            .unwrap();
        let bounded_page = db
            .pending_summary_extractions_page(None, &high_water, 64)
            .unwrap();
        assert!(
            bounded_page
                .iter()
                .all(|marker| !marker.key.ends_with(".zzzz"))
        );
    }

    #[test]
    fn legacy_summary_marker_is_read_as_immediately_runnable() {
        let db = test_db();
        let session = db.create_session("legacy summary marker").unwrap();
        let key = format!(
            "fact_extraction_episode_pending.{}.{}",
            session.id,
            new_id("msg")
        );
        db.set_kv(&key, &session.id).unwrap();

        let high_water = db.pending_summary_extraction_high_water().unwrap().unwrap();
        let marker = db
            .pending_summary_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);

        assert_eq!(marker.value, session.id);
        let state = marker.state.unwrap();
        assert_eq!(state.attempt, 0);
        assert_eq!(state.next_attempt_at_ms, 0);
    }

    #[test]
    fn malformed_summary_retry_metadata_is_quarantined_and_requeued() {
        let db = test_db();
        let session = db.create_session("repair summary marker").unwrap();
        let episode_id = new_id("msg");
        db.add_episode_with_pending_extraction(
            &session.id,
            "A summary whose durable extraction marker can be repaired safely.",
            &episode_id,
            true,
        )
        .unwrap();
        let key = format!(
            "fact_extraction_episode_pending.{}.{}",
            session.id, episode_id
        );
        db.set_kv(&key, "not-a-summary-marker").unwrap();

        assert!(
            db.repair_summary_extraction_marker_if_current(&key, "not-a-summary-marker")
                .unwrap()
        );
        let high_water = db.pending_summary_extraction_high_water().unwrap().unwrap();
        let marker = db
            .pending_summary_extractions_page(None, &high_water, 64)
            .unwrap()
            .remove(0);
        assert_eq!(marker.state.unwrap().attempt, 0);
        assert_eq!(marker.value, format!("{}:0:0", session.id));
        assert_eq!(
            db.get_kv(&format!(
                "fact_extraction_episode_pending_poison.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .as_deref(),
            Some("not-a-summary-marker")
        );
        assert!(
            !db.repair_summary_extraction_marker_if_current(&key, "not-a-summary-marker")
                .unwrap()
        );
    }

    #[test]
    fn startup_baseline_is_atomic_and_leaves_later_sessions_unbaselined() {
        let db = test_db();
        let existing = db.create_session("baseline existing").unwrap();
        db.conn()
            .execute(
                "INSERT INTO session_events
                    (session_id, sequence, event_type, event_version, payload, created_at)
                 VALUES (?1, 1, 'usage_recorded', 1, '{}', '2026-10-06T00:00:00Z')",
                rusqlite::params![existing.id],
            )
            .unwrap();

        db.baseline_missing_memory_event_cursors_to_latest()
            .unwrap();
        assert_eq!(db.memory_event_cursor(&existing.id).unwrap(), 1);

        let created_after_baseline = db.create_session("created after baseline").unwrap();
        assert_eq!(
            db.memory_event_cursor_optional(&created_after_baseline.id)
                .unwrap(),
            None
        );
    }
}
