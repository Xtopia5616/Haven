//! Current SQLite schema for Haven's local store.
//!
//! The test version deliberately has one schema contract instead of carrying
//! an in-process upgrade framework. A database created by an older build is a
//! reset boundary: accepting a partially migrated shape would make the
//! session projections and memory graph appear valid while silently losing
//! recovery semantics. The current schema is created idempotently, while its
//! version stamp rejects both older and newer database contracts.

/// Current database contract. Any schema change requires a fresh database.
pub const SCHEMA_VERSION: i32 = 16;
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
        voice INTEGER NOT NULL DEFAULT 0,
        ingress_seq INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS message_ingress_cursors (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        last_ingress_seq INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS react_checkpoints (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        revision INTEGER NOT NULL DEFAULT 0,
        event_cursor INTEGER NOT NULL DEFAULT 0,
        message_ingress_seq INTEGER NOT NULL DEFAULT 0,
        step_seq INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE TABLE IF NOT EXISTS session_step_cursors (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        last_step_seq INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS session_steps (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        step_number INTEGER NOT NULL,
        action_index INTEGER NOT NULL DEFAULT 0,
        tool_name TEXT NOT NULL,
        input TEXT NOT NULL DEFAULT '{}',
        output TEXT NOT NULL DEFAULT '{}',
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK(status IN ('pending','running','completed','failed','cancelled','unknown')),
        is_high_risk INTEGER NOT NULL DEFAULT 0,
        confirmed INTEGER,
        started_at TEXT,
        completed_at TEXT,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        silent INTEGER NOT NULL DEFAULT 0,
        thought TEXT,
        action_tool TEXT,
        action_input TEXT,
        tool_call_id TEXT,
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
        kind TEXT NOT NULL CHECK(kind IN ('user','concept')),
        label TEXT NOT NULL CHECK(length(trim(label)) > 0),
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
        content TEXT NOT NULL CHECK(length(trim(content)) > 0),
        topics TEXT NOT NULL DEFAULT '[]',
        entities TEXT NOT NULL DEFAULT '[]',
        created_at TEXT NOT NULL
    )",
    // Today's facts as SPO edges. Keeps `fact-*` ids. `entity_type='fact'`
    // in memory_embeddings points directly at these edge rows.
    "CREATE TABLE IF NOT EXISTS memory_edges (
        id TEXT PRIMARY KEY,
        subject TEXT NOT NULL CHECK(length(trim(subject)) > 0),
        subject_id TEXT REFERENCES memory_nodes(id) ON DELETE SET NULL,
        predicate TEXT NOT NULL CHECK(length(trim(predicate)) > 0),
        object TEXT NOT NULL CHECK(length(trim(object)) > 0),
        object_id TEXT REFERENCES memory_nodes(id) ON DELETE SET NULL,
        source TEXT NOT NULL DEFAULT 'inferred'
            CHECK(source IN ('user','inferred')),
        confidence REAL NOT NULL DEFAULT 1.0 CHECK(confidence >= 0.0 AND confidence <= 1.0),
        created_at TEXT NOT NULL,
        tags TEXT NOT NULL DEFAULT '[]',
        durability REAL NOT NULL DEFAULT 1.0 CHECK(durability >= 0.0 AND durability <= 1.0),
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
         context_tokens INTEGER NOT NULL DEFAULT 0,
         context_window INTEGER,
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
         context_tokens INTEGER NOT NULL DEFAULT 0,
         context_window INTEGER,
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

/// Vector index for semantic memory. `entity_type` selects the owning memory
/// domain (`fact` = `memory_edges`, `episode` = `memory_items`). The domain is
/// intentionally closed: a polymorphic index without a closed vocabulary is
/// impossible to validate and used to preserve obsolete rows.
const MEMORY_EMBEDDINGS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memory_embeddings (
    entity_type TEXT NOT NULL CHECK(entity_type IN ('fact', 'episode')),
    entity_id TEXT NOT NULL CHECK(length(trim(entity_id)) > 0),
    model TEXT NOT NULL CHECK(length(trim(model)) > 0),
    vector BLOB NOT NULL CHECK(length(vector) > 0 AND length(vector) % 4 = 0),
    text TEXT NOT NULL CHECK(length(trim(text)) > 0),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (entity_type, entity_id, model)
);
CREATE INDEX IF NOT EXISTS idx_memory_embeddings_type ON memory_embeddings(entity_type);
CREATE INDEX IF NOT EXISTS idx_memory_embeddings_type_model
    ON memory_embeddings(entity_type, model, updated_at);
CREATE TABLE IF NOT EXISTS embedding_lsh (
    entity_type TEXT NOT NULL CHECK(entity_type IN ('fact', 'episode')),
    entity_id TEXT NOT NULL CHECK(length(trim(entity_id)) > 0),
    model TEXT NOT NULL CHECK(length(trim(model)) > 0),
    bucket INTEGER NOT NULL,
    PRIMARY KEY (entity_type, entity_id, model)
);
CREATE INDEX IF NOT EXISTS idx_embedding_lsh_probe
    ON embedding_lsh(entity_type, model, bucket);
";

/// Triggers that keep `memory_embeddings` in sync with their owning memory
/// rows. Invalidate on DELETE, and on UPDATE only when the embedded surface
/// text changes (fact reinforcement must not drop vectors).
fn ensure_fact_embedding_triggers(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS facts_embed_del;
         DROP TRIGGER IF EXISTS facts_embed_upd;
         DROP TRIGGER IF EXISTS memory_edges_embed_del;
         DROP TRIGGER IF EXISTS memory_edges_embed_upd;
         DROP TRIGGER IF EXISTS memory_items_embed_del;
         DROP TRIGGER IF EXISTS memory_items_embed_upd;
         CREATE TRIGGER memory_edges_embed_del AFTER DELETE ON memory_edges BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'fact' AND entity_id = old.id;
             DELETE FROM embedding_lsh WHERE entity_type = 'fact' AND entity_id = old.id;
         END;
         CREATE TRIGGER memory_edges_embed_upd
         AFTER UPDATE OF subject, predicate, object ON memory_edges
         BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'fact' AND entity_id = old.id;
             DELETE FROM embedding_lsh WHERE entity_type = 'fact' AND entity_id = old.id;
         END;
         CREATE TRIGGER memory_items_embed_del AFTER DELETE ON memory_items BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'episode' AND entity_id = old.id;
             DELETE FROM embedding_lsh WHERE entity_type = 'episode' AND entity_id = old.id;
         END;
         CREATE TRIGGER memory_items_embed_upd
         AFTER UPDATE OF content ON memory_items
         BEGIN
             DELETE FROM memory_embeddings WHERE entity_type = 'episode' AND entity_id = old.id;
             DELETE FROM embedding_lsh WHERE entity_type = 'episode' AND entity_id = old.id;
         END;",
    )?;
    // The embedding table intentionally cannot have a polymorphic foreign key.
    // Repair rows created outside the repository write boundary while opening
    // the database, rather than waiting for periodic maintenance.
    conn.execute_batch(
        "DELETE FROM memory_embeddings
          WHERE (entity_type = 'fact' AND entity_id NOT IN (SELECT id FROM memory_edges))
             OR (entity_type = 'episode' AND entity_id NOT IN (SELECT id FROM memory_items));
         DELETE FROM embedding_lsh
          WHERE (entity_type, entity_id, model) NOT IN (
              SELECT entity_type, entity_id, model FROM memory_embeddings
          );",
    )?;
    Ok(())
}

/// Unified FTS5 over memory edges + items (trigram). It is part of the current
/// database contract; tokenizer state in `kv_store` makes a tokenizer change
/// rebuild the derived index exactly once.
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

fn record_fts_tokenizer(conn: &rusqlite::Connection, key: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO kv_store (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        rusqlite::params![key, FTS_TOKENIZER],
    )?;
    Ok(())
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
        if let Err(error) = conn.execute_batch(&fts_sql) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(anyhow::anyhow!(
                "memory_fts current contract could not be created with the bundled SQLite FTS5 feature: {error}"
            ));
        }
        record_fts_tokenizer(conn, MEMORY_FTS_TOKENIZER_KV_KEY)?;
    }
    Ok(())
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> anyhow::Result<bool> {
    Ok(conn
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row(rusqlite::params![table], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false))
}

const REQUIRED_COLUMNS: &[(&str, &str)] = &[
    ("sessions", "transcript"),
    ("messages", "voice"),
    ("session_steps", "thought"),
    ("memory_nodes", "kind"),
    ("memory_items", "content"),
    ("memory_edges", "durability"),
    ("actions", "kind"),
];

fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    Ok(conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2")?
        .query_row(rusqlite::params![table, column], |row| row.get::<_, i32>(0))?
        > 0)
}

