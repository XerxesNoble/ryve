// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Database connection and migration for the Sparks system.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::forge_dir::ForgeDir;
use crate::sparks::error::SparksError;

/// Open (or create) the sparks database for a workshop directory.
///
/// Creates `.forge/sparks.db` inside `workshop_dir`, runs all pending
/// migrations, and returns a connection pool.
pub async fn open_sparks_db(workshop_dir: &Path) -> Result<SqlitePool, SparksError> {
    let forge_dir = ForgeDir::new(workshop_dir);
    forge_dir.ensure_exists().await.map_err(SparksError::Io)?;

    let db_path = forge_dir.sparks_db_path();

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .map_err(SparksError::Database)?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| SparksError::Database(e.into()))?;

    Ok(pool)
}
