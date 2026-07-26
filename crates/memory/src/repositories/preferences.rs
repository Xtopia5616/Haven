use crate::db::Database;
use chrono::Utc;

/// Inferrable preference entries extracted from conversation messages.
#[derive(Debug, Clone)]
pub struct InferredPreference {
    pub key: String,
    pub value: String,
    /// 0.0–1.0; user-set preferences have confidence 1.0.
    pub confidence: f64,
}

impl Database {
    pub fn set_preference(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO preferences (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![key, value, now],
        )?;
        Ok(())
    }

    pub fn get_preference(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT value FROM preferences WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn set_preference_bool(&self, key: &str, value: bool) -> anyhow::Result<()> {
        self.set_preference(key, if value { "1" } else { "0" })
    }

    pub fn get_preference_bool(&self, key: &str, default: bool) -> bool {
        self.get_preference(key)
            .ok()
            .flatten()
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(default)
    }

    pub fn set_preference_i64(&self, key: &str, value: i64) -> anyhow::Result<()> {
        self.set_preference(key, &value.to_string())
    }

    pub fn get_preference_i64(&self, key: &str, default: i64) -> i64 {
        self.get_preference(key)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    pub fn delete_preference(&self, key: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM preferences WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    pub fn list_preferences(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key, value FROM preferences ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut list = Vec::new();
        for row in rows {
            list.push(row?);
        }
        Ok(list)
    }

    /// Increment an integer counter stored as a preference key. Used by the
    /// preference auto-learning mechanism (e.g. tracking which tools the user
    /// invokes most often). Creates the counter at 1 when the key does not yet
    /// exist.
    pub fn increment_counter(&self, key: &str, by: i64) -> anyhow::Result<i64> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let current: i64 = conn
            .query_row(
                "SELECT value FROM preferences WHERE key = ?1",
                rusqlite::params![key],
                |r| {
                    Ok(r.get::<_, String>(0)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0))
                },
            )
            .unwrap_or(0);
        let new_val = current + by;
        conn.execute(
            "INSERT INTO preferences (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![key, new_val.to_string(), now],
        )?;
        Ok(new_val)
    }

    /// Retrieve the items currently driving inferred preferences (e.g. the most
    /// frequently used tool). Returns `None` if no data has been recorded.
    pub fn most_used_tool(&self) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT key FROM preferences
             WHERE key LIKE 'tool_usage.%'
             ORDER BY CAST(value AS INTEGER) DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            return Ok(Some(key.trim_start_matches("tool_usage.").to_string()));
        }
        Ok(None)
    }

    /// Record a successful tool invocation together with its parameters.
    /// Parameter patterns are aggregated: if the same tool+param_key+param_value
    /// triple repeats, the counter is incremented so frequently-used patterns
    /// float to the top in preference summaries.
    pub fn record_tool_usage(
        &self,
        tool_name: &str,
        params: &serde_json::Value,
        success: bool,
    ) -> anyhow::Result<()> {
        self.increment_counter(&format!("tool_usage.{}", tool_name), 1)?;
        if !success || params.is_null() {
            return Ok(());
        }
        let params_obj = match params {
            serde_json::Value::Object(map) => map,
            _ => return Ok(()),
        };
        for (param_key, param_value) in params_obj {
            let val_str = match param_value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if val_str.len() > 200 {
                continue;
            }
            self.increment_counter(
                &format!("tool_param.{}.{}.{}", tool_name, param_key, val_str),
                1,
            )?;
        }
        Ok(())
    }

    /// Extract (key, value, confidence) tuples from conversation messages via
    /// rule-based patterns. Lower-case matching is used so capitalisation does
    /// not affect extraction. Confidence is always below 1.0 so user-set
    /// preferences take precedence.
    pub fn infer_preferences_from_messages(
        &self,
        messages: &[super::messages::Message],
    ) -> Vec<InferredPreference> {
        let mut prefs = Vec::new();
        for msg in messages {
            let content_lower = msg.content.to_lowercase();
            let content_orig = &msg.content;

            // Preferred language / locale
            for pattern in &["i speak ", "my language is ", "respond in "] {
                if let Some(idx) = content_lower.find(pattern) {
                    let candidate = content_orig[idx + pattern.len()..]
                        .split(['.', ',', '!', '?', '\n'])
                        .next()
                        .unwrap_or("")
                        .trim();
                    if !candidate.is_empty() && candidate.len() < 40 {
                        prefs.push(InferredPreference {
                            key: "inferred.language".into(),
                            value: candidate.to_string(),
                            confidence: 0.75,
                        });
                        break;
                    }
                }
            }

            // Working directory / project path
            for pattern in &[
                "my project is at ",
                "my project at ",
                "my code is at ",
                "my workspace is ",
                "work in ",
                "working in ",
            ] {
                if let Some(idx) = content_lower.find(pattern) {
                    let candidate = content_orig[idx + pattern.len()..]
                        .split(['.', ',', '!', '?'])
                        .next()
                        .unwrap_or("")
                        .trim();
                    if !candidate.is_empty()
                        && (candidate.contains('/') || candidate.contains('\\'))
                        && candidate.len() < 300
                    {
                        prefs.push(InferredPreference {
                            key: "inferred.working_dir".into(),
                            value: candidate.to_string(),
                            confidence: 0.7,
                        });
                        break;
                    }
                }
            }

            // Preferred tool / tool family hint
            for tool_hint in &[
                "use ",
                "using ",
                "prefer ",
                "always use ",
            ] {
                let needle = format!("{} {}", tool_hint, "search");
                if content_lower.contains(&needle) {
                    prefs.push(InferredPreference {
                        key: "inferred.tool_pref".into(),
                        value: "search".into(),
                        confidence: 0.55,
                    });
                }
            }

            // Preferred editor / IDE
            for editor in &["vscode", "intellij", "vim", "neovim", "emacs", "notepad"] {
                if content_lower.contains(&format!("use {}", editor))
                    || content_lower.contains(&format!("using {}", editor))
                {
                    prefs.push(InferredPreference {
                        key: "inferred.editor".into(),
                        value: editor.to_string(),
                        confidence: 0.65,
                    });
                }
            }

            // Preferred output format — short vs detailed
            if content_lower.contains("be concise")
                || content_lower.contains("keep it short")
                || content_lower.contains("brief")
            {
                prefs.push(InferredPreference {
                    key: "inferred.verbosity".into(),
                    value: "concise".into(),
                    confidence: 0.6,
                });
            }
            if content_lower.contains("be detailed")
                || content_lower.contains("explain in detail")
                || content_lower.contains("thorough")
            {
                prefs.push(InferredPreference {
                    key: "inferred.verbosity".into(),
                    value: "detailed".into(),
                    confidence: 0.6,
                });
            }
        }

        // Normalise: deduplicate by key, keep highest confidence
        let mut best: std::collections::HashMap<String, (String, f64)> =
            std::collections::HashMap::new();
        for p in prefs {
            let entry = best
                .entry(p.key.clone())
                .or_insert_with(|| (p.value.clone(), 0.0));
            if p.confidence > entry.1 {
                *entry = (p.value, p.confidence);
            }
        }
        best.into_iter()
            .map(|(k, (v, c))| InferredPreference {
                key: k,
                value: v,
                confidence: c,
            })
            .collect()
    }

    /// Persist inferred preferences to the `preferences` table, but only when
    /// no user-set (`inferred.`-free) key exists for the same logical domain.
    /// For example `inferred.language` is skipped when `language` already exists.
    pub fn save_inferred_preferences(&self, prefs: &[InferredPreference]) -> anyhow::Result<()> {
        for p in prefs {
            let user_key = p.key.trim_start_matches("inferred.");
            if self.get_preference(user_key)?.is_some() {
                continue;
            }
            let val = format!("{}|{}", p.confidence, p.value);
            self.set_preference(&p.key, &val)?;
        }
        Ok(())
    }

    /// Build a human-readable summary of all preferences.
    /// User-set (non-`inferred.*`) keys are listed first and tagged `[user]`;
    /// inferred keys follow and are tagged `[inferred]`.
    pub fn get_preference_summary(&self) -> anyhow::Result<Vec<(String, String)>> {
        let all = self.list_preferences()?;
        let mut summary = Vec::new();

        // tool_usage counters → most-used tool
        if let Ok(Some(tool)) = self.most_used_tool() {
            summary.push((
                "most_used_tool".into(),
                format!("[inferred] {}", tool),
            ));
        }

        for (key, value) in &all {
            if key.starts_with("tool_usage.") || key.starts_with("tool_param.") || key.starts_with("cfg.") {
                continue;
            }
            let label = if key.starts_with("inferred.") {
                // format: "inferred.xxx" → stored value is "0.7|actual_value"
                let mut parts = value.splitn(2, '|');
                let _conf = parts.next().unwrap_or("");
                let val = parts.next().unwrap_or(value);
                format!("[inferred] {}", val)
            } else {
                format!("[user] {}", value)
            };
            let display_key = key.trim_start_matches("inferred.").to_string();
            summary.push((display_key, label));
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn make_message(content: &str) -> super::super::messages::Message {
        super::super::messages::Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test-session".into(),
            role: "user".into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: chrono::Utc::now().to_rfc3339(),
            tool_call_id: None,
            compaction_id: None,
            is_compacted: false,
            parent_message_id: None,
        }
    }

    #[test]
    fn set_and_get_preference() {
        let db = test_db();
        db.set_preference("editor", "vscode").unwrap();
        assert_eq!(db.get_preference("editor").unwrap(), Some("vscode".into()));
        assert_eq!(db.get_preference("nonexistent").unwrap(), None);
    }

    #[test]
    fn set_preference_updates_existing() {
        let db = test_db();
        db.set_preference("lang", "en").unwrap();
        db.set_preference("lang", "zh").unwrap();
        assert_eq!(db.get_preference("lang").unwrap(), Some("zh".into()));
    }

    #[test]
    fn delete_preference() {
        let db = test_db();
        db.set_preference("key1", "val1").unwrap();
        db.delete_preference("key1").unwrap();
        assert_eq!(db.get_preference("key1").unwrap(), None);
    }

    #[test]
    fn list_preferences_sorted() {
        let db = test_db();
        db.set_preference("b", "2").unwrap();
        db.set_preference("a", "1").unwrap();
        let list = db.list_preferences().unwrap();
        assert_eq!(list[0].0, "a");
        assert_eq!(list[1].0, "b");
    }

    #[test]
    fn increment_counter_new_key() {
        let db = test_db();
        let val = db.increment_counter("my.count", 1).unwrap();
        assert_eq!(val, 1);
        let stored = db.get_preference("my.count").unwrap().unwrap();
        assert_eq!(stored, "1");
    }

    #[test]
    fn increment_counter_existing_key() {
        let db = test_db();
        db.increment_counter("cnt", 3).unwrap();
        let val = db.increment_counter("cnt", 2).unwrap();
        assert_eq!(val, 5);
        let stored = db.get_preference("cnt").unwrap().unwrap();
        assert_eq!(stored, "5");
    }

    #[test]
    fn most_used_tool_returns_top() {
        let db = test_db();
        db.increment_counter("tool_usage.read_file", 10).unwrap();
        db.increment_counter("tool_usage.search", 3).unwrap();
        db.increment_counter("tool_usage.edit", 1).unwrap();
        assert_eq!(db.most_used_tool().unwrap(), Some("read_file".into()));
    }

    #[test]
    fn most_used_tool_empty() {
        let db = test_db();
        assert_eq!(db.most_used_tool().unwrap(), None);
    }

    #[test]
    fn record_tool_usage_params() {
        let db = test_db();
        let params: serde_json::Value =
            serde_json::json!({"path": "/src/main.rs", "mode": "read"});
        db.record_tool_usage("read_file", &params, true).unwrap();

        let tool_count = db.get_preference("tool_usage.read_file").unwrap().unwrap();
        let tool_count_val: i64 = tool_count.parse().unwrap();
        assert!(tool_count_val >= 1);

        let param_count = db
            .get_preference("tool_param.read_file.path./src/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(param_count, "1");

        let mode_count = db
            .get_preference("tool_param.read_file.mode.read")
            .unwrap()
            .unwrap();
        assert_eq!(mode_count, "1");
    }

    #[test]
    fn record_tool_usage_failed_success_skips_params() {
        let db = test_db();
        let params: serde_json::Value = serde_json::json!({"x": "y"});
        db.record_tool_usage("fail_tool", &params, false).unwrap();
        assert!(db.get_preference("tool_param.fail_tool.x.y").unwrap().is_none());
        let count: i64 = db
            .get_preference("tool_usage.fail_tool")
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(count >= 1);
    }

    #[test]
    fn record_tool_usage_null_params() {
        let db = test_db();
        db.record_tool_usage("noop", &serde_json::Value::Null, true)
            .unwrap();
        let count: i64 = db
            .get_preference("tool_usage.noop")
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(count >= 1);
    }

    #[test]
    fn infer_preferences_language() {
        let db = test_db();
        let msgs = vec![make_message("I speak Chinese, please respond in 中文")];
        let prefs = db.infer_preferences_from_messages(&msgs);
        assert!(prefs.iter().any(|p| p.key == "inferred.language"));
    }

    #[test]
    fn infer_preferences_working_dir() {
        let db = test_db();
        let msgs = vec![make_message("my project is at D:\\Workspace\\Haven")];
        let prefs = db.infer_preferences_from_messages(&msgs);
        assert!(prefs
            .iter()
            .any(|p| p.key == "inferred.working_dir"
                && p.value.contains("D:\\Workspace\\Haven")));
    }

    #[test]
    fn infer_preferences_verbosity() {
        let db = test_db();
        let msgs = vec![make_message("please be concise and keep it short")];
        let prefs = db.infer_preferences_from_messages(&msgs);
        assert!(prefs.iter().any(|p| p.key == "inferred.verbosity" && p.value == "concise"));
    }

    #[test]
    fn infer_preferences_detailed() {
        let db = test_db();
        let msgs = vec![make_message("be thorough and explain in detail")];
        let prefs = db.infer_preferences_from_messages(&msgs);
        assert!(prefs.iter().any(|p| p.key == "inferred.verbosity" && p.value == "detailed"));
    }

    #[test]
    fn infer_preferences_editor() {
        let db = test_db();
        let msgs = vec![make_message("I use vscode for everything")];
        let prefs = db.infer_preferences_from_messages(&msgs);
        assert!(prefs.iter().any(|p| p.key == "inferred.editor" && p.value == "vscode"));
    }

    #[test]
    fn infer_preferences_empty_messages() {
        let db = test_db();
        let prefs = db.infer_preferences_from_messages(&[]);
        assert!(prefs.is_empty());
    }

    #[test]
    fn save_inferred_preferences_persists() {
        let db = test_db();
        let prefs = vec![InferredPreference {
            key: "inferred.language".into(),
            value: "Chinese".into(),
            confidence: 0.75,
        }];
        db.save_inferred_preferences(&prefs).unwrap();
        let stored = db.get_preference("inferred.language").unwrap().unwrap();
        assert!(stored.contains("0.75") && stored.contains("Chinese"));
    }

    #[test]
    fn save_inferred_preferences_respects_user_set() {
        let db = test_db();
        db.set_preference("language", "en").unwrap();
        let prefs = vec![InferredPreference {
            key: "inferred.language".into(),
            value: "zh".into(),
            confidence: 0.8,
        }];
        db.save_inferred_preferences(&prefs).unwrap();
        assert!(db.get_preference("inferred.language").unwrap().is_none());
        assert_eq!(db.get_preference("language").unwrap(), Some("en".into()));
    }

    #[test]
    fn get_preference_summary_empty() {
        let db = test_db();
        let summary = db.get_preference_summary().unwrap();
        assert!(summary.is_empty());
    }

    #[test]
    fn get_preference_summary_user_and_inferred() {
        let db = test_db();
        db.set_preference("language", "en").unwrap();
        db.set_preference("inferred.editor", "0.65|vscode").unwrap();
        db.increment_counter("tool_usage.search", 5).unwrap();
        let summary = db.get_preference_summary().unwrap();
        let keys: Vec<&str> = summary.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"most_used_tool"));
        assert!(keys.contains(&"language"));
        assert!(keys.contains(&"editor"));
        let lang_val = summary
            .iter()
            .find(|(k, _)| k == "language")
            .map(|(_, v)| v.as_str());
        assert_eq!(lang_val, Some("[user] en"));
    }

    #[test]
    fn infer_preferences_dedup_best_confidence() {
        let db = test_db();
        let msgs = vec![
            make_message("I use vscode for coding"),
            make_message("I like using vscode"),
        ];
        let prefs = db.infer_preferences_from_messages(&msgs);
        let editor_prefs: Vec<_> = prefs.iter().filter(|p| p.key == "inferred.editor").collect();
        assert_eq!(editor_prefs.len(), 1);
    }
}
