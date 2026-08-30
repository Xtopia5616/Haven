//! Database schema initialization with a versioned migration layer.
//!
//! The current schema shape lives in [`SCHEMA_SQL`] and is created
//! idempotently on every open. Versioning uses `PRAGMA user_version`:
//!
//! - A brand-new database (or a v0 database whose shape happens to match the
//!   current schema — built by a pre-versioning binary) is stamped with
//!   [`SCHEMA_VERSION`] after initialization.
//! - An older database (`user_version < SCHEMA_VERSION`) is upgraded by
//!   running every migration in [`MIGRATIONS`] with a version above its own.
//! - A NEWER database (`user_version > SCHEMA_VERSION`) is rejected — the
//!   binary is older than the database and could corrupt it.
//!
//! Any schema change must be a new entry in [`MIGRATIONS`] (bumping
//! [`SCHEMA_VERSION`]), not an edit to `SCHEMA_SQL` alone: a fresh DB runs
//! `SCHEMA_SQL` and gets the final version stamp, an existing DB runs only
//! the migrations it has not seen yet.

#[path = "migrations.rs"]
mod migrations;

use migrations::{MIGRATIONS, SCHEMA_VERSION, apply_migrations, set_user_version, user_version};
#[cfg(test)]
use migrations::{
    Migration, migrate_v10_llm_usage_cache_accounting, migrate_v11_usage_cache_diagnostics,
};
/// Current schema, created idempotently on every open.
const SCHEMA_SQL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        input_text TEXT NOT NULL DEFAULT '',
        title TEXT,
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK(status IN ('pending','running','paused','paused_awaiting_answer','paused_awaiting_confirm','completed','failed','error')),
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
        message_type TEXT CHECK(message_type IN ('text','thought','action','observation','reasoning','peer_kickoff')),
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
    // Typed memory graph (X1): nodes + SPO edges + episodic items.
    "CREATE TABLE IF NOT EXISTS memory_nodes (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        label TEXT NOT NULL,
        aliases TEXT NOT NULL DEFAULT '[]',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        UNIQUE(kind, label)
    )",
    // Episodic items (compaction summaries / notes). NOT all messages —
    // only curated memory rows. Shares `msg-*` id space with transcript
    // bubbles when a compaction summary is mirrored.
    "CREATE TABLE IF NOT EXISTS memory_items (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        kind TEXT NOT NULL CHECK(kind IN ('episode_summary','utterance','note')),
        content TEXT NOT NULL,
        topics TEXT NOT NULL DEFAULT '[]',
        entities TEXT NOT NULL DEFAULT '[]',
        created_at TEXT NOT NULL
    )",
    // Today's facts as SPO edges. Keeps `fact-*` ids. `entity_type='fact'`
    // in memory_embeddings remains the domain alias for these edge rows.
    "CREATE TABLE IF NOT EXISTS memory_edges (
        id TEXT PRIMARY KEY,
        subject TEXT NOT NULL,
        subject_id TEXT REFERENCES memory_nodes(id) ON DELETE SET NULL,
        predicate TEXT NOT NULL,
        object TEXT NOT NULL,
        object_id TEXT REFERENCES memory_nodes(id) ON DELETE SET NULL,
        source TEXT NOT NULL DEFAULT 'inferred'
            CHECK(source IN ('user','inferred')),
        confidence REAL NOT NULL DEFAULT 1.0,
        created_at TEXT NOT NULL,
        tags TEXT NOT NULL DEFAULT '[]',
        durability REAL NOT NULL DEFAULT 1.0,
        mention_count INTEGER NOT NULL DEFAULT 0,
        last_seen_at TEXT,
        provenance_item_id TEXT REFERENCES memory_items(id) ON DELETE SET NULL,
        provenance_record_id TEXT,
        provenance_snippet TEXT
    )",
    "CREATE TABLE IF NOT EXISTS actions (
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
         cached_tokens INTEGER NOT NULL DEFAULT 0,
         cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
         cache_miss_tokens INTEGER NOT NULL DEFAULT 0,
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
         cached_tokens INTEGER NOT NULL DEFAULT 0,
         cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
         cache_accounting TEXT NOT NULL DEFAULT 'unknown',
         cache_miss_tokens INTEGER NOT NULL DEFAULT 0,
         cache_diagnostics TEXT,
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
    "CREATE INDEX IF NOT EXISTS idx_memory_edges_subject ON memory_edges(subject)",
    "CREATE INDEX IF NOT EXISTS idx_memory_edges_confidence ON memory_edges(confidence)",
    "CREATE INDEX IF NOT EXISTS idx_memory_items_session ON memory_items(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_memory_items_created ON memory_items(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_memory_nodes_label ON memory_nodes(label)",
    "CREATE INDEX IF NOT EXISTS idx_llm_usage_session ON llm_usage(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status)",
];

/// Vector index for semantic memory. `entity_type` selects the memory domain
/// ('fact' = memory_edges SPO rows, 'episode' = memory_items). Domain string
/// values stay `fact`/`episode` as aliases for edge/item to limit caller churn.
/// `entity_id` references the owning row. `vector` is a little-endian f32 blob;
/// `text` keeps the embedded surface text so keyword search and display don't
/// need to re-derive it.
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
CREATE INDEX IF NOT EXISTS idx_memory_embeddings_type_model
    ON memory_embeddings(entity_type, model, updated_at);
