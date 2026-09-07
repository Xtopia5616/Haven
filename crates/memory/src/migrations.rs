//! Versioned SQLite migrations for the Memory store.
//!
//! This module owns historical data/schema transitions only. The current
//! idempotent schema and repair pass remain in the parent schema module.
//! Keeping the migration catalog here makes version order and upgrade behavior
//! reviewable without mixing it with the current table definition.

/// Current schema version. Bump whenever `MIGRATIONS` gains an entry.
pub(super) const SCHEMA_VERSION: i32 = 13;

/// A single forward migration: bumps the database from `version - 1` to
/// `version`. Entries run in order on every open of an older database.
pub(super) struct Migration {
    pub(super) version: i32,
    pub(super) apply: fn(&rusqlite::Connection) -> anyhow::Result<()>,
}

/// Ordered list of migrations, oldest first. Each entry's `version` must be
/// `SCHEMA_VERSION - len`..=SCHEMA_VERSION and strictly increasing; version 1
/// is the initial full schema (no migration). History of migrations that
/// altered an existing schema:
///
/// - v2: backfill legacy fact predicate spellings to the canonical aliases
///   introduced by `normalize_predicate` (workspace → project_path, etc.) and
///   collapse the resulting duplicates, so single-valued constraints and the
///   "forget this fact" path work against rows written by older binaries.
/// - v3: allow `paused_awaiting_answer` on `sessions.status` (Phase 4 / F2)
///   so ask-gate survives process restart without JSON heuristics.
/// - v4: allow `paused_awaiting_confirm` on `sessions.status` (Phase 5 / E3)
///   so safety-confirm pause survives process restart.
/// - v5: episode `topics`/`entities` JSON columns (P2-10 / L6); backfill
///   bare `company` → `works_at` predicate alias (P2-11).
/// - v6: `embedding_lsh` side table for large-partition ANN probing (M5).
/// - v7: allow `peer_kickoff` on `messages.message_type` (Plan A multi-agent
///   spawn brief rows).
/// - v8: prompt-cache token columns on `session_usage` / `llm_usage`.
/// - v9: typed memory graph — `memory_nodes` / `memory_edges` / `memory_items`
///   replace `facts` / `memory_episodes`; unified contentless `memory_fts`.
/// - v10: per-call prompt-cache accounting provenance, so mixed providers do
///   not infer cache-hit rates from aggregate token values.
/// - v11: cache miss totals and non-sensitive per-call cache diagnostics.
/// - v12: stable action ordering/provider tool-call identity plus explicit
///   cancelled/unknown action outcomes on `session_steps`.
/// - v13: monotonic per-session message ingress sequence used as the durable
///   resume cursor, independent of wall-clock timestamps.
pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 2,
        apply: migrate_v2_backfill_predicate_aliases,
    },
    Migration {
        version: 3,
        apply: migrate_v3_paused_awaiting_answer_status,
    },
    Migration {
        version: 4,
        apply: migrate_v4_paused_awaiting_confirm_status,
    },
    Migration {
        version: 5,
        apply: migrate_v5_episodes_structured_and_company_alias,
    },
    Migration {
        version: 6,
        apply: migrate_v6_embedding_lsh,
    },
    Migration {
        version: 7,
        apply: migrate_v7_peer_kickoff_message_type,
    },
    Migration {
        version: 8,
        apply: migrate_v8_usage_cache_tokens,
    },
    Migration {
        version: 9,
        apply: migrate_v9_memory_graph,
    },
    Migration {
        version: 10,
        apply: migrate_v10_llm_usage_cache_accounting,
    },
    Migration {
        version: 11,
        apply: migrate_v11_usage_cache_diagnostics,
    },
    Migration {
        version: 12,
        apply: migrate_v12_session_steps,
    },
    Migration {
        version: 13,
        apply: migrate_v13_message_ingress_seq,
    },
];

