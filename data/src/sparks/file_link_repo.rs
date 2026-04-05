// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for spark-file link associations.

use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::{NewSparkFileLink, SparkFileLink};

/// Create a new spark-file link.
pub async fn create(pool: &SqlitePool, link: &NewSparkFileLink) -> Result<i64, SparksError> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query_scalar::<_, i64>(
        "INSERT INTO spark_file_links (spark_id, file_path, line_start, line_end, workshop_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(&link.spark_id)
    .bind(&link.file_path)
    .bind(link.line_start)
    .bind(link.line_end)
    .bind(&link.workshop_id)
    .bind(&now)
    .fetch_one(pool)
    .await?;

    Ok(result)
}

/// List all links for a given spark.
pub async fn list_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<SparkFileLink>, SparksError> {
    let links = sqlx::query_as::<_, SparkFileLink>(
        "SELECT * FROM spark_file_links WHERE spark_id = ? ORDER BY file_path, line_start",
    )
    .bind(spark_id)
    .fetch_all(pool)
    .await?;

    Ok(links)
}

/// List all links for a given file path within a workshop.
pub async fn list_for_file(
    pool: &SqlitePool,
    file_path: &str,
    workshop_id: &str,
) -> Result<Vec<SparkFileLink>, SparksError> {
    let links = sqlx::query_as::<_, SparkFileLink>(
        "SELECT * FROM spark_file_links WHERE file_path = ? AND workshop_id = ? ORDER BY line_start",
    )
    .bind(file_path)
    .bind(workshop_id)
    .fetch_all(pool)
    .await?;

    Ok(links)
}

/// Delete a spark-file link by ID.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), SparksError> {
    sqlx::query("DELETE FROM spark_file_links WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    Ok(())
}
