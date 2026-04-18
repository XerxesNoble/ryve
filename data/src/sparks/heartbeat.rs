// SPDX-License-Identifier: AGPL-3.0-or-later

//! Durable heartbeat emission for active Hand assignments.
//!
//! Per parent epic ryve-cf05fd85 [sp-85034c27], a spawned Hand emits a
//! `HeartbeatReceived` event every `heartbeat_interval_secs` while its
//! assignment is active. This module owns the single write path that
//! both stamps `assignments.last_heartbeat_at` and appends a row to
//! `event_outbox` so the projector / relay subscribers see the same
//! timestamp. Both writes share one transaction: a crash between them
//! cannot leave the event logged without the column advancing (or
//! vice versa).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use super::error::SparksError;

/// `event_type` tag written into `event_outbox.event_type` for a
/// `HeartbeatReceived` event. Kept here so producers (this module) and
/// consumers (the projector, the relay subscribers) pin one value.
pub const HEARTBEAT_EVENT_TYPE: &str = "heartbeat_received";

/// Canonical schema version stamped on every heartbeat row in
/// `event_outbox`. Matches `projector::CURRENT_SCHEMA_VERSION` — bumping
/// this is the migration boundary for downstream consumers.
pub const HEARTBEAT_SCHEMA_VERSION: i64 = 1;

/// Payload shape serialised to `event_outbox.payload`. Mirrors the
/// fields on `projector::Event::HeartbeatReceived` so the pure projector
/// deserialises the row without any extra translation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HeartbeatEventPayload {
    HeartbeatReceived {
        event_id: String,
        schema_version: i64,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
    },
}

/// Outcome of [`emit_heartbeat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// The assignment was still `active`: `last_heartbeat_at` was
    /// advanced and a `HeartbeatReceived` row was appended to
    /// `event_outbox`.
    Emitted,
    /// No active assignment for this (session, spark) pair. The caller
    /// should stop its heartbeat loop — the session has ended, the
    /// claim was released, or the spark was closed.
    AssignmentInactive,
}

impl HeartbeatOutcome {
    /// True when the heartbeater loop should keep running.
    pub fn should_continue(self) -> bool {
        matches!(self, Self::Emitted)
    }
}

/// Emit a single heartbeat for the assignment identified by
/// `(session_id, spark_id)`.
///
/// Advances `assignments.last_heartbeat_at` AND appends a
/// `HeartbeatReceived` row to `event_outbox`, all inside one
/// transaction. If no active assignment matches, returns
/// [`HeartbeatOutcome::AssignmentInactive`] without writing anything —
/// the caller's loop treats this as the clean exit condition.
pub async fn emit_heartbeat(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
) -> Result<HeartbeatOutcome, SparksError> {
    let mut tx = pool.begin().await?;

    let assignment_id: Option<String> = sqlx::query_scalar(
        "SELECT assignment_id FROM assignments \
         WHERE session_id = ? AND spark_id = ? AND status = 'active' \
         LIMIT 1",
    )
    .bind(session_id)
    .bind(spark_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(assignment_id) = assignment_id else {
        tx.rollback().await?;
        return Ok(HeartbeatOutcome::AssignmentInactive);
    };

    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE assignments SET last_heartbeat_at = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(session_id)
    .bind(spark_id)
    .execute(&mut *tx)
    .await?;

    let event_id = format!("evt-{}", Uuid::new_v4());
    let payload = HeartbeatEventPayload::HeartbeatReceived {
        event_id: event_id.clone(),
        schema_version: HEARTBEAT_SCHEMA_VERSION,
        timestamp: now.clone(),
        assignment_id: assignment_id.clone(),
        actor_id: session_id.to_string(),
    };
    let payload_json =
        serde_json::to_string(&payload).map_err(|e| SparksError::Serialization(e.to_string()))?;

    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(HEARTBEAT_SCHEMA_VERSION)
    .bind(&now)
    .bind(&assignment_id)
    .bind(session_id)
    .bind(HEARTBEAT_EVENT_TYPE)
    .bind(&payload_json)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(HeartbeatOutcome::Emitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparks::assignment_repo::{assign, complete};
    use crate::sparks::types::{
        AssignmentRole, NewAgentSession, NewHandAssignment, NewSpark, SparkType,
    };
    use crate::sparks::{agent_session_repo, spark_repo};

    async fn fresh_pool() -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("ryve-hb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::open_sparks_db(&dir).await.unwrap()
    }

    async fn make_session(pool: &SqlitePool, ws: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        agent_session_repo::create(
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
                archetype_id: None,
            },
        )
        .await
        .unwrap();
        id
    }

    async fn make_spark(pool: &SqlitePool, ws: &str, title: &str) -> String {
        spark_repo::create(
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
    async fn emit_heartbeat_appends_event_row_and_stamps_column() {
        let pool = fresh_pool().await;
        let sess = make_session(&pool, "ws-a").await;
        let spark = make_spark(&pool, "ws-a", "beating").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: sess.clone(),
                spark_id: spark.clone(),
                role: AssignmentRole::Owner,
                actor_id: None,
            },
        )
        .await
        .unwrap();

        // Two beats back-to-back — both should be `Emitted` and surface on the outbox.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let first = emit_heartbeat(&pool, &sess, &spark).await.unwrap();
        assert_eq!(first, HeartbeatOutcome::Emitted);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let second = emit_heartbeat(&pool, &sess, &spark).await.unwrap();
        assert_eq!(second, HeartbeatOutcome::Emitted);

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox \
             WHERE event_type = ? AND actor_id = ?",
        )
        .bind(HEARTBEAT_EVENT_TYPE)
        .bind(&sess)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 2, "expected two outbox rows after two emits");

        let last_heartbeat: Option<String> = sqlx::query_scalar(
            "SELECT last_heartbeat_at FROM assignments \
             WHERE session_id = ? AND spark_id = ?",
        )
        .bind(&sess)
        .bind(&spark)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            last_heartbeat.is_some(),
            "last_heartbeat_at must be stamped after emit"
        );
    }

    #[tokio::test]
    async fn emit_heartbeat_returns_inactive_and_writes_nothing_for_completed_assignment() {
        let pool = fresh_pool().await;
        let sess = make_session(&pool, "ws-a").await;
        let spark = make_spark(&pool, "ws-a", "done").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: sess.clone(),
                spark_id: spark.clone(),
                role: AssignmentRole::Owner,
                actor_id: None,
            },
        )
        .await
        .unwrap();

        complete(&pool, &sess, &spark).await.unwrap();

        let outcome = emit_heartbeat(&pool, &sess, &spark).await.unwrap();
        assert_eq!(outcome, HeartbeatOutcome::AssignmentInactive);
        assert!(
            !outcome.should_continue(),
            "should_continue must be false on inactive"
        );

        // Nothing should have been written to the outbox: the early return
        // rolled back the read-only transaction.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox \
             WHERE event_type = ? AND actor_id = ?",
        )
        .bind(HEARTBEAT_EVENT_TYPE)
        .bind(&sess)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "inactive assignment must not produce outbox rows");
    }
}
