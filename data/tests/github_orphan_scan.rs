// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for the orphan-detection scanner.
//!
//! Asserts the full contract of [`data::github::run_orphan_scan_with`]:
//!
//! 1. Only phases `≥ AwaitingReview` with a NULL `github_artifact_pr_number`
//!    produce a warning row.
//! 2. Each orphan emits exactly one `github.orphan_assignment_warning`
//!    row per debounce bucket (idempotency).
//! 3. The synthetic dedup key lands in `github_events_seen` so operators
//!    can audit which orphans were paged.
//! 4. Advancing wall clock past the debounce window re-emits the warning.
//!
//! Fulfils spark [sp-73e42cac] — the orphan-detection leaf of epic
//! ryve-73e42cac.

use chrono::{DateTime, TimeZone, Utc};
use data::github::{
    EVT_ORPHAN_ASSIGNMENT_WARNING, GithubEventsSeenRepo, ORPHAN_SCAN_ACTOR, ORPHAN_SCAN_EVENT_TYPE,
    run_orphan_scan_with,
};

const DEBOUNCE: i64 = 60;

async fn seed_base(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, \
         workshop_id, metadata, created_at, updated_at) \
         VALUES ('sp-orphan', 'orphan spark', '', 'open', 2, 'task', 'ws-test', '{}', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed sparks");
}

async fn insert_assignment(
    pool: &sqlx::SqlitePool,
    assignment_id: &str,
    phase: &str,
    pr_number: Option<i64>,
) {
    sqlx::query(
        "INSERT INTO assignments \
         (assignment_id, spark_id, actor_id, session_id, status, role, event_version, \
          assignment_phase, source_branch, target_branch, \
          github_artifact_branch, github_artifact_pr_number, \
          assigned_at, created_at, updated_at) \
         VALUES (?, 'sp-orphan', 'actor-a', 'sess-a', 'active', 'owner', 1, \
                 ?, 'hand/a', 'main', \
                 ?, ?, \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .bind(assignment_id)
    .bind(phase)
    .bind(pr_number.map(|_| "hand/a".to_string()))
    .bind(pr_number)
    .execute(pool)
    .await
    .expect("seed assignment");
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

#[sqlx::test]
async fn scan_warns_only_on_post_review_phases_without_artifact(pool: sqlx::SqlitePool) {
    seed_base(&pool).await;

    // Phases < AwaitingReview — must NOT warn even without an artifact.
    insert_assignment(&pool, "asgn-assigned", "assigned", None).await;
    insert_assignment(&pool, "asgn-inprogress", "in_progress", None).await;
    // Phases >= AwaitingReview without artifact — must warn.
    insert_assignment(&pool, "asgn-awaiting", "awaiting_review", None).await;
    insert_assignment(&pool, "asgn-approved", "approved", None).await;
    insert_assignment(&pool, "asgn-rejected", "rejected", None).await;
    insert_assignment(&pool, "asgn-repair", "in_repair", None).await;
    insert_assignment(&pool, "asgn-rfm", "ready_for_merge", None).await;
    insert_assignment(&pool, "asgn-merged", "merged", None).await;
    // Post-review phase but artifact IS present — must NOT warn.
    insert_assignment(&pool, "asgn-withpr", "awaiting_review", Some(42)).await;

    let now = t("2026-04-18T12:00:00Z");
    let seen = GithubEventsSeenRepo::new();

    let mut tx = pool.begin().await.unwrap();
    let outcome = run_orphan_scan_with(&mut tx, now, DEBOUNCE, &seen)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(outcome.scanned, 6);
    assert_eq!(outcome.warned, 6);
    assert_eq!(outcome.debounced, 0);

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT event_type, assignment_id, actor_id FROM event_outbox \
         ORDER BY assignment_id ASC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let expected: Vec<(&str, &str, &str)> = vec![
        (
            EVT_ORPHAN_ASSIGNMENT_WARNING,
            "asgn-approved",
            ORPHAN_SCAN_ACTOR,
        ),
        (
            EVT_ORPHAN_ASSIGNMENT_WARNING,
            "asgn-awaiting",
            ORPHAN_SCAN_ACTOR,
        ),
        (
            EVT_ORPHAN_ASSIGNMENT_WARNING,
            "asgn-merged",
            ORPHAN_SCAN_ACTOR,
        ),
        (
            EVT_ORPHAN_ASSIGNMENT_WARNING,
            "asgn-rejected",
            ORPHAN_SCAN_ACTOR,
        ),
        (
            EVT_ORPHAN_ASSIGNMENT_WARNING,
            "asgn-repair",
            ORPHAN_SCAN_ACTOR,
        ),
        (EVT_ORPHAN_ASSIGNMENT_WARNING, "asgn-rfm", ORPHAN_SCAN_ACTOR),
    ];
    let actual: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(t, a, actor)| (t.as_str(), a.as_str(), actor.as_str()))
        .collect();
    assert_eq!(actual, expected);
}

#[sqlx::test]
async fn scan_is_debounced_within_same_bucket(pool: sqlx::SqlitePool) {
    seed_base(&pool).await;
    insert_assignment(&pool, "asgn-dup", "awaiting_review", None).await;

    let now = t("2026-04-18T12:00:00Z");
    let seen = GithubEventsSeenRepo::new();

    // First scan: emits one warning.
    let mut tx = pool.begin().await.unwrap();
    let first = run_orphan_scan_with(&mut tx, now, DEBOUNCE, &seen)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(first.warned, 1);
    assert_eq!(first.debounced, 0);

    // Second scan at the *same* wall-clock — same bucket, so the synthetic
    // dedup key collides and the scanner suppresses the repeat warning.
    let mut tx = pool.begin().await.unwrap();
    let second = run_orphan_scan_with(&mut tx, now, DEBOUNCE, &seen)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(second.scanned, 1);
    assert_eq!(second.warned, 0, "debounce must suppress repeat warnings");
    assert_eq!(second.debounced, 1);

    // Still the original single row.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE event_type = ?")
        .bind(EVT_ORPHAN_ASSIGNMENT_WARNING)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // The synthetic dedup marker is in `github_events_seen` so operators
    // can audit what the scanner has silenced.
    let seen_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT github_event_id, event_type FROM github_events_seen")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(seen_rows.len(), 1);
    let (key, etype) = &seen_rows[0];
    assert!(
        key.starts_with("orphan-scan:asgn-dup:"),
        "synthetic key shape drifted: {key}",
    );
    assert_eq!(etype, ORPHAN_SCAN_EVENT_TYPE);
}

#[sqlx::test]
async fn scan_re_emits_after_debounce_window_elapses(pool: sqlx::SqlitePool) {
    seed_base(&pool).await;
    insert_assignment(&pool, "asgn-persist", "ready_for_merge", None).await;

    let seen = GithubEventsSeenRepo::new();

    // T0: first bucket — emit.
    let t0 = Utc.timestamp_opt(1_770_000_000, 0).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let r0 = run_orphan_scan_with(&mut tx, t0, DEBOUNCE, &seen)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(r0.warned, 1);

    // T0 + DEBOUNCE: new bucket — must re-emit.
    let t1 = Utc.timestamp_opt(1_770_000_000 + DEBOUNCE, 0).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let r1 = run_orphan_scan_with(&mut tx, t1, DEBOUNCE, &seen)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        r1.warned, 1,
        "new bucket must re-page persistent orphan; outcome={r1:?}",
    );
    assert_eq!(r1.debounced, 0);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE event_type = ?")
        .bind(EVT_ORPHAN_ASSIGNMENT_WARNING)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2, "one warning per elapsed bucket");
}
