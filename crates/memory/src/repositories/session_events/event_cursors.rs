use super::*;

impl SessionStore {
    #[cfg(any(test, feature = "test-support"))]
    pub fn initialize_memory_event_cursor_if_absent(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<bool> {
        ensure_session_id(session_id)?;
        self.db
            .initialize_memory_event_cursor_if_absent(session_id, sequence)
    }

    /// Read the durable memory event cursor. This clock is independent from
    /// the fact-extraction message-id cursor and projection cursors.
    pub(super) fn memory_event_cursor(&self, session_id: &str) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        self.db.memory_event_cursor(session_id)
    }

    /// Read the memory event cursor on SQLite's blocking pool with
    /// cancellation for an in-flight database operation.
    pub async fn memory_event_cursor_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| store.memory_event_cursor(&session_id))
            .await
    }

    /// Read the durable cursor while preserving a missing key as `None`.
    pub async fn memory_event_cursor_optional_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<Option<i64>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |db| {
                db.memory_event_cursor_optional(&session_id)
            })
            .await
    }

    pub async fn baseline_missing_memory_event_cursors_to_latest_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<usize> {
        self.db
            .run_blocking_cancellable(cancel, |db| {
                db.baseline_missing_memory_event_cursors_to_latest()
            })
            .await
    }

    /// Advance the durable memory event cursor monotonically.
    pub(super) fn checkpoint_memory_event_cursor(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        self.db.checkpoint_memory_event_cursor(session_id, sequence)
    }

    /// Persist a memory event checkpoint on SQLite's blocking pool with
    /// cancellation for an in-flight database operation.
    pub async fn checkpoint_memory_event_cursor_cancellable(
        &self,
        session_id: &str,
        sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| {
                store.checkpoint_memory_event_cursor(&session_id, sequence)
            })
            .await
    }

    /// Clear one session's durable memory event cursor.
    #[cfg(test)]
    pub(super) fn clear_memory_event_cursor(&self, session_id: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        self.db.clear_memory_event_cursor(session_id)
    }

    /// Synchronous clock inspection for Memory unit tests.
    #[cfg(test)]
    pub(in crate::repositories) fn cursor(
        &self,
        session_id: &str,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let conn = self.db.try_conn()?;
        Self::cursor_in_connection(&conn, session_id)
    }
}
