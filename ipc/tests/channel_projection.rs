// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `ipc::channel_projection`.
//!
//! Exercises the five filter axes, FTS, mention override, the
//! empty-state return, and preset CRUD including the unread-count
//! derivation. Uses the real migrations (including `023_projection_presets`)
//! so the suite proves the module against its production schema.

use chrono::{Duration, Utc};
use ipc::channel_projection::{
    self, ChannelProjectionQuery, NewProjectionPreset, PresetFilters, ProjectionError,
    ProjectionOutput,
};
use sqlx::SqlitePool;

const EPIC: &str = "sp-proj-epic-1";
const CHANNEL: &str = "#ryve:epic:sp-proj-epic-1";

/// Seed the parent epic so `irc_messages.epic_id` FK is satisfied.
async fn seed_epic(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO sparks \
         (id, title, description, status, priority, spark_type, workshop_id, created_at, updated_at) \
         VALUES (?, 'Projection Fixture', '', 'open', 1, 'epic', 'ws-proj', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

/// Bundle of outbox + irc_message fields the seed helper needs. Using a
/// struct keeps the helper readable and avoids clippy's
/// `too_many_arguments` lint without suppressing it.
struct Seed<'a> {
    event_id: &'a str,
    event_type: &'a str,
    assignment_id: &'a str,
    actor_id: &'a str,
    payload_extra: serde_json::Value,
    raw_text: &'a str,
    offset_secs: i64,
}

/// Insert a fully structured outbox event + its matching irc_message.
/// Returns the message id so callers can pin assertions to specific rows.
async fn seed_outbox_and_message(pool: &SqlitePool, seed: Seed<'_>) -> i64 {
    let timestamp = (Utc::now() + Duration::seconds(seed.offset_secs)).to_rfc3339();
    let payload = {
        let mut obj = serde_json::Map::new();
        obj.insert("epic_id".into(), serde_json::Value::String(EPIC.into()));
        obj.insert(
            "epic_name".into(),
            serde_json::Value::String("Projection".into()),
        );
        obj.insert(
            "assignment_id".into(),
            serde_json::Value::String(seed.assignment_id.into()),
        );
        if let serde_json::Value::Object(extra) = seed.payload_extra {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
        serde_json::Value::Object(obj)
    };

    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, 1, ?, ?, ?, ?, ?)",
    )
    .bind(seed.event_id)
    .bind(&timestamp)
    .bind(seed.assignment_id)
    .bind(seed.actor_id)
    .bind(seed.event_type)
    .bind(payload.to_string())
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO irc_messages \
         (epic_id, channel, irc_message_id, sender_actor_id, command, raw_text, \
          structured_event_id, created_at) \
         VALUES (?, ?, ?, ?, 'PRIVMSG', ?, ?, ?)",
    )
    .bind(EPIC)
    .bind(CHANNEL)
    .bind(format!("irc-{}", seed.event_id))
    .bind::<Option<&str>>(None)
    .bind(seed.raw_text)
    .bind(seed.event_id)
    .bind(&timestamp)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Insert a plain chat message (no outbox row) so tests can exercise
/// messages that have no structured_event_id.
async fn seed_plain_message(pool: &SqlitePool, raw_text: &str, offset_secs: i64) -> i64 {
    let timestamp = (Utc::now() + Duration::seconds(offset_secs)).to_rfc3339();
    sqlx::query(
        "INSERT INTO irc_messages \
         (epic_id, channel, irc_message_id, sender_actor_id, command, raw_text, \
          structured_event_id, created_at) \
         VALUES (?, ?, ?, NULL, 'PRIVMSG', ?, NULL, ?)",
    )
    .bind(EPIC)
    .bind(CHANNEL)
    .bind(format!("chat-{offset_secs}-{raw_text}"))
    .bind(raw_text)
    .bind(&timestamp)
    .execute(pool)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .unwrap();
    id
}

