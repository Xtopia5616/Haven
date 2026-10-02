//! Durable authorization decisions scoped to one persisted conversation.

use crate::Database;
use haven_common::types::{CapabilityScope, PermissionEffect, PermissionScope, PermissionTarget};

/// A session-scoped authorization rule stored with its lifetime and the
/// target selected in the confirmation UI. `capability` is the validated
/// canonical capability resolved by the backend from that target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAuthorizationGrant {
    pub capability: CapabilityScope,
    pub scope: PermissionScope,
    pub target: PermissionTarget,
    pub effect: PermissionEffect,
}

/// A session grant paired with the session that owns it, used when a live
/// security-policy apply clears the AuthorizationEngine's process-local map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionAuthorizationGrant {
    pub session_id: String,
    pub session_title: Option<String>,
    pub grant: SessionAuthorizationGrant,
}

impl SessionAuthorizationGrant {
    pub fn session(
        capability: CapabilityScope,
        target: PermissionTarget,
        effect: PermissionEffect,
    ) -> Self {
        Self {
            capability,
            scope: PermissionScope::Session,
            target,
            effect,
        }
    }
}

impl Database {
    /// Save or replace one exact session capability decision. The foreign key
    /// rejects grants for missing sessions; a failed write never becomes a
    /// live in-memory authorization decision.
    pub fn save_session_authorization_grant(
        &self,
        session_id: &str,
        grant: &SessionAuthorizationGrant,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            grant.scope == PermissionScope::Session,
            "session authorization grant must use session scope"
        );
        anyhow::ensure!(
            !grant.capability.is_empty(),
            "session authorization grant capability cannot be empty"
        );
        let conn = self.conn();
        conn.execute(
            "INSERT INTO session_authorization_grants
                (session_id, capability_key, permission_scope, permission_target, effect)
             VALUES (?1, ?2, 'session', ?3, ?4)
             ON CONFLICT(session_id, capability_key) DO UPDATE SET
                permission_scope = excluded.permission_scope,
                permission_target = excluded.permission_target,
                effect = excluded.effect",
            rusqlite::params![
                session_id,
                grant.capability.as_str(),
                grant.target.as_str(),
                permission_effect_name(grant.effect),
            ],
        )?;
        Ok(())
    }

    /// Load every authorization rule for one persisted session. Any malformed
    /// row is an error so callers can fail closed instead of restoring a
    /// partial trust set.
    pub fn session_authorization_grants(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionAuthorizationGrant>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT capability_key, permission_scope, permission_target, effect
             FROM session_authorization_grants
             WHERE session_id = ?1
             ORDER BY capability_key",
        )?;
        let rows = statement.query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| parse_grant_row(row?)).collect()
    }

    /// Load all grants for live policy reapplication. The session id remains
    /// part of the typed result so each decision is restored into its owner
    /// scope rather than becoming a process-wide permission.
    pub fn all_session_authorization_grants(
        &self,
    ) -> anyhow::Result<Vec<StoredSessionAuthorizationGrant>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT grants.session_id, sessions.title, grants.capability_key,
                    grants.permission_scope, grants.permission_target, grants.effect
             FROM session_authorization_grants AS grants
             JOIN sessions ON sessions.id = grants.session_id
             ORDER BY grants.session_id, grants.capability_key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (session_id, session_title, capability_key, scope, target, effect) = row?;
            Ok(StoredSessionAuthorizationGrant {
                session_id,
                session_title,
                grant: parse_grant_row((capability_key, scope, target, effect))?,
            })
        })
        .collect()
    }

    /// Remove one exact capability decision from its owning session.
    pub fn revoke_session_authorization_grant(
        &self,
        session_id: &str,
        capability: &CapabilityScope,
    ) -> anyhow::Result<usize> {
        let conn = self.conn();
        Ok(conn.execute(
            "DELETE FROM session_authorization_grants
             WHERE session_id = ?1 AND capability_key = ?2",
            rusqlite::params![session_id, capability.as_str()],
        )?)
    }

    /// Clear every session grant as part of an explicit policy reset.
    pub fn clear_session_authorization_grants(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        Ok(conn.execute("DELETE FROM session_authorization_grants", [])?)
    }
}

fn parse_grant_row(
    (capability_key, scope, target, effect): (String, String, String, String),
) -> anyhow::Result<SessionAuthorizationGrant> {
    let capability = CapabilityScope::try_new(capability_key).map_err(anyhow::Error::msg)?;
    let scope = parse_permission_scope(&scope)?;
    anyhow::ensure!(
        scope == PermissionScope::Session,
        "stored session authorization grant has invalid scope"
    );
    Ok(SessionAuthorizationGrant {
        capability,
        scope,
        target: parse_permission_target(&target)?,
        effect: parse_permission_effect(&effect)?,
    })
}