/// Add invocation identity and expand the action-step outcome CHECK in one
/// migration. SQLite cannot alter a CHECK in place, so old tables are rebuilt
/// after the identity columns are added.
pub(super) fn migrate_v12_session_steps(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    if !table_exists(conn, "session_steps")? {
        return Ok(());
    }

    if !column_exists(conn, "session_steps", "action_index")? {
        conn.execute(
            "ALTER TABLE session_steps ADD COLUMN action_index INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !column_exists(conn, "session_steps", "tool_call_id")? {
        conn.execute("ALTER TABLE session_steps ADD COLUMN tool_call_id TEXT", [])?;
    }

    let table_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'session_steps'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    if table_sql.contains("'cancelled'") && table_sql.contains("'unknown'") {
        return Ok(());
    }

    conn.execute_batch("DROP TABLE IF EXISTS session_steps_v12")?;
    conn.execute_batch("PRAGMA foreign_keys=OFF")?;
    let rebuild = conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        CREATE TABLE session_steps_v12 (
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
        );
        INSERT INTO session_steps_v12
            (id, session_id, step_number, action_index, tool_name, input, output, status,
             is_high_risk, confirmed, started_at, completed_at, created_at,
             silent, thought, action_tool, action_input, tool_call_id, observation)
        SELECT id, session_id, step_number, action_index, tool_name, input, output,
               CASE WHEN status = 'error' THEN 'failed' ELSE status END,
               is_high_risk, confirmed, started_at, completed_at, created_at,
               silent, thought, action_tool, action_input, tool_call_id, observation
          FROM session_steps;
        DROP TABLE session_steps;
        ALTER TABLE session_steps_v12 RENAME TO session_steps;
        CREATE INDEX IF NOT EXISTS idx_session_steps_session ON session_steps(session_id);
        COMMIT;
        "#,
    );
    let restore = conn.execute_batch("PRAGMA foreign_keys=ON");
    rebuild?;
    restore?;
    Ok(())
}

/// Add a durable, monotonic ingress cursor to message rows. Existing rows are
/// numbered in their historical `(created_at, rowid)` order. New writes use a
/// separate cursor row inside the same transaction; deleting messages during
/// rollback therefore cannot make a future input reuse an old sequence.
pub(super) fn migrate_v13_message_ingress_seq(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_ingress_cursors (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            last_ingress_seq INTEGER NOT NULL DEFAULT 0
        )",
    )?;
    if !table_exists(conn, "messages")? {
        return Ok(());
    }
    if !column_exists(conn, "messages", "ingress_seq")? {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN ingress_seq INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    conn.execute_batch(
        r#"
        WITH ordered AS (
            SELECT rowid AS message_rowid,
                   ROW_NUMBER() OVER (
                       PARTITION BY session_id
                       ORDER BY created_at ASC, rowid ASC
                   ) AS seq
            FROM messages
        )
        UPDATE messages
           SET ingress_seq = (
               SELECT seq FROM ordered
                WHERE ordered.message_rowid = messages.rowid
           );
        CREATE INDEX IF NOT EXISTS idx_messages_session_ingress_seq
            ON messages(session_id, ingress_seq);
        INSERT INTO message_ingress_cursors (session_id, last_ingress_seq)
        SELECT session_id, MAX(ingress_seq)
          FROM messages
         GROUP BY session_id
        ON CONFLICT(session_id) DO UPDATE SET
            last_ingress_seq = MAX(message_ingress_cursors.last_ingress_seq,
                                   excluded.last_ingress_seq);
        "#,
    )?;
    Ok(())
}

