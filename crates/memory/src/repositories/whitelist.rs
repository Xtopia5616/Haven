use crate::db::Database;
use chrono::Utc;

impl Database {
    pub fn add_whitelist(&self, tool_name: &str, pattern: Option<&str>) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn().execute(
            "INSERT OR REPLACE INTO whitelist (tool_name, pattern, added_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![tool_name, pattern, now],
        )?;
        Ok(())
    }

    pub fn remove_whitelist(&self, tool_name: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "DELETE FROM whitelist WHERE tool_name = ?1",
            rusqlite::params![tool_name],
        )?;
        Ok(())
    }

    pub fn is_whitelisted(&self, tool_name: &str) -> anyhow::Result<bool> {
        let count: i32 = self.conn().query_row(
            "SELECT COUNT(*) FROM whitelist WHERE tool_name = ?1",
            rusqlite::params![tool_name],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn list_whitelist(&self) -> anyhow::Result<Vec<(String, Option<String>, String)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT tool_name, pattern, added_at FROM whitelist ORDER BY tool_name")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut list = Vec::new();
        for row in rows {
            list.push(row?);
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_add_whitelist_with_pattern() {
        let db = create_db();
        db.add_whitelist("explorer", Some(r"C:\Windows")).unwrap();
        assert!(db.is_whitelisted("explorer").unwrap());
    }

    #[test]
    fn test_add_whitelist_without_pattern() {
        let db = create_db();
        db.add_whitelist("notepad", None).unwrap();
        assert!(db.is_whitelisted("notepad").unwrap());
    }

    #[test]
    fn test_add_whitelist_replace_existing() {
        let db = create_db();
        db.add_whitelist("tool", Some("pat1")).unwrap();
        db.add_whitelist("tool", Some("pat2")).unwrap();
        let list = db.list_whitelist().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1, Some("pat2".into()));
    }

    #[test]
    fn test_remove_whitelist_existing() {
        let db = create_db();
        db.add_whitelist("explorer", None).unwrap();
        db.remove_whitelist("explorer").unwrap();
        assert!(!db.is_whitelisted("explorer").unwrap());
    }

    #[test]
    fn test_remove_whitelist_non_existing() {
        let db = create_db();
        let result = db.remove_whitelist("nonexistent");
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_whitelisted_true() {
        let db = create_db();
        db.add_whitelist("explorer", None).unwrap();
        assert!(db.is_whitelisted("explorer").unwrap());
    }

    #[test]
    fn test_is_whitelisted_false() {
        let db = create_db();
        assert!(!db.is_whitelisted("not_in_list").unwrap());
    }

    #[test]
    fn test_is_whitelisted_false_after_removal() {
        let db = create_db();
        db.add_whitelist("explorer", None).unwrap();
        db.remove_whitelist("explorer").unwrap();
        assert!(!db.is_whitelisted("explorer").unwrap());
    }

    #[test]
    fn test_list_whitelist_empty() {
        let db = create_db();
        let list = db.list_whitelist().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_list_whitelist_with_entries() {
        let db = create_db();
        db.add_whitelist("browser", Some("https://*")).unwrap();
        db.add_whitelist("explorer", None).unwrap();
        db.add_whitelist("access", Some("*.mdb")).unwrap();

        let list = db.list_whitelist().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].0, "access");
        assert_eq!(list[1].0, "browser");
        assert_eq!(list[2].0, "explorer");
    }

    #[test]
    fn test_list_whitelist_ordering() {
        let db = create_db();
        db.add_whitelist("c", None).unwrap();
        db.add_whitelist("a", None).unwrap();
        db.add_whitelist("b", None).unwrap();

        let list = db.list_whitelist().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].0, "a");
        assert_eq!(list[1].0, "b");
        assert_eq!(list[2].0, "c");
    }

    #[test]
    fn test_whitelist_pattern_stored() {
        let db = create_db();
        db.add_whitelist("filesystem", Some(r"C:\Users\*\Documents\*")).unwrap();
        let list = db.list_whitelist().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1, Some(r"C:\Users\*\Documents\*".into()));
    }
}
