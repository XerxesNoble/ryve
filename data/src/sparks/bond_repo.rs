// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD operations for Bonds (dependencies between sparks).

use std::collections::HashSet;

use sqlx::SqlitePool;

use super::error::SparksError;
use super::graph;
use super::types::*;

/// Create a bond. For blocking bond types, checks for cycles first.
/// The cycle check and INSERT are wrapped in a transaction to prevent TOCTOU races.
pub async fn create(
    pool: &SqlitePool,
    from_id: &str,
    to_id: &str,
    bond_type: BondType,
) -> Result<Bond, SparksError> {
    let mut tx = pool.begin().await?;

    if bond_type.is_blocking() {
        if graph::would_create_cycle(pool, from_id, to_id).await? {
            return Err(SparksError::CycleDetected {
                from: from_id.to_string(),
                to: to_id.to_string(),
            });
        }
    }

    sqlx::query("INSERT INTO bonds (from_id, to_id, bond_type) VALUES (?, ?, ?)")
        .bind(from_id)
        .bind(to_id)
        .bind(bond_type.as_str())
        .execute(&mut *tx)
        .await?;

    // Fetch the created bond
    let bond = sqlx::query_as::<_, Bond>(
        "SELECT * FROM bonds WHERE from_id = ? AND to_id = ? AND bond_type = ?",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(bond_type.as_str())
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(bond)
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<(), SparksError> {
    let result = sqlx::query("DELETE FROM bonds WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("bond {id}")));
    }
    Ok(())
}

/// List all bonds where from_id or to_id matches the given spark.
pub async fn list_for_spark(pool: &SqlitePool, spark_id: &str) -> Result<Vec<Bond>, SparksError> {
    Ok(
        sqlx::query_as::<_, Bond>("SELECT * FROM bonds WHERE from_id = ? OR to_id = ?")
            .bind(spark_id)
            .bind(spark_id)
            .fetch_all(pool)
            .await?,
    )
}

/// List sparks that block the given spark (i.e., bonds where to_id = spark_id
/// and bond_type is blocking).
pub async fn list_blockers(pool: &SqlitePool, spark_id: &str) -> Result<Vec<Bond>, SparksError> {
    Ok(sqlx::query_as::<_, Bond>(
        "SELECT * FROM bonds WHERE to_id = ? AND bond_type IN ('blocks', 'conditional_blocks')",
    )
    .bind(spark_id)
    .fetch_all(pool)
    .await?)
}

/// Return the set of spark IDs in the workshop that have at least one
/// open (non-closed) blocking bond pointing at them. Used by the UI to
/// surface a "blocked" indicator on the sparks panel and to remind agents
/// not to claim blocked sparks.
pub async fn list_blocked_spark_ids(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<HashSet<String>, SparksError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT b.to_id
         FROM bonds b
         JOIN sparks blocker ON blocker.id = b.from_id
         JOIN sparks blocked ON blocked.id = b.to_id
         WHERE b.bond_type IN ('blocks', 'conditional_blocks')
           AND blocker.status != 'closed'
           AND blocked.workshop_id = ?",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
