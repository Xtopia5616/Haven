use std::sync::Arc;
use std::sync::Mutex;

use haven_memory::Database;

pub struct SessionManager {
    db: Arc<Database>,
    session_id: Mutex<String>,
    session_window_size: usize,
}

impl SessionManager {
    pub fn new(db: Arc<Database>, session_window_size: usize) -> Self {
        let session_id = db
            .get_or_create_active_session()
            .map(|s| s.id)
            .unwrap_or_else(|_| "default".to_string());
        Self {
            db,
            session_id: Mutex::new(session_id),
            session_window_size,
        }
    }

    /// Return the current active session ID, creating a new session if needed.
    pub fn ensure_session(&self) -> String {
        let mut guard = self.session_id.lock().unwrap();
        if *guard == "default"
            && let Ok(s) = self.db.get_or_create_active_session()
        {
            *guard = s.id.clone();
            return s.id;
        }
        guard.clone()
    }

    /// Create a new session and switch the agent to it. Returns the new session
    /// ID. Each new task gets its own session so conversation history does not
    /// leak between tasks.
    /// Holds session_id lock across the entire operation to prevent a concurrent
    /// ensure_session or persist_message from seeing a stale session ID.
    pub fn start_new_session(&self) -> anyhow::Result<String> {
        let mut guard = self.session_id.lock().unwrap();
        if *guard != "default" {
            let _ = self.db.close_session(&guard);
        }
        let session = self.db.create_session(None)?;
        *guard = session.id.clone();
        Ok(session.id)
    }

    /// Persist a message to the active session with the configured window size.
    /// Holds the session_id lock across the DB write to prevent a TOCTOU race
    /// with concurrent start_new_session / supplement_task calls.
    pub fn persist_message(&self, role: &str, content: &str, message_type: Option<&str>) {
        let window_size = self.session_window_size;
        let guard = self.session_id.lock().unwrap();
        tracing::trace!(
            "persist_message: session={} role={} type={:?} {} chars",
            &guard,
            role,
            message_type,
            content.len()
        );
        let _ =
            self.db
                .add_message_with_window(&guard, role, content, message_type, None, window_size);
    }

    /// Load the most recent conversation messages from DB as text lines that
    /// get fed into the ReAct system prompt as Conversation History.
    pub fn load_conversation_history(&self) -> Vec<String> {
        let guard = self.session_id.lock().unwrap();
        self.db
            .get_session_messages_limit(&guard, self.session_window_size)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect()
    }

    /// Get the current session ID without creating a new one.
    pub fn current_session_id(&self) -> String {
        self.session_id.lock().unwrap().clone()
    }

    /// Switch to a specific session ID. Used when supplementing a task
    /// that belongs to a different session.
    pub fn switch_to_session(&self, session_id: &str) {
        tracing::debug!("switch_to_session: {}", session_id);
        *self.session_id.lock().unwrap() = session_id.to_string();
    }
}
