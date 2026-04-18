// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Integration tests for the heartbeat watchdog.
//!
//! Parent epic `ryve-cf05fd85`, spark `ryve-fe4e03d3`: the watchdog must
//! transition active assignments Healthy -> AtRisk -> Stuck as the time
//! since their last heartbeat crosses each threshold, emit a canonical
//! outbox event on each edge, and stay Healthy for as long as heartbeats
//! keep arriving.
//!
//! These tests simulate wall-clock time by passing explicit
//! `DateTime<Utc>` values into [`tick`]. The production [`run`] loop
//! uses [`Utc::now`], but every decision in the watchdog is parameterised
//! by `now`, so a fake clock here is just a series of `tick` calls at
//! advancing instants.

use std::time::Duration;

use chrono::{DateTime, Utc};
use data::sparks::heartbeat_watchdog::{
    LIVENESS_TRANSITIONED_EVENT_TYPE, LivenessTransitionedPayload, WATCHDOG_ACTOR, WatchdogConfig,
    tick,
};
use data::sparks::types::AssignmentLiveness;
use sqlx::SqlitePool;

const WS: &str = "ws-watchdog";

async fn seed_spark(pool: &SqlitePool, spark_id: &str) {
    sqlx::query(
        "INSERT INTO sparks ( \
             id, title, description, status, priority, spark_type, \
             workshop_id, metadata, created_at, updated_at \
         ) VALUES (?, 'test', '', 'in_progress', 2, 'task', ?, '{}', ?, ?)",
    )
    .bind(spark_id)
    .bind(WS)
    .bind("2026-04-17T10:00:00+00:00")
    .bind("2026-04-17T10:00:00+00:00")
    .execute(pool)
    .await
    .unwrap();
}

