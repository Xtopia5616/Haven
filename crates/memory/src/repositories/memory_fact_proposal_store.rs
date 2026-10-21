//! Durable queue for model-inferred facts that need user review after evidence validation.

use std::collections::{HashMap, HashSet};
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
/// Minimum stored score for a fact that a source-grounded verifier confirmed against user text.
pub const VERIFIED_FACT_CONFIDENCE_FLOOR: f64 = 0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactEvidenceStatus {
    Supported,
    Uncertain,
    Contradicted,
    Unverified,
}

impl FactEvidenceStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Uncertain => "uncertain",
            Self::Contradicted => "contradicted",
            Self::Unverified => "unverified",
        }
    }

    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "supported" => Ok(Self::Supported),
            "uncertain" => Ok(Self::Uncertain),
            "contradicted" => Ok(Self::Contradicted),
            "unverified" => Ok(Self::Unverified),
            _ => anyhow::bail!("unknown fact evidence status"),
        }
    }
}

/// A validated extraction candidate. Supported candidates can be written directly;
/// all other statuses remain in the bounded review queue.
#[derive(Debug, Clone)]
pub struct MemoryFactProposalWrite {
    pub write: MemoryFactWrite,
    pub evidence_status: FactEvidenceStatus,
    pub evidence_quote: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactProposalCommitOutcome {
    pub queued: usize,
    pub auto_accepted: usize,
    pub facts_written: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FactProposal {
    pub proposal_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub durability: f64,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub created_at: String,
    pub source_ref: Option<FactSourceRef>,
    pub evidence_status: FactEvidenceStatus,
    pub evidence_quote: Option<String>,
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
        let writes = writes
            .into_iter()
            .map(|write| MemoryFactProposalWrite {
                write,
                evidence_status: FactEvidenceStatus::Unverified,
                evidence_quote: None,
            })
            .collect();
        Ok(self
            .commit_validated_ordinary_extraction(writes, session_id, last_message_id)
            .await?
            .queued)
    }

    /// Commit verified facts, pending proposals, and the ordinary extraction cursor atomically.
    pub async fn commit_validated_ordinary_extraction(
        &self,
        writes: Vec<MemoryFactProposalWrite>,
        session_id: &str,
        last_message_id: &str,
    ) -> anyhow::Result<FactProposalCommitOutcome> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "invalid session id for ordinary extraction"
        );
        anyhow::ensure!(
            is_canonical_id(last_message_id, "msg"),
            "invalid message id for ordinary extraction"
        );
        let writes = prepare_proposal_writes(writes)?;
        let session_id = session_id.to_owned();
        let last_message_id = last_message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.with_fact_write(|| {
                    let conn = db.try_conn()?;
                    in_immediate_transaction(&conn, |conn| {
                        let outcome = apply_validated_proposals(conn, db, &writes, &session_id)?;
                        let cursor_key = format!("fact_extraction.{session_id}");
                        set_kv_on_connection(conn, &cursor_key, &last_message_id)?;
                        Ok(outcome)
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
        let writes = writes
            .into_iter()
            .map(|write| MemoryFactProposalWrite {
                write,
                evidence_status: FactEvidenceStatus::Unverified,
                evidence_quote: None,
            })
            .collect();
        Ok(self
            .commit_validated_summary_extraction(writes, session_id, episode_id)
            .await?
            .queued)
    }

    /// Commit summary proposals and their completion marker atomically. Summary-derived facts
    /// should normally remain Unverified because a summary is not primary user evidence.
    pub async fn commit_validated_summary_extraction(
        &self,
        writes: Vec<MemoryFactProposalWrite>,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<FactProposalCommitOutcome> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "invalid session id for summary extraction"
        );
        anyhow::ensure!(
            is_canonical_id(episode_id, "msg"),
            "invalid episode id for summary extraction"
        );
        anyhow::ensure!(
            writes
                .iter()
                .all(|proposal| proposal.evidence_status != FactEvidenceStatus::Supported),
            "summary-derived facts cannot be automatically accepted"
        );
        let writes = prepare_proposal_writes(writes)?;
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
                            return Ok(FactProposalCommitOutcome::default());
                        }
                        let outcome = apply_validated_proposals(conn, db, &writes, &session_id)?;
                        set_kv_on_connection(conn, &marker_key, &session_id)?;
                        Ok(outcome)
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
                            durability, created_at, provenance_record_id, provenance_snippet,
                            evidence_status, evidence_quote, valid_from, valid_to
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
                                        provenance_record_id, provenance_snippet, valid_from, valid_to
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
                                        valid_from: row.get(8)?,
                                        valid_to: row.get(9)?,
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

fn prepare_proposal_writes(
    writes: Vec<MemoryFactProposalWrite>,
) -> anyhow::Result<Vec<MemoryFactProposalWrite>> {
    writes
        .iter()
        .map(|proposal| {
            let mut write = FactGraph::validate_inferred_proposal(&proposal.write)?;
            if let Some(quote) = proposal.evidence_quote.as_deref() {
                anyhow::ensure!(
                    quote.trim() == quote && !quote.is_empty() && quote.chars().count() <= 120,
                    "fact evidence quote must be non-empty and bounded"
                );
                anyhow::ensure!(
                    !is_sensitive_text(quote),
                    "sensitive text cannot be persisted as fact evidence"
                );
            }
            if proposal.evidence_status == FactEvidenceStatus::Supported {
                let quote = proposal.evidence_quote.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("supported fact needs an exact evidence quote")
                })?;
                let source_ref = write.source_ref.as_mut().ok_or_else(|| {
                    anyhow::anyhow!("supported fact needs a user source reference")
                })?;
                anyhow::ensure!(
                    is_canonical_id(&source_ref.message_id, "msg"),
                    "supported fact source message id must be canonical"
                );
                source_ref.snippet = quote.to_owned();
            }
            Ok(MemoryFactProposalWrite {
                write,
                evidence_status: proposal.evidence_status,
                evidence_quote: proposal.evidence_quote.clone(),
            })
        })
        .collect()
}

fn apply_validated_proposals(
    conn: &Connection,
    db: &Database,
    proposals: &[MemoryFactProposalWrite],
    session_id: &str,
) -> anyhow::Result<FactProposalCommitOutcome> {
    let mut outcome = FactProposalCommitOutcome::default();
    let mut pending = Vec::new();
    let mut unique: Vec<MemoryFactProposalWrite> = Vec::new();
    let mut positions: HashMap<(String, String, String), usize> = HashMap::new();
    for proposal in proposals {
        let key = (
            proposal.write.subject.clone(),
            proposal.write.predicate.clone(),
            proposal.write.object.clone(),
        );
        if let Some(index) = positions.get(&key).copied() {
            if proposal.evidence_status == FactEvidenceStatus::Supported
                && unique[index].evidence_status != FactEvidenceStatus::Supported
            {
                unique[index] = proposal.clone();
            }
        } else {
            positions.insert(key, unique.len());
            unique.push(proposal.clone());
        }
    }
    for proposal in &unique {
        if proposal.evidence_status != FactEvidenceStatus::Supported {
            pending.push(proposal.clone());
            continue;
        }
        validate_supported_evidence(conn, proposal, session_id)?;
        let mut write = proposal.write.clone();
        write.confidence = write.confidence.max(VERIFIED_FACT_CONFIDENCE_FLOOR);
        if let Some(source_ref) = write.source_ref.as_mut()
            && let Some(quote) = proposal.evidence_quote.as_deref()
        {
            source_ref.snippet = quote.to_owned();
        }
        // A newly verified direct statement supersedes an older unverified suggestion for the
        // same triple. The FactGraph upsert remains the sole fact/history writer.
        conn.execute(
            "DELETE FROM fact_proposals WHERE subject = ?1 AND predicate = ?2 AND object = ?3",
            rusqlite::params![write.subject, write.predicate, write.object],
        )?;
        if FactGraph::new(db).upsert_inferred_batch_on_connection(conn, &[write], 0.0)? {
            outcome.facts_written += 1;
        }
        outcome.auto_accepted += 1;
    }
    outcome.queued = insert_pending_proposals(conn, &pending)?;
    Ok(outcome)
}

fn validate_supported_evidence(
    conn: &Connection,
    proposal: &MemoryFactProposalWrite,
    session_id: &str,
) -> anyhow::Result<()> {
    let source_ref = proposal
        .write
        .source_ref
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("supported fact needs a user source reference"))?;
    let quote = proposal
        .evidence_quote
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("supported fact needs an exact evidence quote"))?;
    let source: Option<(String, String, String)> = conn
        .query_row(
            "SELECT role, content, session_id FROM messages WHERE id = ?1",
            rusqlite::params![source_ref.message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((role, content, source_session_id)) = source else {
        anyhow::bail!("supported fact source is not a stored user message");
    };
    anyhow::ensure!(
        source_session_id == session_id,
        "supported fact source must belong to the extraction session"
    );
    anyhow::ensure!(
        role == "user",
        "supported fact source must be a user message"
    );
    anyhow::ensure!(
        content.contains(quote),
        "supported fact evidence quote must occur in its source message"
    );
    Ok(())
}

fn insert_pending_proposals(
    conn: &Connection,
    proposals: &[MemoryFactProposalWrite],
) -> anyhow::Result<usize> {
    let mut unique = HashSet::new();
    let mut new_writes = Vec::new();
    for proposal in proposals {
        anyhow::ensure!(
            proposal.evidence_status != FactEvidenceStatus::Supported,
            "supported facts must be applied before pending proposals are inserted"
        );
        let write = &proposal.write;
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
            new_writes.push(proposal);
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
    for proposal in new_writes {
        let write = &proposal.write;
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
                created_at, provenance_record_id, provenance_snippet, evidence_status,
                evidence_quote, valid_from, valid_to
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                proposal.evidence_status.as_str(),
                proposal.evidence_quote,
                write.valid_from,
                write.valid_to,
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
    let evidence_status = row.get::<_, String>(10).and_then(|value| {
        FactEvidenceStatus::parse(&value).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                Type::Text,
                "fact evidence status is outside the current contract".into(),
            )
        })
    })?;
    let evidence_quote: Option<String> = row.get(11)?;
    let evidence_quote = evidence_quote.filter(|quote| !is_sensitive_text(quote));
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
        evidence_status,
        evidence_quote,
        valid_from: row.get(12)?,
        valid_to: row.get(13)?,
    })
}

#[cfg(test)]
#[path = "memory_fact_proposal_store_tests.rs"]
mod tests;