fn validate_current_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    for (table, column) in REQUIRED_COLUMNS {
        anyhow::ensure!(
            table_exists(conn, table)? && column_exists(conn, table, column)?,
            "current schema is incomplete: missing {table}.{column}; delete haven.db and restart"
        );
    }
    for table in ["facts", "memory_episodes"] {
        anyhow::ensure!(
            !table_exists(conn, table)?,
            "current schema contains removed table {table}; delete haven.db and restart"
        );
    }
    Ok(())
}

fn has_user_tables(conn: &rusqlite::Connection) -> anyhow::Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         )",
        [],
        |row| row.get(0),
    )?)
}

fn user_version(conn: &rusqlite::Connection) -> anyhow::Result<i32> {
    Ok(conn
        .prepare("PRAGMA user_version")?
        .query_row([], |row| row.get(0))?)
}

fn set_user_version(conn: &rusqlite::Connection, version: i32) -> anyhow::Result<()> {
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    Ok(())
}

/// Create the current schema or reject a database from another contract.
///
/// There is intentionally no in-process migration path. Memory, session
/// projection, and snapshot formats are one atomic local contract; mixing
/// versions would make a database look readable while producing incomplete
/// recovery state. Users must reset an older database at the release boundary.
pub fn init_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let version = user_version(conn)?;
    if version > SCHEMA_VERSION {
        anyhow::bail!(
            "database schema version {version} is NEWER than this Haven binary \
             (supports up to {SCHEMA_VERSION}). Update Haven to open this database."
        );
    }
    if version != 0 && version != SCHEMA_VERSION {
        anyhow::bail!(
            "database schema version {version} is incompatible with this Haven build \
             (requires {SCHEMA_VERSION}); delete haven.db and restart to create a fresh database."
        );
    }
    if version == 0 && has_user_tables(conn)? {
        anyhow::bail!(
            "unversioned Haven database is incompatible with the current memory contract; \
             delete haven.db and restart to create a fresh database."
        );
    }
    if version == SCHEMA_VERSION {
        validate_current_schema(conn)?;
    }
    for sql in SCHEMA_SQL {
        conn.execute_batch(sql)
            .map_err(|e| anyhow::anyhow!("schema SQL failed: {e}\n---\n{sql}"))?;
    }
    conn.execute_batch(MEMORY_EMBEDDINGS_SCHEMA)?;
    ensure_memory_fts(conn)?;
    ensure_fact_embedding_triggers(conn)?;
    if version == 0 {
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
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn
    }

    fn user_tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn init_schema_creates_current_contract() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();

        for table in [
            "actions",
            "embedding_lsh",
            "kv_store",
            "llm_usage",
            "memory_edges",
            "memory_embeddings",
            "memory_items",
            "memory_nodes",
            "message_ingress_cursors",
            "messages",
            "partial_messages",
            "react_checkpoints",
            "session_steps",
            "session_step_cursors",
            "session_usage",
            "sessions",
        ] {
            assert!(user_tables(&conn).iter().any(|name| name == table));
        }

        let version = user_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(table_exists(&conn, "memory_fts").unwrap());
    }

    #[test]
    fn init_schema_is_idempotent() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        let before = user_tables(&conn);
        init_schema(&conn).unwrap();
        assert_eq!(user_tables(&conn), before);
    }

    #[test]
    fn init_schema_rejects_partial_current_contract() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute_batch("DROP TABLE messages;").unwrap();

        let error = init_schema(&conn).unwrap_err().to_string();
        assert!(error.contains("current schema is incomplete"));
        assert!(error.contains("messages.voice"));
    }

    #[test]
    fn init_schema_rejects_old_and_new_contracts() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();

        set_user_version(&conn, SCHEMA_VERSION - 1).unwrap();
        let old = init_schema(&conn).unwrap_err().to_string();
        assert!(old.contains("incompatible"));

        set_user_version(&conn, SCHEMA_VERSION + 1).unwrap();
        let new = init_schema(&conn).unwrap_err().to_string();
        assert!(new.contains("NEWER"));
    }

    #[test]
    fn init_schema_rejects_unversioned_existing_database() {
        let conn = create_test_conn();
        conn.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY);")
            .unwrap();

        let error = init_schema(&conn).unwrap_err().to_string();
        assert!(error.contains("unversioned"));
        assert!(error.contains("delete haven.db"));
    }

    #[test]
    fn current_memory_schema_rejects_invalid_domains_and_values() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, created_at)
             VALUES ('fact-1', 'user', 'likes', 'Rust', '2026-01-01')",
            [],
        )
        .unwrap();

        assert!(
            conn.execute(
                "INSERT INTO memory_embeddings
                    (entity_type, entity_id, model, vector, text)
                 VALUES ('tool', 'fact-1', 'm', X'00000000', 'Rust')",
                [],
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "INSERT INTO memory_embeddings
                    (entity_type, entity_id, model, vector, text)
                 VALUES ('fact', 'fact-1', 'm', X'00', 'Rust')",
                [],
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "INSERT INTO memory_edges
                    (id, subject, predicate, object, confidence, created_at)
                 VALUES ('fact-2', ' ', 'likes', 'Rust', 1.0, '2026-01-01')",
                [],
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "INSERT INTO memory_edges
                    (id, subject, predicate, object, confidence, created_at)
                 VALUES ('fact-3', 'user', 'likes', 'Rust', 1.1, '2026-01-01')",
                [],
            )
            .is_err()
        );
    }

    #[test]
    fn embedding_triggers_keep_derived_rows_consistent() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, created_at)
             VALUES ('fact-1', 'user', 'likes', 'Rust', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings
                (entity_type, entity_id, model, vector, text)
             VALUES ('fact', 'fact-1', 'm', X'00000000', 'user likes Rust')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE memory_edges SET confidence = 0.5 WHERE id = 'fact-1'",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE entity_id = 'fact-1'",
                [],
                |row| row.get::<_, i32>(0),
            )
            .unwrap(),
            1
        );
        conn.execute(
            "UPDATE memory_edges SET object = 'Golang' WHERE id = 'fact-1'",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE entity_id = 'fact-1'",
                [],
                |row| row.get::<_, i32>(0),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn fts_triggers_index_current_memory() {
        let conn = create_test_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memory_edges (id, subject, predicate, object, tags, created_at)
             VALUES ('fact-1', 'user', 'likes', 'Rust', '[\"dev\"]', '2026-01-01')",
            [],
        )
        .unwrap();
        let hits: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts
                 WHERE entity_type = 'edge' AND memory_fts MATCH '\"Rust\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }
}
