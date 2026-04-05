// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for commit-spark linkage.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::*;

pub async fn create(pool: &SqlitePool, new: NewCommitLink) -> Result<CommitLink, SparksError> {
    let now = Utc::now().to_rfc3339();

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO commit_links (spark_id, commit_hash, commit_message, author, committed_at, workshop_id, linked_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(spark_id, commit_hash) DO UPDATE SET commit_message=excluded.commit_message
         RETURNING id",
    )
    .bind(&new.spark_id)
    .bind(&new.commit_hash)
    .bind(&new.commit_message)
    .bind(&new.author)
    .bind(&new.committed_at)
    .bind(&new.workshop_id)
    .bind(&new.linked_by)
    .bind(&now)
    .fetch_one(pool)
    .await?;

    Ok(
        sqlx::query_as::<_, CommitLink>("SELECT * FROM commit_links WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn list_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<CommitLink>, SparksError> {
    Ok(sqlx::query_as::<_, CommitLink>(
        "SELECT * FROM commit_links WHERE spark_id = ? ORDER BY committed_at DESC",
    )
    .bind(spark_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list_for_commit(
    pool: &SqlitePool,
    commit_hash: &str,
) -> Result<Vec<CommitLink>, SparksError> {
    Ok(sqlx::query_as::<_, CommitLink>(
        "SELECT * FROM commit_links WHERE commit_hash = ? ORDER BY spark_id",
    )
    .bind(commit_hash)
    .fetch_all(pool)
    .await?)
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), SparksError> {
    let result = sqlx::query("DELETE FROM commit_links WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("commit_link {id}")));
    }
    Ok(())
}