/// Rewrite pre-normalization predicate spellings to the canonical alias (the
/// same map as `haven_memory::repositories::facts::normalize_predicate`),
/// then collapse rows that became duplicates on (subject, predicate, object).
/// The keeper rule mirrors `dedup_facts`: highest confidence, then newest
/// `created_at`. Operates on `facts` (pre-v9) or `memory_edges` (post-v9
/// re-run / test stamp-back). No-op when neither table exists.
fn migrate_v2_backfill_predicate_aliases(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let Some(table) = legacy_fact_table(conn)? else {
        return Ok(());
    };
    conn.execute_batch(&format!(
        r#"
        UPDATE {table} SET predicate = 'project_path'
         WHERE predicate IN ('workspace','workspace_path','project_location','working_directory','working_dir');
        UPDATE {table} SET predicate = 'works_at'
         WHERE predicate IN ('employer','company_name','company');
        UPDATE {table} SET predicate = 'language'
         WHERE predicate IN ('favorite_language','preferred_language');
        UPDATE {table} SET predicate = 'verbosity'
         WHERE predicate IN ('preferred_verbosity','verbosity_level');
        UPDATE {table} SET predicate = 'shell'
         WHERE predicate IN ('preferred_shell','shell_choice');
        UPDATE {table} SET predicate = 'os'
         WHERE predicate IN ('os_name','operating_system');

        DELETE FROM {table}
         WHERE id NOT IN (
             SELECT id FROM (
                 SELECT id, ROW_NUMBER() OVER (
                     PARTITION BY subject, predicate, object
                     ORDER BY confidence DESC, created_at DESC
                 ) AS rn FROM {table}
             ) WHERE rn = 1
         );
        "#
    ))?;
    Ok(())
}

/// Pre-v9 `facts` or post-v9 `memory_edges` — whichever holds SPO rows.
fn legacy_fact_table(conn: &rusqlite::Connection) -> anyhow::Result<Option<&'static str>> {
    if table_exists(conn, "facts")? {
        Ok(Some("facts"))
    } else if table_exists(conn, "memory_edges")? {
        Ok(Some("memory_edges"))
    } else {
        Ok(None)
    }
}

/// Expand `sessions.status` CHECK to include `paused_awaiting_answer`.
/// SQLite cannot ALTER a CHECK constraint in place, so rebuild the table.
///
/// Crash-safe:
/// - Interrupted prior attempt with only `sessions_v3` left → finish rename.
/// - Rebuild runs in one transaction so DROP+RENAME either both commit or
///   both roll back (never leave sessions missing while stamping v3).
fn migrate_v3_paused_awaiting_answer_status(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let has_sessions = table_exists(conn, "sessions")?;
    let has_v3 = table_exists(conn, "sessions_v3")?;
    if !has_sessions && has_v3 {
        // Prior crash after DROP, before RENAME: finish the rename.
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            ALTER TABLE sessions_v3 RENAME TO sessions;
            PRAGMA foreign_keys=ON;
            "#,
        )?;
        return Ok(());
    }
    if !has_sessions {
        // Neither table — SCHEMA_SQL will create an empty sessions table.
        return Ok(());
    }
    if has_v3 {
        // Leftover from a failed CREATE before DROP; drop and rebuild cleanly.
        conn.execute_batch("DROP TABLE IF EXISTS sessions_v3")?;
    }
    // foreign_keys cannot change inside a transaction; flip it off first.
    conn.execute_batch("PRAGMA foreign_keys=OFF")?;
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
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
        COMMIT;
        "#,
    )?;
    conn.execute_batch("PRAGMA foreign_keys=ON")?;
    Ok(())
}

/// Expand `sessions.status` CHECK to include `paused_awaiting_confirm`.
/// Same rebuild pattern as v3 (SQLite cannot ALTER CHECK in place).
fn migrate_v4_paused_awaiting_confirm_status(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let has_sessions = table_exists(conn, "sessions")?;
    let has_v4 = table_exists(conn, "sessions_v4")?;
    if !has_sessions && has_v4 {
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            ALTER TABLE sessions_v4 RENAME TO sessions;
            PRAGMA foreign_keys=ON;
            "#,
        )?;
        return Ok(());
    }
    if !has_sessions {
        return Ok(());
    }
    if has_v4 {
        conn.execute_batch("DROP TABLE IF EXISTS sessions_v4")?;
    }
    conn.execute_batch("PRAGMA foreign_keys=OFF")?;
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        CREATE TABLE sessions_v4 (
            id TEXT PRIMARY KEY,
            input_text TEXT NOT NULL DEFAULT '',
            title TEXT,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK(status IN ('pending','running','paused','paused_awaiting_answer','paused_awaiting_confirm','completed','failed','error')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            transcript TEXT NOT NULL DEFAULT '',
            react_state TEXT
        );
        INSERT INTO sessions_v4
            (id, input_text, title, status, created_at, updated_at, transcript, react_state)
        SELECT id, input_text, title, status, created_at, updated_at, transcript, react_state
          FROM sessions;
        DROP TABLE sessions;
        ALTER TABLE sessions_v4 RENAME TO sessions;
        COMMIT;
        "#,
    )?;
    conn.execute_batch("PRAGMA foreign_keys=ON")?;
    Ok(())
}

