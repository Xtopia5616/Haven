use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub config: String,
    pub enabled: bool,
    pub created_at: String,
}

impl Database {
    pub fn save_mcp_server(
        &self,
        name: &str,
        transport: &str,
        config: &str,
    ) -> anyhow::Result<McpServer> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO mcp_servers (id, name, transport, config, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            rusqlite::params![id, name, transport, config, now],
        )?;
        Ok(McpServer {
            id,
            name: name.into(),
            transport: transport.into(),
            config: config.into(),
            enabled: true,
            created_at: now,
        })
    }

    pub fn list_mcp_servers(&self) -> anyhow::Result<Vec<McpServer>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, transport, config, enabled, created_at FROM mcp_servers ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(McpServer {
                id: row.get(0)?,
                name: row.get(1)?,
                transport: row.get(2)?,
                config: row.get(3)?,
                enabled: row.get::<_, i32>(4)? != 0,
                created_at: row.get(5)?,
            })
        })?;
        let mut list = Vec::new();
        for row in rows {
            list.push(row?);
        }
        Ok(list)
    }

    pub fn delete_mcp_server(&self, id: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "DELETE FROM mcp_servers WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    pub fn set_mcp_server_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE mcp_servers SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![enabled as i32, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_save_mcp_server() {
        let db = create_db();
        let server = db
            .save_mcp_server("my-server", "stdio", r#"{"command":"node"}"#)
            .unwrap();
        assert!(!server.id.is_empty());
        assert_eq!(server.name, "my-server");
        assert_eq!(server.transport, "stdio");
        assert_eq!(server.config, r#"{"command":"node"}"#);
        assert!(server.enabled);
        assert!(!server.created_at.is_empty());
    }

    #[test]
    fn test_save_mcp_server_sse() {
        let db = create_db();
        let server = db
            .save_mcp_server("api", "sse", r#"{"url":"http://localhost:3000"}"#)
            .unwrap();
        assert_eq!(server.transport, "sse");
        assert!(server.enabled);
    }

    #[test]
    fn test_list_mcp_servers_empty() {
        let db = create_db();
        let servers = db.list_mcp_servers().unwrap();
        assert!(servers.is_empty());
    }

    #[test]
    fn test_list_mcp_servers_with_entries() {
        let db = create_db();
        db.save_mcp_server("server-b", "stdio", "{}").unwrap();
        db.save_mcp_server("server-a", "sse", "{}").unwrap();
        db.save_mcp_server("server-c", "stdio", "{}").unwrap();

        let servers = db.list_mcp_servers().unwrap();
        assert_eq!(servers.len(), 3);
    }

    #[test]
    fn test_list_mcp_servers_ordering() {
        let db = create_db();
        db.save_mcp_server("c-server", "stdio", "{}").unwrap();
        db.save_mcp_server("a-server", "sse", "{}").unwrap();
        db.save_mcp_server("b-server", "stdio", "{}").unwrap();

        let servers = db.list_mcp_servers().unwrap();
        assert_eq!(servers[0].name, "a-server");
        assert_eq!(servers[1].name, "b-server");
        assert_eq!(servers[2].name, "c-server");
    }

    #[test]
    fn test_delete_mcp_server_existing() {
        let db = create_db();
        let server = db
            .save_mcp_server("server", "stdio", "{}")
            .unwrap();
        db.delete_mcp_server(&server.id).unwrap();
        let servers = db.list_mcp_servers().unwrap();
        assert!(servers.is_empty());
    }

    #[test]
    fn test_delete_mcp_server_non_existing() {
        let db = create_db();
        let result = db.delete_mcp_server("non-existent-id");
        assert!(result.is_ok());
    }

    #[test]
    fn test_set_mcp_server_enabled_true() {
        let db = create_db();
        let server = db
            .save_mcp_server("server", "stdio", "{}")
            .unwrap();
        assert!(server.enabled);
        db.set_mcp_server_enabled(&server.id, false).unwrap();

        let servers = db.list_mcp_servers().unwrap();
        let s = servers.iter().find(|s| s.id == server.id).unwrap();
        assert!(!s.enabled);
    }

    #[test]
    fn test_set_mcp_server_enabled_false() {
        let db = create_db();
        let server = db
            .save_mcp_server("server", "stdio", "{}")
            .unwrap();
        db.set_mcp_server_enabled(&server.id, false).unwrap();
        db.set_mcp_server_enabled(&server.id, true).unwrap();

        let servers = db.list_mcp_servers().unwrap();
        let s = servers.iter().find(|s| s.id == server.id).unwrap();
        assert!(s.enabled);
    }

    #[test]
    fn test_mcp_server_full_lifecycle() {
        let db = create_db();
        assert!(db.list_mcp_servers().unwrap().is_empty());

        let s1 = db
            .save_mcp_server("server-1", "stdio", r#"{"cmd":"a"}"#)
            .unwrap();
        let s2 = db
            .save_mcp_server("server-2", "sse", r#"{"url":"b"}"#)
            .unwrap();

        let servers = db.list_mcp_servers().unwrap();
        assert_eq!(servers.len(), 2);

        db.set_mcp_server_enabled(&s1.id, false).unwrap();
        let servers = db.list_mcp_servers().unwrap();
        let s1_refreshed = servers.iter().find(|s| s.id == s1.id).unwrap();
        assert!(!s1_refreshed.enabled);
        let s2_refreshed = servers.iter().find(|s| s.id == s2.id).unwrap();
        assert!(s2_refreshed.enabled);

        db.delete_mcp_server(&s1.id).unwrap();
        let servers = db.list_mcp_servers().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, s2.id);

        db.delete_mcp_server(&s2.id).unwrap();
        assert!(db.list_mcp_servers().unwrap().is_empty());
    }
}