async fn seed_mixed_fixtures(pool: &SqlitePool) {
    seed_epic(pool, EPIC).await;

    seed_outbox_and_message(
        pool,
        Seed {
            event_id: "evt-1",
            event_type: "assignment.created",
            assignment_id: "asgn-1",
            actor_id: "alice",
            payload_extra: serde_json::json!({ "spark_id": "sp-child-1", "actor": "alice" }),
            raw_text: "[assignment] asgn-1 created for alice",
            offset_secs: 0,
        },
    )
    .await;

    seed_outbox_and_message(
        pool,
        Seed {
            event_id: "evt-2",
            event_type: "assignment.transitioned",
            assignment_id: "asgn-1",
            actor_id: "bob",
            payload_extra: serde_json::json!({
                "spark_id": "sp-child-1",
                "from": "open",
                "to": "in_progress",
                "actor": "bob",
            }),
            raw_text: "[assignment] asgn-1 moved open -> in_progress by bob",
            offset_secs: 1,
        },
    )
    .await;

    seed_outbox_and_message(
        pool,
        Seed {
            event_id: "evt-3",
            event_type: "github.pr.opened",
            assignment_id: "asgn-2",
            actor_id: "alice",
            payload_extra: serde_json::json!({
                "spark_id": "sp-child-2",
                "pr_number": 42,
                "author": "alice",
                "title": "fix: thing",
            }),
            raw_text: "[github] PR #42 opened by alice: fix: thing",
            offset_secs: 2,
        },
    )
    .await;

    seed_outbox_and_message(
        pool,
        Seed {
            event_id: "evt-4",
            event_type: "review.completed",
            assignment_id: "asgn-2",
            actor_id: "carol",
            payload_extra: serde_json::json!({
                "spark_id": "sp-child-2",
                "reviewer": "carol",
                "outcome": "approved",
            }),
            raw_text: "[review] asgn-2 approved by carol",
            offset_secs: 3,
        },
    )
    .await;

    // Plain chat that mentions @alice (no structured event).
    seed_plain_message(pool, "hey @alice can you take a look?", 4).await;
    // Plain chat not mentioning anybody.
    seed_plain_message(pool, "merge build passed on CI", 5).await;
}

// ── Axis 1: epic_id ────────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn epic_id_axis_filters_to_one_channel(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    // Seed a second epic with one message so we can prove cross-epic
    // filtering.
    seed_epic(&pool, "sp-proj-epic-other").await;
    sqlx::query(
        "INSERT INTO irc_messages (epic_id, channel, irc_message_id, command, raw_text, created_at) \
         VALUES ('sp-proj-epic-other', '#other', 'irc-x', 'PRIVMSG', 'unrelated', '2026-04-18T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 6, "scoped to seeded fixtures only");
    for m in &msgs {
        assert_eq!(m.epic_id, EPIC);
    }
}

// ── Axis 2: spark_id ───────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn spark_id_axis_filters_via_payload_json(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        spark_id: Some("sp-child-1".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 2);
    let sparks: Vec<_> = msgs
        .iter()
        .filter_map(|m| m.metadata.as_ref().and_then(|x| x.spark_id.clone()))
        .collect();
    assert!(sparks.iter().all(|s| s == "sp-child-1"));
}

// ── Axis 3: assignment_id ──────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn assignment_id_axis_filters_via_outbox_column(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        assignment_id: Some("asgn-2".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 2);
    for m in &msgs {
        let metadata = m.metadata.as_ref().expect("outbox-sourced row");
        assert_eq!(metadata.assignment_id.as_deref(), Some("asgn-2"));
    }
}

// ── Axis 4: pr_number ──────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn pr_number_axis_filters_via_payload_json(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        pr_number: Some(42),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 1);
    let metadata = msgs[0].metadata.as_ref().expect("pr row has metadata");
    assert_eq!(metadata.pr_number, Some(42));
    assert_eq!(msgs[0].event_type.as_deref(), Some("github.pr.opened"));
}

// ── Axis 5: actor_id ───────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn actor_id_axis_filters_via_outbox_column(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        actor_id: Some("carol".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 1);
    let metadata = msgs[0].metadata.as_ref().unwrap();
    assert_eq!(metadata.actor_id.as_deref(), Some("carol"));
}

// ── FTS ────────────────────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn fts_axis_matches_raw_text_via_virtual_table(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        fts_query: Some("approved".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].raw_text.contains("approved"));
    assert_eq!(msgs[0].event_type.as_deref(), Some("review.completed"));
}

