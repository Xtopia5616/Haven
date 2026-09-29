use std::sync::Arc;
use std::time::Instant;

use haven_common::ActionStatus;
use haven_common::types::new_id;
use haven_memory::{ActionStore, Database, MemoryStore};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const SAMPLE_COUNT: usize = 257;
const WARMUP_COUNT: usize = 1;

fn percentile(samples: &mut [u128], numerator: usize) -> u128 {
    samples.sort_unstable();
    let rank = samples.len().saturating_mul(numerator).div_ceil(100);
    samples[rank.saturating_sub(1)]
}

fn distribution(samples_ns: &[u128]) -> (f64, f64) {
    (
        percentile(&mut samples_ns.to_vec(), 50) as f64 / 1_000.0,
        percentile(&mut samples_ns.to_vec(), 95) as f64 / 1_000.0,
    )
}

fn pending_action_count(db: &Database) -> i64 {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM action_completion_outbox WHERE delivered_at IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("query action outbox depth")
}

#[tokio::test]
#[ignore = "manual performance profile; run with --ignored --nocapture"]
async fn action_completion_outbox_latency_depth_and_throughput_profile() {
    let directory = tempfile::tempdir().expect("temporary profile directory");
    let db = Arc::new(
        Database::open(&directory.path().join("action-outbox-profile.db"))
            .expect("temporary disk database"),
    );
    let store = ActionStore::new(db.clone());
    let session_id = new_id("ses");

    let warmup_id = new_id("act");
    store
        .save_background_action(
            warmup_id.clone(),
            Some(session_id.clone()),
            "profile warmup".into(),
            "started".into(),
        )
        .await
        .unwrap();
    store
        .finish_background_action_with_completion(
            warmup_id.clone(),
            ActionStatus::Completed,
            Some("ok".into()),
            None,
            None,
            None,
            Some(0),
            "finished".into(),
            json!({"action_id": warmup_id, "status": "completed", "output": "ok"}),
        )
        .await
        .unwrap();
    let warmup = store
        .claim_pending_completion()
        .await
        .unwrap()
        .expect("warmup completion");
    store
        .acknowledge_completion(warmup.action_result_id)
        .await
        .unwrap();

    let mut action_ids = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let action_id = new_id("act");
        store
            .save_background_action(
                action_id.clone(),
                Some(session_id.clone()),
                "profile action".into(),
                "started".into(),
            )
            .await
            .unwrap();
        action_ids.push(action_id);
    }

    let mut enqueue_samples_ns = Vec::with_capacity(SAMPLE_COUNT);
    let enqueue_wall_started = Instant::now();
    let mut oldest_enqueued = None;
    for action_id in &action_ids {
        let started = Instant::now();
        let result = store
            .finish_background_action_with_completion(
                action_id.clone(),
                ActionStatus::Completed,
                Some("profile result".into()),
                None,
                None,
                None,
                Some(0),
                "finished".into(),
                json!({"action_id": action_id, "status": "completed", "output": "profile result"}),
            )
            .await
            .unwrap();
        assert!(result, "each terminal action must enqueue exactly once");
        oldest_enqueued.get_or_insert_with(Instant::now);
        enqueue_samples_ns.push(started.elapsed().as_nanos());
    }
    let enqueue_wall = enqueue_wall_started.elapsed();
    let high_water = pending_action_count(&db) as usize;
    assert_eq!(high_water, SAMPLE_COUNT);
    let oldest_pending_age_us = oldest_enqueued.unwrap().elapsed().as_micros();

    let mut claim_samples_ns = Vec::with_capacity(SAMPLE_COUNT);
    let mut ack_samples_ns = Vec::with_capacity(SAMPLE_COUNT);
    let drain_wall_started = Instant::now();
    for (index, _) in action_ids.iter().enumerate() {
        let depth_before_claim = pending_action_count(&db) as usize;
        assert_eq!(depth_before_claim, SAMPLE_COUNT - index);
        let claim_started = Instant::now();
        let completion = store
            .claim_pending_completion()
            .await
            .unwrap()
            .expect("pending completion");
        claim_samples_ns.push(claim_started.elapsed().as_nanos());

        let ack_started = Instant::now();
        assert!(
            store
                .acknowledge_completion(completion.action_result_id)
                .await
                .unwrap()
        );
        ack_samples_ns.push(ack_started.elapsed().as_nanos());
    }
    let drain_wall = drain_wall_started.elapsed();
    assert_eq!(pending_action_count(&db), 0);

    let (enqueue_p50, enqueue_p95) = distribution(&enqueue_samples_ns);
    let (claim_p50, claim_p95) = distribution(&claim_samples_ns);
    let (ack_p50, ack_p95) = distribution(&ack_samples_ns);
    println!(
        "profile action_outbox backend=temp_disk_sqlite samples={SAMPLE_COUNT} warmup={WARMUP_COUNT} pending_high_water={high_water} oldest_pending_age_us={oldest_pending_age_us} finish_commit_enqueue_p50_us={enqueue_p50:.2} finish_commit_enqueue_p95_us={enqueue_p95:.2} enqueue_per_s={:.1} claim_reconcile_p50_us={claim_p50:.2} claim_reconcile_p95_us={claim_p95:.2} ack_p50_us={ack_p50:.2} ack_p95_us={ack_p95:.2} drain_per_s={:.1}",
        SAMPLE_COUNT as f64 / enqueue_wall.as_secs_f64(),
        SAMPLE_COUNT as f64 / drain_wall.as_secs_f64(),
    );
}