CREATE TABLE IF NOT EXISTS embedding_lsh (
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    model TEXT NOT NULL,
    bucket INTEGER NOT NULL,
    PRIMARY KEY (entity_type, entity_id, model)
);
CREATE INDEX IF NOT EXISTS idx_embedding_lsh_probe
    ON embedding_lsh(entity_type, model, bucket);
";

/// Triggers that keep `memory_embeddings` in sync with `memory_edges`.
/// Invalidate on DELETE, and on UPDATE only when SPO surface text changes
/// (reinforcement of mention_count / confidence / provenance must not drop
/// vectors).
fn ensure_fact_embedding_triggers(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS facts_embed_del;
         DROP TRIGGER IF EXISTS facts_embed_upd;
         DROP TRIGGER IF EXISTS memory_edges_embed_del;
         DROP TRIGGER IF EXISTS memory_edges_embed_upd;
         CREATE TRIGGER memory_edges_embed_del AFTER DELETE ON memory_edges BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'fact' AND entity_id = old.id;
             DELETE FROM embedding_lsh WHERE entity_type = 'fact' AND entity_id = old.id;
         END;
         CREATE TRIGGER memory_edges_embed_upd
         AFTER UPDATE OF subject, predicate, object ON memory_edges
         BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'fact' AND entity_id = old.id;
             DELETE FROM embedding_lsh WHERE entity_type = 'fact' AND entity_id = old.id;
         END;",
    )?;
    Ok(())
}

/// Unified contentless FTS5 over memory edges + items (trigram). Best-effort:
/// if FTS5 is unavailable, callers fall back to LIKE. Tokenizer recorded in
/// `kv_store` so a tokenizer change rebuilds once.
const FTS_TOKENIZER: &str = "trigram";
const MEMORY_FTS_TOKENIZER_KV_KEY: &str = "memory_fts_tokenizer";

fn applied_fts_tokenizer(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM kv_store WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get(0),
    )
    .ok()
}

fn record_fts_tokenizer(conn: &rusqlite::Connection, key: &str) {
    let _ = conn.execute(
        "INSERT INTO kv_store (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        rusqlite::params![key, FTS_TOKENIZER],
    );
}

