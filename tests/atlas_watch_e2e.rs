// SPDX-License-Identifier: AGPL-3.0-or-later

//! Atlas watch hook — restart-durability end-to-end test
//! (spark ryve-638c69fd [sp-ee3f5c74]).
//!
//! Atlas is the primary consumer of the watch primitive. This test pins
//! the three guarantees Atlas depends on when it delegates long-running
//! coordination (PR open → merge, after-merge rebase, release edits):
//!
//! 1. A watch with a 2s interval and an `UntilSparkStatus` stop-condition
//!    fires exactly once per scheduled slot when ticked.
//! 2. Dropping the runner and the `SqlitePool` (process-restart proxy)
//!    and reopening against the same on-disk sqlite file preserves the
//!    watch — status is still `active`, `next_fire_at` / `last_fired_at`
//!    survive, and no duplicate `WatchFired` event lands for a slot that
//!    already fired pre-restart.
//! 3. After restart, flipping the target spark to the stop status causes
//!    the next tick to emit exactly one final fire (with
//!    `stop_condition_satisfied = true`) and transitions the watch to
//!    `completed` — subsequent ticks fire nothing.
//!
//! These are the invariants an Atlas reaction loop reads on every wake.
//! If any of them break, Atlas can silently duplicate coordination steps
//! (double-merge a PR, double-tag a release) or miss a terminator.

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
        "ryve-atlas-watch-{tag}-{nanos}-{}",
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

