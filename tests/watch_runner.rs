// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration test for the durable, restart-safe watch scheduler
//! ([sp-ee3f5c74], spark ryve-6ab1980c).
//!
//! Exactly-once firing per `(watch_id, scheduled_fire_at)` slot is the
//! central invariant: a single tick emits exactly one `WatchFired`
//! outbox event per due watch, and dropping the runner and reopening the
//! same sqlite DB does **not** re-fire the same slot. These tests pin
//! those guarantees against a real sqlx pool (no mocks).
//!
//! The runner's transactional tick lives in
//! `data::sparks::watch_runner::tick`, so the tests drive it directly
//! — side-stepping the tokio timer in `src/watch_runner.rs` to advance
//! wall-clock time deterministically.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use data::sparks::types::{
    NewSpark, NewWatch, SparkStatus, SparkType, UpdateSpark, WatchCadence, WatchStopCondition,
};
use data::sparks::{spark_repo, watch_repo, watch_runner};
use sqlx::SqlitePool;

fn fresh_workshop_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ryve-watch-runner-{tag}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create tempdir");
    root
}

async fn open_pool(root: &Path) -> SqlitePool {
    data::db::open_sparks_db(root)
        .await
        .expect("open sparks db")
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

async fn count_watch_fired_events(pool: &SqlitePool, watch_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM event_outbox \
           WHERE event_type = 'WatchFired' AND assignment_id = ?",
    )
    .bind(watch_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn fetch_payload(pool: &SqlitePool, watch_id: &str) -> Vec<watch_runner::WatchFiredPayload> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox \
           WHERE event_type = 'WatchFired' AND assignment_id = ? \
           ORDER BY timestamp ASC",
    )
    .bind(watch_id)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|s| serde_json::from_str(&s).unwrap())
        .collect()
}

/// A watch with a 1-second interval fires exactly one `WatchFired` event
/// per slot, and a second tick at the same wall-clock instant does not
/// re-fire — next_fire_at has already advanced past `now`.
#[tokio::test]
async fn tick_fires_exactly_once_per_slot() {
    let root = fresh_workshop_root("single-slot");
    let pool = open_pool(&root).await;

    let t0: DateTime<Utc> = "2026-04-17T12:00:00+00:00".parse().unwrap();
    let w = watch_repo::create(
        &pool,
        NewWatch {
            target_spark_id: "ryve-target".into(),
            cadence: WatchCadence::Interval { secs: 1 },
            stop_condition: None,
            intent_label: "tick-test".into(),
            next_fire_at: rfc(t0),
            created_by: Some("test".into()),
        },
    )
    .await
    .unwrap();

    // First tick at t0: watch is due, one event emitted.
    let out = watch_runner::tick(&pool, t0).await.unwrap();
    assert_eq!(out.fired, 1);
    assert_eq!(out.completed, 0);
    assert_eq!(out.skipped, 0);
    assert_eq!(count_watch_fired_events(&pool, &w.id).await, 1);

    // Second tick at the same instant: next_fire_at is now > t0, so the
    // watch is not in the due set and no new event fires.
    let out = watch_runner::tick(&pool, t0).await.unwrap();
    assert_eq!(out.fired, 0);
    assert_eq!(count_watch_fired_events(&pool, &w.id).await, 1);

    // Payload carries the slot we fired for — the deduplication key.
    let payloads = fetch_payload(&pool, &w.id).await;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].watch_id, w.id);
    assert_eq!(payloads[0].scheduled_fire_at, rfc(t0));
    assert!(!payloads[0].stop_condition_satisfied);
}

/// Simulate a process restart: drop the pool, reopen against the same
/// sqlite file, tick again at the same wall-clock instant. No duplicate
/// event may fire for the slot that was already handled — `next_fire_at`
/// advanced inside the firing transaction, so the invariant holds across
/// process boundaries.
#[tokio::test]
async fn restart_does_not_double_fire_same_slot() {
    let root = fresh_workshop_root("restart");

    let t0: DateTime<Utc> = "2026-04-17T13:00:00+00:00".parse().unwrap();
    let watch_id: String;

    // Session 1: create watch, tick once, then drop the pool (simulated
    // process exit).
    {
        let pool = open_pool(&root).await;
        let w = watch_repo::create(
            &pool,
            NewWatch {
                target_spark_id: "ryve-restart".into(),
                cadence: WatchCadence::Interval { secs: 1 },
                stop_condition: None,
                intent_label: "restart-test".into(),
                next_fire_at: rfc(t0),
                created_by: Some("test".into()),
            },
        )
        .await
        .unwrap();
        watch_id = w.id.clone();

        let out = watch_runner::tick(&pool, t0).await.unwrap();
        assert_eq!(out.fired, 1);
        pool.close().await;
    }

    // Session 2: reopen the same DB — a fresh `watch_runner` with no
    // in-memory state. Ticking at the same `t0` must not double-fire.
    {
        let pool = open_pool(&root).await;
        let out = watch_runner::tick(&pool, t0).await.unwrap();
        assert_eq!(out.fired, 0);
        assert_eq!(count_watch_fired_events(&pool, &watch_id).await, 1);

        // Advancing wall-clock past the next slot fires exactly one more
        // event — cadence resumes after restart.
        let t2 = t0 + chrono::Duration::seconds(2);
        let out = watch_runner::tick(&pool, t2).await.unwrap();
        assert_eq!(out.fired, 1);
        assert_eq!(count_watch_fired_events(&pool, &watch_id).await, 2);
        pool.close().await;
    }
}

