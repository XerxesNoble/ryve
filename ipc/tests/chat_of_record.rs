// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::chat_of_record`.
//!
//! Covers the acceptance criteria on spark ryve-d9523f48:
//! - `post_message` writes a row to `irc_messages` and returns its id.
//! - `tail` paginates by `since`, bounds by `limit`, filters by `author`.
//! - Invalid `limit` values are surfaced, not silently clamped.

use ipc::channel_manager::{EpicRef, channel_name};
use ipc::chat_of_record::{
    ChatError, NewPost, TAIL_MAX_LIMIT, TailFilter, count_posts_since_claim, post_message, tail,
};
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

/// Raw insert helper for `count_posts_since_claim` tests: bypasses
/// [`post_message`]'s channel→epic resolution so rows can be tagged with
/// any spark id (child-task ids, not just epics). The mandatory-post
/// gate queries `irc_messages.epic_id` directly, so the tests exercise
/// that column as a generic spark FK.
async fn insert_irc_row(
    pool: &SqlitePool,
    epic_id: &str,
    author: Option<&str>,
    created_at: &str,
    body: &str,
) {
    sqlx::query(
        "INSERT INTO irc_messages \
         (epic_id, channel, irc_message_id, sender_actor_id, command, raw_text, created_at) \
         VALUES (?, '#test', ?, ?, 'PRIVMSG', ?, ?)",
    )
    .bind(epic_id)
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(author)
    .bind(body)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../data/migrations")]
async fn count_posts_since_claim_counts_only_matching_session_spark_and_cutoff(pool: SqlitePool) {
    // Seed the sparks referenced by epic_id (FK NOT NULL).
    seed_epic(&pool, "sp-target").await;
    seed_epic(&pool, "sp-other").await;
    seed_session(&pool, "sess-claimer").await;
    seed_session(&pool, "sess-stranger").await;

    let claim_ts = "2026-04-20T00:00:00Z";

    // Before the claim: must NOT count, even though session + spark match.
    insert_irc_row(
        &pool,
        "sp-target",
        Some("sess-claimer"),
        "2026-04-19T23:59:59Z",
        "pre-claim",
    )
    .await;

    // After claim, right session + right spark: counts.
    insert_irc_row(
        &pool,
        "sp-target",
        Some("sess-claimer"),
        "2026-04-20T00:05:00Z",
        "on-handoff one",
    )
    .await;
    insert_irc_row(
        &pool,
        "sp-target",
        Some("sess-claimer"),
        "2026-04-20T00:10:00Z",
        "on-handoff two",
    )
    .await;

    // After claim, right session but wrong spark: does NOT count (scope).
    insert_irc_row(
        &pool,
        "sp-other",
        Some("sess-claimer"),
        "2026-04-20T00:06:00Z",
        "wrong spark",
    )
    .await;

    // After claim, right spark but wrong session: does NOT count (authorship).
    insert_irc_row(
        &pool,
        "sp-target",
        Some("sess-stranger"),
        "2026-04-20T00:07:00Z",
        "someone else",
    )
    .await;

    // Anonymous post (sender_actor_id NULL): does NOT count — the gate
    // must attribute the post to the closing session specifically.
    insert_irc_row(&pool, "sp-target", None, "2026-04-20T00:08:00Z", "anon").await;

    let n = count_posts_since_claim(&pool, "sess-claimer", "sp-target", claim_ts)
        .await
        .unwrap();
    assert_eq!(
        n, 2,
        "only rows with matching session, matching spark, and created_at >= claim_ts count",
    );
}

#[sqlx::test(migrations = "../data/migrations")]
async fn count_posts_since_claim_returns_zero_when_nothing_posted(pool: SqlitePool) {
    seed_epic(&pool, "sp-empty").await;
    seed_session(&pool, "sess-claimer").await;
    seed_session(&pool, "sess-other").await;

    // Cross-noise to prove the query is scoped, not returning everything.
    insert_irc_row(
        &pool,
        "sp-empty",
        Some("sess-other"),
        "2026-04-20T00:05:00Z",
        "not me",
    )
    .await;

    let n = count_posts_since_claim(&pool, "sess-claimer", "sp-empty", "2026-04-20T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "../data/migrations")]
async fn count_posts_since_claim_boundary_is_inclusive(pool: SqlitePool) {
    seed_epic(&pool, "sp-edge").await;
    seed_session(&pool, "sess-edge").await;

    let claim_ts = "2026-04-20T00:00:00Z";

    // A post at exactly the claim timestamp must satisfy the gate — the
    // "since" boundary is inclusive (claim and post land in the same tx
    // for same-instant race cases).
    insert_irc_row(
        &pool,
        "sp-edge",
        Some("sess-edge"),
        claim_ts,
        "same instant",
    )
    .await;

    let n = count_posts_since_claim(&pool, "sess-edge", "sp-edge", claim_ts)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// PR #54 Copilot c3 regression: when a Hand on a non-epic spark
/// (a task under a parent epic) posts to the parent epic's channel,
/// `count_posts_since_claim(session, task_spark_id, ...)` must count
/// that post via the parent_child bond fallback. Without the
/// fallback, the gate would always count 0 for non-epic assignments
/// and refuse every close.
#[sqlx::test(migrations = "../data/migrations")]
async fn count_posts_since_claim_falls_back_to_parent_epic_via_bond(pool: SqlitePool) {
    // Seed: epic + child task spark + parent_child bond.
    seed_epic(&pool, "sp-parent-epic").await;
    sqlx::query(
        "INSERT INTO sparks \
         (id, title, description, status, priority, spark_type, workshop_id, created_at, updated_at) \
         VALUES ('sp-child-task', 'child task', '', 'in_progress', 1, 'task', 'ws-chat', \
                 '2026-04-20T00:00:00Z', '2026-04-20T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO bonds (from_id, to_id, bond_type) \
         VALUES ('sp-parent-epic', 'sp-child-task', 'parent_child')",
    )
    .execute(&pool)
    .await
    .unwrap();
    seed_session(&pool, "sess-task-hand").await;

    let claim_ts = "2026-04-20T01:00:00Z";

    // The Hand posts to the EPIC channel (epic_id = "sp-parent-epic"),
    // because that's where chat-of-record discipline lands chatter for
    // children of an epic. The post happens after the claim ts.
    insert_irc_row(
        &pool,
        "sp-parent-epic",
        Some("sess-task-hand"),
        "2026-04-20T01:05:00Z",
        "claim: starting the task",
    )
    .await;

    // Counting posts for the CHILD spark must surface the post via the
    // parent_child bond, not return zero just because epic_id !=
    // child task id.
    let n = count_posts_since_claim(&pool, "sess-task-hand", "sp-child-task", claim_ts)
        .await
        .unwrap();
    assert_eq!(
        n, 1,
        "post to parent epic's channel must count for the child task's gate"
    );
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
