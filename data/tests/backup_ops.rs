// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Integration tests for `data::backup`.
//!
//! The tests here exercise the full backup/restore cycle against a real
//! SQLite database opened through the production `data::db::open_sparks_db`
//! entry point so the snapshot path uses the same connection settings
//! (WAL mode, foreign keys, migrations) as the running app.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use data::backup::{
    self, DEFAULT_BACKUP_RETENTION, SNAPSHOT_PREFIX, Snapshot, apply_retention, list_snapshots,
    parse_stamp, restore_snapshot, snapshot_and_retain, take_snapshot,
};
use data::db::open_sparks_db;
use data::ryve_dir::RyveDir;
use data::sparks::spark_repo;
use data::sparks::types::{NewSpark, SparkType};

/// RAII guard that removes its path when dropped so each test leaves
/// no residue even if it panics midway through.
struct TempWorkshopDir(PathBuf);

impl Drop for TempWorkshopDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a fresh workshop directory under the system temp dir, open
/// its sparks database, and return the paths + pool. The caller owns
/// the guard so the directory lives until they drop it.
async fn fresh_workshop() -> (TempWorkshopDir, PathBuf, RyveDir, sqlx::SqlitePool) {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("ryve-backup-test-{pid}-{id}"));
    // Ensure a pristine directory on every call.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create root");
    let guard = TempWorkshopDir(root.clone());
    let ryve_dir = RyveDir::new(&root);
    ryve_dir.ensure_exists().await.expect("ensure_exists");
    let pool = open_sparks_db(&root).await.expect("open_sparks_db");
    (guard, root, ryve_dir, pool)
}

