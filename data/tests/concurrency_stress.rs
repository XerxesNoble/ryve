// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix
//
// Stress tests for the fix introduced in spark ryve-a78568b9
// (Workgraph SQLite: enforce WAL + busy_timeout + write serialization).
//
// These tests reproduce the corruption pattern from 2026-04-08 — many
// parallel writers hammering a single `sparks.db` — and verify that the
// hardened connection setup in `data::db` keeps the database intact.

use std::path::PathBuf;
use std::sync::Arc;

use data::db::{self, open_sparks_db};
use data::sparks::spark_repo;
use data::sparks::types::*;
use sqlx::Row;
use tokio::task::JoinSet;

/// Unique temp directory per test so parallel cargo test runs don't collide.
fn unique_tempdir(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ryve-stress-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn new_spark(workshop: &str, i: usize) -> NewSpark {
    // Use Epic here so the stress test doesn't have to co-ordinate parent
    // creation with each concurrent writer. The no-orphan invariant only
    // rejects non-epic sparks without a parent; epics may be top-level.
    // This test is about write contention, not hierarchy.
    NewSpark {
        title: format!("stress spark #{i}"),
        description: format!("concurrent write {i}"),
        spark_type: SparkType::Epic,
        priority: 2,
        workshop_id: workshop.to_string(),
        assignee: None,
        owner: None,
        parent_id: None,
        due_at: None,
        estimated_minutes: None,
        metadata: None,
        risk_level: None,
        scope_boundary: None,
    }
}

/// Run `PRAGMA integrity_check` and return true iff SQLite reports "ok".
async fn integrity_ok(pool: &sqlx::SqlitePool) -> bool {
    let row = sqlx::query("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .expect("integrity_check query");
    let result: String = row.get(0);
    result == "ok"
}

/// Verify that every connection applied WAL + busy_timeout.
#[tokio::test]
async fn pragmas_are_applied_on_every_connection() {
    let dir = unique_tempdir("pragma");
    let pool = open_sparks_db(&dir).await.unwrap();

    // Check pragmas on several connections to cover the whole pool.
    for _ in 0..5 {
        let row = sqlx::query("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mode: String = row.get(0);
        assert_eq!(mode.to_lowercase(), "wal", "journal_mode must be WAL");

        let row = sqlx::query("PRAGMA busy_timeout")
            .fetch_one(&pool)
            .await
            .unwrap();
        let timeout: i64 = row.get(0);
        assert!(
            timeout >= 5000,
            "busy_timeout must be >= 5000ms, got {timeout}"
        );

        let row = sqlx::query("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        let fk: i64 = row.get(0);
        assert_eq!(fk, 1, "foreign_keys must be ON");
    }

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fan out 50 concurrent `spark_repo::create` calls against a single pool
/// and assert that (a) every insert succeeds, (b) row count matches, and
/// (c) `PRAGMA integrity_check` returns "ok" afterward.
///
/// This is the in-process analogue of the 2026-04-08 corruption pattern:
/// many tasks racing to write to `sparks.db` at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn fifty_concurrent_spark_creates_keep_db_intact() {
    let dir = unique_tempdir("fanout");
    let pool = open_sparks_db(&dir).await.unwrap();

    const N: usize = 50;
    let mut set = JoinSet::new();
    for i in 0..N {
        let pool = pool.clone();
        set.spawn(async move {
            spark_repo::create(&pool, new_spark("ws-stress", i))
                .await
                .map(|s| s.id)
        });
    }

    let mut ids = Vec::with_capacity(N);
    while let Some(res) = set.join_next().await {
        let id = res.expect("task panicked").expect("insert failed");
        ids.push(id);
    }
    assert_eq!(ids.len(), N, "all 50 inserts must succeed");

    // Row count matches.
    let row = sqlx::query("SELECT COUNT(*) FROM sparks WHERE workshop_id = ?")
        .bind("ws-stress")
        .fetch_one(&pool)
        .await
        .unwrap();
    let count: i64 = row.get(0);
    assert_eq!(count, N as i64);

    // Integrity check passes.
    assert!(integrity_ok(&pool).await, "PRAGMA integrity_check != ok");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reproduce the original failure mode: simultaneous writers from what
/// would be independent callers (multiple pools over the same file mimic
/// multiple processes) with an interleaved reader hammering the DB. After
/// the storm, the database must still open cleanly and `integrity_check`
/// must return "ok" — i.e. it is recoverable.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn simulated_multi_writer_storm_is_recoverable() {
    let dir = unique_tempdir("storm");

    // Multiple pools opened against the same file simulate multiple
    // ryve CLI / Hand processes sharing sparks.db. Each pool has its own
    // file handle and lock state, so inter-pool contention exercises the
    // same OS-level lock path that inter-process contention would.
    let pool_a = open_sparks_db(&dir).await.unwrap();
    let pool_b = open_sparks_db(&dir).await.unwrap();
    let pool_c = open_sparks_db(&dir).await.unwrap();

    let writers_done = Arc::new(tokio::sync::Notify::new());
    let mut set = JoinSet::new();

    // Three writer swarms, one per pool, 20 inserts each = 60 writers.
    for (idx, pool) in [pool_a.clone(), pool_b.clone(), pool_c.clone()]
        .into_iter()
        .enumerate()
    {
        for i in 0..20 {
            let pool = pool.clone();
            let tag = format!("ws-storm-{idx}");
            set.spawn(async move {
                // Wrap in busy retry as a second line of defense; the
                // primary retry mechanism is SQLite's busy_timeout.
                db::with_busy_retry(|| async {
                    spark_repo::create(&pool, new_spark(&tag, i))
                        .await
                        .map_err(|e| match e {
                            data::sparks::error::SparksError::Database(e) => e,
                            other => sqlx::Error::Protocol(other.to_string()),
                        })
                })
                .await
                .expect("insert under contention failed");
            });
        }
    }

    // A concurrent reader that keeps probing while writes are flying.
    {
        let pool = pool_a.clone();
        let notify = writers_done.clone();
        set.spawn(async move {
            let mut reads = 0u32;
            loop {
                if tokio::time::timeout(std::time::Duration::from_millis(1), notify.notified())
                    .await
                    .is_ok()
                {
                    break;
                }
                let row = sqlx::query("SELECT COUNT(*) FROM sparks")
                    .fetch_one(&pool)
                    .await
                    .expect("reader under contention failed");
                let _: i64 = row.get(0);
                reads += 1;
                if reads > 10_000 {
                    break;
                }
            }
        });
    }

    // Wait for all writers + the reader.
    let mut remaining = set.len();
    while let Some(res) = set.join_next().await {
        res.expect("task panicked");
        remaining -= 1;
        // When only the reader is left, signal it to stop.
        if remaining == 1 {
            writers_done.notify_waiters();
        }
    }

    // Expect 60 total rows across the three workshops.
    let row = sqlx::query("SELECT COUNT(*) FROM sparks")
        .fetch_one(&pool_a)
        .await
        .unwrap();
    let count: i64 = row.get(0);
    assert_eq!(count, 60, "all 60 writes must be durable after the storm");

    // Close everything, then reopen to verify the DB is recoverable —
    // this is the strongest evidence that no corruption occurred.
    pool_a.close().await;
    pool_b.close().await;
    pool_c.close().await;

    let reopened = open_sparks_db(&dir).await.expect("reopen after storm");
    assert!(
        integrity_ok(&reopened).await,
        "integrity_check failed after storm — DB not recoverable"
    );
    let row = sqlx::query("SELECT COUNT(*) FROM sparks")
        .fetch_one(&reopened)
        .await
        .unwrap();
    let count: i64 = row.get(0);
    assert_eq!(count, 60);

    reopened.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Sanity check that `with_busy_retry` gives up after exhausting attempts
/// on persistent non-busy errors (so we don't accidentally mask real bugs).
#[tokio::test]
async fn busy_retry_propagates_non_busy_errors() {
    let dir = unique_tempdir("retry");
    let pool = open_sparks_db(&dir).await.unwrap();

    let result: Result<(), sqlx::Error> = db::with_busy_retry(|| async {
        // Invalid SQL → a hard parse error, not SQLITE_BUSY.
        sqlx::query("SELECT * FROM no_such_table_xyz")
            .execute(&pool)
            .await
            .map(|_| ())
    })
    .await;

    assert!(result.is_err(), "non-busy errors must propagate");
    assert!(
        !db::is_busy(result.as_ref().err().unwrap()),
        "error should not be classified as busy"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
