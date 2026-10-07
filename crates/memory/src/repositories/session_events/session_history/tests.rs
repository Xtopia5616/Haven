use super::*;
use crate::Database;
use std::sync::Arc;

fn store() -> (Arc<Database>, SessionStore, String) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("input").unwrap();
    let store = SessionStore::new(db.clone());
    (db, store, session.id)
}

fn resume_test_attachment(filename: &str) -> MessageAttachment {
    let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
    attachment.asset_id = Some(haven_common::types::new_id("asset"));
    attachment.filename = Some(filename.to_owned());
    attachment
}

#[tokio::test]
async fn session_store_latest_record_preserves_recent_history_order_and_empty_result() {
    let (db, store, first_id) = store();
    let second = db.create_session("second").unwrap();
    let third = db.create_session("third").unwrap();
    let conn = db.conn();
    for (session_id, created_at) in [
        (&first_id, "2026-09-20T10:00:00.000Z"),
        (&second.id, "2026-09-21T10:00:00.000Z"),
        (&third.id, "2026-09-22T10:00:00.000Z"),
    ] {
        conn.execute(
            "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![created_at, session_id],
        )
        .unwrap();
    }
    drop(conn);

    let latest = store.latest_session_record().await.unwrap();
    assert_eq!(
        latest.as_ref().map(|session| session.id.as_str()),
        Some(third.id.as_str())
    );
    assert_eq!(
        serde_json::to_value(latest).unwrap(),
        serde_json::to_value(db.list_persisted_sessions(1, 0).unwrap().into_iter().next()).unwrap()
    );

    let empty_db = Arc::new(Database::open_in_memory().unwrap());
    let empty_store = SessionStore::new(empty_db);
    assert!(empty_store.latest_session_record().await.unwrap().is_none());
}

