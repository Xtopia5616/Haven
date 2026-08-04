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
        parent_message_id TEXT REFERENCES messages(id)
    )",
    "CREATE TABLE IF NOT EXISTS task_steps (
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
    )",
    "CREATE TABLE IF NOT EXISTS preferences (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
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
    // Appended last (append-only): the reminders table was originally added
    // mid-array, which existing databases (user_version already past its
    // index) never re-ran — leaving them without the table and bricking the
    // startup migration. Schema includes all current columns so fresh
    // databases skip the per-column ALTERs below.
    "CREATE TABLE IF NOT EXISTS reminders (
        id TEXT PRIMARY KEY,
        due_at TEXT NOT NULL,
        title TEXT NOT NULL DEFAULT 'Haven',
        body TEXT NOT NULL,
        mode TEXT NOT NULL DEFAULT 'tool',
        task_id TEXT,
        tool_name TEXT,
        tool_args TEXT,
        prompt TEXT,
        fired INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
];

pub fn run_migrations(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    let mut ran_any = false;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        if i as i32 >= version {
            // execute_batch (not execute) so no-op placeholder statements
            // such as "SELECT 1" (removed tables) are allowed.
            conn.execute_batch(sql)?;
            ran_any = true;
        }
    }
    if ran_any {
        conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len()))?;
    }

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
                .prepare("SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='parent_id'")?
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

    // Reminders: ensure the table exists BEFORE the per-column ALTERs below.
    // The CREATE lives in MIGRATIONS too, but the version-gated loop cannot
    // be relied on: some existing databases carry a user_version higher than
    // the array length (past migrations were pruned from the array), so they
    // would never re-run the CREATE and every ALTER below would fail with
    // "no such table". Full current schema — the ALTERs then no-op.
    let has_reminders_table: bool = conn
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='reminders'")?
        .query_row([], |r| r.get::<_, i32>(0))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !has_reminders_table {
        conn.execute_batch(
            "CREATE TABLE reminders (
                id TEXT PRIMARY KEY,
                due_at TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT 'Haven',
                body TEXT NOT NULL,
                mode TEXT NOT NULL DEFAULT 'tool',
                task_id TEXT,
                tool_name TEXT,
                tool_args TEXT,
                prompt TEXT,
                fired INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )?;
    }

    // Reminders get an optional prompt column: when set, the app wakes the
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
        ("mode", "ALTER TABLE reminders ADD COLUMN mode TEXT NOT NULL DEFAULT 'tool'"),
        ("task_id", "ALTER TABLE reminders ADD COLUMN task_id TEXT"),
        ("tool_name", "ALTER TABLE reminders ADD COLUMN tool_name TEXT"),
        ("tool_args", "ALTER TABLE reminders ADD COLUMN tool_args TEXT"),
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

    // Fix CHECK constraint typo: 'pendingleted' → 'completed'.
    // Use a user_version-based gating so this runs exactly once.
    if version <= MIGRATIONS.len() as i32 {
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
        // Bump user_version past MIGRATIONS so this never runs again.
        conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len() + 1))?;
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
            "compaction_entries",
            "facts",
            "messages",
            "preferences",
            "reminders",
            "task_steps",
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
        assert!(
            !tables.iter().any(|n| n == "sessions"),
            "sessions table should be gone"
        );
        assert_eq!(tables.len(), expected.len());
    }

    #[test]
    fn test_run_migrations_creates_all_indexes() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let indexes = get_indexes(&conn);

        let expected = &[
            "idx_facts_confidence",
            "idx_facts_subject",
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
            !indexes.iter().any(|n| n == "idx_messages_session" || n == "idx_tasks_session"),
            "legacy session indexes should be gone"
        );
        assert_eq!(indexes.len(), expected.len());
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
    fn test_compaction_columns_exist_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let has_is_compacted: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='is_compacted'")
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(has_is_compacted);
        let has_compaction_id: bool = conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name='compaction_id'",
            )
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(has_compaction_id);
    }

    #[test]
    fn test_task_steps_columns_exist_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        for col in &["thought", "action_tool", "action_input", "observation"] {
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
    fn test_parent_task_id_column_exists_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let has: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='parent_task_id'")
            .unwrap()
            .query_row([], |r| r.get::<_, i32>(0))
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(has);
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

        // Messages now belong to the task that owned the session.
        let task_id: String = conn
            .query_row("SELECT task_id FROM messages WHERE id = 'm1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(task_id, "t1");
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Compaction entries migrated.
        let comp_task: String = conn
            .query_row(
                "SELECT task_id FROM compaction_entries WHERE id = 'c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(comp_task, "t1");

        // tasks no longer carry session_id; parent_task_id is NULL here.
        let has_session_col: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='session_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_session_col, 0);
        let parent: Option<String> = conn
            .query_row("SELECT parent_task_id FROM tasks WHERE id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(parent.is_none());

        // The created_at index survives the table rebuild (it was dropped
        // with the old messages table and must be recreated).
        let idx_created_at: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_messages_created_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_created_at, 1, "idx_messages_created_at must survive conversion");
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

        // t2's parent_task_id points at t1 (the owner of the parent session).
        let parent: String = conn
            .query_row("SELECT parent_task_id FROM tasks WHERE id = 't2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(parent, "t1");
        // Messages routed to their own tasks.
        let t1_msgs: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages WHERE task_id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(t1_msgs, 1);
        let t2_msgs: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages WHERE task_id = 't2'", [], |r| {
                r.get(0)
            })
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
            .query_row("SELECT status FROM tasks WHERE id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "error", "cancelled rows must be normalized to error");
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
}