async fn seed_one_spark(pool: &sqlx::SqlitePool, title: &str) -> String {
    let spark = spark_repo::create(
        pool,
        NewSpark {
            title: title.to_string(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: "ws-test".to_string(),
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
    .expect("create spark");
    spark.id
}

#[tokio::test]
async fn take_snapshot_writes_sqlite_file_to_backups_dir() {
    let (_tmp, _root, ryve_dir, pool) = fresh_workshop().await;
    seed_one_spark(&pool, "first").await;

    let snap = take_snapshot(&pool, &ryve_dir).await.expect("snapshot");

    assert!(snap.exists(), "snapshot file should exist on disk");
    assert!(snap.starts_with(ryve_dir.backups_dir()));
    let name = snap.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with(SNAPSHOT_PREFIX), "name={name}");
    assert!(name.ends_with(".db"), "name={name}");

    // The snapshot must be a valid SQLite database and contain the row
    // we just inserted — i.e. it's a real copy, not a placeholder file.
    let snap_url = format!("sqlite://{}", snap.display());
    let snap_pool = sqlx::SqlitePool::connect(&snap_url)
        .await
        .expect("open snapshot");
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sparks")
        .fetch_one(&snap_pool)
        .await
        .expect("count");
    assert_eq!(count.0, 1);
    snap_pool.close().await;
}

#[tokio::test]
async fn list_snapshots_sorted_chronologically() {
    let (_tmp, _root, ryve_dir, _pool) = fresh_workshop().await;

    // Manually drop three files with known stamps so we don't have to
    // wait a real second between snapshots.
    for stamp in [
        "20260101T000000Z",
        "20260401T120000Z",
        "20260301T060000Z",
    ] {
        let path = ryve_dir
            .backups_dir()
            .join(format!("{SNAPSHOT_PREFIX}{stamp}.db"));
        tokio::fs::write(&path, b"sqlite placeholder")
            .await
            .expect("write");
    }
    // Plus a file that does NOT match the prefix — must be ignored.
    tokio::fs::write(
        ryve_dir.backups_dir().join("README.txt"),
        b"notes",
    )
    .await
    .expect("write");

    let snaps: Vec<Snapshot> = list_snapshots(&ryve_dir).await.expect("list");
    assert_eq!(snaps.len(), 3, "non-matching files must be ignored");

    let stamps: Vec<_> = snaps
        .iter()
        .filter_map(|s| s.taken_at)
        .collect();
    assert_eq!(stamps.len(), 3);
    assert!(stamps[0] < stamps[1]);
    assert!(stamps[1] < stamps[2]);
}

#[tokio::test]
async fn apply_retention_prunes_oldest_snapshots() {
    let (_tmp, _root, ryve_dir, _pool) = fresh_workshop().await;

    // Seed five fake snapshots with monotonically increasing stamps.
    let stamps = [
        "20260101T000000Z",
        "20260102T000000Z",
        "20260103T000000Z",
        "20260104T000000Z",
        "20260105T000000Z",
    ];
    for s in &stamps {
        tokio::fs::write(
            ryve_dir
                .backups_dir()
                .join(format!("{SNAPSHOT_PREFIX}{s}.db")),
            b"x",
        )
        .await
        .unwrap();
    }

    let deleted = apply_retention(&ryve_dir, 2).await.expect("retention");
    assert_eq!(deleted.len(), 3, "keep=2 of 5 means 3 deleted");

    let remaining: Vec<Snapshot> = list_snapshots(&ryve_dir).await.unwrap();
    assert_eq!(remaining.len(), 2);
    // The two newest ones should remain.
    let kept_names: Vec<String> = remaining.iter().map(|s| s.file_name()).collect();
    assert!(kept_names[0].contains("20260104"));
    assert!(kept_names[1].contains("20260105"));
}

#[tokio::test]
async fn apply_retention_zero_keeps_everything() {
    // keep=0 is a safety fallback — it must NOT wipe the backups dir.
    let (_tmp, _root, ryve_dir, _pool) = fresh_workshop().await;
    tokio::fs::write(
        ryve_dir.backups_dir().join(format!(
            "{SNAPSHOT_PREFIX}20260101T000000Z.db"
        )),
        b"x",
    )
    .await
    .unwrap();

    let deleted = apply_retention(&ryve_dir, 0).await.expect("retention");
    assert!(deleted.is_empty());
    let remaining = list_snapshots(&ryve_dir).await.unwrap();
    assert_eq!(remaining.len(), 1);
}

#[tokio::test]
async fn snapshot_and_retain_prunes_after_writing() {
    let (_tmp, _root, ryve_dir, pool) = fresh_workshop().await;

    // Pre-seed an old snapshot so retention has something to prune.
    let old = ryve_dir
        .backups_dir()
        .join(format!("{SNAPSHOT_PREFIX}20000101T000000Z.db"));
    tokio::fs::write(&old, b"x").await.unwrap();

    let new_snap = snapshot_and_retain(&pool, &ryve_dir, 1)
        .await
        .expect("snapshot+retain");

    assert!(new_snap.exists());
    assert!(!old.exists(), "old snapshot should have been pruned");
    assert!(
        DEFAULT_BACKUP_RETENTION > 0,
        "default retention must be positive"
    );
}

#[tokio::test]
async fn restore_snapshot_replaces_live_db_with_snapshot_contents() {
    let (_tmp, _root, ryve_dir, pool) = fresh_workshop().await;
    let _first = seed_one_spark(&pool, "snapshot-only").await;

    // Take a snapshot that contains exactly one spark.
    let snap = take_snapshot(&pool, &ryve_dir).await.expect("snapshot");

    // Now mutate the live DB so it diverges from the snapshot.
    seed_one_spark(&pool, "post-snapshot").await;
    let (live_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sparks")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(live_count, 2);

    // Close the pool before restoring — the CLI does this too, and
    // SQLite cannot open the same WAL from two processes.
    pool.close().await;

    let outcome = restore_snapshot(&ryve_dir, &snap)
        .await
        .expect("restore");
    assert_eq!(outcome.restored_db, ryve_dir.sparks_db_path());
    assert!(
        outcome.previous_db_backup.is_some(),
        "previous db must be moved aside, not deleted"
    );
    let prev = outcome.previous_db_backup.as_ref().unwrap();
    assert!(prev.exists(), "pre-restore backup should exist");

    // Reopen the restored database and confirm it has the snapshot state.
    let pool = open_sparks_db(ryve_dir.root().parent().unwrap())
        .await
        .expect("reopen");
    let (count_after,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sparks")
        .fetch_one(&pool)
        .await
        .expect("count after restore");
    assert_eq!(
        count_after, 1,
        "restored db must match the snapshot's row count"
    );
    pool.close().await;
}

#[tokio::test]
async fn restore_snapshot_rejects_missing_file() {
    let (_tmp, _root, ryve_dir, pool) = fresh_workshop().await;
    pool.close().await;
    let missing = ryve_dir.backups_dir().join("does-not-exist.db");
    let err = restore_snapshot(&ryve_dir, &missing).await;
    assert!(matches!(err, Err(backup::BackupError::NotFound(_))));
}

#[test]
fn parse_stamp_roundtrip() {
    let name = format!("{SNAPSHOT_PREFIX}20260408T130500Z.db");
    let ts = parse_stamp(&name).expect("parse");
    assert_eq!(backup::format_stamp(ts), "20260408T130500Z");
}

#[test]
fn parse_stamp_rejects_non_matching_names() {
    assert!(parse_stamp("notes.txt").is_none());
    assert!(parse_stamp("sparks-bogus.db").is_none());
    assert!(parse_stamp("sparks-20260408T130500Z.txt").is_none());
}
