//! Database schema initialization (no migration layer).
//!
//! The schema below is the single source of truth for the current database
//! shape. It runs idempotently (`CREATE ... IF NOT EXISTS`) on every open,
//! and any database whose shape predates it — a schema that is missing any
//! required column — is rejected with a clear error telling the user to
//! delete the file and rebuild. There is deliberately no upgrade path: old
//! databases are not migrated, converted, or rewritten.

/// Current schema, created idempotently on every open.
const SCHEMA_SQL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        input_text TEXT NOT NULL DEFAULT '',
        title TEXT,
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK(status IN ('pending','running','paused','completed','failed','error')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
        transcript TEXT NOT NULL DEFAULT '',
        react_state TEXT
    )",
    "CREATE TABLE IF NOT EXISTS messages (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
        content TEXT NOT NULL,
        message_type TEXT CHECK(message_type IN ('text','thought','action','observation','reasoning')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        tool_call_id TEXT,
        attachments TEXT,
        voice INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS session_steps (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        step_number INTEGER NOT NULL,
        tool_name TEXT NOT NULL,
        input TEXT NOT NULL DEFAULT '{}',
        output TEXT NOT NULL DEFAULT '{}',
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK(status IN ('pending','running','completed','failed','error')),
        is_high_risk INTEGER NOT NULL DEFAULT 0,
        confirmed INTEGER,
        started_at TEXT,
        completed_at TEXT,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        silent INTEGER NOT NULL DEFAULT 0,
        thought TEXT,
        action_tool TEXT,
        action_input TEXT,
        observation TEXT
    )",
    // Internal key-value store (fact-extraction cursors, etc.).
    "CREATE TABLE IF NOT EXISTS kv_store (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    // Episodic long-term memory: compaction summaries persisted when the
    // react loop compresses a conversation. Indexed (embedding + keyword)
    // as `episode` entities alongside user messages, so context that was
    // summarized away remains retrievable across sessions.
    "CREATE TABLE IF NOT EXISTS memory_episodes (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        summary TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE TABLE IF NOT EXISTS facts (
        id TEXT PRIMARY KEY,
        subject TEXT NOT NULL,
        predicate TEXT NOT NULL,
        object TEXT NOT NULL,
        source TEXT NOT NULL DEFAULT 'inferred'
            CHECK(source IN ('user','inferred')),
        confidence REAL NOT NULL DEFAULT 1.0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        tags TEXT NOT NULL DEFAULT '[]',
        durability REAL NOT NULL DEFAULT 1.0,
        mention_count INTEGER NOT NULL DEFAULT 0,
        last_seen_at TEXT,
        source_ref TEXT
    )",
    "CREATE TABLE IF NOT EXISTS tasks (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL DEFAULT 'scheduled',
        due_at TEXT,
        title TEXT NOT NULL DEFAULT 'Haven',
        body TEXT,
        mode TEXT NOT NULL DEFAULT 'tool',
        session_id TEXT,
        tool_name TEXT,
        tool_args TEXT,
        prompt TEXT,
        fired INTEGER NOT NULL DEFAULT 0,
        status TEXT,
        command TEXT,
        output TEXT,
        error TEXT,
        error_reason TEXT,
        log_path TEXT,
        exit_code INTEGER,
        started_at TEXT,
        finished_at TEXT,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE TABLE IF NOT EXISTS session_usage (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        prompt_tokens INTEGER NOT NULL DEFAULT 0,
        completion_tokens INTEGER NOT NULL DEFAULT 0,
        total_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd REAL NOT NULL DEFAULT 0,
        has_cost INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    // Per-LLM-call usage detail: one row per successful model response,
    // carrying the endpoint role, model name, token counts, cost and
    // wall-clock duration. `session_usage` keeps the ses-level cumulative
    // counters; this table keeps the granular history behind them.
    "CREATE TABLE IF NOT EXISTS llm_usage (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        step_number INTEGER,
        role TEXT NOT NULL,
        model TEXT,
        prompt_tokens INTEGER NOT NULL DEFAULT 0,
        completion_tokens INTEGER NOT NULL DEFAULT 0,
        total_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd REAL NOT NULL DEFAULT 0,
        has_cost INTEGER NOT NULL DEFAULT 0,
        duration_ms INTEGER,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    // Scratch table for in-flight streamed text (crash/stop partial-reply
    // recovery).
    "CREATE TABLE IF NOT EXISTS partial_messages (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        content TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_session_steps_session ON session_steps(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject)",
    "CREATE INDEX IF NOT EXISTS idx_facts_confidence ON facts(confidence)",
    "CREATE INDEX IF NOT EXISTS idx_memory_episodes_session ON memory_episodes(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_memory_episodes_created ON memory_episodes(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_llm_usage_session ON llm_usage(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status)",
];

