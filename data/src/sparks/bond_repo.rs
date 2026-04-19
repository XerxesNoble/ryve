// SPDX-License-Identifier: AGPL-3.0-or-later

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

    if bond_type.is_blocking() && graph::would_create_cycle(pool, from_id, to_id).await? {
        return Err(SparksError::CycleDetected {
            from: from_id.to_string(),
            to: to_id.to_string(),
        });
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

/// Return the `from_id` of every bond where `to_id = spark_id` and the bond
/// type is exactly `"blocks"`. Other bond types (`conditional_blocks`,
/// `related`, `parent_child`, …) are intentionally excluded.
pub async fn list_blocks_predecessors(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<String>, SparksError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT from_id FROM bonds WHERE to_id = ? AND bond_type = 'blocks'")
            .bind(spark_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
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

#[cfg(test)]
mod list_blocks_predecessors_tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn fresh_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        pool
    }

    async fn insert_spark(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO sparks (id, title, description, status, priority, spark_type, workshop_id, metadata, created_at, updated_at)
             VALUES (?, ?, '', 'open', 2, 'task', 'ws', '{}', '2026-04-19T00:00:00+00:00', '2026-04-19T00:00:00+00:00')",
        )
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .expect("insert spark");
    }

    async fn insert_bond(pool: &SqlitePool, from: &str, to: &str, bond_type: &str) {
        sqlx::query("INSERT INTO bonds (from_id, to_id, bond_type) VALUES (?, ?, ?)")
            .bind(from)
            .bind(to)
            .bind(bond_type)
            .execute(pool)
            .await
            .expect("insert bond");
    }

    #[tokio::test]
    async fn returns_empty_when_no_predecessors() {
        let pool = fresh_pool().await;
        insert_spark(&pool, "target").await;

        let preds = list_blocks_predecessors(&pool, "target")
            .await
            .expect("query succeeds");

        assert!(preds.is_empty(), "expected no predecessors, got {preds:?}");
    }

    #[tokio::test]
    async fn returns_single_predecessor() {
        let pool = fresh_pool().await;
        insert_spark(&pool, "target").await;
        insert_spark(&pool, "blocker").await;
        insert_bond(&pool, "blocker", "target", "blocks").await;

        let preds = list_blocks_predecessors(&pool, "target")
            .await
            .expect("query succeeds");

        assert_eq!(preds, vec!["blocker".to_string()]);
    }

    #[tokio::test]
    async fn returns_two_predecessors() {
        let pool = fresh_pool().await;
        insert_spark(&pool, "target").await;
        insert_spark(&pool, "blocker_a").await;
        insert_spark(&pool, "blocker_b").await;
        insert_bond(&pool, "blocker_a", "target", "blocks").await;
        insert_bond(&pool, "blocker_b", "target", "blocks").await;

        let mut preds = list_blocks_predecessors(&pool, "target")
            .await
            .expect("query succeeds");
        preds.sort();

        assert_eq!(
            preds,
            vec!["blocker_a".to_string(), "blocker_b".to_string()]
        );
    }

    #[tokio::test]
    async fn ignores_non_blocks_bond_types() {
        let pool = fresh_pool().await;
        insert_spark(&pool, "target").await;
        insert_spark(&pool, "related_src").await;
        insert_spark(&pool, "parent_src").await;
        insert_spark(&pool, "cond_src").await;
        insert_spark(&pool, "waits_src").await;
        insert_bond(&pool, "related_src", "target", "related").await;
        insert_bond(&pool, "parent_src", "target", "parent_child").await;
        insert_bond(&pool, "cond_src", "target", "conditional_blocks").await;
        insert_bond(&pool, "waits_src", "target", "waits_for").await;

        let preds = list_blocks_predecessors(&pool, "target")
            .await
            .expect("query succeeds");

        assert!(
            preds.is_empty(),
            "only `blocks` bonds should be returned, got {preds:?}"
        );
    }
}