#[tokio::test]
#[ignore = "manual performance profile; run with --ignored --nocapture"]
async fn memory_fact_outbox_latency_depth_and_throughput_profile() {
    let directory = tempfile::tempdir().expect("temporary profile directory");
    let db = Arc::new(
        Database::open(&directory.path().join("memory-outbox-profile.db"))
            .expect("temporary disk database"),
    );
    let store = MemoryStore::new(db.clone());
    let cancellation = CancellationToken::new();

    let warmup_id = db
        .create_session("memory outbox profile warmup")
        .expect("warmup session")
        .id;
    store
        .enqueue_fact_extraction_cancellable(&warmup_id, false, &cancellation)
        .await
        .unwrap();
    store
        .pending_fact_extractions_cancellable(&cancellation)
        .await
        .unwrap();
    store
        .clear_pending_fact_extraction_if_not_upgraded_cancellable(&warmup_id, false, &cancellation)
        .await
        .unwrap();

    let session_ids = (0..SAMPLE_COUNT)
        .map(|index| {
            db.create_session(&format!("memory outbox profile {index}"))
                .expect("profile session")
                .id
        })
        .collect::<Vec<_>>();
    let mut enqueue_samples_ns = Vec::with_capacity(SAMPLE_COUNT);
    let enqueue_wall_started = Instant::now();
    let mut oldest_enqueued = None;
    for session_id in &session_ids {
        let started = Instant::now();
        store
            .enqueue_fact_extraction_cancellable(session_id, false, &cancellation)
            .await
            .unwrap();
        oldest_enqueued.get_or_insert_with(Instant::now);
        enqueue_samples_ns.push(started.elapsed().as_nanos());
    }
    let enqueue_wall = enqueue_wall_started.elapsed();
    let pending = store
        .pending_fact_extractions_cancellable(&cancellation)
        .await
        .unwrap();
    let high_water = pending.len();
    assert_eq!(high_water, SAMPLE_COUNT);
    let oldest_pending_age_us = oldest_enqueued.unwrap().elapsed().as_micros();

    let mut list_samples_ns = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let started = Instant::now();
        let rows = store
            .pending_fact_extractions_cancellable(&cancellation)
            .await
            .unwrap();
        assert_eq!(rows.len(), high_water);
        list_samples_ns.push(started.elapsed().as_nanos());
    }

    let mut ack_samples_ns = Vec::with_capacity(SAMPLE_COUNT);
    let drain_wall_started = Instant::now();
    for session_id in &session_ids {
        let started = Instant::now();
        store
            .clear_pending_fact_extraction_if_not_upgraded_cancellable(
                session_id,
                false,
                &cancellation,
            )
            .await
            .unwrap();
        ack_samples_ns.push(started.elapsed().as_nanos());
    }
    let drain_wall = drain_wall_started.elapsed();
    assert!(
        store
            .pending_fact_extractions_cancellable(&cancellation)
            .await
            .unwrap()
            .is_empty()
    );

    let (enqueue_p50, enqueue_p95) = distribution(&enqueue_samples_ns);
    let (list_p50, list_p95) = distribution(&list_samples_ns);
    let (ack_p50, ack_p95) = distribution(&ack_samples_ns);
    println!(
        "profile memory_fact_outbox backend=temp_disk_sqlite samples={SAMPLE_COUNT} warmup={WARMUP_COUNT} pending_high_water={high_water} oldest_pending_age_us={oldest_pending_age_us} durable_enqueue_p50_us={enqueue_p50:.2} durable_enqueue_p95_us={enqueue_p95:.2} enqueue_per_s={:.1} pending_list_at_depth_{high_water}_p50_us={list_p50:.2} pending_list_at_depth_{high_water}_p95_us={list_p95:.2} conditional_ack_p50_us={ack_p50:.2} conditional_ack_p95_us={ack_p95:.2} clear_per_s={:.1}",
        SAMPLE_COUNT as f64 / enqueue_wall.as_secs_f64(),
        SAMPLE_COUNT as f64 / drain_wall.as_secs_f64(),
    );
}
