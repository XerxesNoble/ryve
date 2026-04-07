// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Database connection and migration for the Workgraph system.

use std::path::Path;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::ryve_dir::RyveDir;
use crate::sparks::error::SparksError;

/// Open (or create) the sparks database for a workshop directory.
///
/// Creates `.ryve/sparks.db` inside `workshop_dir`, runs all pending
/// migrations, and returns a connection pool.
pub async fn open_sparks_db(workshop_dir: &Path) -> Result<SqlitePool, SparksError> {
    let ryve_dir = RyveDir::new(workshop_dir);
    ryve_dir.ensure_exists().await.map_err(SparksError::Io)?;

    let db_path = ryve_dir.sparks_db_path();

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);

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