/// Missed-ticks invariant: a watch whose cadence slot fell 100 intervals
/// in the past during "downtime" is fired exactly once on catch-up, not
/// 100 times.
#[tokio::test]
async fn catch_up_fires_at_most_once_per_watch_per_tick() {
    let root = fresh_workshop_root("catch-up");
    let pool = open_pool(&root).await;

    let t0: DateTime<Utc> = "2026-04-17T14:00:00+00:00".parse().unwrap();
    let w = watch_repo::create(
        &pool,
        NewWatch {
            target_spark_id: "ryve-backlog".into(),
            cadence: WatchCadence::Interval { secs: 1 },
            stop_condition: None,
            intent_label: "catch-up".into(),
            next_fire_at: rfc(t0),
            created_by: Some("test".into()),
        },
    )
    .await
    .unwrap();

    // Jump 100 seconds into the future — the watch's next_fire_at is 100
    // slots behind now. A single tick must collapse the backlog to one
    // event and advance next_fire_at strictly past now.
    let now = t0 + chrono::Duration::seconds(100);
    let out = watch_runner::tick(&pool, now).await.unwrap();
    assert_eq!(out.fired, 1);
    assert_eq!(count_watch_fired_events(&pool, &w.id).await, 1);

    // Second tick at the same instant: no further events.
    let out = watch_runner::tick(&pool, now).await.unwrap();
    assert_eq!(out.fired, 0);
    assert_eq!(count_watch_fired_events(&pool, &w.id).await, 1);

    // The refreshed row's next_fire_at is strictly greater than `now`.
    let reloaded = watch_repo::get(&pool, &w.id).await.unwrap();
    let next: DateTime<Utc> = reloaded.next_fire_at.parse().unwrap();
    assert!(next > now, "expected {next} > {now}");
}

/// `UntilSparkStatus` stop condition: when the target spark reaches the
/// configured status, the runner fires one final `WatchFired` event (with
/// `stop_condition_satisfied = true`) and transitions the watch to
/// `completed`. Future ticks emit no further events.
#[tokio::test]
async fn until_spark_status_transitions_watch_to_completed() {
    let root = fresh_workshop_root("stop-cond");
    let pool = open_pool(&root).await;

    // `spark_repo::create` rejects non-epic sparks without a parent, so
    // seed an epic here — the concrete spark_type is irrelevant to the
    // stop-condition check, which only compares the `status` column.
    let workshop_id = "test-ws".to_string();
    let spark = spark_repo::create(
        &pool,
        NewSpark {
            title: "target".into(),
            description: String::new(),
            spark_type: SparkType::Epic,
            priority: 2,
            workshop_id: workshop_id.clone(),
            assignee: None,
            owner: None,
            parent_id: None,
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        },
    )
    .await
    .unwrap();

    let t0: DateTime<Utc> = "2026-04-17T15:00:00+00:00".parse().unwrap();
    let w = watch_repo::create(
        &pool,
        NewWatch {
            target_spark_id: spark.id.clone(),
            cadence: WatchCadence::Interval { secs: 1 },
            stop_condition: Some(WatchStopCondition::UntilSparkStatus {
                spark_id: spark.id.clone(),
                status: "closed".into(),
            }),
            intent_label: "until-closed".into(),
            next_fire_at: rfc(t0),
            created_by: Some("test".into()),
        },
    )
    .await
    .unwrap();

    // Spark is still open: watch fires normally and remains active.
    let out = watch_runner::tick(&pool, t0).await.unwrap();
    assert_eq!(out.fired, 1);
    assert_eq!(out.completed, 0);
    assert_eq!(
        watch_repo::get(&pool, &w.id).await.unwrap().status,
        "active"
    );

    // Close the target spark; the next tick observes the satisfied stop
    // condition, emits one final event, and transitions to completed.
    spark_repo::update(
        &pool,
        &spark.id,
        UpdateSpark {
            status: Some(SparkStatus::Closed),
            ..Default::default()
        },
        "test",
    )
    .await
    .unwrap();

    let t2 = t0 + chrono::Duration::seconds(2);
    let out = watch_runner::tick(&pool, t2).await.unwrap();
    assert_eq!(out.fired, 1);
    assert_eq!(out.completed, 1);

    let reloaded = watch_repo::get(&pool, &w.id).await.unwrap();
    assert_eq!(reloaded.status, "completed");

    let payloads = fetch_payload(&pool, &w.id).await;
    assert_eq!(payloads.len(), 2);
    assert!(!payloads[0].stop_condition_satisfied);
    assert!(payloads[1].stop_condition_satisfied);

    // Further ticks must emit nothing: `due_at` filters status=active.
    let t3 = t2 + chrono::Duration::seconds(5);
    let out = watch_runner::tick(&pool, t3).await.unwrap();
    assert_eq!(out.fired, 0);
    assert_eq!(count_watch_fired_events(&pool, &w.id).await, 2);
}
