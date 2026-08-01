pub const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        started_at TEXT NOT NULL DEFAULT (datetime('now')),
        ended_at TEXT,
        status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','closed'))
    )",
    "CREATE TABLE IF NOT EXISTS messages (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK(role IN ('user','assistant','system','tool')),
        content TEXT NOT NULL,
        message_type TEXT CHECK(message_type IN ('text','thought','action','observation')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        tool_call_id TEXT
    )",
    "CREATE TABLE IF NOT EXISTS tasks (
        id TEXT PRIMARY KEY,
        session_id TEXT REFERENCES sessions(id),
        input_text TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'pending'
            CHECK(status IN ('pending','running','paused','completed','failed','error')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
        transcript TEXT NOT NULL DEFAULT '',
        react_state TEXT
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
    "CREATE TABLE IF NOT EXISTS mcp_servers (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        transport TEXT NOT NULL CHECK(transport IN ('stdio','sse')),
        config TEXT NOT NULL DEFAULT '{}',
        enabled INTEGER NOT NULL DEFAULT 1,
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
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_task_steps_task ON task_steps(task_id)",
    "CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject)",
    "CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at)",
    "CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status)",
    "CREATE INDEX IF NOT EXISTS idx_facts_confidence ON facts(confidence)",
];

pub fn run_migrations(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    let mut ran_any = false;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        if i as i32 >= version {
            conn.execute(sql, [])?;
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

    // §3.5: add parent_id column to sessions table
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

    // §3.2: create compaction_entries table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS compaction_entries (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            summary TEXT NOT NULL,
            first_kept_entry_id TEXT,
            tokens_before INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )?;

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
                  session_id TEXT REFERENCES sessions(id),
                  input_text TEXT NOT NULL DEFAULT '',
                  title TEXT,
                  status TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending','running','paused','completed','failed','error')),
                  created_at TEXT NOT NULL DEFAULT (datetime('now')),
                  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                  transcript TEXT NOT NULL DEFAULT '',
                  react_state TEXT
              );
               INSERT INTO tasks_rebuild SELECT id, session_id, input_text, NULL, status, created_at, updated_at, transcript, react_state FROM tasks;
             DROP TABLE tasks;
             ALTER TABLE tasks_rebuild RENAME TO tasks;
             CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id);
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
                  session_id TEXT REFERENCES sessions(id),
                  input_text TEXT NOT NULL DEFAULT '',
                  title TEXT,
                  status TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending','running','paused','completed','failed','error')),
                  created_at TEXT NOT NULL DEFAULT (datetime('now')),
                  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                  transcript TEXT NOT NULL DEFAULT '',
                  react_state TEXT
              );
              INSERT INTO tasks_rebuild SELECT id, session_id, input_text, title, status, created_at, updated_at, transcript, react_state FROM tasks;
             DROP TABLE tasks;
             ALTER TABLE tasks_rebuild RENAME TO tasks;
             CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id);
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
    fn test_run_migrations_creates_all_tables() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let tables = get_tables(&conn);

        let expected = &[
            "compaction_entries",
            "facts",
            "mcp_servers",
            "messages",
            "preferences",
            "sessions",
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
            "idx_messages_session",
            "idx_task_steps_task",
            "idx_tasks_created_at",
            "idx_tasks_session",
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
    fn test_parent_id_column_exists_after_migration() {
        let conn = create_test_conn();
        run_migrations(&conn).unwrap();
        let has: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='parent_id'")
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
}