/// M5: LSH bucket side table for ANN probing once embedding partitions grow.
fn migrate_v6_embedding_lsh(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embedding_lsh (
            entity_type TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            model TEXT NOT NULL,
            bucket INTEGER NOT NULL,
            PRIMARY KEY (entity_type, entity_id, model)
         );
         CREATE INDEX IF NOT EXISTS idx_embedding_lsh_probe
             ON embedding_lsh(entity_type, model, bucket);",
    )?;
    Ok(())
}

/// Expand `messages.message_type` CHECK to include `peer_kickoff` (Plan A).
/// SQLite cannot ALTER CHECK in place, so rebuild the table (same crash-safe
/// pattern as sessions status migrations).
fn migrate_v7_peer_kickoff_message_type(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let has_messages = table_exists(conn, "messages")?;
    let has_v7 = table_exists(conn, "messages_v7")?;
    if !has_messages && has_v7 {
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys=OFF;
            ALTER TABLE messages_v7 RENAME TO messages;
            PRAGMA foreign_keys=ON;
            "#,
        )?;
        return Ok(());
    }
    if !has_messages {
        return Ok(());
    }
    if has_v7 {
        conn.execute_batch("DROP TABLE IF EXISTS messages_v7")?;
    }
    conn.execute_batch("PRAGMA foreign_keys=OFF")?;
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        CREATE TABLE messages_v7 (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
            content TEXT NOT NULL,
            message_type TEXT CHECK(message_type IN ('text','thought','action','observation','reasoning','peer_kickoff')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            tool_call_id TEXT,
            attachments TEXT,
            voice INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO messages_v7
            (id, session_id, role, content, message_type, created_at, tool_call_id, attachments, voice)
        SELECT id, session_id, role, content, message_type, created_at, tool_call_id, attachments, voice
          FROM messages;
        DROP TABLE messages;
        ALTER TABLE messages_v7 RENAME TO messages;
        CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
        COMMIT;
        "#,
    )?;
    conn.execute_batch("PRAGMA foreign_keys=ON")?;
    Ok(())
}