// ── Axes combine (AND) ─────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn axes_and_together_and_exclude_non_matches(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    // alice + asgn-1: only evt-1 (assignment.created for asgn-1 by alice)
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        assignment_id: Some("asgn-1".to_string()),
        actor_id: Some("alice".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].event_type.as_deref(), Some("assignment.created"));
}

// ── Mentions override ─────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn mentions_override_surfaces_addressed_messages_through_filters(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    // Filter to carol's events — on its own this returns one row (the
    // review.completed). With current_actor_id=alice, the @alice
    // plain-chat mention also surfaces, flagged with matched_by_mention.
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        actor_id: Some("carol".to_string()),
        current_actor_id: Some("alice".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    let msgs = out.into_vec();
    assert_eq!(msgs.len(), 2, "carol's event + alice mention");

    let (mention, filter_hit): (Vec<_>, Vec<_>) =
        msgs.into_iter().partition(|m| m.matched_by_mention);
    assert_eq!(mention.len(), 1, "exactly one mention-surfaced row");
    assert!(mention[0].raw_text.contains("@alice"));
    assert_eq!(filter_hit.len(), 1);
    assert_eq!(
        filter_hit[0].event_type.as_deref(),
        Some("review.completed")
    );
}

// ── Empty state ────────────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn empty_state_is_distinct_from_error(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        pr_number: Some(999_999),
        ..ChannelProjectionQuery::default()
    };
    let out = channel_projection::query(&pool, &q).await.unwrap();
    assert!(matches!(out, ProjectionOutput::Empty));
    assert!(out.is_empty());
}

#[sqlx::test(migrations = "../data/migrations")]
async fn invalid_fts_query_is_an_error_not_an_empty_state(pool: SqlitePool) {
    seed_epic(&pool, EPIC).await;
    let q = ChannelProjectionQuery {
        epic_id: Some(EPIC.to_string()),
        fts_query: Some("".to_string()),
        ..ChannelProjectionQuery::default()
    };
    let result = channel_projection::query(&pool, &q).await;
    assert!(matches!(result, Err(ProjectionError::InvalidFtsQuery(_))));
}

// ── Preset persistence ─────────────────────────────────

#[sqlx::test(migrations = "../data/migrations")]
async fn preset_crud_round_trips_filters(pool: SqlitePool) {
    let filters = PresetFilters {
        epic_id: Some(EPIC.into()),
        actor_id: Some("alice".into()),
        pr_number: Some(42),
        ..PresetFilters::default()
    };
    let preset = channel_projection::create_preset(
        &pool,
        NewProjectionPreset {
            workshop_id: "ws-proj".into(),
            channel: CHANNEL.into(),
            name: "my-alice-view".into(),
            filters: filters.clone(),
            last_seen_message_id: None,
        },
    )
    .await
    .unwrap();
    assert!(preset.id > 0);
    assert_eq!(preset.last_seen_message_id, 0);

    // get_preset round-trips.
    let fetched = channel_projection::get_preset(&pool, preset.id)
        .await
        .unwrap()
        .expect("preset exists");
    assert_eq!(fetched.filters, filters);
    assert_eq!(fetched.name, "my-alice-view");

    // list_presets returns it scoped to workshop+channel.
    let listed = channel_projection::list_presets(&pool, "ws-proj", CHANNEL)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, preset.id);

    // Update filters.
    let new_filters = PresetFilters {
        assignment_id: Some("asgn-1".into()),
        ..PresetFilters::default()
    };
    let updated = channel_projection::update_preset_filters(&pool, preset.id, new_filters.clone())
        .await
        .unwrap();
    assert_eq!(updated.filters, new_filters);
    assert!(updated.updated_at >= preset.updated_at);

    // Delete.
    assert!(
        channel_projection::delete_preset(&pool, preset.id)
            .await
            .unwrap()
    );
    let gone = channel_projection::get_preset(&pool, preset.id)
        .await
        .unwrap();
    assert!(gone.is_none());
}