/// Vector index for semantic memory. `entity_type` selects the memory domain
/// ('fact' = facts rows, 'episode' = conversation events / compaction
/// summaries); `entity_id` references the owning row. `vector` is a
/// little-endian f32 blob; `text` keeps the embedded surface text so keyword
/// search and display don't need to re-derive it.
const MEMORY_EMBEDDINGS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memory_embeddings (
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (entity_type, entity_id, model)
);
CREATE INDEX IF NOT EXISTS idx_memory_embeddings_type ON memory_embeddings(entity_type);
";

/// Triggers that keep `memory_embeddings` in sync with the `facts` table: any
/// fact row UPDATE or DELETE invalidates the fact's embedding, so the next
/// embedding pass re-indexes the current surface text.
fn ensure_fact_embedding_triggers(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS facts_embed_del;
         DROP TRIGGER IF EXISTS facts_embed_upd;
         CREATE TRIGGER facts_embed_del AFTER DELETE ON facts BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'fact' AND entity_id = old.id;
         END;
         CREATE TRIGGER facts_embed_upd AFTER UPDATE ON facts BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'fact' AND entity_id = old.id;
         END;",
    )?;
    Ok(())
}

/// FTS5 full-text index over facts so search_facts can rank by relevance
/// (BM25) instead of doing LIKE scans. Uses an external-content table synced
/// by triggers. Best-effort: bundled SQLite ships FTS5, but if it is ever
/// unavailable the setup fails gracefully and search_facts falls back to the
/// LIKE path. The whole block runs inside one transaction and the idempotency
/// guard checks that the sync triggers exist too, so a crash partway through
/// (table created but triggers missing) is repaired on the next startup.
fn ensure_facts_fts(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let has_fts: bool = conn
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts_fts'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    let triggers_missing = ["facts_ai", "facts_ad", "facts_au"].iter().any(|name| {
        conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?1")
            .and_then(|mut stmt| stmt.query_row(rusqlite::params![name], |r| r.get::<_, i32>(0)))
            .map(|c| c == 0)
            .unwrap_or(true)
    });
    if !has_fts || triggers_missing {
        // DROP first so a partially-applied previous attempt (table present
        // but triggers missing) is rebuilt cleanly; external-content tables
        // re-index from the facts table via the 'rebuild' command.
        let fts_sql = "BEGIN;
            DROP TABLE IF EXISTS facts_fts;
            DROP TRIGGER IF EXISTS facts_ai;
            DROP TRIGGER IF EXISTS facts_ad;
            DROP TRIGGER IF EXISTS facts_au;
            CREATE VIRTUAL TABLE facts_fts USING fts5(
                subject, predicate, object, tags,
                content='facts', content_rowid='rowid'
            );
            CREATE TRIGGER facts_ai AFTER INSERT ON facts BEGIN
                INSERT INTO facts_fts(rowid, subject, predicate, object, tags)
                VALUES (new.rowid, new.subject, new.predicate, new.object, new.tags);
            END;
            CREATE TRIGGER facts_ad AFTER DELETE ON facts BEGIN
                INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object, tags)
                VALUES ('delete', old.rowid, old.subject, old.predicate, old.object, old.tags);
            END;
            CREATE TRIGGER facts_au AFTER UPDATE ON facts BEGIN
                INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object, tags)
                VALUES ('delete', old.rowid, old.subject, old.predicate, old.object, old.tags);
                INSERT INTO facts_fts(rowid, subject, predicate, object, tags)
                VALUES (new.rowid, new.subject, new.predicate, new.object, new.tags);
            END;
            INSERT INTO facts_fts(facts_fts) VALUES ('rebuild');
            COMMIT;";
        if let Err(e) = conn.execute_batch(fts_sql) {
            tracing::warn!("FTS5 unavailable, facts search falls back to LIKE: {}", e);
            // execute_batch auto-commits each statement, but the explicit
            // BEGIN above means a mid-batch failure leaves the transaction
            // open — roll it back so the connection is left in a clean state.
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    Ok(())
}

/// Required columns per table. A database missing any of these predates the
/// current schema and cannot be used — it is rejected with a clear error.
const REQUIRED_COLUMNS: &[(&str, &str)] = &[
    ("sessions", "transcript"),
    ("messages", "voice"),
    ("session_steps", "thought"),
    ("facts", "tags"),
    ("facts", "durability"),
    ("tasks", "kind"),
];

