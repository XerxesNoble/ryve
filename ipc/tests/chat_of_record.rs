// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::chat_of_record`.
//!
//! Covers the acceptance criteria on spark ryve-d9523f48:
//! - `post_message` writes a row to `irc_messages` and returns its id.
//! - `tail` paginates by `since`, bounds by `limit`, filters by `author`.
//! - Invalid `limit` values are surfaced, not silently clamped.

use ipc::channel_manager::{EpicRef, channel_name};
use ipc::chat_of_record::{ChatError, NewPost, TAIL_MAX_LIMIT, TailFilter, post_message, tail};
use sqlx::SqlitePool;

const EPIC_TITLE: &str = "Chat epic";

/// Seed one epic spark so the `epic_id` FK on `irc_messages` is satisfied.
async fn seed_epic(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO sparks \
         (id, title, description, status, priority, spark_type, workshop_id, created_at, updated_at) \
         VALUES (?, ?, '', 'open', 1, 'epic', 'ws-chat', \
                 '2026-04-19T00:00:00Z', '2026-04-19T00:00:00Z')",
    )
    .bind(id)
    .bind(EPIC_TITLE)
    .execute(pool)
    .await
    .unwrap();
}

/// Seed an agent session so posts can reference a sender_actor_id that
/// satisfies the FK on `irc_messages.sender_actor_id`.
async fn seed_session(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO agent_sessions \
         (id, workshop_id, agent_name, agent_command, agent_args, status, started_at) \
         VALUES (?, 'ws-chat', 'test', '/bin/true', '[]', 'running', \
                 '2026-04-19T00:00:00Z')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

fn channel_for(epic_id: &str) -> String {
    channel_name(&EpicRef {
        id: epic_id.to_string(),
        name: EPIC_TITLE.to_string(),
    })
}

async fn post(pool: &SqlitePool, epic_id: &str, body: &str, author: Option<&str>) -> i64 {
    post_message(
        pool,
        NewPost {
            channel: channel_for(epic_id),
            body: body.to_string(),
            author_session_id: author.map(str::to_string),
            epic_id: None,
        },
    )
    .await
    .expect("post should succeed")
}

#[sqlx::test(migrations = "../data/migrations")]
async fn post_message_inserts_row_and_returns_id(pool: SqlitePool) {
    seed_epic(&pool, "1").await;

    let id = post(&pool, "1", "plan: land the foundation", None).await;
    assert!(id > 0, "post should return a positive id");

    let rows = tail(
        &pool,
        TailFilter::for_channel(channel_for("1")).with_limit(10),
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].raw_text, "plan: land the foundation");
    assert_eq!(rows[0].command, "PRIVMSG");
    assert_eq!(rows[0].epic_id, "1");
    assert!(rows[0].sender_actor_id.is_none());
    assert!(!rows[0].irc_message_id.is_empty());
}

#[sqlx::test(migrations = "../data/migrations")]
async fn post_message_rejects_non_epic_channel_without_explicit_epic_id(pool: SqlitePool) {
    let err = post_message(
        &pool,
        NewPost {
            channel: "#atlas".to_string(),
            body: "hello atlas".to_string(),
            author_session_id: None,
            epic_id: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ChatError::UnresolvedEpic(ref c) if c == "#atlas"),
        "expected UnresolvedEpic for #atlas, got {err:?}"
    );
}