#[sqlx::test(migrations = "../data/migrations")]
async fn preset_unique_per_workshop_channel_name(pool: SqlitePool) {
    let mk = |workshop: &str, channel: &str, name: &str| NewProjectionPreset {
        workshop_id: workshop.into(),
        channel: channel.into(),
        name: name.into(),
        filters: PresetFilters::default(),
        last_seen_message_id: None,
    };
    // First create succeeds.
    channel_projection::create_preset(&pool, mk("ws-a", "#c", "my-view"))
        .await
        .unwrap();
    // Same identity in the same workshop+channel is rejected by the unique constraint.
    let duplicate = channel_projection::create_preset(&pool, mk("ws-a", "#c", "my-view")).await;
    assert!(
        duplicate.is_err(),
        "duplicate should fail the unique constraint"
    );
    // Different channel with the same name is allowed.
    channel_projection::create_preset(&pool, mk("ws-a", "#other", "my-view"))
        .await
        .unwrap();
    // Different workshop with the same name is allowed.
    channel_projection::create_preset(&pool, mk("ws-b", "#c", "my-view"))
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../data/migrations")]
async fn preset_last_seen_is_monotonic(pool: SqlitePool) {
    let preset = channel_projection::create_preset(
        &pool,
        NewProjectionPreset {
            workshop_id: "ws-proj".into(),
            channel: CHANNEL.into(),
            name: "monotonic".into(),
            filters: PresetFilters::default(),
            last_seen_message_id: None,
        },
    )
    .await
    .unwrap();

    channel_projection::bump_last_seen(&pool, preset.id, 100)
        .await
        .unwrap();
    let p = channel_projection::get_preset(&pool, preset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.last_seen_message_id, 100);

    // Stale bump is a no-op (preserves the high-water mark).
    channel_projection::bump_last_seen(&pool, preset.id, 50)
        .await
        .unwrap();
    let p = channel_projection::get_preset(&pool, preset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.last_seen_message_id, 100);

    // Strictly-greater bump succeeds.
    channel_projection::bump_last_seen(&pool, preset.id, 200)
        .await
        .unwrap();
    let p = channel_projection::get_preset(&pool, preset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.last_seen_message_id, 200);
}

#[sqlx::test(migrations = "../data/migrations")]
async fn preset_unread_count_respects_filters_and_last_seen(pool: SqlitePool) {
    seed_mixed_fixtures(&pool).await;

    // Preset filtering to assignment_id=asgn-2 on CHANNEL. That matches
    // evt-3 (PR #42) and evt-4 (review.completed) — two messages.
    let preset = channel_projection::create_preset(
        &pool,
        NewProjectionPreset {
            workshop_id: "ws-proj".into(),
            channel: CHANNEL.into(),
            name: "asgn-2".into(),
            filters: PresetFilters {
                epic_id: Some(EPIC.into()),
                assignment_id: Some("asgn-2".into()),
                ..PresetFilters::default()
            },
            last_seen_message_id: None,
        },
    )
    .await
    .unwrap();

    // With last_seen=0 the unread count is 2.
    let unread = channel_projection::preset_unread_count(&pool, preset.id)
        .await
        .unwrap();
    assert_eq!(unread, 2);

    // Bump past the first matching message. Depends on insertion order:
    // the first asgn-2 message is evt-3. Fetch its id so the test
    // doesn't depend on autoincrement arithmetic across seeds.
    let first_asgn2_id: i64 = sqlx::query_scalar(
        "SELECT MIN(m.id) FROM irc_messages m \
         JOIN event_outbox e ON e.event_id = m.structured_event_id \
         WHERE m.channel = ? AND e.assignment_id = 'asgn-2'",
    )
    .bind(CHANNEL)
    .fetch_one(&pool)
    .await
    .unwrap();
    channel_projection::bump_last_seen(&pool, preset.id, first_asgn2_id)
        .await
        .unwrap();
    let unread = channel_projection::preset_unread_count(&pool, preset.id)
        .await
        .unwrap();
    assert_eq!(unread, 1, "one asgn-2 message remains after bump");
}

#[sqlx::test(migrations = "../data/migrations")]
async fn preset_not_found_yields_structured_error(pool: SqlitePool) {
    let err = channel_projection::preset_unread_count(&pool, 9_999_999)
        .await
        .unwrap_err();
    assert!(matches!(err, ProjectionError::PresetNotFound(9_999_999)));

    let err = channel_projection::update_preset_filters(&pool, 9_999_999, PresetFilters::default())
        .await
        .unwrap_err();
    assert!(matches!(err, ProjectionError::PresetNotFound(9_999_999)));
}
