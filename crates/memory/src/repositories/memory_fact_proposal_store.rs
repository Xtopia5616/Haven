//! Durable queue for model-inferred facts awaiting an explicit user decision.

use std::collections::HashSet;
use std::sync::Arc;

use crate::db::Database;
use crate::repositories::fact_graph::{FactGraph, in_immediate_transaction};
use crate::repositories::fact_security::invisible_fact_where_sql;
use crate::repositories::fact_store::MemoryFactWrite;
use crate::repositories::facts::{FactSourceRef, is_sensitive_text};
use chrono::{SecondsFormat, Utc};
use haven_common::types::{is_canonical_id, new_id};
use rusqlite::{Connection, OptionalExtension, types::Type};

/// Maximum number of pending proposals; a full queue leaves extraction markers unchanged.
pub const MAX_PENDING_FACT_PROPOSALS: usize = 500;
/// Maximum proposals returned by one UI page.
pub const MAX_FACT_PROPOSAL_PAGE_SIZE: usize = 50;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FactProposal {
    pub proposal_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub durability: f64,
    pub created_at: String,
    pub source_ref: Option<FactSourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactProposalCursor {
    pub created_at: String,
    pub proposal_id: String,
}

#[derive(Debug, Clone)]
pub struct FactProposalPage {
    pub proposals: Vec<FactProposal>,
    pub next_cursor: Option<FactProposalCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProposalDecision {
    Accept,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProposalReviewOutcome {
    /// The proposal was removed and its accepted value was written or reinforced.
    Accepted { fact_written: bool },
    /// The proposal was removed without changing the facts graph.
    Rejected,
    /// Another review already consumed this proposal, or it no longer exists.
    AlreadyReviewed,
}

#[derive(Clone)]
pub struct MemoryFactProposalStore {
    db: Arc<Database>,
}

impl MemoryFactProposalStore {
    #[cfg(feature = "test-support")]
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Queue ordinary extraction proposals and advance its message cursor in one transaction.
    pub async fn commit_ordinary_extraction(
        &self,
        writes: Vec<MemoryFactWrite>,
        session_id: &str,
        last_message_id: &str,
    ) -> anyhow::Result<usize> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "invalid session id for ordinary extraction"
        );
        anyhow::ensure!(
            is_canonical_id(last_message_id, "msg"),
            "invalid message id for ordinary extraction"
        );
        let writes = prepare_writes(writes)?;
        let session_id = session_id.to_owned();
        let last_message_id = last_message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.with_fact_write(|| {
                    let conn = db.try_conn()?;
                    in_immediate_transaction(&conn, |conn| {
                        let inserted = insert_pending_proposals(conn, &writes)?;
                        let cursor_key = format!("fact_extraction.{session_id}");
                        set_kv_on_connection(conn, &cursor_key, &last_message_id)?;
                        Ok(inserted)
                    })
                })
            })
            .await
    }

    /// Queue one summary's proposals and its durable completion marker atomically.
    pub async fn commit_summary_extraction(
        &self,
        writes: Vec<MemoryFactWrite>,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<usize> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "invalid session id for summary extraction"
        );
        anyhow::ensure!(
            is_canonical_id(episode_id, "msg"),
            "invalid episode id for summary extraction"
        );
        let writes = prepare_writes(writes)?;
        let session_id = session_id.to_owned();
        let episode_id = episode_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.with_fact_write(|| {
                    let conn = db.try_conn()?;
                    in_immediate_transaction(&conn, |conn| {
                        ensure_episode_summary_owner(conn, &session_id, &episode_id)?;
                        let marker_key = summary_extraction_done_key(&session_id, &episode_id);
                        if conn
                            .query_row(
                                "SELECT 1 FROM kv_store WHERE key = ?1",
                                rusqlite::params![marker_key],
                                |row| row.get::<_, i32>(0),
                            )
                            .optional()?
                            .is_some()
                        {
                            return Ok(0);
                        }
                        let inserted = insert_pending_proposals(conn, &writes)?;
                        set_kv_on_connection(conn, &marker_key, &session_id)?;
                        Ok(inserted)
                    })
                })
            })
            .await
    }

    pub async fn summary_extraction_completed(
        &self,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "invalid session id for summary extraction"
        );
        anyhow::ensure!(
            is_canonical_id(episode_id, "msg"),
            "invalid episode id for summary extraction"
        );
        let marker_key = summary_extraction_done_key(session_id, episode_id);
        let session_id = session_id.to_owned();
        let episode_id = episode_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let conn = db.try_conn()?;
                ensure_episode_summary_owner(&conn, &session_id, &episode_id)?;
                Ok(conn
                    .query_row(
                        "SELECT 1 FROM kv_store WHERE key = ?1",
                        rusqlite::params![marker_key],
                        |row| row.get::<_, i32>(0),
                    )
                    .optional()?
                    .is_some())
            })
            .await
    }

    /// Return a visibility-filtered keyset page of pending suggestions.
    pub async fn list_pending_page(
        &self,
        cursor: Option<FactProposalCursor>,
        limit: usize,
    ) -> anyhow::Result<FactProposalPage> {
        let limit = limit.min(MAX_FACT_PROPOSAL_PAGE_SIZE);
        if limit == 0 {
            return Ok(FactProposalPage {
                proposals: Vec::new(),
                next_cursor: None,
            });
        }
        if let Some(cursor) = cursor.as_ref() {
            anyhow::ensure!(
                is_canonical_id(&cursor.proposal_id, "fprop"),
                "fact proposal cursor id must be canonical"
            );
            anyhow::ensure!(
                cursor.created_at.len() <= 64,
                "fact proposal cursor timestamp is too long"
            );
            chrono::DateTime::parse_from_rfc3339(&cursor.created_at)
                .map_err(|_| anyhow::anyhow!("fact proposal cursor timestamp must be RFC3339"))?;
        }
        self.db
            .run_blocking(move |db| {
                let conn = db.try_conn()?;
                let invisible = invisible_fact_where_sql();
                let (cursor_time, cursor_id) = cursor
                    .as_ref()
                    .map(|cursor| {
                        (
                            Some(cursor.created_at.as_str()),
                            Some(cursor.proposal_id.as_str()),
                        )
                    })
                    .unwrap_or((None, None));
                let mut statement = conn.prepare(&format!(
                    "SELECT proposal_id, subject, predicate, object, confidence, tags,
                            durability, created_at, provenance_record_id, provenance_snippet
                     FROM fact_proposals
                     WHERE NOT {invisible}
                       AND (?1 IS NULL OR created_at > ?1 OR (created_at = ?1 AND proposal_id > ?2))
                     ORDER BY created_at, proposal_id LIMIT ?3"
                ))?;
                let rows = statement.query_map(
                    rusqlite::params![cursor_time, cursor_id, (limit + 1) as i64],
                    proposal_from_row,
                )?;
                let proposals = rows.collect::<rusqlite::Result<Vec<_>>>()?;
                let mut proposals = proposals;
                let has_more = proposals.len() > limit;
                proposals.truncate(limit);
                let next_cursor = has_more
                    .then(|| {
                        proposals.last().map(|proposal| FactProposalCursor {
                            created_at: proposal.created_at.clone(),
                            proposal_id: proposal.proposal_id.clone(),
                        })
                    })
                    .flatten();
                Ok(FactProposalPage {
                    proposals,
                    next_cursor,
                })
            })
            .await
    }

    /// Accept/reject a pending proposal exactly once. Acceptance shares the SQLite transaction with
    /// the existing FactGraph upsert; a rollback leaves the proposal available for retry.
    pub async fn review(
        &self,
        proposal_id: String,
        decision: FactProposalDecision,
    ) -> anyhow::Result<FactProposalReviewOutcome> {
        anyhow::ensure!(
            is_canonical_id(&proposal_id, "fprop"),
            "fact proposal id must be canonical"
        );
        self.db
            .run_blocking(move |db| {
                db.with_fact_write(|| {
                    let conn = db.try_conn()?;
                    in_immediate_transaction(&conn, |conn| {
                        let proposal = conn
                            .query_row(
                                "SELECT subject, predicate, object, confidence, tags, durability,
                                        provenance_record_id, provenance_snippet
                                 FROM fact_proposals WHERE proposal_id = ?1",
                                rusqlite::params![proposal_id],
                                |row| {
                                    let tags: String = row.get(4)?;
                                    Ok(MemoryFactWrite {
                                        subject: row.get(0)?,
                                        predicate: row.get(1)?,
                                        object: row.get(2)?,
                                        confidence: row.get(3)?,
                                        tags: serde_json::from_str(&tags).unwrap_or_default(),
                                        durability: row.get(5)?,
                                        source_ref: match (
                                            row.get::<_, Option<String>>(6)?,
                                            row.get::<_, Option<String>>(7)?,
                                        ) {
                                            (Some(message_id), Some(snippet)) => {
                                                Some(FactSourceRef {
                                                    message_id,
                                                    snippet,
                                                })
                                            }
                                            _ => None,
                                        },
                                    })
                                },
                            )
                            .optional()?;
                        let Some(write) = proposal else {
                            return Ok(FactProposalReviewOutcome::AlreadyReviewed);
                        };
                        let outcome =
                            if decision == FactProposalDecision::Accept {
                                let wrote = FactGraph::new(db)
                                    .upsert_inferred_batch_on_connection(conn, &[write], 0.0)?;
                                FactProposalReviewOutcome::Accepted {
                                    fact_written: wrote,
                                }
                            } else {
                                FactProposalReviewOutcome::Rejected
                            };
                        let removed = conn.execute(
                            "DELETE FROM fact_proposals WHERE proposal_id = ?1",
                            rusqlite::params![proposal_id],
                        )?;
                        anyhow::ensure!(removed == 1, "fact proposal changed during review");
                        Ok(outcome)
                    })
                })
            })
            .await
    }
}