#[tokio::test]
async fn session_store_loads_title_generation_context_with_original_filter_and_limit() {
    let (db, store, session_id) = store();
    let missing_session_id = haven_common::types::new_id("ses");
    assert!(
        store
            .title_generation_context(&missing_session_id)
            .await
            .unwrap()
            .is_none()
    );

    for index in 0..12 {
        let role = if index % 2 == 0 {
            haven_common::types::CanonicalRole::Assistant
        } else {
            haven_common::types::CanonicalRole::User
        };
        db.add_message(
            &session_id,
            role,
            &format!("{role}-{index}"),
            Some("text"),
            None,
        )
        .unwrap();
    }

    let context = store
        .title_generation_context(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        context,
        SessionTitleGenerationContext {
            user_messages: vec![
                "user-3".into(),
                "user-5".into(),
                "user-7".into(),
                "user-9".into(),
                "user-11".into(),
            ],
        }
    );

    db.update_session_title(&session_id, "Already titled")
        .unwrap();
    assert!(
        store
            .title_generation_context(&session_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn session_store_lists_latest_prompt_messages_in_chronological_order() {
    let (db, store, session_id) = store();
    let _old = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "old",
            Some("text"),
            None,
        )
        .unwrap();
    let middle = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "middle",
            Some("text"),
            None,
        )
        .unwrap();
    let latest = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "latest",
            Some("text"),
            None,
        )
        .unwrap();

    let window = store
        .list_session_prompt_messages(&session_id, 2)
        .await
        .unwrap();

    assert_eq!(
        window,
        vec![
            SessionMessageText {
                id: middle.id,
                role: haven_common::types::CanonicalRole::Assistant,
                content: "middle".into(),
            },
            SessionMessageText {
                id: latest.id,
                role: haven_common::types::CanonicalRole::User,
                content: "latest".into(),
            },
        ]
    );
    let missing_session_id = haven_common::types::new_id("ses");
    assert!(
        store
            .list_session_prompt_messages(&missing_session_id, 2)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn session_store_session_resume_media_uses_first_user_and_message_order() {
    let (db, store, session_id) = store();
    let assistant_attachment = resume_test_attachment("assistant.png");
    db.add_message_full(
        &session_id,
        haven_common::types::CanonicalRole::Assistant,
        "before input",
        Some("text"),
        None,
        std::slice::from_ref(&assistant_attachment),
        false,
        None,
    )
    .unwrap();
    let initial_attachment = resume_test_attachment("initial.png");
    let initial_message = db
        .add_message_full(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "initial input",
            Some("text"),
            None,
            std::slice::from_ref(&initial_attachment),
            false,
            None,
        )
        .unwrap();
    let later_attachment = resume_test_attachment("later.png");
    db.add_message_full(
        &session_id,
        haven_common::types::CanonicalRole::User,
        "later input",
        Some("text"),
        None,
        std::slice::from_ref(&later_attachment),
        false,
        None,
    )
    .unwrap();

    let resume_media = store.session_resume_media(&session_id).await.unwrap();
    let persisted_messages = db.list_session_messages(&session_id).unwrap();

    assert_eq!(
        resume_media.initial_message_id.as_deref(),
        Some(initial_message.id.as_str())
    );
    assert_eq!(
        resume_media
            .initial_attachments
            .iter()
            .filter_map(|attachment| attachment.filename.as_deref())
            .collect::<Vec<_>>(),
        vec!["initial.png"]
    );
    assert_eq!(
        resume_media.initial_media_inputs,
        persisted_messages[1].media_inputs
    );
    assert_eq!(
        resume_media
            .all_attachments
            .iter()
            .filter_map(|attachment| attachment.filename.as_deref())
            .collect::<Vec<_>>(),
        vec!["assistant.png", "initial.png", "later.png"]
    );
}

#[tokio::test]
async fn session_store_session_resume_media_returns_empty_media_and_isolates_sessions() {
    let (db, store, session_id) = store();
    let initial_message = db
        .add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "plain input",
            Some("text"),
            None,
        )
        .unwrap();
    let other_session = db.create_session("other session").unwrap();
    let other_attachment = resume_test_attachment("other-session.png");
    db.add_message_full(
        &other_session.id,
        haven_common::types::CanonicalRole::User,
        "other input",
        Some("text"),
        None,
        std::slice::from_ref(&other_attachment),
        false,
        None,
    )
    .unwrap();

    let resume_media = store.session_resume_media(&session_id).await.unwrap();

    assert_eq!(
        resume_media.initial_message_id.as_deref(),
        Some(initial_message.id.as_str())
    );
    assert!(resume_media.initial_attachments.is_empty());
    assert!(resume_media.initial_media_inputs.is_empty());
    assert!(resume_media.all_attachments.is_empty());
}

#[tokio::test]
async fn session_store_history_ports_preserve_database_query_semantics() {
    let (db, store, first_id) = store();
    let second = db.create_session("history needle two").unwrap();
    let third = db.create_session("history other three").unwrap();
    db.update_session_title(&second.id, "named needle").unwrap();

    let conn = db.conn();
    for (session_id, created_at) in [
        (&first_id, "2026-09-20T10:00:00.000Z"),
        (&second.id, "2026-09-21T10:00:00.000Z"),
        (&third.id, "2026-09-22T10:00:00.000Z"),
    ] {
        conn.execute(
            "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![created_at, session_id],
        )
        .unwrap();
    }
    drop(conn);

    let as_json = |sessions: Vec<Session>| serde_json::to_value(sessions).unwrap();

    assert_eq!(
        as_json(store.list_session_history(2, 1).await.unwrap()),
        as_json(db.list_persisted_sessions(2, 1).unwrap())
    );
    assert_eq!(
        store.count_session_history().await.unwrap(),
        db.count_sessions().unwrap()
    );
    assert_eq!(
        as_json(
            store
                .search_session_history_paginated("needle".into(), 1, 1)
                .await
                .unwrap()
        ),
        as_json(db.search_sessions_paginated("needle", 1, 1).unwrap())
    );
    assert_eq!(
        store
            .count_session_history_search("needle".into())
            .await
            .unwrap(),
        db.count_sessions_search("needle").unwrap()
    );
    assert_eq!(
        as_json(store.search_session_history("needle".into()).await.unwrap()),
        as_json(db.search_sessions("needle").unwrap())
    );

    let filter = SessionHistoryFilter {
        query: Some("needle".into()),
        status: Some("pending".into()),
        start_date: None,
        end_date: None,
        limit: 10,
        offset: 0,
    };
    assert_eq!(
        as_json(
            store
                .search_session_history_filtered(filter.clone())
                .await
                .unwrap()
        ),
        as_json(
            db.search_sessions_filtered(
                filter.query.as_deref(),
                filter.status.as_deref(),
                filter.start_date.as_deref(),
                filter.end_date.as_deref(),
                filter.limit,
                filter.offset,
            )
            .unwrap()
        )
    );
}