async fn fetch_payloads(pool: &SqlitePool, watch_id: &str) -> Vec<watch_runner::WatchFiredPayload> {
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

/// Distinct scheduled-slot timestamps across every `WatchFired` row for a
/// watch. The runner's central invariant — "exactly once per
/// `(watch_id, scheduled_fire_at)`" — means this must equal the total row
/// count, even across a simulated restart.
async fn distinct_slots(pool: &SqlitePool, watch_id: &str) -> Vec<String> {
    let mut slots: Vec<String> = fetch_payloads(pool, watch_id)
        .await
        .into_iter()
        .map(|p| p.scheduled_fire_at)
        .collect();
    slots.sort();
    slots.dedup();
    slots
}

/// End-to-end restart-durability test. A 2-second interval watch with an
/// `UntilSparkStatus` stop condition must survive a process restart
/// without losing state, without double-firing the same slot, and must
/// still transition to `completed` when the target spark reaches the
/// stop status.
///
/// The test drives `watch_runner::tick` directly so wall-clock time is
/// deterministic — the tokio-timer wrapper in `src/watch_runner.rs` is
/// exercised by `src/app.rs` lifecycle tests and is intentionally not
/// the subject here.
#[tokio::test]
async fn atlas_watch_survives_restart_and_completes_on_target_status() {
    let root = fresh_workshop_root("restart-durability");

    // ── Seed the target spark. `spark_repo::create` rejects non-epic
    // sparks without a parent, so use an epic — the stop-condition
    // evaluator only cares about the `status` column, not the type.
    let workshop_id = "atlas-test-ws".to_string();
    let (target_spark_id, first_watch_id, t0, t2) = {
        let pool = open_pool(&root).await;

        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "release/0.1.0 follow-through".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 1,
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

        // Pick a fixed starting instant so slot math is deterministic.
        // Every slot is `t0 + k * 2s` for k ≥ 0.
        let t0: DateTime<Utc> = "2026-04-17T09:00:00+00:00".parse().unwrap();
        let watch = watch_repo::create(
            &pool,
            NewWatch {
                target_spark_id: spark.id.clone(),
                cadence: WatchCadence::Interval { secs: 2 },
                stop_condition: Some(WatchStopCondition::UntilSparkStatus {
                    spark_id: spark.id.clone(),
                    status: "closed".into(),
                }),
                intent_label: "release-monitor".into(),
                next_fire_at: rfc(t0),
                created_by: Some("atlas".into()),
            },
        )
        .await
        .unwrap();

        // Session 1: tick the runner at slot 0 — exactly one fire, still
        // active because the target is still `open`.
        let out = watch_runner::tick(&pool, t0).await.unwrap();
        assert_eq!(out.fired, 1, "tick at t0 must fire the watch exactly once");
        assert_eq!(out.completed, 0, "target still open — watch stays active");
        assert_eq!(count_watch_fired_events(&pool, &watch.id).await, 1);

        // A second tick at the same instant must be a no-op: `next_fire_at`
        // advanced inside the firing transaction past `t0`. This guards
        // the exactly-once invariant *within* a session.
        let out = watch_runner::tick(&pool, t0).await.unwrap();
        assert_eq!(
            out.fired, 0,
            "re-ticking the same instant must not double-fire"
        );
        assert_eq!(count_watch_fired_events(&pool, &watch.id).await, 1);

        let reloaded = watch_repo::get(&pool, &watch.id).await.unwrap();
        assert_eq!(reloaded.status, "active");
        assert_eq!(
            reloaded.last_fired_at.as_deref(),
            Some(rfc(t0).as_str()),
            "last_fired_at must persist the slot we just handled"
        );
        let expected_next = t0 + chrono::Duration::seconds(2);
        let stored_next: DateTime<Utc> = reloaded.next_fire_at.parse().unwrap();
        assert_eq!(
            stored_next, expected_next,
            "next_fire_at must advance exactly one 2s step past the fired slot"
        );

        let t2_local = t0 + chrono::Duration::seconds(2);

        // Simulate process exit — close the pool so the on-disk file is
        // flushed and any in-memory runner state is dropped. This is the
        // crash boundary every restart-durability guarantee is measured
        // against.
        pool.close().await;

        (spark.id, watch.id, t0, t2_local)
    };

    // ── Session 2: reopen the same on-disk DB. A "fresh" runner has no
    // knowledge of what fired pre-restart; only the watch row can tell
    // it. Ticking at the same t0 must stay a no-op (same slot already
    // handled) and ticking at t2 must fire exactly once (next slot).
    let pool = open_pool(&root).await;

    // Watch row survived and is still active with the pre-restart
    // fingerprint — this is the "watch survives" assertion.
    let reloaded = watch_repo::get(&pool, &first_watch_id).await.unwrap();
    assert_eq!(
        reloaded.status, "active",
        "watch must survive restart active"
    );
    assert_eq!(
        reloaded.last_fired_at.as_deref(),
        Some(rfc(t0).as_str()),
        "last_fired_at must survive restart"
    );
    let stored_next: DateTime<Utc> = reloaded.next_fire_at.parse().unwrap();
    assert_eq!(
        stored_next,
        t0 + chrono::Duration::seconds(2),
        "next_fire_at must survive restart"
    );

    // Re-ticking at the already-handled slot post-restart must not
    // re-fire — the durable `next_fire_at` is the dedup key.
    let out = watch_runner::tick(&pool, t0).await.unwrap();
    assert_eq!(
        out.fired, 0,
        "restart must not re-fire a slot that already fired pre-restart"
    );
    assert_eq!(count_watch_fired_events(&pool, &first_watch_id).await, 1);

    // Advance to slot 1 (t0 + 2s). One fire, still active.
    let out = watch_runner::tick(&pool, t2).await.unwrap();
    assert_eq!(out.fired, 1, "fresh slot at t0+2s must fire");
    assert_eq!(out.completed, 0);
    assert_eq!(count_watch_fired_events(&pool, &first_watch_id).await, 2);

    // "Exactly one fire per slot across the restart boundary" — every
    // `scheduled_fire_at` in the outbox is distinct.
    let slots = distinct_slots(&pool, &first_watch_id).await;
    assert_eq!(
        slots.len() as i64,
        count_watch_fired_events(&pool, &first_watch_id).await,
        "each WatchFired row must correspond to a unique scheduled slot"
    );
    assert_eq!(slots, vec![rfc(t0), rfc(t2)]);

    // ── Flip the target spark to `closed`. The next tick must fire once
    // more (with `stop_condition_satisfied = true`) and transition the
    // watch to `completed` in the same transaction.
    spark_repo::update(
        &pool,
        &target_spark_id,
        UpdateSpark {
            status: Some(SparkStatus::Closed),
            ..Default::default()
        },
        "atlas-test",
    )
    .await
    .unwrap();

    let t4 = t0 + chrono::Duration::seconds(4);
    let out = watch_runner::tick(&pool, t4).await.unwrap();
    assert_eq!(
        out.fired, 1,
        "tick after target closed must fire once with stop_condition_satisfied"
    );
    assert_eq!(out.completed, 1, "watch must transition to completed");

    let final_watch = watch_repo::get(&pool, &first_watch_id).await.unwrap();
    assert_eq!(
        final_watch.status, "completed",
        "watch must be completed once the target reached the stop status"
    );

    // The final payload carries the stop-satisfied flag — Atlas reads
    // this to know it does not need to cancel the watch itself.
    let payloads = fetch_payloads(&pool, &first_watch_id).await;
    assert_eq!(payloads.len(), 3, "three fires total: t0, t2, t4");
    assert!(!payloads[0].stop_condition_satisfied);
    assert!(!payloads[1].stop_condition_satisfied);
    assert!(
        payloads[2].stop_condition_satisfied,
        "the final fire must report the stop condition as satisfied"
    );

    // Exactly-once across the full lifecycle: three slots, three rows.
    let slots = distinct_slots(&pool, &first_watch_id).await;
    assert_eq!(
        slots,
        vec![rfc(t0), rfc(t2), rfc(t4)],
        "every fired slot must be unique across the restart boundary + completion"
    );

    // Any further ticks are no-ops — a completed watch is outside
    // `due_at`, so the scheduler emits nothing.
    let t_far = t4 + chrono::Duration::seconds(60);
    let out = watch_runner::tick(&pool, t_far).await.unwrap();
    assert_eq!(out.fired, 0);
    assert_eq!(count_watch_fired_events(&pool, &first_watch_id).await, 3);

    pool.close().await;
}