fn prepare_writes(writes: Vec<MemoryFactWrite>) -> anyhow::Result<Vec<MemoryFactWrite>> {
    writes
        .iter()
        .map(FactGraph::validate_inferred_proposal)
        .collect()
}

fn insert_pending_proposals(
    conn: &Connection,
    writes: &[MemoryFactWrite],
) -> anyhow::Result<usize> {
    let mut unique = HashSet::new();
    let mut new_writes = Vec::new();
    for write in writes {
        let key = (
            write.subject.as_str(),
            write.predicate.as_str(),
            write.object.as_str(),
        );
        if !unique.insert(key) {
            continue;
        }
        let exists = conn
            .query_row(
                "SELECT 1 FROM facts WHERE subject = ?1 AND predicate = ?2 AND object = ?3
                 UNION ALL
                 SELECT 1 FROM fact_proposals WHERE subject = ?1 AND predicate = ?2 AND object = ?3
                 LIMIT 1",
                rusqlite::params![write.subject, write.predicate, write.object],
                |row| row.get::<_, i32>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            new_writes.push(write);
        }
    }
    if new_writes.is_empty() {
        return Ok(0);
    }
    let pending: i64 =
        conn.query_row("SELECT COUNT(*) FROM fact_proposals", [], |row| row.get(0))?;
    anyhow::ensure!(
        pending + new_writes.len() as i64 <= MAX_PENDING_FACT_PROPOSALS as i64,
        "pending fact proposal queue is full; review proposals before retrying extraction"
    );
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut inserted = 0;
    for write in new_writes {
        let proposal_id = new_id("fprop");
        anyhow::ensure!(
            is_canonical_id(&proposal_id, "fprop"),
            "generated proposal id is invalid"
        );
        let tags = serde_json::to_string(&write.tags)?;
        let (provenance_record_id, provenance_snippet) = write
            .source_ref
            .as_ref()
            .map(|source| {
                (
                    Some(source.message_id.as_str()),
                    Some(source.snippet.as_str()),
                )
            })
            .unwrap_or((None, None));
        inserted += conn.execute(
            "INSERT INTO fact_proposals (
                proposal_id, subject, predicate, object, confidence, tags, durability,
                created_at, provenance_record_id, provenance_snippet
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                proposal_id,
                write.subject,
                write.predicate,
                write.object,
                write.confidence,
                tags,
                write.durability,
                created_at,
                provenance_record_id,
                provenance_snippet,
            ],
        )?;
    }
    Ok(inserted)
}

