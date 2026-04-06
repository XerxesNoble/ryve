// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Hand-spark assignment with liveness-aware claims, heartbeat, and handoff.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::*;

/// Assign a Hand to a Spark. Fails if an active owner already exists.
/// The check and INSERT are wrapped in a transaction to prevent TOCTOU races.
pub async fn assign(
    pool: &SqlitePool,
    new: NewHandAssignment,
) -> Result<HandAssignment, SparksError> {
    let mut tx = pool.begin().await?;

    // Check for existing active owner (only enforced for owner role)
    if new.role == AssignmentRole::Owner {
        let existing = sqlx::query_as::<_, HandAssignment>(
            "SELECT * FROM hand_assignments WHERE spark_id = ? AND status = 'active' AND role = 'owner' LIMIT 1",
        )
        .bind(&new.spark_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(existing) = existing {
            return Err(SparksError::AlreadyClaimed {
                spark_id: new.spark_id,
                session_id: existing.session_id,
            });
        }
    }

    let now = Utc::now().to_rfc3339();

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO hand_assignments (session_id, spark_id, status, role, assigned_at, last_heartbeat_at)
         VALUES (?, ?, 'active', ?, ?, ?)
         RETURNING id",
    )
    .bind(&new.session_id)
    .bind(&new.spark_id)
    .bind(new.role.as_str())
    .bind(&now)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;

    let assignment = sqlx::query_as::<_, HandAssignment>("SELECT * FROM hand_assignments WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(assignment)
}

/// Mark a Hand's assignment as completed.
pub async fn complete(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();

    let result = sqlx::query(
        "UPDATE hand_assignments SET status = 'completed', completed_at = ? WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(session_id)
    .bind(spark_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!(
            "active assignment for session {session_id} on spark {spark_id}"
        )));
    }
    Ok(())
}

/// Hand off a Spark from one session to another.
pub async fn handoff(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
    to_session_id: &str,
    reason: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();

    let result = sqlx::query(
        "UPDATE hand_assignments SET status = 'handed_off', completed_at = ?, handoff_to = ?, handoff_reason = ? WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(to_session_id)
    .bind(reason)
    .bind(session_id)
    .bind(spark_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!(
            "active assignment for session {session_id} on spark {spark_id}"
        )));
    }
    Ok(())
}

/// Abandon a Hand's claim on a Spark.
pub async fn abandon(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE hand_assignments SET status = 'abandoned', completed_at = ? WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(session_id)
    .bind(spark_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update the heartbeat timestamp for a Hand's assignment.
pub async fn heartbeat(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE hand_assignments SET last_heartbeat_at = ? WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(session_id)
    .bind(spark_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Expire stale claims where the heartbeat is older than `max_age_seconds`.
/// Returns the expired assignments.
pub async fn expire_stale_claims(
    pool: &SqlitePool,
    max_age_seconds: i64,
) -> Result<Vec<HandAssignment>, SparksError> {
    let now = Utc::now().to_rfc3339();

    // Find stale claims
    let stale = sqlx::query_as::<_, HandAssignment>(
        "SELECT * FROM hand_assignments WHERE status = 'active' AND last_heartbeat_at IS NOT NULL AND datetime(last_heartbeat_at, '+' || ? || ' seconds') < datetime(?)",
    )
    .bind(max_age_seconds)
    .bind(&now)
    .fetch_all(pool)
    .await?;

    if !stale.is_empty() {
        sqlx::query(
            "UPDATE hand_assignments SET status = 'expired', completed_at = ? WHERE status = 'active' AND last_heartbeat_at IS NOT NULL AND datetime(last_heartbeat_at, '+' || ? || ' seconds') < datetime(?)",
        )
        .bind(&now)
        .bind(max_age_seconds)
        .bind(&now)
        .execute(pool)
        .await?;
    }

    Ok(stale)
}

/// List all active hand assignments across all sparks.
pub async fn list_active(pool: &SqlitePool) -> Result<Vec<HandAssignment>, SparksError> {
    Ok(sqlx::query_as::<_, HandAssignment>(
        "SELECT * FROM hand_assignments WHERE status = 'active'",
    )
    .fetch_all(pool)
    .await?)
}

/// Get the active owner assignment for a Spark, if any.
pub async fn active_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Option<HandAssignment>, SparksError> {
    Ok(sqlx::query_as::<_, HandAssignment>(
        "SELECT * FROM hand_assignments WHERE spark_id = ? AND status = 'active' AND role = 'owner' LIMIT 1",
    )
    .bind(spark_id)
    .fetch_optional(pool)
    .await?)
}

/// List all assignments for a session.
pub async fn list_for_session(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<HandAssignment>, SparksError> {
    Ok(sqlx::query_as::<_, HandAssignment>(
        "SELECT * FROM hand_assignments WHERE session_id = ? ORDER BY assigned_at DESC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?)
}

/// Check if a Spark is currently claimed by any active owner.
pub async fn is_spark_claimed(pool: &SqlitePool, spark_id: &str) -> Result<bool, SparksError> {
    Ok(active_for_spark(pool, spark_id).await?.is_some())
}
