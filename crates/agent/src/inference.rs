use std::sync::Arc;

use haven_memory::Database;

use crate::session::SessionManager;

pub struct InferenceEngine {
    db: Arc<Database>,
    sessions: Arc<SessionManager>,
}

impl InferenceEngine {
    pub fn new(db: Arc<Database>, sessions: Arc<SessionManager>) -> Self {
        Self { db, sessions }
    }

    /// Run fact inference from the current session messages (M6-04).
    /// Uses rule-based extraction and optionally LLM-assisted inference.
    pub fn infer_facts(&self) {
        let sid = self.sessions.current_session_id();
        if let Ok(messages) = self.db.get_session_messages(&sid) {
            let user_messages: Vec<_> = messages.into_iter().filter(|m| m.role == "user").collect();
            let inferred = self.db.infer_facts_from_messages(&user_messages);
            for (subject, predicate, object, confidence) in inferred {
                let _ = self
                    .db
                    .insert_fact(&subject, &predicate, &object, "inferred", confidence);
            }
            let _ = self.db.dedup_facts();
            let _ = self.db.flush_low_confidence(0.3);
        }
    }

    /// Run cross-session preference inference after a task completes (M6-02).
    /// Extracts patterns such as preferred language, working directory, editor,
    /// and verbosity from the conversation messages and persists them as
    /// `inferred.*` preference keys. User-set keys always take precedence.
    pub fn infer_preferences(&self) {
        let sid = self.sessions.current_session_id();
        if let Ok(messages) = self.db.get_session_messages(&sid) {
            let inferred = self.db.infer_preferences_from_messages(&messages);
            let _ = self.db.save_inferred_preferences(&inferred);
        }
    }

    /// Run both fact and preference inference (common exit point in the ReAct loop).
    pub fn infer_all(&self) {
        self.infer_facts();
        self.infer_preferences();
    }
}
