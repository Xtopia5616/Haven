use super::{SessionStore, ensure_session_id};
use crate::repositories::session_authorization::{
    SessionAuthorizationGrant, StoredSessionAuthorizationGrant,
};

impl SessionStore {
    /// Persist one session-scoped authorization decision. This typed session
    /// boundary ensures grants share the session row's deletion and retention
    /// lifecycle rather than becoming global config.
    pub async fn save_session_authorization_grant(
        &self,
        session_id: &str,
        grant: SessionAuthorizationGrant,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.save_session_authorization_grant(&session_id, &grant))
            .await
    }

    /// Load the complete typed authorization set for one persisted session.
    /// Invalid rows are reported so the Agent can refuse to install a session
    /// with a partially restored trust set.
    pub async fn session_authorization_grants(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionAuthorizationGrant>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.session_authorization_grants(&session_id))
            .await
    }

    /// Load every stored grant with its owning session for a live security
    /// policy reapply that invalidates process-local authorization state.
    pub async fn list_session_authorization_grants(
        &self,
    ) -> anyhow::Result<Vec<StoredSessionAuthorizationGrant>> {
        self.db
            .run_blocking(|db| db.list_session_authorization_grants())
            .await
    }

    /// Remove one exact capability grant from its owning session.
    pub async fn revoke_session_authorization_grant(
        &self,
        session_id: &str,
        capability: haven_common::types::CapabilityScope,
    ) -> anyhow::Result<usize> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.revoke_session_authorization_grant(&session_id, &capability))
            .await
    }

    /// Clear all session authorization rules after an explicit global reset.
    pub async fn clear_session_authorization_grants(&self) -> anyhow::Result<usize> {
        self.db
            .run_blocking(|db| db.clear_session_authorization_grants())
            .await
    }
}