/// Insert an `active` assignment directly — bypassing the repo's
/// `record_heartbeat` path so the test can pin exactly what
/// `last_heartbeat_at` the watchdog will read.
async fn seed_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
    spark_id: &str,
    actor_id: &str,
    assigned_at: &str,
    last_heartbeat_at: &str,
) {
    sqlx::query(
        "INSERT INTO assignments ( \
             assignment_id, spark_id, actor_id, assignment_phase, \
             event_version, created_at, updated_at, \
             session_id, status, role, assigned_at, last_heartbeat_at, \
             repair_cycle_count, liveness \
         ) VALUES (?, ?, ?, 'in_progress', 0, ?, ?, ?, 'active', 'owner', ?, ?, 0, 'healthy')",
    )
    .bind(assignment_id)
    .bind(spark_id)
    .bind(actor_id)
    .bind(assigned_at)
    .bind(last_heartbeat_at)
    .bind(actor_id)
    .bind(assigned_at)
    .bind(last_heartbeat_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn load_liveness(pool: &SqlitePool, assignment_id: &str) -> AssignmentLiveness {
    let s: String = sqlx::query_scalar("SELECT liveness FROM assignments WHERE assignment_id = ?")
        .bind(assignment_id)
        .fetch_one(pool)
        .await
        .unwrap();
    AssignmentLiveness::from_str(&s).unwrap_or_else(|| panic!("unknown liveness {s:?}"))
}

async fn outbox_rows_for(pool: &SqlitePool, assignment_id: &str) -> Vec<(String, String)> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT event_type, payload FROM event_outbox \
         WHERE assignment_id = ? ORDER BY timestamp ASC, event_id ASC",
    )
    .bind(assignment_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn bump_heartbeat(pool: &SqlitePool, assignment_id: &str, when: &str) {
    sqlx::query("UPDATE assignments SET last_heartbeat_at = ? WHERE assignment_id = ?")
        .bind(when)
        .bind(assignment_id)
        .execute(pool)
        .await
        .unwrap();
}

fn fake_config() -> WatchdogConfig {
    // 30s / 300s matches the parent epic's defaults. Uses the strict
    // ordering required by `WatchdogConfig::new`.
    WatchdogConfig::new(30, 300, Duration::from_millis(10)).unwrap()
}

fn parse(ts: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(ts)
        .unwrap()
        .with_timezone(&Utc)
}

#[sqlx::test]
async fn heartbeat_keeps_assignment_healthy(pool: SqlitePool) {
    // Hand emits a heartbeat every 30s under the default config. Each
    // tick advances the fake clock by 29s and stamps a fresh
    // last_heartbeat_at — age stays < 2*30s so the watchdog never moves
    // the assignment off Healthy.
    seed_spark(&pool, "ryve-alpha").await;
    seed_assignment(
        &pool,
        "asgn-healthy",
        "ryve-alpha",
        "hand-1",
        "2026-04-17T12:00:00+00:00",
        "2026-04-17T12:00:00+00:00",
    )
    .await;

    let config = fake_config();

    for i in 1..=20 {
        let beat_ts = format!(
            "2026-04-17T12:{:02}:{:02}+00:00",
            (i * 29 / 60),
            (i * 29) % 60
        );
        bump_heartbeat(&pool, "asgn-healthy", &beat_ts).await;
        let now = parse(&beat_ts) + chrono::Duration::seconds(1);
        let outcome = tick(&pool, now, &config).await.unwrap();
        assert_eq!(outcome.scanned, 1, "one active assignment must be scanned");
        assert_eq!(
            outcome.transitioned, 0,
            "healthy heartbeat must not trigger a transition (iter {i})"
        );
    }

    assert_eq!(
        load_liveness(&pool, "asgn-healthy").await,
        AssignmentLiveness::Healthy
    );

    let rows = outbox_rows_for(&pool, "asgn-healthy").await;
    assert!(
        rows.is_empty(),
        "no outbox events expected for a healthy heartbeat stream, got {rows:?}"
    );
}

#[sqlx::test]
async fn stopped_heartbeat_walks_healthy_at_risk_stuck(pool: SqlitePool) {
    // Hand stops beating at t=12:00:00. Under the default 30s/300s
    // config the watchdog must:
    //   - stay Healthy while age <= 60s,
    //   - transition to AtRisk once age > 60s,
    //   - transition to Stuck once age > 300s.
    // Each transition writes a canonical LivenessTransitioned row to
    // event_outbox so the existing relay can deliver it to IRC.
    seed_spark(&pool, "ryve-silent").await;
    seed_assignment(
        &pool,
        "asgn-silent",
        "ryve-silent",
        "hand-silent",
        "2026-04-17T11:59:00+00:00",
        "2026-04-17T12:00:00+00:00",
    )
    .await;

    let config = fake_config();

    // Age = 30s — still Healthy.
    let outcome = tick(&pool, parse("2026-04-17T12:00:30+00:00"), &config)
        .await
        .unwrap();
    assert_eq!(outcome.transitioned, 0);
    assert_eq!(
        load_liveness(&pool, "asgn-silent").await,
        AssignmentLiveness::Healthy
    );

    // Age = 61s — crosses the Healthy -> AtRisk boundary.
    let outcome = tick(&pool, parse("2026-04-17T12:01:01+00:00"), &config)
        .await
        .unwrap();
    assert_eq!(outcome.transitioned, 1);
    assert_eq!(outcome.became_at_risk, 1);
    assert_eq!(outcome.became_stuck, 0);
    assert_eq!(
        load_liveness(&pool, "asgn-silent").await,
        AssignmentLiveness::AtRisk
    );

    // Another tick at age 120s — already AtRisk, not Stuck yet, so no
    // new transition should fire (watchdog is idempotent).
    let outcome = tick(&pool, parse("2026-04-17T12:02:00+00:00"), &config)
        .await
        .unwrap();
    assert_eq!(
        outcome.transitioned, 0,
        "AtRisk must not be re-emitted while still AtRisk"
    );

    // Age = 301s — crosses the AtRisk -> Stuck boundary.
    let outcome = tick(&pool, parse("2026-04-17T12:05:01+00:00"), &config)
        .await
        .unwrap();
    assert_eq!(outcome.transitioned, 1);
    assert_eq!(outcome.became_stuck, 1);
    assert_eq!(outcome.became_at_risk, 0);
    assert_eq!(
        load_liveness(&pool, "asgn-silent").await,
        AssignmentLiveness::Stuck
    );

    // Stuck is sticky — one more tick with no new heartbeat does not
    // re-emit.
    let outcome = tick(&pool, parse("2026-04-17T12:06:00+00:00"), &config)
        .await
        .unwrap();
    assert_eq!(outcome.transitioned, 0);

    // Outbox must hold exactly the two transitions, in order, each with
    // actor = watchdog and event_type = LivenessTransitioned.
    let rows = outbox_rows_for(&pool, "asgn-silent").await;
    assert_eq!(
        rows.len(),
        2,
        "expected two liveness outbox rows (AtRisk + Stuck), got {rows:?}"
    );
    for (event_type, _) in &rows {
        assert_eq!(event_type, LIVENESS_TRANSITIONED_EVENT_TYPE);
    }

    let first: LivenessTransitionedPayload = serde_json::from_str(&rows[0].1).unwrap();
    assert_eq!(first.from_liveness, AssignmentLiveness::Healthy);
    assert_eq!(first.to_liveness, AssignmentLiveness::AtRisk);
    assert_eq!(first.spark_id, "ryve-silent");
    assert_eq!(first.assignment_id, "asgn-silent");
    assert!(first.age_secs > 60);

    let second: LivenessTransitionedPayload = serde_json::from_str(&rows[1].1).unwrap();
    assert_eq!(second.from_liveness, AssignmentLiveness::AtRisk);
    assert_eq!(second.to_liveness, AssignmentLiveness::Stuck);
    assert!(second.age_secs > 300);

    // actor_id on each outbox row is the watchdog's stable id so the
    // IRC subscriber can filter watchdog traffic end-to-end.
    let actors: Vec<String> = sqlx::query_scalar(
        "SELECT actor_id FROM event_outbox WHERE assignment_id = ? ORDER BY timestamp ASC",
    )
    .bind("asgn-silent")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(actors.iter().all(|a| a == WATCHDOG_ACTOR));
}

#[sqlx::test]
async fn non_active_assignments_are_ignored(pool: SqlitePool) {
    // A completed assignment must never be transitioned by the watchdog
    // — its liveness stays whatever it was at completion time. We seed
    // two rows with identical stale heartbeats; only the active one is
    // allowed to move.
    seed_spark(&pool, "ryve-live").await;
    seed_spark(&pool, "ryve-done").await;

    seed_assignment(
        &pool,
        "asgn-live",
        "ryve-live",
        "hand-live",
        "2026-04-17T11:00:00+00:00",
        "2026-04-17T11:00:00+00:00",
    )
    .await;
    seed_assignment(
        &pool,
        "asgn-done",
        "ryve-done",
        "hand-done",
        "2026-04-17T11:00:00+00:00",
        "2026-04-17T11:00:00+00:00",
    )
    .await;
    sqlx::query(
        "UPDATE assignments SET status = 'completed', completed_at = ? \
         WHERE assignment_id = ?",
    )
    .bind("2026-04-17T11:30:00+00:00")
    .bind("asgn-done")
    .execute(&pool)
    .await
    .unwrap();

    let config = fake_config();
    let outcome = tick(&pool, parse("2026-04-17T12:10:00+00:00"), &config)
        .await
        .unwrap();
    // Only the active assignment is scanned; the completed one is
    // filtered out by status='active'.
    assert_eq!(outcome.scanned, 1);
    assert_eq!(outcome.transitioned, 1);

    assert_eq!(
        load_liveness(&pool, "asgn-live").await,
        AssignmentLiveness::Stuck
    );
    assert_eq!(
        load_liveness(&pool, "asgn-done").await,
        AssignmentLiveness::Healthy,
        "completed assignment must keep whatever liveness it had"
    );

    let done_rows = outbox_rows_for(&pool, "asgn-done").await;
    assert!(
        done_rows.is_empty(),
        "watchdog must never emit events for completed assignments"
    );
}
