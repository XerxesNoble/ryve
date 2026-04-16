// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hand-spark assignment with liveness-aware claims, heartbeat, and handoff.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::*;

const HA_SELECT_COLS: &str = "id, session_id, spark_id, status, role, assigned_at, last_heartbeat_at, \
     lease_expires_at, completed_at, handoff_to, handoff_reason";

/// Assign a Hand to a Spark. Fails if an active owner already exists.
/// The check and INSERT are wrapped in a transaction to prevent TOCTOU races.
pub async fn assign(
    pool: &SqlitePool,
    new: NewHandAssignment,
) -> Result<HandAssignment, SparksError> {
    let mut tx = pool.begin().await?;

    if new.role == AssignmentRole::Owner {
        let q = format!(
            "SELECT {HA_SELECT_COLS} FROM assignments \
             WHERE spark_id = ? AND status = 'active' AND role = 'owner' LIMIT 1"
        );
        let existing = sqlx::query_as::<_, HandAssignment>(&q)
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
    let asgn_id = generate_id("asgn");

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO assignments \
         (assignment_id, spark_id, actor_id, session_id, status, role, assigned_at, \
          last_heartbeat_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'active', ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(&asgn_id)
    .bind(&new.spark_id)
    .bind(&new.session_id)
    .bind(&new.session_id)
    .bind(new.role.as_str())
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;

    let q = format!("SELECT {HA_SELECT_COLS} FROM assignments WHERE id = ?");
    let assignment = sqlx::query_as::<_, HandAssignment>(&q)
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
        "UPDATE assignments SET status = 'completed', completed_at = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
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
        "UPDATE assignments SET status = 'handed_off', completed_at = ?, \
         handoff_to = ?, handoff_reason = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
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
        "UPDATE assignments SET status = 'abandoned', completed_at = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
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
        "UPDATE assignments SET last_heartbeat_at = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
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

    let q = format!(
        "SELECT {HA_SELECT_COLS} FROM assignments \
         WHERE status = 'active' AND last_heartbeat_at IS NOT NULL \
         AND datetime(last_heartbeat_at, '+' || ? || ' seconds') < datetime(?)"
    );
    let stale = sqlx::query_as::<_, HandAssignment>(&q)
        .bind(max_age_seconds)
        .bind(&now)
        .fetch_all(pool)
        .await?;

    if !stale.is_empty() {
        sqlx::query(
            "UPDATE assignments SET status = 'expired', completed_at = ? \
             WHERE status = 'active' AND last_heartbeat_at IS NOT NULL \
             AND datetime(last_heartbeat_at, '+' || ? || ' seconds') < datetime(?)",
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
    let q = format!("SELECT {HA_SELECT_COLS} FROM assignments WHERE status = 'active'");
    Ok(sqlx::query_as::<_, HandAssignment>(&q)
        .fetch_all(pool)
        .await?)
}

/// List active hand assignments for sparks belonging to a specific workshop.
pub async fn list_active_for_workshop(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<HandAssignment>, SparksError> {
    Ok(sqlx::query_as::<_, HandAssignment>(
        "SELECT a.id, a.session_id, a.spark_id, a.status, a.role, a.assigned_at, \
         a.last_heartbeat_at, a.lease_expires_at, a.completed_at, a.handoff_to, a.handoff_reason \
         FROM assignments a \
         INNER JOIN sparks s ON s.id = a.spark_id \
         WHERE a.status = 'active' AND s.workshop_id = ?",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?)
}

/// Get the active owner assignment for a Spark, if any.
pub async fn active_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Option<HandAssignment>, SparksError> {
    let q = format!(
        "SELECT {HA_SELECT_COLS} FROM assignments \
         WHERE spark_id = ? AND status = 'active' AND role = 'owner' LIMIT 1"
    );
    Ok(sqlx::query_as::<_, HandAssignment>(&q)
        .bind(spark_id)
        .fetch_optional(pool)
        .await?)
}

/// List all assignments for a session.
pub async fn list_for_session(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<HandAssignment>, SparksError> {
    let q = format!(
        "SELECT {HA_SELECT_COLS} FROM assignments \
         WHERE session_id = ? ORDER BY assigned_at DESC"
    );
    Ok(sqlx::query_as::<_, HandAssignment>(&q)
        .bind(session_id)
        .fetch_all(pool)
        .await?)
}

/// Check if a Spark is currently claimed by any active owner.
pub async fn is_spark_claimed(pool: &SqlitePool, spark_id: &str) -> Result<bool, SparksError> {
    Ok(active_for_spark(pool, spark_id).await?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparks::types::{NewAgentSession, NewSpark, SparkType};

    async fn fresh_pool() -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("ryve-assign-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::open_sparks_db(&dir).await.unwrap()
    }

    async fn make_session(pool: &SqlitePool, ws: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        crate::sparks::agent_session_repo::create(
            pool,
            &NewAgentSession {
                id: id.clone(),
                workshop_id: ws.into(),
                agent_name: "stub".into(),
                agent_command: "echo".into(),
                agent_args: vec![],
                session_label: None,
                child_pid: None,
                resume_id: None,
                log_path: None,
                parent_session_id: None,
            },
        )
        .await
        .unwrap();
        id
    }

    async fn make_spark(pool: &SqlitePool, ws: &str, title: &str) -> String {
        crate::sparks::spark_repo::create(
            pool,
            NewSpark {
                title: title.into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: ws.into(),
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
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn list_active_for_workshop_filters_by_workshop() {
        let pool = fresh_pool().await;
        let sid = make_session(&pool, "ws-a").await;
        let spark_a = make_spark(&pool, "ws-a", "spark a").await;
        let spark_b = make_spark(&pool, "ws-b", "spark b").await;

        // Assign to both sparks
        assign(
            &pool,
            NewHandAssignment {
                session_id: sid.clone(),
                spark_id: spark_a.clone(),
                role: AssignmentRole::Owner,
            },
        )
        .await
        .unwrap();

        let sid2 = make_session(&pool, "ws-b").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: sid2,
                spark_id: spark_b.clone(),
                role: AssignmentRole::Owner,
            },
        )
        .await
        .unwrap();

        // list_active returns both
        let all = list_active(&pool).await.unwrap();
        assert_eq!(all.len(), 2);

        // list_active_for_workshop returns only ws-a's
        let ws_a = list_active_for_workshop(&pool, "ws-a").await.unwrap();
        assert_eq!(ws_a.len(), 1);
        assert_eq!(ws_a[0].spark_id, spark_a);

        let ws_b = list_active_for_workshop(&pool, "ws-b").await.unwrap();
        assert_eq!(ws_b.len(), 1);
        assert_eq!(ws_b[0].spark_id, spark_b);
    }
}
