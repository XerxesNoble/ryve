//! Integration tests for `irc_repo`.
//!
//! Covers the acceptance criteria on spark `sp-ddf6fd7f`: insert, list
//! pagination by `(epic_id, created_at)`, and full-text search over
//! `raw_text` via the `irc_messages_fts` virtual table.

use data::sparks::irc_repo;
use data::sparks::types::*;

/// Seed one epic spark so the `epic_id` FK on `irc_messages` is satisfied.
async fn seed_epic(pool: &sqlx::SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO sparks \
         (id, title, description, status, priority, spark_type, workshop_id, created_at, updated_at) \
         VALUES (?, 'Review epic', '', 'open', 1, 'epic', 'ws-irc', \
                 '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

fn new_privmsg(epic_id: &str, text: &str, irc_message_id: &str) -> NewIrcMessage {
    NewIrcMessage {
        epic_id: epic_id.to_string(),
        channel: format!("#ryve:epic:{epic_id}"),
        irc_message_id: irc_message_id.to_string(),
        sender_actor_id: None,
        command: IrcCommand::Privmsg,
        raw_text: text.to_string(),
        structured_event_id: None,
    }
}

#[sqlx::test]
async fn insert_message_assigns_id_and_timestamp(pool: sqlx::SqlitePool) {
    seed_epic(&pool, "sp-irc-epic-1").await;

    let row = irc_repo::insert_message(
        &pool,
        new_privmsg("sp-irc-epic-1", "hello channel", "irc-msg-1"),
    )
    .await
    .unwrap();

    assert!(row.id > 0, "insert should assign a positive id");
    assert!(!row.created_at.is_empty(), "insert should stamp created_at");
    assert_eq!(row.epic_id, "sp-irc-epic-1");
    assert_eq!(row.channel, "#ryve:epic:sp-irc-epic-1");
    assert_eq!(row.irc_message_id, "irc-msg-1");
    assert_eq!(row.command, "PRIVMSG");
    assert_eq!(row.raw_text, "hello channel");
    assert!(row.sender_actor_id.is_none());
    assert!(row.structured_event_id.is_none());
}

#[sqlx::test]
async fn list_by_epic_paginates_in_chronological_order(pool: sqlx::SqlitePool) {
    seed_epic(&pool, "sp-irc-epic-2").await;

    // Insert five messages with strictly increasing `created_at` so the
    // `since`-based pagination cursor is unambiguous.
    let mut inserted = Vec::new();
    for i in 0..5 {
        let row = irc_repo::insert_message(
            &pool,
            new_privmsg(
                "sp-irc-epic-2",
                &format!("msg {i}"),
                &format!("irc-msg-{i}"),
            ),
        )
        .await
        .unwrap();
        inserted.push(row);
        // Ensure RFC-3339 timestamps differ; chrono's to_rfc3339 is
        // microsecond-resolution on macOS but tokio's scheduler can fire
        // this loop inside the same microsecond, so sleep briefly.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // First page of 2.
    let page_1 = irc_repo::list_by_epic(&pool, "sp-irc-epic-2", None, 2)
        .await
        .unwrap();
    assert_eq!(page_1.len(), 2);
    assert_eq!(page_1[0].raw_text, "msg 0");
    assert_eq!(page_1[1].raw_text, "msg 1");

    // Second page, cursor after page_1.
    let cursor = page_1.last().unwrap().created_at.clone();
    let page_2 = irc_repo::list_by_epic(&pool, "sp-irc-epic-2", Some(&cursor), 2)
        .await
        .unwrap();
    assert_eq!(page_2.len(), 2);
    assert_eq!(page_2[0].raw_text, "msg 2");
    assert_eq!(page_2[1].raw_text, "msg 3");

    // Cursor past the last row returns an empty page.
    let tail_cursor = inserted.last().unwrap().created_at.clone();
    let tail = irc_repo::list_by_epic(&pool, "sp-irc-epic-2", Some(&tail_cursor), 10)
        .await
        .unwrap();
    assert!(tail.is_empty());
}

#[sqlx::test]
async fn list_by_epic_scopes_results_to_epic(pool: sqlx::SqlitePool) {
    seed_epic(&pool, "sp-irc-epic-3").await;
    seed_epic(&pool, "sp-irc-epic-4").await;

    irc_repo::insert_message(&pool, new_privmsg("sp-irc-epic-3", "in epic 3", "a"))
        .await
        .unwrap();
    irc_repo::insert_message(&pool, new_privmsg("sp-irc-epic-4", "in epic 4", "b"))
        .await
        .unwrap();

    let rows = irc_repo::list_by_epic(&pool, "sp-irc-epic-3", None, 100)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_text, "in epic 3");
}

#[sqlx::test]
async fn search_text_matches_on_raw_text_via_fts(pool: sqlx::SqlitePool) {
    seed_epic(&pool, "sp-irc-epic-5").await;

    irc_repo::insert_message(
        &pool,
        new_privmsg("sp-irc-epic-5", "reviewer approved the merge", "m1"),
    )
    .await
    .unwrap();
    irc_repo::insert_message(
        &pool,
        new_privmsg("sp-irc-epic-5", "build passed on CI", "m2"),
    )
    .await
    .unwrap();
    irc_repo::insert_message(
        &pool,
        new_privmsg("sp-irc-epic-5", "review requested by head", "m3"),
    )
    .await
    .unwrap();

    // Exact-term match, case-insensitive courtesy of the unicode61
    // tokenizer (Postgres `simple`-equivalent per the migration note).
    let hits = irc_repo::search_text(&pool, "sp-irc-epic-5", "APPROVED", 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].raw_text, "reviewer approved the merge");

    // Stem-prefix match via FTS5 `review*` — should match both "reviewer"
    // and "review".
    let stem = irc_repo::search_text(&pool, "sp-irc-epic-5", "review*", 10)
        .await
        .unwrap();
    assert_eq!(stem.len(), 2);
    let texts: std::collections::HashSet<_> = stem.iter().map(|m| m.raw_text.clone()).collect();
    assert!(texts.contains("reviewer approved the merge"));
    assert!(texts.contains("review requested by head"));

    // Token that appears nowhere returns no matches.
    let none = irc_repo::search_text(&pool, "sp-irc-epic-5", "kubernetes", 10)
        .await
        .unwrap();
    assert!(none.is_empty());
}

#[sqlx::test]
async fn search_text_is_scoped_to_the_requested_epic(pool: sqlx::SqlitePool) {
    seed_epic(&pool, "sp-irc-epic-6").await;
    seed_epic(&pool, "sp-irc-epic-7").await;

    irc_repo::insert_message(&pool, new_privmsg("sp-irc-epic-6", "migration passed", "a"))
        .await
        .unwrap();
    irc_repo::insert_message(&pool, new_privmsg("sp-irc-epic-7", "migration passed", "b"))
        .await
        .unwrap();

    let hits = irc_repo::search_text(&pool, "sp-irc-epic-6", "migration", 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].epic_id, "sp-irc-epic-6");
}

#[sqlx::test]
async fn insert_rejects_invalid_command(pool: sqlx::SqlitePool) {
    // CHECK constraint in migration 019 restricts `command` to
    // PRIVMSG / NOTICE / TOPIC. A hand-crafted INSERT bypassing the
    // typed repo must fail on the constraint — guards against a
    // future migration drifting away from the spec.
    seed_epic(&pool, "sp-irc-epic-8").await;
    let result = sqlx::query(
        "INSERT INTO irc_messages \
         (epic_id, channel, irc_message_id, command, raw_text, created_at) \
         VALUES ('sp-irc-epic-8', '#x', 'irc-msg-bad', 'JOIN', 'hi', \
                 '2026-04-17T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    assert!(result.is_err(), "JOIN must fail the command CHECK");
}
