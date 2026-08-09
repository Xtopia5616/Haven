pub const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS tasks (
        id TEXT PRIMARY KEY,
        input_text TEXT NOT NULL DEFAULT '',
        title TEXT,
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK(status IN ('pending','running','paused','completed','failed','error')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
        transcript TEXT NOT NULL DEFAULT '',
        react_state TEXT,
        parent_task_id TEXT REFERENCES tasks(id)
    )",
    "CREATE TABLE IF NOT EXISTS messages (
        id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
        content TEXT NOT NULL,
        message_type TEXT CHECK(message_type IN ('text','thought','action','observation','reasoning')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        tool_call_id TEXT,
        attachments TEXT,
        is_compacted INTEGER NOT NULL DEFAULT 0,
        compaction_id TEXT,
        parent_message_id TEXT REFERENCES messages(id),
        voice INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS task_steps (
        id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
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
        silent INTEGER NOT NULL DEFAULT 0
    )",
    // Internal key-value store (fact-extraction cursors, etc.). User-facing
    // preferences live in the `facts` table (tag `preference`) — the old
    // `preferences` table was removed without a data migration.
    "CREATE TABLE IF NOT EXISTS kv_store (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    // Episodic long-term memory: compaction summaries persisted when the
    // react loop compresses a conversation. Indexed (embedding + keyword)
    // as `episode` entities alongside user messages, so context that was
    // summarized away remains retrievable across tasks. The old
    // `compaction_entries` table (same idea, never read) was dropped.
    "CREATE TABLE IF NOT EXISTS memory_episodes (
        id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        summary TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE TABLE IF NOT EXISTS whitelist (
        tool_name TEXT NOT NULL PRIMARY KEY,
        pattern TEXT,
        added_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    // No-op placeholder: the `mcp_servers` table was removed (MCP config now
    // lives in config.toml `[[mcp_servers]]`). Kept to preserve the append-only
    // migration index — existing databases keep their (unused) table.
    "SELECT 1 -- mcp_servers table removed; MCP config now in config.toml",
    "CREATE TABLE IF NOT EXISTS facts (
        id TEXT PRIMARY KEY,
        subject TEXT NOT NULL,
        predicate TEXT NOT NULL,
        object TEXT NOT NULL,
        source TEXT NOT NULL DEFAULT 'inferred'
            CHECK(source IN ('user','inferred')),
        confidence REAL NOT NULL DEFAULT 1.0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE TABLE IF NOT EXISTS compaction_entries (
        id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        summary TEXT NOT NULL,
        first_kept_entry_id TEXT,
        tokens_before INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status)",
    "CREATE INDEX IF NOT EXISTS idx_task_steps_task ON task_steps(task_id)",
    "CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject)",
    "CREATE INDEX IF NOT EXISTS idx_facts_confidence ON facts(confidence)",
    "CREATE INDEX IF NOT EXISTS idx_memory_episodes_task ON memory_episodes(task_id)",
    "CREATE INDEX IF NOT EXISTS idx_memory_episodes_created ON memory_episodes(created_at)",
    // Appended last (append-only): the reminders table was originally added
    // mid-array, which existing databases (user_version already past its
    // index) never re-ran — leaving them without the table and bricking the
    // startup migration. Schema includes all current columns so fresh
    // databases skip the per-column ALTERs below.
    "CREATE TABLE IF NOT EXISTS reminders (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL DEFAULT 'reminder',
        due_at TEXT,
        title TEXT NOT NULL DEFAULT 'Haven',
        body TEXT,
        mode TEXT NOT NULL DEFAULT 'tool',
        task_id TEXT,
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
    "CREATE TABLE IF NOT EXISTS task_usage (
        task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
        prompt_tokens INTEGER NOT NULL DEFAULT 0,
        completion_tokens INTEGER NOT NULL DEFAULT 0,
        total_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd REAL NOT NULL DEFAULT 0,
        has_cost INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
];

/// Vector index for semantic memory. `entity_type` selects the memory domain
/// ('fact' = facts rows, 'episode' = conversation events / compaction
/// summaries); `entity_id` references the owning row. `vector` is a
/// little-endian f32 blob; `text` keeps the embedded surface text so keyword
/// fallback and display don't need to re-derive it.
///
/// Deliberately NOT part of `MIGRATIONS`: the array is gated by
/// `PRAGMA user_version`, and a table appended at index N is silently skipped
/// on databases whose user_version already exceeds N (e.g. after a build with
/// one more entry ran once) — while any later entry referencing the table
/// (its own index!) still executes, bricking startup with
/// "no such table". This exact failure happened to `reminders` historically
/// and to `memory_embeddings` in the wild. Ensured idempotently on every open
/// instead, independent of user_version.
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

/// Create (idempotently) the triggers that keep `memory_embeddings` in sync
/// with the `facts` table: any fact row UPDATE or DELETE invalidates the
/// fact's embedding, so the next embedding pass re-indexes the current
/// surface text. Dropped temporarily by the §ID id-migration below, which
/// rewrites facts primary keys and would otherwise trip the triggers.
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

pub fn run_migrations(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    // Every entry in `MIGRATIONS` is idempotent (CREATE TABLE/INDEX IF NOT
    // EXISTS or a no-op placeholder), so the array runs UNCONDITIONALLY on
    // every open instead of being gated by `PRAGMA user_version`.
    //
    // The version gate was the root cause of two production brickings
    // (reminders, memory_embeddings): a table appended at array index N was
    // silently skipped on databases whose user_version already exceeded N,
    // while a later entry referencing the table still executed. With an
    // all-idempotent array, gating adds risk and buys nothing; `user_version`
    // is kept as informational only.
    for sql in MIGRATIONS {
        // execute_batch (not execute) so no-op placeholder statements
        // such as "SELECT 1" (removed tables) are allowed.
        conn.execute_batch(sql)?;
    }
    conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len()))?;

    // Ensure the vector index table exists on every open, independent of
    // `user_version` (see MEMORY_EMBEDDINGS_SCHEMA doc). Idempotent.
    conn.execute_batch(MEMORY_EMBEDDINGS_SCHEMA)?;

    // Keep the vector index consistent with the facts table: any fact
    // mutation (value correction, reinforcement, deletion, dedup, flush)
    // invalidates the fact's embedding, so the next embedding pass re-indexes
    // the current surface text instead of letting stale vectors linger.
    // `memory_embeddings` is ensured above, and `facts` in MIGRATIONS.
    ensure_fact_embedding_triggers(conn)?;

    // Ensure attachments column exists on the messages table (multimodal §M7).
    // Stores a JSON array of image attachments: [{"media_type": "...", "data": "<base64>"}].
    let has_attachments: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='attachments'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_attachments {
        conn.execute("ALTER TABLE messages ADD COLUMN attachments TEXT", [])?;
    }

    // Ensure react_state column exists on the tasks table.
    let has_react_state: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='react_state'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_react_state {
        conn.execute("ALTER TABLE tasks ADD COLUMN react_state TEXT", [])?;
    }

    // §3.3: add new schema columns to task_steps table
    let has_thought: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('task_steps') WHERE name='thought'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_thought {
        conn.execute("ALTER TABLE task_steps ADD COLUMN thought TEXT", [])?;
        conn.execute("ALTER TABLE task_steps ADD COLUMN action_tool TEXT", [])?;
        conn.execute("ALTER TABLE task_steps ADD COLUMN action_input TEXT", [])?;
        conn.execute("ALTER TABLE task_steps ADD COLUMN observation TEXT", [])?;
        // Backfill: migrate legacy "thought" rows
        conn.execute(
            "UPDATE task_steps SET thought = input WHERE tool_name = 'thought' OR tool_name = 'supplement'",
            [],
        )?;
        conn.execute(
            "UPDATE task_steps SET action_tool = tool_name, action_input = input WHERE tool_name NOT IN ('thought', 'supplement')",
            [],
        )?;
    }

    // §3.7: persist the `silent` flag on action steps so the history review
    // can hide the same tool badges the live chat hides (the LLM can request
    // `"silent": true` on e.g. shell to suppress output from the user while
    // the agent still sees it).
    let has_silent: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('task_steps') WHERE name='silent'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_silent {
        conn.execute(
            "ALTER TABLE task_steps ADD COLUMN silent INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // §3.2: add compaction columns to messages table
    let has_is_compacted: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='is_compacted'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_is_compacted {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN is_compacted INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        conn.execute("ALTER TABLE messages ADD COLUMN compaction_id TEXT", [])?;
    }

    // §2: add parent_message_id column to messages table for tree structure
    let has_parent_message_id: bool = conn
        .prepare(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='parent_message_id'",
        )?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_parent_message_id {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN parent_message_id TEXT REFERENCES messages(id)",
            [],
        )?;
    }

    // §3.6: hindsight_store removed — facts now support tags + search.
    conn.execute_batch("DROP TABLE IF EXISTS hindsight_store")?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_hindsight_key")?;
    conn.execute_batch("DROP INDEX IF EXISTS idx_hindsight_session")?;

    // Scratch table for in-flight streamed text (crash/stop partial-reply
    // recovery). Ensured on every open, independent of user_version, so
    // existing databases get it even though the versioned MIGRATIONS array
    // predates it (see MEMORY_EMBEDDINGS_SCHEMA doc).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS partial_messages (
            task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
            content TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )?;

    // Add tags column to facts table (JSON array of strings, default '[]').
    let has_facts_tags: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='tags'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_facts_tags {
        conn.execute(
            "ALTER TABLE facts ADD COLUMN tags TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }

    // §P1: facts lifecycle columns — `mention_count` tracks how often a fact
    // is re-confirmed (reinforcement) and `last_seen_at` records when it was
    // last observed, so stale facts can decay instead of living forever at
    // full confidence. Guarded per-column like the tags migration above.
    // `durability` (0..1) rates how long a fact stays useful: it scales the
    // effective confidence used for ranking and pruning, so transient,
    // low-durability facts (extracted at low durability) die out fast while
    // stable ones keep full weight. Existing rows default to 1.0 — the
    // pre-durability behavior — so the upgrade changes nothing for stored
    // memory; only newly extracted facts carry explicit durability.
    let has_durability: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='durability'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_durability {
        conn.execute(
            "ALTER TABLE facts ADD COLUMN durability REAL NOT NULL DEFAULT 1.0",
            [],
        )?;
    }
    let has_mention_count: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='mention_count'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_mention_count {
        conn.execute(
            "ALTER TABLE facts ADD COLUMN mention_count INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    let has_last_seen_at: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='last_seen_at'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_last_seen_at {
        conn.execute("ALTER TABLE facts ADD COLUMN last_seen_at TEXT", [])?;
        // Backfill: existing rows have no observation history, so stamp them
        // as seen NOW rather than at creation. Using the upgrade time as the
        // grace point keeps recency decay from retroactively pruning every
        // old fact on the first inference pass after the upgrade — a fact
        // created months ago would otherwise decay below the flush threshold
        // (e.g. 0.9 confidence at a 90-day half-life is ~0.23 after 6
        // months) and be silently deleted before the user ever re-confirms.
        //
        // The backfill is a schema migration, not a fact mutation: with
        // `facts_embed_upd` live (installed above), this UPDATE would fire
        // the trigger on every row and delete the entire fact embedding
        // index on legacy DBs in one upgrade. Drop the trigger around the
        // UPDATE and recreate it — same pattern the §ID block below uses for
        // its facts-row rewrite.
        conn.execute_batch("DROP TRIGGER IF EXISTS facts_embed_upd")?;
        conn.execute(
            "UPDATE facts SET last_seen_at = ?1 WHERE last_seen_at IS NULL",
            rusqlite::params![chrono::Utc::now().to_rfc3339()],
        )?;
        ensure_fact_embedding_triggers(conn)?;
    }

    // §P2: source_ref — JSON `{message_id, snippet}` pointing at the
    // conversation message a fact was extracted from, for traceability and
    // contradiction checks. Existing rows simply have no reference.
    let has_source_ref: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='source_ref'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_source_ref {
        conn.execute("ALTER TABLE facts ADD COLUMN source_ref TEXT", [])?;
    }

    // §P0: FTS5 full-text index over facts so search_facts can rank by
    // relevance (BM25) instead of doing LIKE scans. Uses an external-content
    // table synced by triggers. Best-effort: bundled SQLite ships FTS5, but
    // if it is ever unavailable the setup fails gracefully and search_facts
    // falls back to the LIKE path. The whole block runs inside one
    // transaction and the idempotency guard checks that the sync triggers
    // exist too, so a crash partway through (table created but triggers
    // missing) is repaired on the next startup instead of leaving the index
    // silently unmaintained.
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

    // Merge sessions into tasks: the `sessions` table was a thin wrapper
    // (a UUID + timestamps + a branching pointer) that every task duplicated.
    // Tasks now own their messages directly and branching is expressed via
    // `tasks.parent_task_id`. Old databases are converted in place:
    //   - messages.session_id  → messages.task_id  (via tasks.session_id)
    //   - compaction_entries.session_id → task_id
    //   - tasks.session_id dropped; sessions.parent_id → tasks.parent_task_id
    //   - the sessions table and its indexes are dropped
    let has_sessions: bool = conn
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if has_sessions {
        // The whole conversion runs inside a single transaction: a crash
        // mid-migration rolls everything back, so the next startup re-runs
        // the conversion from a clean state instead of failing on partial
        // artifacts (messages_rebuild left behind, session_id already gone).
        // PRAGMA foreign_keys must be toggled OUTSIDE the transaction (it is
        // a no-op inside one), so it happens before BEGIN.
        let fk_on: bool = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap_or(false);
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=OFF")?;
        }
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            // Very old databases may lack sessions.parent_id (added later).
            let has_parent_id: bool = conn
                .prepare(
                    "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='parent_id'",
                )?
                .query_row([], |r| r.get::<_, i32>(0))
                .map(|c| c > 0)
                .unwrap_or(false);
            if !has_parent_id {
                conn.execute(
                    "ALTER TABLE sessions ADD COLUMN parent_id TEXT REFERENCES sessions(id)",
                    [],
                )?;
            }

            // 1. messages → task_id. Messages whose session has no task
            //    (orphaned by crashed runs) are dropped.
            conn.execute_batch(
                "CREATE TABLE messages_rebuild (
                    id TEXT PRIMARY KEY,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
                    content TEXT NOT NULL,
                    message_type TEXT CHECK(message_type IN ('text','thought','action','observation','reasoning')),
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    tool_call_id TEXT,
                    attachments TEXT,
                    is_compacted INTEGER NOT NULL DEFAULT 0,
                    compaction_id TEXT,
                    parent_message_id TEXT REFERENCES messages(id)
                );
                INSERT INTO messages_rebuild
                    (id, task_id, role, content, message_type, created_at, tool_call_id,
                     attachments, is_compacted, compaction_id, parent_message_id)
                SELECT m.id, t.id, m.role, m.content, m.message_type, m.created_at, m.tool_call_id,
                       m.attachments, m.is_compacted, m.compaction_id, m.parent_message_id
                FROM messages m
                JOIN tasks t ON t.session_id = m.session_id;
                DROP TABLE messages;
                ALTER TABLE messages_rebuild RENAME TO messages;
                ",
            )?;

            // 2. compaction_entries → task_id (same join, orphans dropped).
            conn.execute_batch(
                "CREATE TABLE compaction_entries_rebuild (
                    id TEXT PRIMARY KEY,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    summary TEXT NOT NULL,
                    first_kept_entry_id TEXT,
                    tokens_before INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO compaction_entries_rebuild
                    (id, task_id, summary, first_kept_entry_id, tokens_before, created_at)
                SELECT c.id, t.id, c.summary, c.first_kept_entry_id, c.tokens_before, c.created_at
                FROM compaction_entries c
                JOIN tasks t ON t.session_id = c.session_id;
                DROP TABLE compaction_entries;
                ALTER TABLE compaction_entries_rebuild RENAME TO compaction_entries;
                ",
            )?;

            // 3. tasks: drop session_id, express branching via parent_task_id

            // 3. tasks: drop session_id, express branching via parent_task_id
            //    (old sessions.parent_id → the task owning the parent session).
            //    status is normalized here too: pre-cancelled-fix databases
            //    can still hold 'cancelled' rows, which the rebuilt CHECK
            //    excludes — copying them verbatim would abort the whole
            //    conversion and brick the database.
            conn.execute_batch(
                "CREATE TABLE tasks_rebuild (
                    id TEXT PRIMARY KEY,
                    input_text TEXT NOT NULL DEFAULT '',
                    title TEXT,
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending','running','paused','completed','failed','error')),
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    transcript TEXT NOT NULL DEFAULT '',
                    react_state TEXT,
                    parent_task_id TEXT REFERENCES tasks(id)
                );
                INSERT INTO tasks_rebuild
                    (id, input_text, title, status, created_at, updated_at, transcript, react_state, parent_task_id)
                SELECT t.id, t.input_text, t.title,
                       CASE WHEN t.status = 'cancelled' THEN 'error' ELSE t.status END,
                       t.created_at, t.updated_at, t.transcript, t.react_state,
                       (SELECT t2.id FROM tasks t2
                        JOIN sessions s2 ON t2.session_id = s2.id
                        WHERE s2.id = (SELECT s.parent_id FROM sessions s WHERE s.id = t.session_id))
                FROM tasks t;
                DROP TABLE tasks;
                ALTER TABLE tasks_rebuild RENAME TO tasks;
                ",
            )?;

            // 4. Drop the sessions table and its legacy indexes.
            conn.execute_batch("DROP TABLE sessions")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else {
            conn.execute_batch("COMMIT")?;
        }
        // Restore the FK pragma on BOTH paths (before `result?` propagates
        // the error) so a failed conversion can never leave the connection
        // with foreign_keys silently disabled.
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=ON")?;
        }
        result?;
    }

    // Compaction summaries were only ever written, never read: the DB keeps
    // full message history and the in-memory compactor reports via events.
    // Dropped idempotently on every open — AFTER the legacy sessions-merge
    // rebuild above (which still recreates the table for old DBs), and
    // unconditionally (fresh DBs create it in MIGRATIONS and drop it here).
    conn.execute_batch("DROP TABLE IF EXISTS compaction_entries")?;

    // §M8: voice flag on the messages table — marks user messages that came
    // from voice transcription so the UI can render the mic style after
    // reload. Runs after the sessions-merge rebuild above, which recreates
    // the messages table from scratch and would drop the column.
    let has_voice: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='voice'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_voice {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN voice INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // §P1: facts lifecycle columns — `mention_count` tracks how often a fact
    // is re-confirmed (reinforcement) and `last_seen_at` records when it was
    // last observed, so stale facts can decay instead of living forever at
    // full confidence. Guarded per-column like the tags migration above.
    // LLM with this text at due time instead of only showing a notification.
    let has_reminder_prompt: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('reminders') WHERE name='prompt'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_reminder_prompt {
        conn.execute("ALTER TABLE reminders ADD COLUMN prompt TEXT", [])?;
    }

    // Reminders get the fire-mode columns: `mode` selects what happens when
    // the reminder fires (tool = call the tool in tool_name/tool_args —
    // use tool_name 'notify' to send a message; continue = resume the task
    // in task_id), and task_id/tool_name/tool_args carry the mode-specific
    // payload. Existing rows default to 'tool'.
    for (col, ddl) in [
        (
            "mode",
            "ALTER TABLE reminders ADD COLUMN mode TEXT NOT NULL DEFAULT 'tool'",
        ),
        ("task_id", "ALTER TABLE reminders ADD COLUMN task_id TEXT"),
        (
            "tool_name",
            "ALTER TABLE reminders ADD COLUMN tool_name TEXT",
        ),
        (
            "tool_args",
            "ALTER TABLE reminders ADD COLUMN tool_args TEXT",
        ),
    ] {
        let has_col: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('reminders') WHERE name=?1")?
            .query_row([col], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        if !has_col {
            conn.execute(ddl, [])?;
        }
    }

    // §activity: unify background jobs and reminders as one activity family
    // stored in this table. `kind` selects the family ('reminder' default for
    // existing rows, 'job' for background shell commands); the job columns
    // below (status/command/output/error/error_reason/log_path/exit_code/
    // started_at/finished_at) carry the job lifecycle and are NULL on
    // reminder rows. Guarded per-column like the mode migration above.
    for (col, ddl) in [
        (
            "kind",
            "ALTER TABLE reminders ADD COLUMN kind TEXT NOT NULL DEFAULT 'reminder'",
        ),
        ("status", "ALTER TABLE reminders ADD COLUMN status TEXT"),
        ("command", "ALTER TABLE reminders ADD COLUMN command TEXT"),
        ("output", "ALTER TABLE reminders ADD COLUMN output TEXT"),
        ("error", "ALTER TABLE reminders ADD COLUMN error TEXT"),
        (
            "error_reason",
            "ALTER TABLE reminders ADD COLUMN error_reason TEXT",
        ),
        ("log_path", "ALTER TABLE reminders ADD COLUMN log_path TEXT"),
        (
            "exit_code",
            "ALTER TABLE reminders ADD COLUMN exit_code INTEGER",
        ),
        (
            "started_at",
            "ALTER TABLE reminders ADD COLUMN started_at TEXT",
        ),
        (
            "finished_at",
            "ALTER TABLE reminders ADD COLUMN finished_at TEXT",
        ),
    ] {
        let has_col: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('reminders') WHERE name=?1")?
            .query_row([col], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        if !has_col {
            conn.execute(ddl, [])?;
        }
    }

    // Backfill legacy 'notify'-mode reminders created under the previous
    // schema (which was dropped in favor of mode='tool'). Those rows carried
    // no tool_name, so map them to the equivalent "send a message" action:
    // mode='tool' calling tool_name='notify' with the reminder title/body.
    conn.execute(
        "UPDATE reminders
         SET mode = 'tool', tool_name = 'notify',
             tool_args = json_object('title', title, 'body', body)
         WHERE mode = 'notify'",
        [],
    )?;

    // Task-usage counters and the internal kv_store (fact-extraction
    // cursors, ...) are created by the MIGRATIONS array, which now runs
    // unconditionally — no separate ensure block needed here.

    // Indexes for the merged schema (idempotent; also covers fresh databases).
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_messages_task ON messages(task_id);
         CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
         DROP INDEX IF EXISTS idx_messages_session;
         DROP INDEX IF EXISTS idx_tasks_session;
         CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);
         CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
        ",
    )?;

    // Fix CHECK constraint typo: 'pendingleted' → 'completed'. Presence-gated
    // on the actual schema (like the cancelled-status rebuild below) instead
    // of user_version, which is no longer authoritative for migrations.
    let check_allows_pendingleted: bool = conn
        .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='tasks'")?
        .query_row([], |r| r.get::<_, String>(0))
        .map(|sql| sql.contains("'pendingleted'"))
        .unwrap_or(false);
    if check_allows_pendingleted {
        // Save foreign_keys setting and temporarily disable during table rebuild.
        let fk_on: bool = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap_or(false);
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=OFF")?;
        }
        conn.execute_batch(
            "CREATE TABLE tasks_rebuild (
                  id TEXT PRIMARY KEY,
                  input_text TEXT NOT NULL DEFAULT '',
                  title TEXT,
                  status TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending','running','paused','completed','failed','error')),
                  created_at TEXT NOT NULL DEFAULT (datetime('now')),
                  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                  transcript TEXT NOT NULL DEFAULT '',
                  react_state TEXT,
                  parent_task_id TEXT REFERENCES tasks(id)
              );
               INSERT INTO tasks_rebuild SELECT id, input_text, title, status, created_at, updated_at, transcript, react_state, parent_task_id FROM tasks;
             DROP TABLE tasks;
             ALTER TABLE tasks_rebuild RENAME TO tasks;
             CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);
             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
        ")?;
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=ON")?;
        }
    }

    // Add title column to tasks table
    let has_title: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='title'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_title {
        conn.execute("ALTER TABLE tasks ADD COLUMN title TEXT", [])?;
    }

    // Remove 'cancelled' status: migrate existing rows to 'error' and
    // rebuild the CHECK constraint to exclude 'cancelled'.
    // SQLite cannot ALTER a CHECK constraint in-place, so we rebuild the
    // table if the old constraint still allows 'cancelled'.
    let check_allows_cancelled: bool = conn
        .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='tasks'")?
        .query_row([], |r| r.get::<_, String>(0))
        .map(|sql| sql.contains("'cancelled'"))
        .unwrap_or(false);
    if check_allows_cancelled {
        // First, migrate existing cancelled rows.
        conn.execute(
            "UPDATE tasks SET status = 'error' WHERE status = 'cancelled'",
            [],
        )?;
        // Rebuild tasks table without 'cancelled' in CHECK.
        let fk_on: bool = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap_or(false);
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=OFF")?;
        }
        conn.execute_batch(
            "CREATE TABLE tasks_rebuild (
                  id TEXT PRIMARY KEY,
                  input_text TEXT NOT NULL DEFAULT '',
                  title TEXT,
                  status TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending','running','paused','completed','failed','error')),
                  created_at TEXT NOT NULL DEFAULT (datetime('now')),
                  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                  transcript TEXT NOT NULL DEFAULT '',
                  react_state TEXT,
                  parent_task_id TEXT REFERENCES tasks(id)
              );
              INSERT INTO tasks_rebuild SELECT id, input_text, title, status, created_at, updated_at, transcript, react_state, parent_task_id FROM tasks;
             DROP TABLE tasks;
             ALTER TABLE tasks_rebuild RENAME TO tasks;
             CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);
             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
        ")?;
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=ON")?;
        }
    }

    // Dead columns after the branch-feature removal and the compaction
    // cleanup: `tasks.parent_task_id` (branching links, no consumers),
    // `messages.parent_message_id` (never written), and
    // `messages.is_compacted`/`compaction_id` (never set). Dropped
    // idempotently after every rebuild that might recreate them; runs on
    // bundled SQLite ≥3.45 (ALTER TABLE DROP COLUMN needs 3.35+).
    for (table, col) in [
        ("messages", "parent_message_id"),
        ("messages", "is_compacted"),
        ("messages", "compaction_id"),
        ("tasks", "parent_task_id"),
    ] {
        let exists: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2")?
            .query_row(rusqlite::params![table, col], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        if exists {
            conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {col}"), [])?;
        }
    }

    // §ID: unify every persisted entity id to the canonical `{prefix}-{uuid32}`
    // format (see haven_common::types / AGENTS.md §ID 规范). Older databases
    // stored bare hyphenated UUIDs (and reminders sometimes as `rem-{uuid}`);
    // this pass rewrites:
    //   - primary keys: tasks.id, messages.id, task_steps.id, facts.id,
    //     reminders.id
    //   - task_id references: messages, task_steps, partial_messages,
    //     task_usage, reminders
    //   - memory_embeddings.entity_id (fact rows → facts.id; episode rows →
    //     messages.id or memory_episodes.id)
    //   - kv_store: `fact_extraction.<task_id>` cursor keys embed the task id
    //     and their values are message ids
    //   - facts.source_ref JSON `{"message_id": ...}` pointers
    // Runs with foreign_keys off inside one transaction so the intermediate
    // state (parents renamed, children not yet) never leaks; the whole block
    // is gated on any unprefixed task id remaining, making it a cheap no-op
    // once done.
    let unprefixed_tasks: i32 = conn
        .prepare("SELECT COUNT(*) FROM tasks WHERE id NOT LIKE 'task-%'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .unwrap_or(0);
    if unprefixed_tasks > 0 {
        // The embedding invalidation triggers fire on every facts row UPDATE
        // and would delete the very embedding rows this migration rewrites
        // (facts.id / source_ref are updated below). Drop them for the
        // duration of the rewrite and recreate afterwards.
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS facts_embed_del;
             DROP TRIGGER IF EXISTS facts_embed_upd;",
        )?;
        let fk_on: bool = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap_or(false);
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=OFF")?;
        }
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            // 1. Episode embeddings reference either a messages.id or a
            //    memory_episodes.id — resolve by table membership BEFORE the
            //    primary keys below are rewritten (their bare ids would no
            //    longer match either table).
            conn.execute(
                "UPDATE memory_embeddings SET entity_id = 'msg-' || replace(entity_id, '-', '')
                 WHERE entity_type = 'episode'
                   AND entity_id IN (SELECT id FROM messages)
                   AND entity_id NOT LIKE 'msg-%'",
                [],
            )?;
            conn.execute(
                "UPDATE memory_embeddings SET entity_id = 'msg-' || replace(entity_id, '-', '')
                 WHERE entity_type = 'episode'
                   AND entity_id IN (SELECT id FROM memory_episodes)
                   AND entity_id NOT LIKE 'msg-%'",
                [],
            )?;
            // 2. Fact embeddings likewise before the facts.id rewrite (and
            //    the embedding triggers are dropped for this block anyway).
            conn.execute(
                "UPDATE memory_embeddings SET entity_id = 'fact-' || replace(entity_id, '-', '')
                 WHERE entity_type = 'fact' AND entity_id NOT LIKE 'fact-%'",
                [],
            )?;
            // 3. facts.source_ref JSON message_id pointers — before the
            //    facts.id rewrite so the embedded reference is already
            //    canonical when the owning row id changes. The json_valid
            //    guard keeps the statement failure-tolerant: source_ref is a
            //    free-text column, and on SQLite >= 3.38 any malformed cell
            //    makes json_type/json_extract RAISE instead of returning NULL
            //    (json_valid is the one JSON function that never raises).
            //    Without the guard a single corrupt row would abort the whole
            //    §ID transaction and, because the unprefixed-task gate never
            //    clears, brick startup forever.
            conn.execute(
                "UPDATE facts SET source_ref = json_set(source_ref, '$.message_id',
                     'msg-' || replace(json_extract(source_ref, '$.message_id'), '-', ''))
                 WHERE source_ref IS NOT NULL
                   AND json_valid(source_ref) = 1
                   AND json_type(source_ref, '$') = 'object'
                   AND json_extract(source_ref, '$.message_id') IS NOT NULL
                   AND json_extract(source_ref, '$.message_id') NOT LIKE 'msg-%'",
                [],
            )?;
            // 4. Primary keys. `replace(id, '-', '')` strips the UUID hyphens
            //    (canonical form is the simple 32-hex encoding).
            conn.execute(
                "UPDATE tasks SET id = 'task-' || replace(id, '-', '') WHERE id NOT LIKE 'task-%'",
                [],
            )?;
            conn.execute(
                "UPDATE messages SET id = 'msg-' || replace(id, '-', '') WHERE id NOT LIKE 'msg-%'",
                [],
            )?;
            conn.execute(
                "UPDATE task_steps SET id = 'step-' || replace(id, '-', '') WHERE id NOT LIKE 'step-%'",
                [],
            )?;
            conn.execute(
                "UPDATE facts SET id = 'fact-' || replace(id, '-', '') WHERE id NOT LIKE 'fact-%'",
                [],
            )?;
            conn.execute(
                "UPDATE memory_episodes SET id = 'msg-' || replace(id, '-', '')
                 WHERE id NOT LIKE 'msg-%'",
                [],
            )?;
            // Reminders have a mixed history (bare UUIDs and rem-{uuid}); the
            // normalize-anywhere form is idempotent by construction.
            conn.execute(
                "UPDATE reminders SET id = 'rem-' || replace(replace(id, 'rem-', ''), '-', '')
                 WHERE id <> 'rem-' || replace(replace(id, 'rem-', ''), '-', '')",
                [],
            )?;
            // 5. task_id references.
            for table in [
                "messages",
                "task_steps",
                "partial_messages",
                "task_usage",
                "memory_episodes",
            ] {
                conn.execute(
                    &format!(
                        "UPDATE {table} SET task_id = 'task-' || replace(task_id, '-', '')
                         WHERE task_id NOT LIKE 'task-%'"
                    ),
                    [],
                )?;
            }
            conn.execute(
                "UPDATE reminders SET task_id = 'task-' || replace(task_id, '-', '')
                 WHERE task_id IS NOT NULL AND task_id NOT LIKE 'task-%'",
                [],
            )?;
            // 6. kv_store: cursor keys embed the task id; values are message
            //    ids — both must carry the canonical (hyphen-free) form.
            conn.execute(
                "UPDATE kv_store
                 SET key = 'fact_extraction.task-' || replace(substr(key, length('fact_extraction.') + 1), '-', '')
                 WHERE key LIKE 'fact_extraction.%' AND key NOT LIKE 'fact_extraction.task-%'",
                [],
            )?;
            conn.execute(
                "UPDATE kv_store SET value = 'msg-' || replace(value, '-', '')
                 WHERE key LIKE 'fact_extraction.%' AND value NOT LIKE 'msg-%'",
                [],
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else {
            conn.execute_batch("COMMIT")?;
        }
        if fk_on {
            conn.execute_batch("PRAGMA foreign_keys=ON")?;
        }
        result?;
        // Restore the embedding invalidation triggers dropped above (also a
        // no-op when the §ID block did not run).
        ensure_fact_embedding_triggers(conn)?;
    }

    // §ID: merge the `epi-` prefix into the message id space. The `episode`
    // memory domain covers user messages (`msg-`) and persisted compaction
    // summaries (memory_episodes) alike, so episodes now share the `msg-`
    // prefix instead of a distinct `epi-`. Databases created with the old
    // scheme carry `epi-{uuid32}` ids in memory_episodes and in the episode
    // embeddings pointing at them; rewrite both to `msg-` (idempotent, and a
    // no-op once no `epi-` id remains).
    conn.execute(
        "UPDATE memory_episodes SET id = 'msg-' || substr(id, 5) WHERE id LIKE 'epi-%'",
        [],
    )?;
    conn.execute(
        "UPDATE memory_embeddings SET entity_id = 'msg-' || substr(entity_id, 5)
         WHERE entity_type = 'episode' AND entity_id LIKE 'epi-%'",
        [],
    )?;

    // §ID: merge the `rem-` prefix into the unified activity id space. Jobs
    // and reminders are one activity family now, so reminders use `act-`
    // instead of the legacy `rem-` (the §ID block above already normalized
    // bare UUIDs to `rem-`). Idempotent and a no-op once no `rem-` id
    // remains; reminders.id has no foreign-key dependents, so the rewrite is
    // safe.
    conn.execute(
        "UPDATE reminders SET id = 'act-' || substr(id, 5) WHERE id LIKE 'rem-%'",
        [],
    )?;

    // §ID: unify the step counter column on the name used by events and the
    // frontend (`step_number`); `step_index` was only ever the DB-internal
    // name. Fresh databases get the new name from the MIGRATIONS CREATE above;
    // this guarded rename covers older ones.
    let has_step_number: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('task_steps') WHERE name='step_number'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_step_number {
        conn.execute(
            "ALTER TABLE task_steps RENAME COLUMN step_index TO step_number",
            [],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json;

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

    /// Build a database shaped like the pre-merge schema (sessions table,
    /// messages.session_id, tasks.session_id) — the full old table set, so
    /// the migration path is exercised end to end.
    fn create_legacy_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                ended_at TEXT,
                status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','closed')),
                parent_id TEXT REFERENCES sessions(id)
            );
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                session_id TEXT REFERENCES sessions(id),
                input_text TEXT NOT NULL DEFAULT '',
                title TEXT,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','paused','completed','failed','error','cancelled')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                transcript TEXT NOT NULL DEFAULT '',
                react_state TEXT
            );
            CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
                content TEXT NOT NULL,
                message_type TEXT CHECK(message_type IN ('text','thought','action','observation')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                tool_call_id TEXT,
                attachments TEXT,
                is_compacted INTEGER NOT NULL DEFAULT 0,
                compaction_id TEXT,
                parent_message_id TEXT REFERENCES messages(id)
            );
            CREATE TABLE task_steps (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                step_index INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                input TEXT NOT NULL DEFAULT '{}',
                output TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','completed','failed','error')),
                is_high_risk INTEGER NOT NULL DEFAULT 0,
                confirmed INTEGER,
                started_at TEXT,
                completed_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE preferences (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE whitelist (
                tool_name TEXT NOT NULL PRIMARY KEY,
                pattern TEXT,
                added_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE facts (
                id TEXT PRIMARY KEY,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'inferred'
                    CHECK(source IN ('user','inferred')),
                confidence REAL NOT NULL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE compaction_entries (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                summary TEXT NOT NULL,
                first_kept_entry_id TEXT,
                tokens_before INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            ",
        )
        .unwrap();
    }

    #[test]
    fn test_run_migrations_creates_all_tables() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let tables = get_tables(&conn);

        let expected = &[
            "facts",
            "memory_embeddings",
            "memory_episodes",
            "messages",
            "partial_messages",
            "kv_store",
            "reminders",
            "task_steps",
            "task_usage",
            "tasks",
            "whitelist",
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
        assert!(
            !tables.iter().any(|n| n == "sessions"),
            "sessions table should be gone"
        );
        let core: Vec<_> = tables
            .iter()
            .filter(|t| !t.starts_with("facts_fts"))
            .collect();
        assert_eq!(core.len(), expected.len());
    }

    #[test]
    fn test_run_migrations_creates_all_indexes() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let indexes = get_indexes(&conn);

        let expected = &[
            "idx_facts_confidence",
            "idx_facts_subject",
            "idx_memory_embeddings_type",
            "idx_memory_episodes_created",
            "idx_memory_episodes_task",
            "idx_messages_created_at",
            "idx_messages_task",
            "idx_task_steps_task",
            "idx_tasks_created_at",
            "idx_tasks_status",
        ];
        for ix in expected {
            assert!(
                indexes.iter().any(|n| n == ix),
                "expected index '{}' not found in {:?}",
                ix,
                indexes
            );
        }
        assert!(
            !indexes
                .iter()
                .any(|n| n == "idx_messages_session" || n == "idx_tasks_session"),
            "legacy session indexes should be gone"
        );
        let core: Vec<_> = indexes
            .iter()
            .filter(|n| !n.starts_with("facts_fts"))
            .collect();

        assert_eq!(core.len(), expected.len());
    }

    /// Reproduces the production startup crash: a database whose
    /// `user_version` is already past the array index where a table used to
    /// live (e.g. after a build with one more migration entry ran once) skips
    /// the CREATE TABLE via the version gate — while any later entry
    /// referencing the table still executes. The idempotent ensure block must
    /// repair it on every open, independent of user_version.
    #[test]
    fn run_migrations_repairs_memory_embeddings_when_version_ahead() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        assert!(get_tables(&conn).contains(&"memory_embeddings".to_string()));

        // Simulate the stale state seen in production: table gone, but
        // user_version left ahead of its former array index.
        conn.execute_batch("DROP TABLE memory_embeddings; PRAGMA user_version = 17;")
            .unwrap();
        assert!(!get_tables(&conn).contains(&"memory_embeddings".to_string()));

        // A fresh open must recreate the table instead of dying with
        // "no such table: main.memory_embeddings".
        run_migrations(&conn).unwrap();
        assert!(get_tables(&conn).contains(&"memory_embeddings".to_string()));
        assert!(get_indexes(&conn).contains(&"idx_memory_embeddings_type".to_string()));
    }

    /// Memory embeddings must not be re-created by the version-gated array
    /// (which would re-run them with a bogus user_version), but the ensure
    /// block must leave an existing index intact.
    #[test]
    fn run_migrations_idempotent_on_existing_memory_embeddings() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        // Second open: nothing should break and the table stays.
        run_migrations(&conn).unwrap();
        assert!(get_tables(&conn).contains(&"memory_embeddings".to_string()));
    }

    #[test]
    fn test_run_migrations_is_idempotent() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap();
        let tables = get_tables(&conn);
        assert!(!tables.is_empty());
    }

    #[test]
    fn test_user_version_prevents_rerun() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(version > 0);

        let tables_before = get_tables(&conn);
        let table_count_before = tables_before.len();
        run_migrations(&conn).unwrap();
        let tables_after = get_tables(&conn);
        assert_eq!(tables_after.len(), table_count_before);
    }

    /// kv_store replaced `preferences` at an early array index; databases
    /// whose user_version already passed that index (17 > array length) must
    /// still get the table via the unconditional ensure block.
    #[test]
    fn run_migrations_repairs_kv_store_when_version_ahead() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        assert!(get_tables(&conn).contains(&"kv_store".to_string()));

        // Simulate the stale state: table gone, user_version ahead of the
        // array index where the CREATE lives.
        conn.execute_batch("DROP TABLE kv_store; PRAGMA user_version = 17;")
            .unwrap();
        assert!(!get_tables(&conn).contains(&"kv_store".to_string()));

        // A fresh open must recreate the table (version-gated loop skips it).
        run_migrations(&conn).unwrap();
        assert!(get_tables(&conn).contains(&"kv_store".to_string()));
    }

    #[test]
    fn test_legacy_notify_reminders_backfilled_to_tool() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        // Simulate a pre-existing row from the previous schema (mode='notify'
        // with no tool_name), which the migration must rewrite to the
        // equivalent tool/notify action.
        conn.execute(
            "INSERT INTO reminders (id, due_at, title, body, mode, tool_name, tool_args, prompt, fired, created_at)
             VALUES ('legacy-1', '2099-01-01T00:00:00Z', 'Drink', 'water', 'notify', NULL, NULL, NULL, 0,
                     (datetime('now')))",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let row: (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT mode, tool_name, tool_args FROM reminders WHERE id='legacy-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, "tool");
        assert_eq!(row.1.as_deref(), Some("notify"));
        let args: serde_json::Value = serde_json::from_str(row.2.as_deref().unwrap()).unwrap();
        assert_eq!(args["title"], "Drink");
        assert_eq!(args["body"], "water");
    }

    #[test]
    fn test_react_state_column_exists_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let has_col: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='react_state'")
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(has_col);
    }

    #[test]
    fn test_compaction_columns_dropped_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        for col in &["is_compacted", "compaction_id"] {
            let has: bool = conn
                .prepare(&format!(
                    "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='{}'",
                    col
                ))
                .unwrap()
                .query_row([], |r| r.get::<_, i32>(0))
                .map(|c| c > 0)
                .unwrap_or(false);
            assert!(!has, "column '{}' should be dropped", col);
        }
    }

    #[test]
    fn test_task_steps_columns_exist_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        for col in &[
            "thought",
            "action_tool",
            "action_input",
            "observation",
            "silent",
        ] {
            let has: bool = conn
                .prepare(&format!(
                    "SELECT COUNT(*) FROM pragma_table_info('task_steps') WHERE name='{}'",
                    col
                ))
                .unwrap()
                .query_row([], |r| r.get::<_, i32>(0))
                .map(|c| c > 0)
                .unwrap_or(false);
            assert!(has, "column '{}' should exist", col);
        }
    }

    #[test]
    fn test_parent_task_id_column_dropped_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let has: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='parent_task_id'")
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(!has);
    }

    #[test]
    fn test_tasks_check_constraint_fix() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, input_text, status) VALUES ('t1', 'test', 'completed')",
            [],
        )
        .unwrap();
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[test]
    fn test_facts_tags_column_exists() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let has: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='tags'")
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(has, "facts.tags column should exist");
    }

    #[test]
    fn test_facts_lifecycle_columns_exist() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        for col in &["mention_count", "last_seen_at", "source_ref"] {
            let has: bool = conn
                .prepare(&format!(
                    "SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name='{}'",
                    col
                ))
                .unwrap()
                .query_row([], |r| r.get::<_, i32>(0))
                .map(|c| c > 0)
                .unwrap_or(false);
            assert!(has, "facts.{} column should exist", col);
        }
    }

    #[test]
    fn test_hindsight_store_dropped() {
        let conn = create_test_conn();
        // First run creates it (from old migration), then the drop removes it.
        // But since we removed the creation, it should never exist.
        run_migrations(&conn).unwrap();
        let exists: bool = conn
            .prepare(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='hindsight_store'",
            )
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(!exists, "hindsight_store table should not exist");
    }

    #[test]
    fn test_task_usage_table_exists_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let exists: bool = conn
            .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_usage'")
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(exists, "task_usage table should exist");
    }

    #[test]
    fn test_legacy_schema_converted_to_tasks() {
        let conn = create_test_conn();
        create_legacy_schema(&conn);
        conn.execute_batch(
            "INSERT INTO sessions (id, status, parent_id) VALUES ('s1', 'active', NULL);
             INSERT INTO tasks (id, session_id, input_text, status) VALUES ('t1', 's1', 'hello', 'paused');
             INSERT INTO messages (id, session_id, role, content, message_type)
                 VALUES ('m1', 's1', 'user', 'hello', 'text'),
                        ('m2', 's1', 'assistant', 'hi', 'text');
             INSERT INTO compaction_entries (id, session_id, summary, tokens_before)
                 VALUES ('c1', 's1', 'compacted', 100);
            ",
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 0").unwrap();
        run_migrations(&conn).unwrap();

        // sessions table gone.
        assert!(!get_tables(&conn).iter().any(|t| t == "sessions"));

        // Messages now belong to the task that owned the session. The §ID
        // migration then prefixes the converted ids to the canonical format.
        let task_id: String = conn
            .query_row(
                "SELECT task_id FROM messages WHERE id = 'msg-m1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(task_id, "task-t1");
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Compaction entries were migrated by the merge rebuild, then the
        // (dead, write-only) table is dropped again on every open.
        assert!(
            !get_tables(&conn).iter().any(|t| t == "compaction_entries"),
            "compaction_entries table should be dropped"
        );

        // tasks no longer carry session_id; the branch-link column
        // (parent_task_id) is dropped after conversion.
        let has_session_col: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='session_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_session_col, 0);
        let has_parent_col: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='parent_task_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_parent_col, 0, "parent_task_id column should be dropped");

        // The created_at index survives the table rebuild (it was dropped
        // with the old messages table and must be recreated).
        let idx_created_at: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_messages_created_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            idx_created_at, 1,
            "idx_messages_created_at must survive conversion"
        );
    }

    #[test]
    fn test_legacy_schema_preserves_branch_chain() {
        let conn = create_test_conn();
        create_legacy_schema(&conn);
        // Parent session s1 (task t1), child session s2 (task t2) — the
        // classic branched-task shape.
        conn.execute_batch(
            "INSERT INTO sessions (id, status, parent_id) VALUES ('s1', 'active', NULL), ('s2', 'active', 's1');
             INSERT INTO tasks (id, session_id, input_text, status) VALUES ('t1', 's1', 'parent', 'paused'), ('t2', 's2', 'child', 'paused');
             INSERT INTO messages (id, session_id, role, content) VALUES ('m1', 's1', 'user', 'a'), ('m2', 's2', 'user', 'b');
            ",
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 0").unwrap();
        run_migrations(&conn).unwrap();

        // The branch-link column is dropped after conversion (the branching
        // feature was removed; nothing consumes the link anymore).
        let has_parent_col: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='parent_task_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_parent_col, 0, "parent_task_id column should be dropped");
        // Messages routed to their own tasks (ids §ID-prefixed afterwards).
        let t1_msgs: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE task_id = 'task-t1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(t1_msgs, 1);
        let t2_msgs: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE task_id = 'task-t2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(t2_msgs, 1);
    }

    #[test]
    fn test_legacy_cancelled_status_normalized_during_conversion() {
        // A database from before the cancelled-fix migration can hold
        // status='cancelled' rows. The conversion rebuilds tasks with a CHECK
        // that excludes 'cancelled' — those rows must be normalized to
        // 'error' instead of aborting the whole migration.
        let conn = create_test_conn();
        create_legacy_schema(&conn);
        conn.execute_batch(
            "INSERT INTO sessions (id, status) VALUES ('s1', 'active');
             INSERT INTO tasks (id, session_id, input_text, status) VALUES ('t1', 's1', 'legacy', 'cancelled');
            ",
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 0").unwrap();
        run_migrations(&conn).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'task-t1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            status, "error",
            "cancelled rows must be normalized to error"
        );
    }

    #[test]
    fn test_legacy_orphan_messages_dropped() {
        let conn = create_test_conn();
        create_legacy_schema(&conn);
        // A session with messages but NO task (crashed run) — those messages
        // cannot be attributed to any task and must not block the migration.
        conn.execute_batch(
            "INSERT INTO sessions (id, status) VALUES ('s1', 'active');
             INSERT INTO messages (id, session_id, role, content) VALUES ('m1', 's1', 'user', 'orphan');
            ",
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 0").unwrap();
        run_migrations(&conn).unwrap();
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "orphaned messages must be dropped");
    }

    #[test]
    fn run_migrations_unifies_legacy_id_formats() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();

        // Seed rows in the pre-§ID format (bare hyphenated UUIDs).
        let old_task = "550e8400-e29b-41d4-a716-446655440000";
        let old_msg = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        let old_step = "7ba7b811-9dad-11d1-80b4-00c04fd430c9";
        let old_fact = "8ba7b812-9dad-11d1-80b4-00c04fd430ca";
        let old_epi = "9ba7b813-9dad-11d1-80b4-00c04fd430cb";
        conn.execute(
            "INSERT INTO tasks (id, input_text, status) VALUES (?1, 'x', 'pending')",
            rusqlite::params![old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (id, task_id, role, content, created_at)
             VALUES (?1, ?2, 'user', 'hi', (datetime('now')))",
            rusqlite::params![old_msg, old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_steps (id, task_id, step_number, tool_name, input, status)
             VALUES (?1, ?2, 1, 'shell', '{}', 'completed')",
            rusqlite::params![old_step, old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, source_ref)
             VALUES (?1, 'u', 'name', 'n', ?2)",
            rusqlite::params![old_fact, format!(r#"{{"message_id":"{old_msg}"}}"#)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_episodes (id, task_id, summary) VALUES (?1, ?2, 'summary')",
            rusqlite::params![old_epi, old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO reminders (id, due_at, title, body, task_id)
             VALUES ('rem-9d4c3b2a-1111-2222-3333-444455556666', '2099-01-01', 't', 'b', ?1)",
            rusqlite::params![old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO partial_messages (task_id, content) VALUES (?1, 'stream')",
            rusqlite::params![old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_usage (task_id, prompt_tokens) VALUES (?1, 10)",
            rusqlite::params![old_task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kv_store (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("fact_extraction.{old_task}"), old_msg],
        )
        .unwrap();
        // Vector index rows pointing at the old ids (fact → facts.id,
        // episode → messages.id and memory_episodes.id).
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('fact', ?1, 'm', X'0102', 'f')",
            rusqlite::params![old_fact],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('episode', ?1, 'm', X'0102', 'e1')",
            rusqlite::params![old_msg],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('episode', ?1, 'm', X'0102', 'e2')",
            rusqlite::params![old_epi],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let canon = |prefix: &str, s: &str| format!("{prefix}-{}", s.replace('-', ""));
        assert_eq!(
            conn.query_row("SELECT id FROM tasks", [], |r| r.get::<_, String>(0))
                .unwrap(),
            canon("task", old_task)
        );
        assert_eq!(
            conn.query_row("SELECT id FROM messages", [], |r| r.get::<_, String>(0))
                .unwrap(),
            canon("msg", old_msg)
        );
        assert_eq!(
            conn.query_row("SELECT id FROM task_steps", [], |r| r.get::<_, String>(0))
                .unwrap(),
            canon("step", old_step)
        );
        assert_eq!(
            conn.query_row("SELECT id FROM facts", [], |r| r.get::<_, String>(0))
                .unwrap(),
            canon("fact", old_fact)
        );
        assert_eq!(
            conn.query_row("SELECT id FROM memory_episodes", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            canon("msg", old_epi)
        );
        let rem_id: String = conn
            .query_row("SELECT id FROM reminders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rem_id,
            format!(
                "act-{}",
                "9d4c3b2a-1111-2222-3333-444455556666".replace('-', "")
            )
        );

        // Every task_id reference follows the renamed task id.
        for sql in [
            "SELECT task_id FROM messages",
            "SELECT task_id FROM task_steps",
            "SELECT task_id FROM partial_messages",
            "SELECT task_id FROM task_usage",
            "SELECT task_id FROM memory_episodes",
        ] {
            assert_eq!(
                conn.query_row(sql, [], |r| r.get::<_, String>(0)).unwrap(),
                canon("task", old_task),
                "{sql}"
            );
        }
        let rem_task: Option<String> = conn
            .query_row("SELECT task_id FROM reminders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rem_task.as_deref(), Some(canon("task", old_task).as_str()));

        // Embeddings follow their owning table's prefix.
        let (fact_emb, msg_emb, epi_emb): (String, String, String) = conn
            .query_row(
                "SELECT
                   (SELECT entity_id FROM memory_embeddings WHERE entity_type='fact'),
                   (SELECT entity_id FROM memory_embeddings WHERE entity_type='episode' AND text='e1'),
                   (SELECT entity_id FROM memory_embeddings WHERE entity_type='episode' AND text='e2')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(fact_emb, canon("fact", old_fact));
        assert_eq!(msg_emb, canon("msg", old_msg));
        assert_eq!(epi_emb, canon("msg", old_epi));

        // kv cursor key/value rewritten (key embeds the task id, value is a
        // message id).
        let (k, v): (String, String) = conn
            .query_row("SELECT key, value FROM kv_store", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(k, format!("fact_extraction.{}", canon("task", old_task)));
        assert_eq!(v, canon("msg", old_msg));

        // facts.source_ref points at the prefixed message id.
        let sr: String = conn
            .query_row("SELECT source_ref FROM facts", [], |r| r.get(0))
            .unwrap();
        let sr_json: serde_json::Value = serde_json::from_str(&sr).unwrap();
        assert_eq!(sr_json["message_id"], canon("msg", old_msg));

        // step_index was renamed to step_number.
        let has_step_number: i32 = conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('task_steps') WHERE name='step_number'",
            )
            .unwrap()
            .query_row([], |r| r.get(0))
            .unwrap();
        assert_eq!(has_step_number, 1);
        let has_step_index: i32 = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('task_steps') WHERE name='step_index'")
            .unwrap()
            .query_row([], |r| r.get(0))
            .unwrap();
        assert_eq!(has_step_index, 0);

        // Idempotent: a second pass changes nothing.
        run_migrations(&conn).unwrap();
        assert_eq!(
            conn.query_row("SELECT id FROM tasks", [], |r| r.get::<_, String>(0))
                .unwrap(),
            canon("task", old_task)
        );
    }

    #[test]
    fn episode_ids_merge_into_message_id_space() {
        // A database already on canonical ids (the §ID gate above does not
        // fire) but storing `epi-{uuid32}` episodes from the old scheme:
        // memory_episodes rows and their episode embeddings must be rewritten
        // to the shared `msg-` id space.
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let task = haven_common::types::new_id("task");
        let epi = haven_common::types::new_id("epi");
        let msg = haven_common::types::new_id("msg");
        conn.execute(
            "INSERT INTO tasks (id, input_text) VALUES (?1, '')",
            rusqlite::params![task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_episodes (id, task_id, summary) VALUES (?1, ?2, 's')",
            rusqlite::params![epi, task],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('episode', ?1, 'm', X'0102', 'e')",
            rusqlite::params![epi],
        )
        .unwrap();
        // A message embedding in the same domain must be left untouched.
        conn.execute(
            "INSERT INTO memory_embeddings (entity_type, entity_id, model, vector, text)
             VALUES ('episode', ?1, 'm', X'0102', 'm')",
            rusqlite::params![msg],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let expected = format!("msg-{}", &epi[4..]);
        let (row_id, emb_id): (String, String) = conn
            .query_row(
                "SELECT
                   (SELECT id FROM memory_episodes),
                   (SELECT entity_id FROM memory_embeddings WHERE text='e')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row_id, expected);
        assert_eq!(emb_id, expected);
        let msg_emb: String = conn
            .query_row(
                "SELECT entity_id FROM memory_embeddings WHERE text='m'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_emb, msg);
        // Idempotent: a second pass changes nothing.
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn facts_durability_column_defaults_to_one() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        // Column exists with the backward-compatible default.
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object)
             VALUES ('f1', 'user', 'likes', 'Rust')",
            [],
        )
        .unwrap();
        let durability: f64 = conn
            .query_row("SELECT durability FROM facts WHERE id='f1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(durability, 1.0);
        // Idempotent on re-run.
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn fact_embedding_triggers_exist_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
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
    fn old_reminders_schema_accepts_job_rows_after_migration() {
        let conn = create_test_conn();
        // Simulate a database created before the activity merge: the old
        // reminders table with NOT NULL due_at/body and no job columns.
        conn.execute_batch(
            "CREATE TABLE reminders (
                id TEXT PRIMARY KEY,
                due_at TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT 'Haven',
                body TEXT NOT NULL,
                fired INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO reminders (id, due_at, title, body) VALUES ('act-old', '2099-01-01', 'Haven', 'b')",
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 0").unwrap();
        run_migrations(&conn).unwrap();

        // New columns were added and old rows default to kind='reminder'.
        let kind: String = conn
            .query_row("SELECT kind FROM reminders WHERE id='act-old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "reminder");

        // A minimal job insert (what `save_job` issues) must succeed against
        // the migrated old table — the NOT NULL reminder columns are covered
        // by the placeholder due_at/body values.
        conn.execute(
            "INSERT INTO reminders (id, kind, task_id, command, status, due_at, body, started_at, created_at)
             VALUES ('act-j1', 'job', NULL, 'echo hi', 'running', '2026-08-09T10:00:00Z', '', '2026-08-09T10:00:00Z', datetime('now'))",
            [],
        )
        .unwrap();
        let (status, body): (String, String) = conn
            .query_row(
                "SELECT status, body FROM reminders WHERE id='act-j1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(body, "");
    }
}
