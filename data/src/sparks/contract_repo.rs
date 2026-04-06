// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for verification contracts on sparks.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::*;

pub async fn create(pool: &SqlitePool, new: NewContract) -> Result<Contract, SparksError> {
    let now = Utc::now().to_rfc3339();

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO contracts (spark_id, kind, description, check_command, pattern, file_glob, enforcement, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?)
         RETURNING id",
    )
    .bind(&new.spark_id)
    .bind(new.kind.as_str())
    .bind(&new.description)
    .bind(&new.check_command)
    .bind(&new.pattern)
    .bind(&new.file_glob)
    .bind(new.enforcement.as_str())
    .bind(&now)
    .fetch_one(pool)
    .await?;

    Ok(
        sqlx::query_as::<_, Contract>("SELECT * FROM contracts WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn list_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<Contract>, SparksError> {
    Ok(
        sqlx::query_as::<_, Contract>("SELECT * FROM contracts WHERE spark_id = ? ORDER BY id ASC")
            .bind(spark_id)
            .fetch_all(pool)
            .await?,
    )
}

pub async fn update_status(
    pool: &SqlitePool,
    id: i64,
    status: ContractStatus,
    checked_by: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();

    let result = sqlx::query(
        "UPDATE contracts SET status = ?, last_checked_at = ?, last_checked_by = ? WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(&now)
    .bind(checked_by)
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("contract {id}")));
    }
    Ok(())
}

/// List contracts that are failing or pending for sparks in a workshop.
pub async fn list_failing(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<Contract>, SparksError> {
    Ok(sqlx::query_as::<_, Contract>(
        "SELECT c.* FROM contracts c
         JOIN sparks s ON c.spark_id = s.id
         WHERE s.workshop_id = ? AND c.status IN ('pending', 'fail') AND c.enforcement = 'required'
         ORDER BY c.spark_id, c.id",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?)
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), SparksError> {
    let result = sqlx::query("DELETE FROM contracts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("contract {id}")));
    }
    Ok(())
}
