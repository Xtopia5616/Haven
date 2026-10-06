//! Read-only session history and transcript context queries.
//!
//! The public `SessionStore` API remains the owner-facing persistence port;
//! this module groups its query-only methods and DTOs without moving SQL or
//! event/projection transaction ownership out of their repositories.

use super::{Session, SessionStore};
use haven_common::media::MediaInput;
use haven_common::types::MessageAttachment;

/// Typed filters for app-facing session history queries.
///
/// `limit` and `offset` are required so each caller keeps ownership of its
/// existing defaults (the history page and export have different limits).
#[derive(Debug, Clone)]
pub struct SessionHistoryFilter {
    pub query: Option<String>,
    pub status: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// The identity, role, and text needed to assemble a fresh-run conversation window.
/// Agent keeps ownership of its `ConversationMessage` prompt type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessageText {
    pub id: String,
    pub role: String,
    pub content: String,
}

/// The materialized media needed to initialize a session run.
///
/// This read model does not define resume authority or register/lease managed
/// assets. Durable event replay and Agent-owned asset lifecycle stay separate.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionResumeMedia {
    pub initial_message_id: Option<String>,
    pub initial_attachments: Vec<MessageAttachment>,
    pub initial_media_inputs: Vec<MediaInput>,
    pub all_attachments: Vec<MessageAttachment>,
}

/// User-only transcript context used to generate a session title.
///
/// The store checks session existence and the persisted title before loading
/// the latest ten title-eligible messages, then applies the existing role
/// filter while preserving chronological order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTitleGenerationContext {
    pub user_messages: Vec<String>,
}

const TITLE_GENERATION_MESSAGE_LIMIT: usize = 10;

impl SessionStore {
    /// Load the latest textual messages for a fresh-run conversation window.
    ///
    /// The underlying query preserves its existing message-type filter,
    /// chronological result order, and `limit` behavior. Dropping this future
    /// cannot interrupt a `run_blocking` query already running on Tokio's
    /// blocking pool.
    pub async fn conversation_window(
        &self,
        session_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMessageText>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                Ok(db
                    .get_session_messages_limit(&session_id, limit)?
                    .into_iter()
                    .map(|message| SessionMessageText {
                        id: message.id,
                        role: message.role,
                        content: message.content,
                    })
                    .collect())
            })
            .await
    }

    /// Load the ordered message media needed to initialize a session run.
    ///
    /// Messages are read and aggregated in one blocking-pool closure using
    /// the existing `get_session_messages` ordering. The first user message
    /// supplies the initial input media; all message attachments are flattened
    /// in message order for the caller's asset registration. This read model
    /// does not replay events or register/lease managed assets.
    pub async fn session_resume_media(
        &self,
        session_id: &str,
    ) -> anyhow::Result<SessionResumeMedia> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let messages = db.get_session_messages(&session_id)?;
                let all_attachments = messages
                    .iter()
                    .flat_map(|message| message.attachments.iter().cloned())
                    .collect();
                let initial_message = messages.iter().find(|message| message.role == "user");
                Ok(SessionResumeMedia {
                    initial_message_id: initial_message.map(|message| message.id.clone()),
                    initial_attachments: initial_message
                        .map(|message| message.attachments.clone())
                        .unwrap_or_default(),
                    initial_media_inputs: initial_message
                        .map(|message| message.media_inputs.clone())
                        .unwrap_or_default(),
                    all_attachments,
                })
            })
            .await
    }

    /// Load the user messages used for title generation on SQLite's blocking
    /// pool. Missing sessions and sessions that already have a title return
    /// `None`; an existing untitled session returns its user-only context,
    /// which may be empty. The original limit, message eligibility, role
    /// filtering, and chronological order are preserved. Dropping this future
    /// cannot interrupt a query already running on Tokio's blocking pool.
    pub async fn title_generation_context(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionTitleGenerationContext>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let Some(session) = db.get_session(&session_id)? else {
                    return Ok(None);
                };
                if session.title.is_some() {
                    return Ok(None);
                }

                let user_messages = db
                    .get_session_messages_limit(&session_id, TITLE_GENERATION_MESSAGE_LIMIT)?
                    .into_iter()
                    .filter(|message| message.role == "user")
                    .map(|message| message.content)
                    .collect();
                Ok(Some(SessionTitleGenerationContext { user_messages }))
            })
            .await
    }

    /// List session history on SQLite's blocking pool.
    ///
    /// This delegates to the existing database query, including its first
    /// page cache behavior and `created_at DESC` ordering. Dropping the
    /// returned future cannot interrupt a `run_blocking` query already
    /// running on Tokio's blocking pool.
    pub async fn list_history(&self, limit: i64, offset: i64) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| db.list_sessions(limit, offset))
            .await
    }

    /// Read the most recently created session using the existing history
    /// ordering and first-row behavior. Dropping this future cannot interrupt
    /// a query already running on Tokio's blocking pool.
    pub async fn latest_session_record(&self) -> anyhow::Result<Option<Session>> {
        Ok(self.list_history(1, 0).await?.into_iter().next())
    }

    /// Count persisted sessions on SQLite's blocking pool.
    ///
    /// Dropping the returned future cannot interrupt a `run_blocking` query
    /// already running on Tokio's blocking pool.
    pub async fn count_history(&self) -> anyhow::Result<i64> {
        self.db.run_blocking(|db| db.count_sessions()).await
    }

    /// Search and page session history using the existing database predicate
    /// and ordering. Dropping the returned future cannot interrupt a
    /// `run_blocking` query already running on Tokio's blocking pool.
    pub async fn search_history_paginated(
        &self,
        query: String,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| db.search_sessions_paginated(&query, limit, offset))
            .await
    }

    /// Count matches using the existing database search predicate.
    /// Dropping the returned future cannot interrupt a `run_blocking` query
    /// already running on Tokio's blocking pool.
    pub async fn count_history_search(&self, query: String) -> anyhow::Result<i64> {
        self.db
            .run_blocking(move |db| db.count_sessions_search(&query))
            .await
    }

    /// Search the first 50 session history matches using the existing
    /// database predicate and ordering. Dropping this future cannot interrupt
    /// a `run_blocking` query already running on Tokio's blocking pool.
    pub async fn search_history(&self, query: String) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| db.search_sessions(&query))
            .await
    }

    /// Apply typed filters through the existing database query, preserving
    /// its empty-filter handling, date conversion, cache behavior, predicate
    /// and ordering. Dropping this future cannot interrupt a
    /// `run_blocking` query already running on Tokio's blocking pool.
    pub async fn search_history_filtered(
        &self,
        filter: SessionHistoryFilter,
    ) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| {
                db.search_sessions_filtered(
                    filter.query.as_deref(),
                    filter.status.as_deref(),
                    filter.start_date.as_deref(),
                    filter.end_date.as_deref(),
                    filter.limit,
                    filter.offset,
                )
            })
            .await
    }
}

#[cfg(test)]
mod tests;