fn permission_effect_name(effect: PermissionEffect) -> &'static str {
    match effect {
        PermissionEffect::Allow => "allow",
        PermissionEffect::Deny => "deny",
    }
}

fn parse_permission_effect(value: &str) -> anyhow::Result<PermissionEffect> {
    match value {
        "allow" => Ok(PermissionEffect::Allow),
        "deny" => Ok(PermissionEffect::Deny),
        _ => anyhow::bail!("stored session authorization grant has invalid effect"),
    }
}

fn parse_permission_scope(value: &str) -> anyhow::Result<PermissionScope> {
    match value {
        "once" => Ok(PermissionScope::Once),
        "session" => Ok(PermissionScope::Session),
        "always" => Ok(PermissionScope::Always),
        _ => anyhow::bail!("stored session authorization grant has invalid scope"),
    }
}

fn parse_permission_target(value: &str) -> anyhow::Result<PermissionTarget> {
    PermissionTarget::parse(value).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(
        capability: &str,
        target: PermissionTarget,
        effect: PermissionEffect,
    ) -> SessionAuthorizationGrant {
        SessionAuthorizationGrant::session(
            CapabilityScope::try_new(capability).unwrap(),
            target,
            effect,
        )
    }

    #[test]
    fn session_grants_round_trip_and_replace_the_exact_capability() {
        let db = Database::open_in_memory().unwrap();
        let session = db.create_session("authorization grants").unwrap();
        let first = grant(
            "system.power",
            PermissionTarget::Group,
            PermissionEffect::Allow,
        );
        db.save_session_authorization_grant(&session.id, &first)
            .unwrap();
        let replacement = grant(
            "system.power",
            PermissionTarget::Group,
            PermissionEffect::Deny,
        );
        db.save_session_authorization_grant(&session.id, &replacement)
            .unwrap();
        db.save_session_authorization_grant(
            &session.id,
            &grant(
                "files.read",
                PermissionTarget::Operation,
                PermissionEffect::Allow,
            ),
        )
        .unwrap();

        assert_eq!(
            db.session_authorization_grants(&session.id).unwrap(),
            vec![
                grant(
                    "files.read",
                    PermissionTarget::Operation,
                    PermissionEffect::Allow,
                ),
                replacement,
            ]
        );
    }

    #[test]
    fn session_grants_require_session_scope_and_existing_session() {
        let db = Database::open_in_memory().unwrap();
        let mut wrong_scope = grant(
            "files.read",
            PermissionTarget::Operation,
            PermissionEffect::Allow,
        );
        wrong_scope.scope = PermissionScope::Always;
        assert!(
            db.save_session_authorization_grant("ses-missing", &wrong_scope)
                .is_err()
        );

        let valid = grant(
            "files.read",
            PermissionTarget::Operation,
            PermissionEffect::Allow,
        );
        assert!(
            db.save_session_authorization_grant("ses-missing", &valid)
                .is_err()
        );
    }

    #[test]
    fn session_delete_retention_revocation_and_reset_clear_grants() {
        let db = Database::open_in_memory().unwrap();
        let old = db.create_session("old authorization session").unwrap();
        let retained = db.create_session("retained authorization session").unwrap();
        let files = grant(
            "files.read",
            PermissionTarget::Operation,
            PermissionEffect::Allow,
        );
        let system = grant(
            "system.power",
            PermissionTarget::Group,
            PermissionEffect::Deny,
        );
        db.save_session_authorization_grant(&old.id, &files)
            .unwrap();
        db.save_session_authorization_grant(&retained.id, &files)
            .unwrap();
        db.save_session_authorization_grant(&retained.id, &system)
            .unwrap();

        db.revoke_session_authorization_grant(&retained.id, &files.capability)
            .unwrap();
        assert_eq!(
            db.session_authorization_grants(&retained.id).unwrap(),
            vec![system.clone()]
        );
        assert_eq!(
            db.session_authorization_grants(&old.id).unwrap(),
            vec![files.clone()]
        );

        db.delete_session(&retained.id).unwrap();
        assert!(
            db.session_authorization_grants(&retained.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.delete_old_sessions(0).unwrap(), 1);
        assert!(db.session_authorization_grants(&old.id).unwrap().is_empty());

        let reset = db.create_session("reset authorization session").unwrap();
        db.save_session_authorization_grant(&reset.id, &files)
            .unwrap();
        db.save_session_authorization_grant(&reset.id, &system)
            .unwrap();
        assert_eq!(db.clear_session_authorization_grants().unwrap(), 2);
        assert!(
            db.session_authorization_grants(&reset.id)
                .unwrap()
                .is_empty()
        );
    }
}