fn column_exists(conn: &rusqlite::Connection, table: &str, col: &str) -> anyhow::Result<bool> {
    Ok(conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2")?
        .query_row(rusqlite::params![table, col], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false))
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> anyhow::Result<bool> {
    Ok(conn
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row(rusqlite::params![table], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false))
}

/// Create the current schema (idempotent). Rejects databases whose shape
/// predates it — there is no migration layer, so the user must delete the
/// file and let a fresh schema be created.
pub fn init_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    // Reject legacy databases BEFORE creating anything: each table's CREATE
    // statement carries all of its columns atomically, so a table that exists
    // while missing a required column can only be an old-version table. An
    // early check turns the confusion of a later "no such column" into one
    // clear message.
    for (table, col) in REQUIRED_COLUMNS {
        if table_exists(conn, table)? && !column_exists(conn, table, col)? {
            return Err(anyhow::anyhow!(
                "database schema is from an old Haven version (missing {table}.{col}). \
                 The current version does not migrate old databases; \
                 delete the database file (haven.db) and restart to create a fresh one."
            ));
        }
    }
    for sql in SCHEMA_SQL {
        conn.execute_batch(sql)
            .map_err(|e| anyhow::anyhow!("schema SQL failed: {e}\n---\n{sql}"))?;
    }
    conn.execute_batch(MEMORY_EMBEDDINGS_SCHEMA)?;
    ensure_facts_fts(conn)?;
    ensure_fact_embedding_triggers(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn create_test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        conn
    }

    fn get_tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    fn get_indexes(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[test]
    fn init_schema_creates_all_tables() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let tables = get_tables(&conn);

        let expected = &[
            "facts",
            "llm_usage",
            "memory_embeddings",
            "memory_episodes",
            "messages",
            "partial_messages",
            "kv_store",
            "tasks",
            "session_steps",
            "session_usage",
            "sessions",
        ];
        for t in expected {
            assert!(
                tables.iter().any(|n| n == t),
                "expected table '{}' not found in {:?}",
                t,
                tables
            );
        }
        // FTS5 external-content index (plus its shadow tables) is expected.
        assert!(
            tables.iter().any(|n| n == "facts_fts"),
            "facts_fts table should exist"
        );
        let core: Vec<_> = tables
            .iter()
            .filter(|t| !t.starts_with("facts_fts"))
            .collect();
        assert_eq!(core.len(), expected.len());
    }

    #[test]
    fn init_schema_creates_all_indexes() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let indexes = get_indexes(&conn);

        let expected = &[
            "idx_facts_confidence",
            "idx_facts_subject",
            "idx_llm_usage_session",
            "idx_memory_embeddings_type",
            "idx_memory_episodes_created",
            "idx_memory_episodes_session",
            "idx_messages_created_at",
            "idx_session_steps_session",
            "idx_sessions_created_at",
            "idx_sessions_status",
        ];
        for ix in expected {
            assert!(
                indexes.iter().any(|n| n == ix),
                "expected index '{}' not found in {:?}",
                ix,
                indexes
            );
        }
        let core: Vec<_> = indexes
            .iter()
            .filter(|n| !n.starts_with("facts_fts"))
            .collect();
        assert_eq!(core.len(), expected.len());
    }

    #[test]
    fn init_schema_is_idempotent() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let tables_before = get_tables(&conn);
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(get_tables(&conn), tables_before);
    }

    #[test]
    fn init_schema_rejects_legacy_database() {
        // Simulate an old-version database: the sessions table predates
        // `transcript`, so the required-column check must reject it.
        let conn = create_test_conn();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                ended_at TEXT,
                status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','closed')),
                parent_id TEXT REFERENCES sessions(id)
            )",
        )
        .unwrap();
        let err = init_schema(&conn).unwrap_err().to_string();
        assert!(
            err.contains("old Haven version"),
            "expected a clear old-database error, got: {err}"
        );
        assert!(err.contains("sessions.transcript"));
    }

    #[test]
    fn init_schema_rejects_messages_without_voice() {
        // A database whose messages table predates the voice flag must be
        // rejected instead of silently running with a missing column.
        let conn = create_test_conn();
        conn.execute_batch(
            "CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT NOT NULL
            )",
        )
        .unwrap();
        conn.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY)")
            .unwrap();
        let err = init_schema(&conn).unwrap_err().to_string();
        assert!(
            err.contains("old Haven version"),
            "expected a clear old-database error, got: {err}"
        );
    }

    #[test]
    fn fact_embedding_triggers_exist_after_init() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        for name in ["facts_embed_del", "facts_embed_upd"] {
            let count: i32 = conn
                .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?1")
                .unwrap()
                .query_row(rusqlite::params![name], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "trigger {} must exist", name);
        }
        // A fact UPDATE/DELETE invalidates its embedding through the trigger.
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object)
             VALUES ('f1', 'user', 'likes', 'Rust')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('fact', 'f1', 'm', X'0102', 'x')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE facts SET confidence = 0.5 WHERE id = 'f1'", [])
            .unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE entity_id='f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "UPDATE must invalidate the embedding");
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('fact', 'f1', 'm', X'0102', 'x')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM facts WHERE id = 'f1'", [])
            .unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE entity_id='f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "DELETE must invalidate the embedding");
    }

    #[test]
    fn facts_defaults_apply() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object)
             VALUES ('f1', 'user', 'likes', 'Rust')",
            [],
        )
        .unwrap();
        let (durability, tags): (f64, String) = conn
            .query_row(
                "SELECT durability, tags FROM facts WHERE id='f1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(durability, 1.0);
        assert_eq!(tags, "[]");
    }

    #[test]
    fn fts_triggers_sync_facts() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, tags)
             VALUES ('f1', 'user', 'likes', 'Rust', '[\"dev\"]')",
            [],
        )
        .unwrap();
        let hits: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH '\"Rust\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "FTS index must contain the inserted fact");
    }
}