fn ensure_episode_summary_owner(
    conn: &Connection,
    session_id: &str,
    episode_id: &str,
) -> anyhow::Result<()> {
    let owner: Option<String> = conn
        .query_row(
            "SELECT session_id FROM memory_items
             WHERE id = ?1 AND kind = 'episode_summary'",
            rusqlite::params![episode_id],
            |row| row.get(0),
        )
        .optional()?;
    anyhow::ensure!(
        owner.as_deref() == Some(session_id),
        "episode_id does not identify an episode_summary owned by session_id"
    );
    Ok(())
}

fn summary_extraction_done_key(session_id: &str, episode_id: &str) -> String {
    format!("fact_extraction_episode_done.{session_id}.{episode_id}")
}

fn set_kv_on_connection(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    conn.execute(
        "INSERT INTO kv_store (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        rusqlite::params![key, value, now],
    )?;
    Ok(())
}

fn proposal_from_row(row: &rusqlite::Row) -> rusqlite::Result<FactProposal> {
    let proposal_id: String = row.get(0)?;
    if !is_canonical_id(&proposal_id, "fprop") {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Text,
            "fact proposal id is not canonical".into(),
        ));
    }
    let tags: String = row.get(5)?;
    let source_ref = match (
        row.get::<_, Option<String>>(8)?,
        row.get::<_, Option<String>>(9)?,
    ) {
        (Some(message_id), Some(snippet)) if is_canonical_id(&message_id, "msg") => {
            Some(FactSourceRef {
                message_id,
                snippet: if is_sensitive_text(&snippet) {
                    "[redacted]".into()
                } else {
                    snippet
                },
            })
        }
        _ => None,
    };
    Ok(FactProposal {
        proposal_id,
        subject: row.get(1)?,
        predicate: row.get(2)?,
        object: row.get(3)?,
        confidence: row.get(4)?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        durability: row.get(6)?,
        created_at: row.get(7)?,
        source_ref,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        FactProposalDecision, FactProposalReviewOutcome, MAX_FACT_PROPOSAL_PAGE_SIZE,
        MAX_PENDING_FACT_PROPOSALS, MemoryFactProposalStore,
    };
    use crate::Database;
    use crate::repositories::fact_store::MemoryFactWrite;
    use crate::repositories::facts::FactSourceRef;
    use haven_common::types::new_id;
    use std::sync::Arc;

    fn write(object: &str) -> MemoryFactWrite {
        MemoryFactWrite {
            subject: "user".into(),
            predicate: "likes".into(),
            object: object.into(),
            confidence: 0.8,
            tags: vec!["preference".into()],
            source_ref: Some(FactSourceRef::from_message(
                "msg-0123456789abcdef0123456789abcdef",
                "I like Rust.",
            )),
            durability: 0.8,
        }
    }

    fn store() -> (Arc<Database>, MemoryFactProposalStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        (db.clone(), MemoryFactProposalStore::new(db))
    }

    #[tokio::test]
    async fn ordinary_extraction_queues_idempotently_and_accepts_once() {
        let (db, store) = store();
        let session_id = db.create_session("proposal extraction").unwrap().session_id;
        let message_id = new_id("msg");

        assert_eq!(
            store
                .commit_ordinary_extraction(vec![write("Rust")], &session_id, &message_id)
                .await
                .unwrap(),
            1
        );
        assert!(db.list_facts().unwrap().is_empty());
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM fact_proposals", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .commit_ordinary_extraction(vec![write("Rust")], &session_id, &message_id)
                .await
                .unwrap(),
            0,
            "same pending SPO must not be duplicated"
        );

        let page = store.list_pending_page(None, 10).await.unwrap();
        let proposal_id = page.proposals[0].proposal_id.clone();
        assert_eq!(
            page.proposals[0].source_ref.as_ref().unwrap().snippet,
            "I like Rust."
        );
        assert_eq!(
            store
                .review(proposal_id.clone(), FactProposalDecision::Accept)
                .await
                .unwrap(),
            FactProposalReviewOutcome::Accepted { fact_written: true }
        );
        assert_eq!(db.list_facts().unwrap().len(), 1);
        assert_eq!(
            store
                .review(proposal_id, FactProposalDecision::Accept)
                .await
                .unwrap(),
            FactProposalReviewOutcome::AlreadyReviewed
        );
        assert!(
            store
                .list_pending_page(None, 10)
                .await
                .unwrap()
                .proposals
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejection_removes_only_the_proposal_and_user_fact_stays_authoritative() {
        let (db, store) = store();
        let session_id = db.create_session("proposal reject").unwrap().session_id;
        let message_id = new_id("msg");
        store
            .commit_ordinary_extraction(vec![write("Rust")], &session_id, &message_id)
            .await
            .unwrap();
        let proposal_id = store.list_pending_page(None, 10).await.unwrap().proposals[0]
            .proposal_id
            .clone();
        store
            .review(proposal_id.clone(), FactProposalDecision::Reject)
            .await
            .unwrap();
        assert!(db.list_facts().unwrap().is_empty());
        assert_eq!(
            store
                .review(proposal_id, FactProposalDecision::Reject)
                .await
                .unwrap(),
            FactProposalReviewOutcome::AlreadyReviewed
        );

        db.set_user_fact("user", "city", "Oslo", &[]).unwrap();
        store
            .commit_ordinary_extraction(
                vec![MemoryFactWrite {
                    subject: "user".into(),
                    predicate: "city".into(),
                    object: "Tokyo".into(),
                    ..write("Tokyo")
                }],
                &session_id,
                &new_id("msg"),
            )
            .await
            .unwrap();
        let proposal = store.list_pending_page(None, 10).await.unwrap().proposals[0].clone();
        assert_eq!(
            store
                .review(proposal.proposal_id, FactProposalDecision::Accept)
                .await
                .unwrap(),
            FactProposalReviewOutcome::Accepted {
                fact_written: false
            }
        );
        assert_eq!(db.list_facts().unwrap()[0].object, "Oslo");
    }

    #[tokio::test]
    async fn cursor_and_proposals_roll_back_together_and_queue_is_bounded() {
        let (db, store) = store();
        let session_id = db.create_session("proposal rollback").unwrap().session_id;
        let message_id = new_id("msg");
        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_cursor BEFORE INSERT ON kv_store
                 WHEN NEW.key GLOB 'fact_extraction.*'
                 BEGIN SELECT RAISE(ABORT, 'cursor unavailable'); END;",
            )
            .unwrap();
        assert!(
            store
                .commit_ordinary_extraction(vec![write("Rust")], &session_id, &message_id)
                .await
                .is_err()
        );
        assert!(
            store
                .list_pending_page(None, 10)
                .await
                .unwrap()
                .proposals
                .is_empty()
        );
        assert!(
            db.get_kv(&format!("fact_extraction.{session_id}"))
                .unwrap()
                .is_none()
        );
        db.conn()
            .execute_batch("DROP TRIGGER reject_cursor")
            .unwrap();

        for index in 0..MAX_PENDING_FACT_PROPOSALS {
            db.conn()
                .execute(
                    "INSERT INTO fact_proposals (
                        proposal_id, subject, predicate, object, confidence, durability, created_at
                     ) VALUES (?1, 'user', 'likes', ?2, 0.8, 0.8, '2026-10-21T00:00:00.000Z')",
                    rusqlite::params![new_id("fprop"), format!("item-{index}")],
                )
                .unwrap();
        }
        assert!(
            store
                .commit_ordinary_extraction(vec![write("another")], &session_id, &new_id("msg"))
                .await
                .is_err()
        );
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM fact_proposals", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            MAX_PENDING_FACT_PROPOSALS as i64
        );
        assert!(
            db.get_kv(&format!("fact_extraction.{session_id}"))
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn proposal_listing_is_keyset_bounded_and_filters_sensitive_values() {
        let (db, store) = store();
        for index in 0..(MAX_FACT_PROPOSAL_PAGE_SIZE + 2) {
            store
                .commit_ordinary_extraction(
                    vec![MemoryFactWrite {
                        subject: "user".into(),
                        predicate: "likes".into(),
                        object: format!("item-{index}"),
                        ..write(&format!("item-{index}"))
                    }],
                    &db.create_session("proposal page").unwrap().session_id,
                    &new_id("msg"),
                )
                .await
                .unwrap();
        }
        db.conn()
            .execute(
                "INSERT INTO fact_proposals (
                    proposal_id, subject, predicate, object, confidence, durability, created_at
                 ) VALUES (?1, 'user', 'api_key', 'hidden', 0.8, 0.8, '2026-10-21T00:00:00.000Z')",
                rusqlite::params![new_id("fprop")],
            )
            .unwrap();
        let first = store.list_pending_page(None, usize::MAX).await.unwrap();
        assert_eq!(first.proposals.len(), MAX_FACT_PROPOSAL_PAGE_SIZE);
        let cursor = first.next_cursor.expect("more proposals should be paged");
        let second = store
            .list_pending_page(Some(cursor), MAX_FACT_PROPOSAL_PAGE_SIZE)
            .await
            .unwrap();
        assert_eq!(second.proposals.len(), 2);
        assert!(
            first
                .proposals
                .iter()
                .chain(&second.proposals)
                .all(|proposal| proposal.predicate != "api_key")
        );
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn summary_extraction_marker_and_proposals_commit_together() {
        let (db, store) = store();
        let session = db.create_session("proposal summary").unwrap();
        let episode_id = new_id("msg");
        db.add_episode_with_pending_extraction(
            &session.session_id,
            "Persisted compaction summary with sufficient detail.",
            &episode_id,
            false,
        )
        .unwrap();
        assert_eq!(
            store
                .commit_summary_extraction(vec![write("Rust")], &session.session_id, &episode_id)
                .await
                .unwrap(),
            1
        );
        assert!(
            store
                .summary_extraction_completed(&session.session_id, &episode_id)
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .commit_summary_extraction(vec![write("Rust")], &session.session_id, &episode_id)
                .await
                .unwrap(),
            0
        );
        assert_eq!(db.list_facts().unwrap().len(), 0);
        assert_eq!(
            store
                .list_pending_page(None, 10)
                .await
                .unwrap()
                .proposals
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn extraction_rejects_invalid_ids_and_summary_owner() {
        let (db, store) = store();
        let session = db.create_session("proposal ids").unwrap();
        let valid_message = new_id("msg");
        assert!(
            store
                .commit_ordinary_extraction(vec![write("Rust")], "bad", &valid_message)
                .await
                .is_err()
        );
        assert!(
            store
                .commit_ordinary_extraction(vec![write("Rust")], &session.session_id, "bad")
                .await
                .is_err()
        );
        let episode_id = new_id("msg");
        assert!(
            store
                .commit_summary_extraction(vec![write("Rust")], &session.session_id, &episode_id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn extraction_rejects_sensitive_subjects_and_sanitizes_tags() {
        let (db, store) = store();
        let session_id = db.create_session("proposal privacy").unwrap().session_id;
        let mut candidate = write("Rust");
        candidate.subject = "password=not-for-memory".into();
        assert!(
            store
                .commit_ordinary_extraction(vec![candidate], &session_id, &new_id("msg"),)
                .await
                .is_err()
        );
        assert!(
            store
                .list_pending_page(None, 10)
                .await
                .unwrap()
                .proposals
                .is_empty()
        );

        let mut candidate = write("Rust");
        candidate.tags = vec![
            "Preference".into(),
            "workspace_path".into(),
            "unknown".into(),
        ];
        store
            .commit_ordinary_extraction(vec![candidate], &session_id, &new_id("msg"))
            .await
            .unwrap();
        assert_eq!(
            store.list_pending_page(None, 10).await.unwrap().proposals[0].tags,
            vec!["preference"]
        );
    }
}