fn ensure_memory_fts(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    if !table_exists(conn, "memory_edges")? || !table_exists(conn, "memory_items")? {
        return Ok(());
    }
    let has_fts: bool = conn
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_fts'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    let trigger_names = [
        "memory_edges_ai",
        "memory_edges_ad",
        "memory_edges_au",
        "memory_items_ai",
        "memory_items_ad",
        "memory_items_au",
    ];
    let triggers_missing = trigger_names.iter().any(|name| {
        conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?1")
            .and_then(|mut stmt| stmt.query_row(rusqlite::params![name], |r| r.get::<_, i32>(0)))
            .map(|c| c == 0)
            .unwrap_or(true)
    });
    let tokenizer_stale =
        applied_fts_tokenizer(conn, MEMORY_FTS_TOKENIZER_KV_KEY).as_deref() != Some(FTS_TOKENIZER);
    if !has_fts || triggers_missing || tokenizer_stale {
        let fts_sql = format!(
            "BEGIN;
            DROP TABLE IF EXISTS memory_fts;
            DROP TRIGGER IF EXISTS memory_edges_ai;
            DROP TRIGGER IF EXISTS memory_edges_ad;
            DROP TRIGGER IF EXISTS memory_edges_au;
            DROP TRIGGER IF EXISTS memory_items_ai;
            DROP TRIGGER IF EXISTS memory_items_ad;
            DROP TRIGGER IF EXISTS memory_items_au;
            CREATE VIRTUAL TABLE memory_fts USING fts5(
                body,
                entity_type UNINDEXED,
                entity_id UNINDEXED,
                content='',
                contentless_delete=1,
                contentless_unindexed=1,
                tokenize='{FTS_TOKENIZER}'
            );
            CREATE TRIGGER memory_edges_ai AFTER INSERT ON memory_edges BEGIN
                INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                VALUES (
                    new.rowid,
                    new.subject || ' ' || new.predicate || ' ' || new.object || ' ' || new.tags,
                    'edge',
                    new.id
                );
            END;
            CREATE TRIGGER memory_edges_ad AFTER DELETE ON memory_edges BEGIN
                DELETE FROM memory_fts WHERE rowid = old.rowid;
            END;
            CREATE TRIGGER memory_edges_au
            AFTER UPDATE OF subject, predicate, object, tags ON memory_edges
            BEGIN
                DELETE FROM memory_fts WHERE rowid = old.rowid;
                INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                VALUES (
                    new.rowid,
                    new.subject || ' ' || new.predicate || ' ' || new.object || ' ' || new.tags,
                    'edge',
                    new.id
                );
            END;
            CREATE TRIGGER memory_items_ai AFTER INSERT ON memory_items BEGIN
                INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                VALUES (
                    -new.rowid,
                    new.content || ' ' || new.topics || ' ' || new.entities,
                    'item',
                    new.id
                );
            END;
            CREATE TRIGGER memory_items_ad AFTER DELETE ON memory_items BEGIN
                DELETE FROM memory_fts WHERE rowid = -old.rowid;
            END;
            CREATE TRIGGER memory_items_au
            AFTER UPDATE OF content, topics, entities ON memory_items
            BEGIN
                DELETE FROM memory_fts WHERE rowid = -old.rowid;
                INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                VALUES (
                    -new.rowid,
                    new.content || ' ' || new.topics || ' ' || new.entities,
                    'item',
                    new.id
                );
            END;
            INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
            SELECT rowid, subject || ' ' || predicate || ' ' || object || ' ' || tags, 'edge', id
              FROM memory_edges;
            INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
            SELECT -rowid, content || ' ' || topics || ' ' || entities, 'item', id
              FROM memory_items;
            COMMIT;"
        );
        if let Err(e) = conn.execute_batch(&fts_sql) {
            // Prefer contentless_delete=1 (DELETE FROM works). Older SQLite may
            // lack it — fall back to classic contentless with full-column
            // 'delete' commands, then to a content-storing FTS table.
            tracing::warn!(
                "contentless_delete memory_fts unavailable ({}), trying classic contentless",
                e
            );
            let _ = conn.execute_batch("ROLLBACK");
            let classic_sql = format!(
                "BEGIN;
                DROP TABLE IF EXISTS memory_fts;
                DROP TRIGGER IF EXISTS memory_edges_ai;
                DROP TRIGGER IF EXISTS memory_edges_ad;
                DROP TRIGGER IF EXISTS memory_edges_au;
                DROP TRIGGER IF EXISTS memory_items_ai;
                DROP TRIGGER IF EXISTS memory_items_ad;
                DROP TRIGGER IF EXISTS memory_items_au;
                CREATE VIRTUAL TABLE memory_fts USING fts5(
                    body,
                    entity_type UNINDEXED,
                    entity_id UNINDEXED,
                    content='',
                    contentless_unindexed=1,
                    tokenize='{FTS_TOKENIZER}'
                );
                CREATE TRIGGER memory_edges_ai AFTER INSERT ON memory_edges BEGIN
                    INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                    VALUES (
                        new.rowid,
                        new.subject || ' ' || new.predicate || ' ' || new.object || ' ' || new.tags,
                        'edge',
                        new.id
                    );
                END;
                CREATE TRIGGER memory_edges_ad AFTER DELETE ON memory_edges BEGIN
                    INSERT INTO memory_fts(memory_fts, rowid, body, entity_type, entity_id)
                    VALUES (
                        'delete', old.rowid,
                        old.subject || ' ' || old.predicate || ' ' || old.object || ' ' || old.tags,
                        'edge', old.id
                    );
                END;
                CREATE TRIGGER memory_edges_au
                AFTER UPDATE OF subject, predicate, object, tags ON memory_edges
                BEGIN
                    INSERT INTO memory_fts(memory_fts, rowid, body, entity_type, entity_id)
                    VALUES (
                        'delete', old.rowid,
                        old.subject || ' ' || old.predicate || ' ' || old.object || ' ' || old.tags,
                        'edge', old.id
                    );
                    INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                    VALUES (
                        new.rowid,
                        new.subject || ' ' || new.predicate || ' ' || new.object || ' ' || new.tags,
                        'edge',
                        new.id
                    );
                END;
                CREATE TRIGGER memory_items_ai AFTER INSERT ON memory_items BEGIN
                    INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                    VALUES (
                        -new.rowid,
                        new.content || ' ' || new.topics || ' ' || new.entities,
                        'item',
                        new.id
                    );
                END;
                CREATE TRIGGER memory_items_ad AFTER DELETE ON memory_items BEGIN
                    INSERT INTO memory_fts(memory_fts, rowid, body, entity_type, entity_id)
                    VALUES (
                        'delete', -old.rowid,
                        old.content || ' ' || old.topics || ' ' || old.entities,
                        'item', old.id
                    );
                END;
                CREATE TRIGGER memory_items_au
                AFTER UPDATE OF content, topics, entities ON memory_items
                BEGIN
                    INSERT INTO memory_fts(memory_fts, rowid, body, entity_type, entity_id)
                    VALUES (
                        'delete', -old.rowid,
                        old.content || ' ' || old.topics || ' ' || old.entities,
                        'item', old.id
                    );
                    INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                    VALUES (
                        -new.rowid,
                        new.content || ' ' || new.topics || ' ' || new.entities,
                        'item',
                        new.id
                    );
                END;
                INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                SELECT rowid, subject || ' ' || predicate || ' ' || object || ' ' || tags, 'edge', id
                  FROM memory_edges;
                INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                SELECT -rowid, content || ' ' || topics || ' ' || entities, 'item', id
                  FROM memory_items;
                COMMIT;"
            );
            if conn.execute_batch(&classic_sql).is_ok() {
                record_fts_tokenizer(conn, MEMORY_FTS_TOKENIZER_KV_KEY);
            } else {
                let _ = conn.execute_batch("ROLLBACK");
                let normal_sql = format!(
                    "BEGIN;
                    DROP TABLE IF EXISTS memory_fts;
                    DROP TRIGGER IF EXISTS memory_edges_ai;
                    DROP TRIGGER IF EXISTS memory_edges_ad;
                    DROP TRIGGER IF EXISTS memory_edges_au;
                    DROP TRIGGER IF EXISTS memory_items_ai;
                    DROP TRIGGER IF EXISTS memory_items_ad;
                    DROP TRIGGER IF EXISTS memory_items_au;
                    CREATE VIRTUAL TABLE memory_fts USING fts5(
                        body,
                        entity_type UNINDEXED,
                        entity_id UNINDEXED,
                        tokenize='{FTS_TOKENIZER}'
                    );
                    CREATE TRIGGER memory_edges_ai AFTER INSERT ON memory_edges BEGIN
                        INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                        VALUES (
                            new.rowid,
                            new.subject || ' ' || new.predicate || ' ' || new.object || ' ' || new.tags,
                            'edge',
                            new.id
                        );
                    END;
                    CREATE TRIGGER memory_edges_ad AFTER DELETE ON memory_edges BEGIN
                        DELETE FROM memory_fts WHERE rowid = old.rowid;
                    END;
                    CREATE TRIGGER memory_edges_au
                    AFTER UPDATE OF subject, predicate, object, tags ON memory_edges
                    BEGIN
                        DELETE FROM memory_fts WHERE rowid = old.rowid;
                        INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                        VALUES (
                            new.rowid,
                            new.subject || ' ' || new.predicate || ' ' || new.object || ' ' || new.tags,
                            'edge',
                            new.id
                        );
                    END;
                    CREATE TRIGGER memory_items_ai AFTER INSERT ON memory_items BEGIN
                        INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                        VALUES (
                            -new.rowid,
                            new.content || ' ' || new.topics || ' ' || new.entities,
                            'item',
                            new.id
                        );
                    END;
                    CREATE TRIGGER memory_items_ad AFTER DELETE ON memory_items BEGIN
                        DELETE FROM memory_fts WHERE rowid = -old.rowid;
                    END;
                    CREATE TRIGGER memory_items_au
                    AFTER UPDATE OF content, topics, entities ON memory_items
                    BEGIN
                        DELETE FROM memory_fts WHERE rowid = -old.rowid;
                        INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                        VALUES (
                            -new.rowid,
                            new.content || ' ' || new.topics || ' ' || new.entities,
                            'item',
                            new.id
                        );
                    END;
                    INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                    SELECT rowid, subject || ' ' || predicate || ' ' || object || ' ' || tags, 'edge', id
                      FROM memory_edges;
                    INSERT INTO memory_fts(rowid, body, entity_type, entity_id)
                    SELECT -rowid, content || ' ' || topics || ' ' || entities, 'item', id
                      FROM memory_items;
                    COMMIT;"
                );
                if let Err(e2) = conn.execute_batch(&normal_sql) {
                    tracing::warn!("FTS5 unavailable, memory search falls back to LIKE: {}", e2);
                    let _ = conn.execute_batch("ROLLBACK");
                } else {
                    record_fts_tokenizer(conn, MEMORY_FTS_TOKENIZER_KV_KEY);
                }
            }
        } else {
            record_fts_tokenizer(conn, MEMORY_FTS_TOKENIZER_KV_KEY);
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
    // Pre-v9 shape (still checked when the legacy table is present).
    ("facts", "tags"),
    ("facts", "durability"),
    ("memory_edges", "tags"),
    ("memory_edges", "durability"),
    ("actions", "kind"),
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

/// Create or upgrade the schema to the current version (idempotent).
///
/// Version resolution:
/// - `user_version > SCHEMA_VERSION` → error (database is from a NEWER Haven).
/// - `user_version == SCHEMA_VERSION` → schema is current; the idempotent
///   full-schema pass below still runs so missing objects (e.g. an FTS table
///   dropped mid-crash) self-heal.
/// - `user_version < SCHEMA_VERSION` → run each pending migration in order.
///   A v0 database (built by a pre-versioning binary) has no stamp: if it
///   carries the required columns it is treated as current-shape and the
///   pending data migrations still run against it (each is guarded against
///   missing tables, so a genuinely fresh DB is a no-op); if any required
///   column is missing it predates the schema and is rejected with a clear
///   error — there is deliberately no upgrade path from that shape, the user
///   must delete the file and rebuild.
pub fn init_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let version = user_version(conn)?;
    if version > SCHEMA_VERSION {
        anyhow::bail!(
            "database schema version {version} is NEWER than this Haven binary \
             (supports up to {SCHEMA_VERSION}). Update Haven to open this database."
        );
    }
    if version < SCHEMA_VERSION {
        if version == 0 {
            // Pre-versioning database. If it is missing a required column it
            // predates the current shape and cannot be migrated — reject it
            // BEFORE creating anything so a later "no such column" turns into
            // one clear message.
            for (table, col) in REQUIRED_COLUMNS {
                if table_exists(conn, table)? && !column_exists(conn, table, col)? {
                    anyhow::bail!(
                        "database schema is from an old Haven version (missing {table}.{col}). \
                         The current version does not migrate such old databases; \
                         delete the database file (haven.db) and restart to create a fresh one."
                    );
                }
            }
            // A v0 database that passes the shape check is "current shape": it
            // still needs the pending data migrations (e.g. the v2 predicate
            // alias backfill) applied before stamping.
            apply_migrations(conn, 0, MIGRATIONS)?;
        } else {
            // Stamped old database: run the pending migrations in order. Each
            // migration is stamped as it completes so a crash mid-chain leaves
            // the database at a consistent, retryable version.
            apply_migrations(conn, version, MIGRATIONS)?;
        }
    }
    for sql in SCHEMA_SQL {
        conn.execute_batch(sql)
            .map_err(|e| anyhow::anyhow!("schema SQL failed: {e}\n---\n{sql}"))?;
    }
    conn.execute_batch(MEMORY_EMBEDDINGS_SCHEMA)?;
    ensure_memory_fts(conn)?;
    ensure_fact_embedding_triggers(conn)?;
    if user_version(conn)? < SCHEMA_VERSION {
        set_user_version(conn, SCHEMA_VERSION)?;
    }
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
            "actions",
            "embedding_lsh",
            "kv_store",
            "llm_usage",
            "memory_edges",
            "memory_embeddings",
            "memory_items",
            "memory_nodes",
            "messages",
            "partial_messages",
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
        assert!(
            !tables.iter().any(|n| n == "facts"),
            "legacy facts table must not exist"
        );
        assert!(
            !tables.iter().any(|n| n == "memory_episodes"),
            "legacy memory_episodes table must not exist"
        );
        assert!(
            tables.iter().any(|n| n == "memory_fts"),
            "memory_fts table should exist"
        );
        assert!(
            !tables.iter().any(|n| n == "facts_fts"),
            "facts_fts must be dropped"
        );
        assert!(
            !tables.iter().any(|n| n == "episodes_fts"),
            "episodes_fts must be dropped"
        );
        let core: Vec<_> = tables
            .iter()
            .filter(|t| !t.starts_with("memory_fts"))
            .collect();
        assert_eq!(core.len(), expected.len());
    }

    #[test]
    fn init_schema_creates_all_indexes() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let indexes = get_indexes(&conn);

        let expected = &[
            "idx_embedding_lsh_probe",
            "idx_llm_usage_session",
            "idx_memory_edges_confidence",
            "idx_memory_edges_subject",
            "idx_memory_embeddings_type",
            "idx_memory_embeddings_type_model",
            "idx_memory_items_created",
            "idx_memory_items_session",
            "idx_memory_nodes_label",
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
            .filter(|n| !n.starts_with("memory_fts"))
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
    fn init_schema_stamps_current_user_version() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn v10_migration_adds_unknown_cache_accounting_to_existing_usage() {
        let conn = create_test_conn();
        conn.execute_batch(
            "CREATE TABLE llm_usage (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO llm_usage (id, session_id, prompt_tokens)
            VALUES ('usage-old', 'ses-old', 42);",
        )
        .unwrap();

        migrate_v10_llm_usage_cache_accounting(&conn).unwrap();
        assert!(column_exists(&conn, "llm_usage", "cache_accounting").unwrap());
        let accounting: String = conn
            .query_row(
                "SELECT cache_accounting FROM llm_usage WHERE id = 'usage-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(accounting, "unknown");
    }

    #[test]
    fn v11_migration_adds_cache_miss_and_diagnostics() {
        let conn = create_test_conn();
        conn.execute_batch(
            "CREATE TABLE session_usage (session_id TEXT PRIMARY KEY);
             CREATE TABLE llm_usage (id TEXT PRIMARY KEY, session_id TEXT NOT NULL);
             INSERT INTO llm_usage (id, session_id) VALUES ('usage-old', 'ses-old');",
        )
        .unwrap();

        migrate_v11_usage_cache_diagnostics(&conn).unwrap();
        migrate_v11_usage_cache_diagnostics(&conn).unwrap();
        assert!(column_exists(&conn, "session_usage", "cache_miss_tokens").unwrap());
        assert!(column_exists(&conn, "llm_usage", "cache_miss_tokens").unwrap());
        assert!(column_exists(&conn, "llm_usage", "cache_diagnostics").unwrap());
        let miss: u32 = conn
            .query_row(
                "SELECT cache_miss_tokens FROM llm_usage WHERE id = 'usage-old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(miss, 0);
    }

    #[test]
    fn init_schema_stamps_legacy_complete_database() {
        // A pre-versioning binary left a schema with all required columns but
        // no user_version stamp (0). init_schema must treat it as current
        // shape and stamp it, not reject it.
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        set_user_version(&conn, 0).unwrap();
        init_schema(&conn).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn init_schema_rejects_newer_database() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        set_user_version(&conn, SCHEMA_VERSION + 5).unwrap();
        let err = init_schema(&conn).unwrap_err().to_string();
        assert!(
            err.contains("NEWER"),
            "expected a newer-version error, got: {err}"
        );
    }

    #[test]
    fn apply_migrations_runs_in_order_and_stamps() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let migrations = [
            Migration {
                version: 2,
                apply: |c| {
                    c.execute_batch("CREATE TABLE IF NOT EXISTS mig_v2 (id TEXT PRIMARY KEY)")
                        .map_err(anyhow::Error::from)
                },
            },
            Migration {
                version: 3,
                apply: |c| {
                    c.execute_batch("CREATE TABLE IF NOT EXISTS mig_v3 (id TEXT PRIMARY KEY)")
                        .map_err(anyhow::Error::from)
                },
            },
        ];
        apply_migrations(&conn, 1, &migrations).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 3);
        assert!(table_exists(&conn, "mig_v2").unwrap());
        assert!(table_exists(&conn, "mig_v3").unwrap());
        // Re-running from the current version is a no-op.
        apply_migrations(&conn, 3, &migrations).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 3);
    }

    #[test]
    fn v2_migration_backfills_legacy_predicate_aliases() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        // Simulate rows written by the pre-normalization binary: legacy
        // spellings plus a canonical row for the same concept.
        conn.execute_batch(
            r#"
            INSERT INTO memory_edges (id, subject, predicate, object, created_at) VALUES
                ('f1', 'user', 'workspace', 'D:/proj', '2026-01-01'),
                ('f2', 'user', 'workspace_path', 'D:/proj', '2026-01-01'),
                ('f3', 'user', 'project_path', 'D:/proj', '2026-01-01'),
                ('f4', 'user', 'employer', 'ACME', '2026-01-01'),
                ('f5', 'user', 'works_at', 'ACME', '2026-01-01'),
                ('f6', 'user', 'favorite_language', 'Rust', '2026-01-01');
            "#,
        )
        .unwrap();
        // Stamp as v1, then reopen: migration v2 must run (on memory_edges).
        set_user_version(&conn, 1).unwrap();
        init_schema(&conn).unwrap();

        let mut stmt = conn
            .prepare("SELECT predicate, object FROM memory_edges ORDER BY predicate")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("language".to_string(), "Rust".to_string()),
                ("project_path".to_string(), "D:/proj".to_string()),
                ("works_at".to_string(), "ACME".to_string()),
            ],
            "legacy aliases must be rewritten and the duplicate collapsed"
        );
    }

    #[test]
    fn v3_migration_allows_paused_awaiting_answer_status() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        // Simulate a v2 DB whose CHECK still rejects the new status.
        set_user_version(&conn, 2).unwrap();
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            CREATE TABLE sessions_v2 (
                id TEXT PRIMARY KEY,
                input_text TEXT NOT NULL DEFAULT '',
                title TEXT,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','paused','completed','failed','error')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                transcript TEXT NOT NULL DEFAULT '',
                react_state TEXT
            );
            INSERT INTO sessions_v2
                (id, input_text, title, status, created_at, updated_at, transcript, react_state)
            SELECT id, input_text, title, status, created_at, updated_at, transcript, react_state
              FROM sessions;
            DROP TABLE sessions;
            ALTER TABLE sessions_v2 RENAME TO sessions;
            PRAGMA foreign_keys=ON;
            "#,
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO sessions (id, status) VALUES ('ses-ask', 'paused_awaiting_answer')",
                [],
            )
            .is_err(),
            "v2 CHECK must reject paused_awaiting_answer"
        );
        init_schema(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        conn.execute(
            "INSERT INTO sessions (id, status) VALUES ('ses-ask', 'paused_awaiting_answer')",
            [],
        )
        .expect("v3 CHECK must accept paused_awaiting_answer");
    }

    #[test]
    fn v3_migration_repairs_orphan_sessions_v3_after_crash() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, input_text, status) VALUES ('ses-keep', 'hello', 'paused')",
            [],
        )
        .unwrap();
        // Simulate crash after DROP sessions, before RENAME.
        set_user_version(&conn, 2).unwrap();
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            CREATE TABLE sessions_v3 (
                id TEXT PRIMARY KEY,
                input_text TEXT NOT NULL DEFAULT '',
                title TEXT,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','paused','paused_awaiting_answer','completed','failed','error')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                transcript TEXT NOT NULL DEFAULT '',
                react_state TEXT
            );
            INSERT INTO sessions_v3
                (id, input_text, title, status, created_at, updated_at, transcript, react_state)
            SELECT id, input_text, title, status, created_at, updated_at, transcript, react_state
              FROM sessions;
            DROP TABLE sessions;
            PRAGMA foreign_keys=ON;
            "#,
        )
        .unwrap();
        assert!(!table_exists(&conn, "sessions").unwrap());
        assert!(table_exists(&conn, "sessions_v3").unwrap());
        init_schema(&conn).unwrap();
        assert!(table_exists(&conn, "sessions").unwrap());
        assert!(!table_exists(&conn, "sessions_v3").unwrap());
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'ses-keep'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "orphan sessions_v3 must be renamed, not wiped");
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v4_migration_allows_paused_awaiting_confirm_status() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        // Simulate a v3 DB whose CHECK still rejects the new status.
        set_user_version(&conn, 3).unwrap();
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            CREATE TABLE sessions_v3 (
                id TEXT PRIMARY KEY,
                input_text TEXT NOT NULL DEFAULT '',
                title TEXT,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','paused','paused_awaiting_answer','completed','failed','error')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                transcript TEXT NOT NULL DEFAULT '',
                react_state TEXT
            );
            INSERT INTO sessions_v3
                (id, input_text, title, status, created_at, updated_at, transcript, react_state)
            SELECT id, input_text, title, status, created_at, updated_at, transcript, react_state
              FROM sessions;
            DROP TABLE sessions;
            ALTER TABLE sessions_v3 RENAME TO sessions;
            PRAGMA foreign_keys=ON;
            "#,
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO sessions (id, status) VALUES ('ses-conf', 'paused_awaiting_confirm')",
                [],
            )
            .is_err(),
            "v3 CHECK must reject paused_awaiting_confirm"
        );
        init_schema(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        conn.execute(
            "INSERT INTO sessions (id, status) VALUES ('ses-conf', 'paused_awaiting_confirm')",
            [],
        )
        .expect("v4 CHECK must accept paused_awaiting_confirm");
    }

    #[test]
    fn v7_migration_allows_peer_kickoff_message_type() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, input_text, status) VALUES ('ses-p', 'hi', 'pending')",
            [],
        )
        .unwrap();
        // Simulate a v6 DB whose CHECK still rejects peer_kickoff.
        set_user_version(&conn, 6).unwrap();
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            CREATE TABLE messages_v6 (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
                content TEXT NOT NULL,
                message_type TEXT CHECK(message_type IN ('text','thought','action','observation','reasoning')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                tool_call_id TEXT,
                attachments TEXT,
                voice INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO messages_v6
                (id, session_id, role, content, message_type, created_at, tool_call_id, attachments, voice)
            SELECT id, session_id, role, content, message_type, created_at, tool_call_id, attachments, voice
              FROM messages;
            DROP TABLE messages;
            ALTER TABLE messages_v6 RENAME TO messages;
            CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
            PRAGMA foreign_keys=ON;
            "#,
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO messages (id, session_id, role, content, message_type)
                 VALUES ('msg-pk', 'ses-p', 'user', 'brief', 'peer_kickoff')",
                [],
            )
            .is_err(),
            "v6 CHECK must reject peer_kickoff"
        );
        init_schema(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, message_type)
             VALUES ('msg-pk', 'ses-p', 'user', 'brief', 'peer_kickoff')",
            [],
        )
        .expect("v7 CHECK must accept peer_kickoff");
    }

    #[test]
    fn v5_migration_adds_episode_columns_and_company_alias() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        // Simulate a v4 DB: pre-graph tables, episodes without topics/entities,
        // and bare `company` facts. Stamp back and let v5..v9 upgrade.
        set_user_version(&conn, 4).unwrap();
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS memory_fts;
            DROP TABLE IF EXISTS memory_edges;
            DROP TABLE IF EXISTS memory_items;
            DROP TABLE IF EXISTS memory_nodes;
            CREATE TABLE memory_episodes (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO sessions (id, input_text, status)
            VALUES ('ses-v5', 'hi', 'pending');
            INSERT INTO memory_episodes (id, session_id, summary, created_at)
            VALUES ('msg-ep1', 'ses-v5', 'old summary', '2026-01-01');
            CREATE TABLE facts (
                id TEXT PRIMARY KEY,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'inferred',
                confidence REAL NOT NULL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                tags TEXT NOT NULL DEFAULT '[]',
                durability REAL NOT NULL DEFAULT 1.0,
                mention_count INTEGER NOT NULL DEFAULT 0,
                last_seen_at TEXT,
                source_ref TEXT
            );
            INSERT INTO facts (id, subject, predicate, object, source, confidence, created_at)
            VALUES
              ('c1', 'user', 'company', 'Acme', 'inferred', 0.9, '2026-01-01'),
              ('c2', 'user', 'company', 'Acme', 'inferred', 0.5, '2026-01-02'),
              ('d1', 'user', 'likes', 'Tea', 'inferred', 0.8, '2026-01-01'),
              ('d2', 'user', 'likes', 'Tea', 'inferred', 0.7, '2026-01-02');
            "#,
        )
        .unwrap();
        init_schema(&conn).unwrap();

        assert!(column_exists(&conn, "memory_items", "topics").unwrap());
        assert!(column_exists(&conn, "memory_items", "entities").unwrap());
        let item_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_items WHERE id = 'msg-ep1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(item_count, 1);
        let pred: String = conn
            .query_row(
                "SELECT predicate FROM memory_edges WHERE id = 'c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pred, "works_at");
        let works_at: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE predicate = 'works_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(works_at, 1);
        let likes: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edges WHERE predicate = 'likes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(likes, 2);
        assert!(!table_exists(&conn, "facts").unwrap());
        assert!(!table_exists(&conn, "memory_episodes").unwrap());
        let has_fts: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_fts, 1);
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v9_migration_copies_facts_and_episodes_into_graph() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        set_user_version(&conn, 8).unwrap();
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS memory_fts;
            DROP TABLE IF EXISTS memory_edges;
            DROP TABLE IF EXISTS memory_items;
            DROP TABLE IF EXISTS memory_nodes;
            CREATE TABLE memory_episodes (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                topics TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE facts (
                id TEXT PRIMARY KEY,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'inferred',
                confidence REAL NOT NULL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                tags TEXT NOT NULL DEFAULT '[]',
                durability REAL NOT NULL DEFAULT 1.0,
                mention_count INTEGER NOT NULL DEFAULT 0,
                last_seen_at TEXT,
                source_ref TEXT
            );
            INSERT INTO sessions (id, input_text, status)
            VALUES ('ses-v9', 'hi', 'pending');
            INSERT INTO memory_episodes (id, session_id, summary, topics, entities, created_at)
            VALUES ('msg-sum', 'ses-v9', 'theme summary', '[\"ui\"]', '[\"Alice\"]', '2026-01-01');
            INSERT INTO facts
                (id, subject, predicate, object, source, confidence, created_at, source_ref)
            VALUES
                ('fact-1', 'user', 'likes', 'Rust', 'inferred', 0.9, '2026-01-01',
                 '{"message_id":"msg-sum","snippet":"likes Rust"}'),
                ('fact-2', 'Alice', 'role', 'dev', 'user', 1.0, '2026-01-02', NULL);
            "#,
        )
        .unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(!table_exists(&conn, "facts").unwrap());
        assert!(!table_exists(&conn, "memory_episodes").unwrap());
        let (kind, content): (String, String) = conn
            .query_row(
                "SELECT kind, content FROM memory_items WHERE id = 'msg-sum'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "episode_summary");
        assert_eq!(content, "theme summary");
        let (subj, prov_item, snippet): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT subject, provenance_item_id, provenance_snippet
                 FROM memory_edges WHERE id = 'fact-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(subj, "user");
        assert_eq!(prov_item.as_deref(), Some("msg-sum"));
        assert_eq!(snippet.as_deref(), Some("likes Rust"));
        let node_kinds: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_nodes WHERE kind IN ('user','concept')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(node_kinds >= 2);
        let user_kind: String = conn
            .query_row(
                "SELECT kind FROM memory_nodes WHERE label = 'user'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(user_kind, "user");
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
        for name in ["memory_edges_embed_del", "memory_edges_embed_upd"] {
            let count: i32 = conn
                .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?1")
                .unwrap()
                .query_row(rusqlite::params![name], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "trigger {} must exist", name);
        }
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, created_at)
             VALUES ('f1', 'user', 'likes', 'Rust', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('fact', 'f1', 'm', X'0102', 'x')",
            [],
        )
        .unwrap();
        // Confidence-only updates must keep the embedding (surface text unchanged).
        conn.execute(
            "UPDATE memory_edges SET confidence = 0.5 WHERE id = 'f1'",
            [],
        )
        .unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE entity_id='f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "reinforcement/demotion must not drop embeddings");
        conn.execute(
            "UPDATE memory_edges SET object = 'Golang' WHERE id = 'f1'",
            [],
        )
        .unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE entity_id='f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "SPO UPDATE must invalidate the embedding");
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('fact', 'f1', 'm', X'0102', 'x')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM memory_edges WHERE id = 'f1'", [])
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
    fn memory_edges_defaults_apply() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, created_at)
             VALUES ('f1', 'user', 'likes', 'Rust', '2026-01-01')",
            [],
        )
        .unwrap();
        let (durability, tags): (f64, String) = conn
            .query_row(
                "SELECT durability, tags FROM memory_edges WHERE id='f1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(durability, 1.0);
        assert_eq!(tags, "[]");
    }

    #[test]
    fn fts_triggers_sync_edges() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, tags, created_at)
             VALUES ('f1', 'user', 'likes', 'Rust', '[\"dev\"]', '2026-01-01')",
            [],
        )
        .unwrap();
        let hits: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"Rust\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "FTS index must contain the inserted edge");
    }

    #[test]
    fn fts_triggers_delete_and_update_surface_text() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, created_at)
             VALUES ('f1', 'user', 'likes', 'Rust', '2026-01-01')",
            [],
        )
        .unwrap();
        // Use ≥3-char tokens so trigram MATCH can hit.
        conn.execute(
            "UPDATE memory_edges SET object = 'Golang' WHERE id = 'f1'",
            [],
        )
        .unwrap();
        let rust_hits: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"Rust\"'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert_eq!(rust_hits, 0, "old surface text must leave FTS after UPDATE");
        let go_hits: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"Golang\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(go_hits, 1, "new surface text must be indexed after UPDATE");

        // Reinforcement-only update must not drop the FTS row.
        conn.execute(
            "UPDATE memory_edges SET mention_count = 3, confidence = 0.95 WHERE id = 'f1'",
            [],
        )
        .unwrap();
        let still: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"Golang\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still, 1, "reinforcement must not rewrite/drop FTS");

        conn.execute("DELETE FROM memory_edges WHERE id = 'f1'", [])
            .unwrap();
        let after_del: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"Golang\"'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert_eq!(after_del, 0, "DELETE must remove FTS row");
    }

    #[test]
    fn fts_trigram_matches_chinese_substrings() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, created_at)
             VALUES ('f1', 'user', 'likes', '喝咖啡和写代码', '2026-01-01')",
            [],
        )
        .unwrap();
        let hits: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"喝咖啡\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "trigram FTS must match Chinese substrings");
    }

    #[test]
    fn fts_tokenizer_recorded_and_stable_across_reinit() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let recorded: String = conn
            .query_row(
                "SELECT value FROM kv_store WHERE key = 'memory_fts_tokenizer'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, FTS_TOKENIZER);
        init_schema(&conn).unwrap();
        let table_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
        conn.execute(
            "DELETE FROM kv_store WHERE key = 'memory_fts_tokenizer'",
            [],
        )
        .unwrap();
        init_schema(&conn).unwrap();
        let rebuilt: String = conn
            .query_row(
                "SELECT value FROM kv_store WHERE key = 'memory_fts_tokenizer'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rebuilt, FTS_TOKENIZER);
    }
}
