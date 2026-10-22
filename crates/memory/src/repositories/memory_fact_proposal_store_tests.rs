use super::{
    FactEvidenceStatus, FactProposalDecision, FactProposalReviewOutcome,
    MAX_FACT_PROPOSAL_PAGE_SIZE, MAX_PENDING_FACT_PROPOSALS, MemoryFactProposalStore,
    MemoryFactProposalWrite, VERIFIED_FACT_CONFIDENCE_FLOOR,
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
        valid_from: None,
        valid_to: None,
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
            .expect("test database connection checkout failed")
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
async fn proposal_validity_survives_review_and_becomes_fact_validity() {
    let (db, store) = store();
    let session_id = db.create_session("proposal valid time").unwrap().session_id;
    let mut candidate = write("Acme");
    candidate.predicate = "employer".into();
    candidate.valid_from = Some("2024-01-01T00:00:00.000Z".into());
    candidate.valid_to = Some("2025-01-01T00:00:00.000Z".into());
    store
        .commit_ordinary_extraction(vec![candidate], &session_id, &new_id("msg"))
        .await
        .unwrap();

    let proposal = store
        .list_pending_page(None, 10)
        .await
        .unwrap()
        .proposals
        .remove(0);
    assert_eq!(
        proposal.valid_from.as_deref(),
        Some("2024-01-01T00:00:00.000Z")
    );
    assert_eq!(
        proposal.valid_to.as_deref(),
        Some("2025-01-01T00:00:00.000Z")
    );
    assert_eq!(
        store
            .review(proposal.proposal_id, FactProposalDecision::Accept)
            .await
            .unwrap(),
        FactProposalReviewOutcome::Accepted { fact_written: true }
    );
    let fact = db.list_facts().unwrap().remove(0);
    assert_eq!(fact.valid_from.as_deref(), Some("2024-01-01T00:00:00.000Z"));
    assert_eq!(fact.valid_to.as_deref(), Some("2025-01-01T00:00:00.000Z"));
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
        .expect("test database connection checkout failed")
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
        .expect("test database connection checkout failed")
        .execute_batch("DROP TRIGGER reject_cursor")
        .unwrap();

    for index in 0..MAX_PENDING_FACT_PROPOSALS {
        db.conn()
            .expect("test database connection checkout failed")
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
            .expect("test database connection checkout failed")
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
        .expect("test database connection checkout failed")
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
    let proposal = store
        .list_pending_page(None, 10)
        .await
        .unwrap()
        .proposals
        .remove(0);
    assert_eq!(proposal.evidence_status, FactEvidenceStatus::Unverified);
}

#[tokio::test]
async fn summary_extraction_cannot_auto_accept_even_with_user_message_evidence() {
    let (db, store) = store();
    let session = db.create_session("summary evidence gate").unwrap();
    let episode_id = new_id("msg");
    db.add_episode_with_pending_extraction(
        &session.session_id,
        "Summary states a user preference.",
        &episode_id,
        false,
    )
    .unwrap();
    let source = db
        .add_message(
            &session.session_id,
            haven_common::types::CanonicalRole::User,
            "I like Rust.",
            None,
            None,
        )
        .unwrap();
    let mut candidate = write("Rust");
    candidate.source_ref = Some(FactSourceRef::from_message(
        &source.message_id,
        "I like Rust.",
    ));

    assert!(
        store
            .commit_validated_summary_extraction(
                vec![MemoryFactProposalWrite {
                    write: candidate,
                    evidence_status: FactEvidenceStatus::Supported,
                    evidence_quote: Some("I like Rust.".into()),
                }],
                &session.session_id,
                &episode_id,
            )
            .await
            .is_err()
    );
    assert!(db.list_facts().unwrap().is_empty());
    assert!(
        !store
            .summary_extraction_completed(&session.session_id, &episode_id)
            .await
            .unwrap()
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

#[tokio::test]
async fn supported_fact_is_written_with_evidence_quote_and_confidence_floor() {
    let (db, store) = store();
    let session_id = db.create_session("verified proposal").unwrap().session_id;
    let message_id = new_id("msg");
    let source = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "I like Rust.",
            None,
            None,
        )
        .unwrap();
    let mut candidate = write("Rust");
    candidate.source_ref = Some(FactSourceRef::from_message(
        &source.message_id,
        "I like Rust.",
    ));
    let outcome = store
        .commit_validated_ordinary_extraction(
            vec![MemoryFactProposalWrite {
                write: candidate,
                evidence_status: FactEvidenceStatus::Supported,
                evidence_quote: Some("I like Rust.".into()),
            }],
            &session_id,
            &message_id,
        )
        .await
        .unwrap();

    assert_eq!(outcome.auto_accepted, 1);
    assert_eq!(outcome.queued, 0);
    assert_eq!(outcome.facts_written, 1);
    let facts = db.list_facts().unwrap();
    assert_eq!(facts.len(), 1);
    assert!(facts[0].confidence >= VERIFIED_FACT_CONFIDENCE_FLOOR);
    assert_eq!(
        facts[0].source_ref.as_ref().unwrap().snippet,
        "I like Rust."
    );
    assert!(
        store
            .list_pending_page(None, 10)
            .await
            .unwrap()
            .proposals
            .is_empty()
    );
    assert_eq!(
        db.get_kv(&format!("fact_extraction.{session_id}"))
            .unwrap()
            .as_deref(),
        Some(message_id.as_str())
    );
}

#[tokio::test]
async fn uncertain_fact_is_queued_with_its_verification_status() {
    let (db, store) = store();
    let session_id = db.create_session("uncertain proposal").unwrap().session_id;
    let outcome = store
        .commit_validated_ordinary_extraction(
            vec![MemoryFactProposalWrite {
                write: write("Rust"),
                evidence_status: FactEvidenceStatus::Uncertain,
                evidence_quote: None,
            }],
            &session_id,
            &new_id("msg"),
        )
        .await
        .unwrap();

    assert_eq!(outcome.queued, 1);
    assert_eq!(outcome.auto_accepted, 0);
    assert!(db.list_facts().unwrap().is_empty());
    let proposal = store
        .list_pending_page(None, 10)
        .await
        .unwrap()
        .proposals
        .remove(0);
    assert_eq!(proposal.evidence_status, FactEvidenceStatus::Uncertain);
    assert!(proposal.evidence_quote.is_none());
}

#[tokio::test]
async fn cursor_failure_rolls_back_verified_fact_write() {
    let (db, store) = store();
    let session_id = db.create_session("verified rollback").unwrap().session_id;
    let source = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "I like Rust.",
            None,
            None,
        )
        .unwrap();
    let mut candidate = write("Rust");
    candidate.source_ref = Some(FactSourceRef::from_message(
        &source.message_id,
        "I like Rust.",
    ));
    let marker_key = format!("fact_extraction.{session_id}");
    db.conn()
        .expect("test database connection checkout failed")
        .execute_batch(&format!(
            "CREATE TRIGGER fail_fact_marker BEFORE INSERT ON kv_store
                 WHEN NEW.key = '{marker_key}'
                 BEGIN SELECT RAISE(ABORT, 'forced marker failure'); END;"
        ))
        .unwrap();

    assert!(
        store
            .commit_validated_ordinary_extraction(
                vec![MemoryFactProposalWrite {
                    write: candidate,
                    evidence_status: FactEvidenceStatus::Supported,
                    evidence_quote: Some("I like Rust.".into()),
                }],
                &session_id,
                &new_id("msg"),
            )
            .await
            .is_err()
    );
    assert!(db.list_facts().unwrap().is_empty());
    assert!(db.get_kv(&marker_key).unwrap().is_none());
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
async fn supported_fact_requires_a_stored_user_message_with_matching_quote() {
    let (db, store) = store();
    let session_id = db
        .create_session("supported evidence gate")
        .unwrap()
        .session_id;
    let missing_source_id = new_id("msg");
    let mut candidate = write("Rust");
    candidate.source_ref = Some(FactSourceRef::from_message(
        &missing_source_id,
        "I like Rust.",
    ));

    assert!(
        store
            .commit_validated_ordinary_extraction(
                vec![MemoryFactProposalWrite {
                    write: candidate.clone(),
                    evidence_status: FactEvidenceStatus::Supported,
                    evidence_quote: Some("I like Rust.".into()),
                }],
                &session_id,
                &new_id("msg"),
            )
            .await
            .is_err()
    );

    let other_session_id = db
        .create_session("other evidence session")
        .unwrap()
        .session_id;
    let other_source = db
        .add_message(
            &other_session_id,
            haven_common::types::CanonicalRole::User,
            "I like Rust.",
            None,
            None,
        )
        .unwrap();
    candidate.source_ref = Some(FactSourceRef::from_message(
        &other_source.message_id,
        "I like Rust.",
    ));
    assert!(
        store
            .commit_validated_ordinary_extraction(
                vec![MemoryFactProposalWrite {
                    write: candidate.clone(),
                    evidence_status: FactEvidenceStatus::Supported,
                    evidence_quote: Some("I like Rust.".into()),
                }],
                &session_id,
                &new_id("msg"),
            )
            .await
            .is_err()
    );

    let user = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "I prefer tea.",
            None,
            None,
        )
        .unwrap();
    candidate.source_ref = Some(FactSourceRef::from_message(
        &user.message_id,
        "I like Rust.",
    ));
    assert!(
        store
            .commit_validated_ordinary_extraction(
                vec![MemoryFactProposalWrite {
                    write: candidate.clone(),
                    evidence_status: FactEvidenceStatus::Supported,
                    evidence_quote: Some("I like Rust.".into()),
                }],
                &session_id,
                &new_id("msg"),
            )
            .await
            .is_err()
    );

    let assistant_id = new_id("msg");
    db.add_message_full(
        &session_id,
        haven_common::types::CanonicalRole::Assistant,
        "I like Rust.",
        None,
        None,
        &[],
        false,
        Some(&assistant_id),
    )
    .unwrap();
    candidate.source_ref = Some(FactSourceRef::from_message(&assistant_id, "I like Rust."));
    assert!(
        store
            .commit_validated_ordinary_extraction(
                vec![MemoryFactProposalWrite {
                    write: candidate,
                    evidence_status: FactEvidenceStatus::Supported,
                    evidence_quote: Some("I like Rust.".into()),
                }],
                &session_id,
                &new_id("msg"),
            )
            .await
            .is_err()
    );
    assert!(db.list_facts().unwrap().is_empty());
    assert!(
        db.get_kv(&format!("fact_extraction.{session_id}"))
            .unwrap()
            .is_none()
    );
}