/// Add prompt-cache hit/write token columns to usage tables.
fn migrate_v8_usage_cache_tokens(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    if table_exists(conn, "session_usage")? {
        if !column_exists(conn, "session_usage", "cached_tokens")? {
            conn.execute(
                "ALTER TABLE session_usage ADD COLUMN cached_tokens INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !column_exists(conn, "session_usage", "cache_creation_tokens")? {
            conn.execute(
                "ALTER TABLE session_usage ADD COLUMN cache_creation_tokens INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
    }
    if table_exists(conn, "llm_usage")? {
        if !column_exists(conn, "llm_usage", "cached_tokens")? {
            conn.execute(
                "ALTER TABLE llm_usage ADD COLUMN cached_tokens INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !column_exists(conn, "llm_usage", "cache_creation_tokens")? {
            conn.execute(
                "ALTER TABLE llm_usage ADD COLUMN cache_creation_tokens INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
    }
    Ok(())
}

/// Existing rows predate provider accounting provenance and remain `unknown`.
/// Reconstructing their mode from token totals would recreate the cache-rate
/// error this column prevents.
pub(super) fn migrate_v10_llm_usage_cache_accounting(
    conn: &rusqlite::Connection,
) -> anyhow::Result<()> {
    if table_exists(conn, "llm_usage")? && !column_exists(conn, "llm_usage", "cache_accounting")? {
        conn.execute(
            "ALTER TABLE llm_usage ADD COLUMN cache_accounting TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
    }
    Ok(())
}

pub(super) fn migrate_v11_usage_cache_diagnostics(
    conn: &rusqlite::Connection,
) -> anyhow::Result<()> {
    for table in ["session_usage", "llm_usage"] {
        if table_exists(conn, table)? && !column_exists(conn, table, "cache_miss_tokens")? {
            conn.execute(
                &format!(
                    "ALTER TABLE {table} ADD COLUMN cache_miss_tokens INTEGER NOT NULL DEFAULT 0"
                ),
                [],
            )?;
        }
    }
    if table_exists(conn, "llm_usage")? && !column_exists(conn, "llm_usage", "cache_diagnostics")? {
        conn.execute(
            "ALTER TABLE llm_usage ADD COLUMN cache_diagnostics TEXT",
            [],
        )?;
    }
    Ok(())
}

/// P2-10 / L6: optional structured fields on episodes; P2-11: bare `company`
/// → `works_at` for rows written before the alias was added.
fn migrate_v5_episodes_structured_and_company_alias(
    conn: &rusqlite::Connection,
) -> anyhow::Result<()> {
    if table_exists(conn, "memory_episodes")? {
        if !column_exists(conn, "memory_episodes", "topics")? {
            conn.execute(
                "ALTER TABLE memory_episodes ADD COLUMN topics TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
        }
        if !column_exists(conn, "memory_episodes", "entities")? {
            conn.execute(
                "ALTER TABLE memory_episodes ADD COLUMN entities TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
        }
    }
    if let Some(table) = legacy_fact_table(conn)? {
        // Collapse only the company→works_at alias collision; general dedup
        // stays in `dedup_facts`.
        conn.execute_batch(&format!(
            r#"
            UPDATE {table} SET predicate = 'works_at' WHERE predicate = 'company';
            DELETE FROM {table}
             WHERE predicate = 'works_at'
               AND id NOT IN (
                 SELECT id FROM (
                   SELECT id, ROW_NUMBER() OVER (
                     PARTITION BY subject, predicate, object
                     ORDER BY confidence DESC, created_at DESC
                   ) AS rn FROM {table} WHERE predicate = 'works_at'
                 ) WHERE rn = 1
               );
            "#
        ))?;
    }
    Ok(())
}

/// X1: replace `facts` / `memory_episodes` with typed memory graph tables
/// (`memory_nodes`, `memory_items`, `memory_edges`) and drop legacy FTS.
/// Idempotent: no-op when `memory_edges` already exists (or `facts` is gone).
fn migrate_v9_memory_graph(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let has_facts = table_exists(conn, "facts")?;
    let has_edges = table_exists(conn, "memory_edges")?;
    let has_episodes = table_exists(conn, "memory_episodes")?;
    let has_items = table_exists(conn, "memory_items")?;

    if has_edges && !has_facts && !has_episodes {
        // Already on the graph shape; still drop any leftover legacy FTS.
        drop_legacy_memory_fts(conn)?;
        return Ok(());
    }

    // One transaction so a failed copy never commits DROP of legacy tables.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = migrate_v9_memory_graph_inner(conn, has_facts, has_edges, has_episodes, has_items);
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn migrate_v9_memory_graph_inner(
    conn: &rusqlite::Connection,
    has_facts: bool,
    has_edges: bool,
    has_episodes: bool,
    has_items: bool,
) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS memory_nodes (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            label TEXT NOT NULL,
            aliases TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(kind, label)
        );
        CREATE TABLE IF NOT EXISTS memory_items (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            kind TEXT NOT NULL CHECK(kind IN ('episode_summary','utterance','note')),
            content TEXT NOT NULL,
            topics TEXT NOT NULL DEFAULT '[]',
            entities TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL
        );
        "#,
    )?;

    if has_episodes {
        // Plain INSERT on first pass so FK failures abort; OR IGNORE only when
        // retrying a partial prior attempt so already-copied ids are skipped.
        let sql = if has_items {
            r#"
            INSERT OR IGNORE INTO memory_items
                (id, session_id, kind, content, topics, entities, created_at)
            SELECT
                id, session_id, 'episode_summary', summary,
                COALESCE(topics, '[]'), COALESCE(entities, '[]'), created_at
            FROM memory_episodes;
            "#
        } else {
            r#"
            INSERT INTO memory_items
                (id, session_id, kind, content, topics, entities, created_at)
            SELECT
                id, session_id, 'episode_summary', summary,
                COALESCE(topics, '[]'), COALESCE(entities, '[]'), created_at
            FROM memory_episodes;
            "#
        };
        conn.execute_batch(sql)?;
        let leftover: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_episodes e
             WHERE NOT EXISTS (SELECT 1 FROM memory_items i WHERE i.id = e.id)",
            [],
            |r| r.get(0),
        )?;
        if leftover > 0 {
            anyhow::bail!(
                "migrate_v9: failed to copy {leftover} memory_episodes row(s) into memory_items \
                 (likely FK / constraint errors); refusing to DROP legacy table"
            );
        }
    }

    if !has_edges {
        conn.execute_batch(
            r#"
            CREATE TABLE memory_edges (
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
            );
            "#,
        )?;
    }

    if has_facts {
        // Copy SPO rows; parse legacy source_ref JSON into provenance columns.
        struct LegacyFactRow {
            id: String,
            subject: String,
            predicate: String,
            object: String,
            source: String,
            confidence: f64,
            created_at: String,
            tags: String,
            durability: f64,
            mention_count: i64,
            last_seen_at: Option<String>,
            source_ref: Option<String>,
        }
        let mut stmt = conn.prepare(
            r#"
            SELECT id, subject, predicate, object, source, confidence, created_at,
                   tags, durability, mention_count, last_seen_at, source_ref
            FROM facts
            "#,
        )?;
        let rows: Vec<LegacyFactRow> = stmt
            .query_map([], |r| {
                Ok(LegacyFactRow {
                    id: r.get(0)?,
                    subject: r.get(1)?,
                    predicate: r.get(2)?,
                    object: r.get(3)?,
                    source: r.get(4)?,
                    confidence: r.get(5)?,
                    created_at: r.get(6)?,
                    tags: r.get(7)?,
                    durability: r.get(8)?,
                    mention_count: r.get(9)?,
                    last_seen_at: r.get(10)?,
                    source_ref: r.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let insert_sql = if has_edges {
            // Retry path: skip ids already copied.
            r#"
            INSERT OR IGNORE INTO memory_edges (
                id, subject, predicate, object, source, confidence, created_at,
                tags, durability, mention_count, last_seen_at,
                provenance_item_id, provenance_record_id, provenance_snippet
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#
        } else {
            r#"
            INSERT INTO memory_edges (
                id, subject, predicate, object, source, confidence, created_at,
                tags, durability, mention_count, last_seen_at,
                provenance_item_id, provenance_record_id, provenance_snippet
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#
        };
        let mut insert = conn.prepare(insert_sql)?;
        for row in rows {
            let (prov_item, prov_record, prov_snippet) =
                parse_legacy_source_ref(conn, row.source_ref.as_deref())?;
            insert.execute(rusqlite::params![
                row.id,
                row.subject,
                row.predicate,
                row.object,
                row.source,
                row.confidence,
                row.created_at,
                row.tags,
                row.durability,
                row.mention_count,
                row.last_seen_at,
                prov_item,
                prov_record,
                prov_snippet,
            ])?;
        }
        drop(insert);
        let leftover: i64 = conn.query_row(
            "SELECT COUNT(*) FROM facts f
             WHERE NOT EXISTS (SELECT 1 FROM memory_edges e WHERE e.id = f.id)",
            [],
            |r| r.get(0),
        )?;
        if leftover > 0 {
            anyhow::bail!(
                "migrate_v9: failed to copy {leftover} facts row(s) into memory_edges; \
                 refusing to DROP legacy table"
            );
        }
    }

    // Backfill nodes from distinct edge labels.
    backfill_memory_nodes_from_edges(conn)?;

    // Drop legacy tables / FTS / triggers only after verified copy.
    drop_legacy_memory_fts(conn)?;
    conn.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS facts_embed_del;
        DROP TRIGGER IF EXISTS facts_embed_upd;
        DROP TABLE IF EXISTS facts;
        DROP TABLE IF EXISTS memory_episodes;
        DROP INDEX IF EXISTS idx_facts_subject;
        DROP INDEX IF EXISTS idx_facts_confidence;
        DROP INDEX IF EXISTS idx_memory_episodes_session;
        DROP INDEX IF EXISTS idx_memory_episodes_created;
        "#,
    )?;
    Ok(())
}

fn parse_legacy_source_ref(
    conn: &rusqlite::Connection,
    raw: Option<&str>,
) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Ok((None, None, None));
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok((None, None, None));
    };
    let message_id = value
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let snippet = value
        .get("snippet")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if message_id.is_empty() {
        return Ok((None, None, snippet));
    }
    let in_items: bool = conn
        .query_row(
            "SELECT 1 FROM memory_items WHERE id = ?1",
            rusqlite::params![message_id],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if in_items {
        Ok((Some(message_id), None, snippet))
    } else {
        Ok((None, Some(message_id), snippet))
    }
}

fn backfill_memory_nodes_from_edges(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"
        SELECT DISTINCT subject AS label FROM memory_edges
        UNION
        SELECT DISTINCT object AS label FROM memory_edges
        "#,
    )?;
    let labels: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let now = chrono::Utc::now().to_rfc3339();
    for label in labels {
        let kind = if label.eq_ignore_ascii_case("user") {
            "user"
        } else {
            "concept"
        };
        let id = haven_common::types::new_id("node");
        conn.execute(
            r#"
            INSERT INTO memory_nodes (id, kind, label, aliases, created_at, updated_at)
            VALUES (?1, ?2, ?3, '[]', ?4, ?4)
            ON CONFLICT(kind, label) DO NOTHING
            "#,
            rusqlite::params![id, kind, label, now],
        )?;
        let node_id: String = conn.query_row(
            "SELECT id FROM memory_nodes WHERE kind = ?1 AND label = ?2",
            rusqlite::params![kind, label],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE memory_edges SET subject_id = ?1 WHERE subject = ?2 AND subject_id IS NULL",
            rusqlite::params![node_id, label],
        )?;
        conn.execute(
            "UPDATE memory_edges SET object_id = ?1 WHERE object = ?2 AND object_id IS NULL",
            rusqlite::params![node_id, label],
        )?;
    }
    Ok(())
}

fn drop_legacy_memory_fts(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS facts_ai;
        DROP TRIGGER IF EXISTS facts_ad;
        DROP TRIGGER IF EXISTS facts_au;
        DROP TRIGGER IF EXISTS episodes_ai;
        DROP TRIGGER IF EXISTS episodes_ad;
        DROP TRIGGER IF EXISTS episodes_au;
        DROP TABLE IF EXISTS facts_fts;
        DROP TABLE IF EXISTS episodes_fts;
        "#,
    )?;
    // Fresh DBs run migrations before SCHEMA_SQL creates kv_store.
    if table_exists(conn, "kv_store")? {
        conn.execute(
            "DELETE FROM kv_store WHERE key IN ('facts_fts_tokenizer', 'episodes_fts_tokenizer')",
            [],
        )?;
    }
    Ok(())
}

pub(super) fn user_version(conn: &rusqlite::Connection) -> anyhow::Result<i32> {
    Ok(conn
        .prepare("PRAGMA user_version")?
        .query_row([], |r| r.get(0))?)
}

pub(super) fn set_user_version(conn: &rusqlite::Connection, version: i32) -> anyhow::Result<()> {
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    Ok(())
}

/// Run every migration in `migrations` whose version is above the database's
/// current version, stamping `user_version` after each one. Split out from
/// `init_schema` so tests can exercise the chain with synthetic migrations.
pub(super) fn apply_migrations(
    conn: &rusqlite::Connection,
    from: i32,
    migrations: &[Migration],
) -> anyhow::Result<()> {
    for migration in migrations.iter().filter(|m| m.version > from) {
        (migration.apply)(conn)?;
        set_user_version(conn, migration.version)?;
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_catalog_is_ordered_and_reaches_current_version() {
        assert!(!MIGRATIONS.is_empty());
        assert_eq!(MIGRATIONS.last().unwrap().version, SCHEMA_VERSION);
        assert!(
            MIGRATIONS
                .windows(2)
                .all(|pair| pair[0].version < pair[1].version)
        );
        assert!(MIGRATIONS.first().unwrap().version > 1);
    }
}
