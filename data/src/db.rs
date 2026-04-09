// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Database connection and migration for the Workgraph system.
//!
//! # Concurrency model
//!
//! `sparks.db` is written to by many independent processes (the GUI, every
//! `ryve` CLI invocation, every Hand subprocess). The 2026-04-08 corruption
//! incident showed that the default SQLite configuration does **not** tolerate
//! this pattern. Every connection opened by this crate must therefore apply:
//!
//! - `PRAGMA journal_mode=WAL` — concurrent readers, single writer, no
//!   reader/writer blocking at the page level.
//! - `PRAGMA busy_timeout=5000` — when a writer encounters a locked database
//!   (another process is in the middle of a write), retry for up to 5s
//!   instead of failing immediately with `SQLITE_BUSY`.
//! - `PRAGMA synchronous=NORMAL` — safe in WAL mode and substantially faster
//!   than the default `FULL`.
//! - `PRAGMA foreign_keys=ON` — enforce referential integrity.
//!
//! Single-writer discipline is enforced two ways:
//!
//! 1. **Across processes**: SQLite's own file-level write lock, with
//!    `busy_timeout` providing graceful back-off. This is the only mechanism
//!    that can serialize writers that live in different OS processes.
//! 2. **Within a process**: repositories acquire a separate in-process write
//!    lock (see [`WriteLock`] / [`new_write_lock`]) before issuing a write
//!    transaction. Reads remain concurrent across the pool's connections.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use tokio::sync::Mutex;

use crate::ryve_dir::RyveDir;
use crate::sparks::error::SparksError;

/// Maximum time any single connection will wait on a locked database before
/// returning `SQLITE_BUSY`. Must be long enough to ride out a burst of
/// writes from sibling processes but short enough that a truly stuck writer
/// is surfaced as an error.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Build the `SqliteConnectOptions` used for both the migration pool and the
/// runtime pool. Every PRAGMA required for safe multi-process access is set
/// here so it is applied on every connection sqlx opens.
fn connect_options(db_path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true)
}

/// Open (or create) the sparks database for a workshop directory.
///
/// Creates `.ryve/sparks.db` inside `workshop_dir`, runs all pending
/// migrations, and returns a connection pool whose every connection has
/// WAL + busy_timeout applied.
pub async fn open_sparks_db(workshop_dir: &Path) -> Result<SqlitePool, SparksError> {
    let ryve_dir = RyveDir::new(workshop_dir);
    ryve_dir.ensure_exists().await.map_err(SparksError::Io)?;

    let db_path = ryve_dir.sparks_db_path();
    let options = connect_options(&db_path);

    // Run migrations on a throwaway single-connection pool, then drop it
    // before opening the real pool. SQLite connections cache prepared-
    // statement column metadata; if a `SELECT *` is prepared on the same
    // connection that later runs `ALTER TABLE ADD COLUMN`, the cached
    // metadata becomes stale and `SqliteRow::new` panics with an index
    // out-of-bounds when the next row is decoded. Using a fresh pool for
    // queries guarantees no such cached statements exist.
    {
        let migration_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .map_err(SparksError::Database)?;

        sqlx::migrate!("./migrations")
            .run(&migration_pool)
            .await
            .map_err(|e| SparksError::Database(e.into()))?;

        migration_pool.close().await;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .map_err(SparksError::Database)?;

    Ok(pool)
}

/// Process-local write serializer.
///
/// SQLite only allows one writer at a time. When many tokio tasks in the same
/// process race to write, sqlx can dispatch those writes on different pool
/// connections, forcing SQLite to arbitrate via `SQLITE_BUSY` + the
/// `busy_timeout` retry loop. That works, but holding this mutex across a
/// `BEGIN IMMEDIATE ... COMMIT` block is far cheaper and keeps hot paths out
/// of the busy-loop entirely.
///
/// Usage pattern:
///
/// ```ignore
/// let _w = write_lock.lock().await;
/// let mut tx = pool.begin().await?;
/// // ... mutations ...
/// tx.commit().await?;
/// drop(_w);
/// ```
///
/// This mutex does **not** protect against writers in other processes — only
/// the file-level busy timeout does that. It is a latency optimization for
/// the in-process case, not a correctness mechanism.
pub type WriteLock = Arc<Mutex<()>>;

/// Create a fresh, un-held write lock. Callers that need single-writer
/// discipline within one process should clone this handle and acquire it
/// around their write transaction.
pub fn new_write_lock() -> WriteLock {
    Arc::new(Mutex::new(()))
}

/// Returns true if a sqlx error is a transient SQLite busy/locked condition
/// that is safe to retry.
pub fn is_busy(err: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = err
        && let Some(code) = db_err.code()
    {
        // 5 = SQLITE_BUSY, 6 = SQLITE_LOCKED
        return code == "5" || code == "6";
    }
    false
}

/// Run a fallible async closure with bounded retry on SQLite busy/locked
/// errors. The `busy_timeout` PRAGMA already retries at the C level, so this
/// is a second line of defense for edge cases where that timeout expires
/// (e.g. extremely contended stress tests).
pub async fn with_busy_retry<F, Fut, T>(mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    const MAX_ATTEMPTS: u32 = 5;
    let mut backoff = Duration::from_millis(25);
    for attempt in 1..=MAX_ATTEMPTS {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if is_busy(&e) && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("loop always returns")
}