#[sqlx::test(migrations = "../data/migrations")]
async fn tail_limits_output_and_returns_chronological_order(pool: SqlitePool) {
    seed_epic(&pool, "2").await;

    // Post five messages with strictly increasing created_at so since /
    // limit are deterministic.
    for i in 0..5 {
        post(&pool, "2", &format!("msg {i}"), None).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let page = tail(
        &pool,
        TailFilter::for_channel(channel_for("2")).with_limit(3),
    )
    .await
    .unwrap();
    assert_eq!(page.len(), 3, "limit must cap result length");
    assert_eq!(page[0].raw_text, "msg 0");
    assert_eq!(page[1].raw_text, "msg 1");
    assert_eq!(page[2].raw_text, "msg 2");
}

#[sqlx::test(migrations = "../data/migrations")]
async fn tail_since_filters_to_strictly_newer_rows(pool: SqlitePool) {
    seed_epic(&pool, "3").await;

    for i in 0..4 {
        post(&pool, "3", &format!("msg {i}"), None).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let all = tail(
        &pool,
        TailFilter::for_channel(channel_for("3")).with_limit(10),
    )
    .await
    .unwrap();
    let cursor = all[1].created_at.clone();

    let page = tail(
        &pool,
        TailFilter::for_channel(channel_for("3"))
            .with_since(Some(cursor.clone()))
            .with_limit(10),
    )
    .await
    .unwrap();
    assert_eq!(page.len(), 2, "since must exclude rows at or before cursor");
    assert_eq!(page[0].raw_text, "msg 2");
    assert_eq!(page[1].raw_text, "msg 3");

    // A cursor past the last row returns an empty page.
    let past = tail(
        &pool,
        TailFilter::for_channel(channel_for("3"))
            .with_since(Some(all.last().unwrap().created_at.clone()))
            .with_limit(10),
    )
    .await
    .unwrap();
    assert!(past.is_empty());
}

#[sqlx::test(migrations = "../data/migrations")]
async fn tail_scopes_to_channel(pool: SqlitePool) {
    seed_epic(&pool, "4").await;
    seed_epic(&pool, "5").await;

    post(&pool, "4", "in four", None).await;
    post(&pool, "5", "in five", None).await;

    let four = tail(
        &pool,
        TailFilter::for_channel(channel_for("4")).with_limit(10),
    )
    .await
    .unwrap();
    assert_eq!(four.len(), 1);
    assert_eq!(four[0].raw_text, "in four");
}

#[sqlx::test(migrations = "../data/migrations")]
async fn tail_author_filter_narrows_to_session(pool: SqlitePool) {
    seed_epic(&pool, "6").await;
    seed_session(&pool, "sess-alice").await;
    seed_session(&pool, "sess-bob").await;

    post(&pool, "6", "alice 1", Some("sess-alice")).await;
    post(&pool, "6", "bob 1", Some("sess-bob")).await;
    post(&pool, "6", "alice 2", Some("sess-alice")).await;
    post(&pool, "6", "anon", None).await;

    let alice = tail(
        &pool,
        TailFilter::for_channel(channel_for("6"))
            .with_author(Some("sess-alice".into()))
            .with_limit(10),
    )
    .await
    .unwrap();
    assert_eq!(alice.len(), 2);
    assert_eq!(alice[0].raw_text, "alice 1");
    assert_eq!(alice[1].raw_text, "alice 2");
    assert!(
        alice
            .iter()
            .all(|m| m.sender_actor_id.as_deref() == Some("sess-alice"))
    );
}

#[sqlx::test(migrations = "../data/migrations")]
async fn tail_rejects_out_of_range_limit(pool: SqlitePool) {
    seed_epic(&pool, "7").await;

    let zero = tail(
        &pool,
        TailFilter::for_channel(channel_for("7")).with_limit(0),
    )
    .await
    .unwrap_err();
    assert!(matches!(zero, ChatError::InvalidLimit { got: 0, .. }));

    let huge = tail(
        &pool,
        TailFilter::for_channel(channel_for("7")).with_limit(TAIL_MAX_LIMIT + 1),
    )
    .await
    .unwrap_err();
    assert!(matches!(huge, ChatError::InvalidLimit { .. }));

    let negative = tail(
        &pool,
        TailFilter::for_channel(channel_for("7")).with_limit(-1),
    )
    .await
    .unwrap_err();
    assert!(matches!(negative, ChatError::InvalidLimit { got: -1, .. }));
}

#[sqlx::test(migrations = "../data/migrations")]
async fn tail_empty_channel_returns_empty(pool: SqlitePool) {
    // Query a channel with no rows — valid channel shape, no epic
    // seeded, tail should simply return [] without hitting an FK.
    let rows = tail(
        &pool,
        TailFilter::for_channel("#epic-999-nothing").with_limit(10),
    )
    .await
    .unwrap();
    assert!(rows.is_empty());
}
