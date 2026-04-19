// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hand-spark assignment with liveness-aware claims, heartbeat, and handoff.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::*;

const HA_SELECT_COLS: &str = "id, session_id, spark_id, status, role, assigned_at, last_heartbeat_at, \
     lease_expires_at, completed_at, handoff_to, handoff_reason, liveness, \
     github_artifact_branch, github_artifact_pr_number";

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
    let actor_id = new.actor_id.as_deref().unwrap_or(new.session_id.as_str());

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO assignments \
         (assignment_id, spark_id, actor_id, session_id, status, role, assigned_at, \
          last_heartbeat_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'active', ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(&asgn_id)
    .bind(&new.spark_id)
    .bind(actor_id)
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

/// Update the `last_heartbeat_at` timestamp for an active assignment.
/// Returns the number of rows touched so callers can distinguish "no
/// active claim" (0) from "beat recorded" (1). Parent epic ryve-cf05fd85
/// composes this with the watchdog and the outbox event writer to keep
/// liveness state authoritative.
pub async fn record_heartbeat(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
) -> Result<u64, SparksError> {
    let now = Utc::now().to_rfc3339();

    let result = sqlx::query(
        "UPDATE assignments SET last_heartbeat_at = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(session_id)
    .bind(spark_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Persist the watchdog's liveness verdict for an active assignment.
/// Only the watchdog and Head/Director overrides should call this —
/// Hands never set their own liveness (see epic ryve-cf05fd85 invariant).
/// Returns the number of rows updated.
pub async fn set_liveness(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
    liveness: AssignmentLiveness,
) -> Result<u64, SparksError> {
    let result = sqlx::query(
        "UPDATE assignments SET liveness = ? \
         WHERE session_id = ? AND spark_id = ? AND status = 'active'",
    )
    .bind(liveness.as_str())
    .bind(session_id)
    .bind(spark_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Bump `repair_cycle_count` by one for an active assignment and return
/// the new value. Called by the phase-transition path on each
/// `Rejected -> InRepair` edge so the watchdog can escalate to Stuck once
/// the workshop's repair_cycle_limit is exceeded.
pub async fn increment_repair_cycle(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
) -> Result<i64, SparksError> {
    let row = sqlx::query_scalar::<_, i64>(
        "UPDATE assignments SET repair_cycle_count = repair_cycle_count + 1 \
         WHERE session_id = ? AND spark_id = ? AND status = 'active' \
         RETURNING repair_cycle_count",
    )
    .bind(session_id)
    .bind(spark_id)
    .fetch_optional(pool)
    .await?;

    row.ok_or_else(|| {
        SparksError::NotFound(format!(
            "active assignment for session {session_id} on spark {spark_id}"
        ))
    })
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
         a.last_heartbeat_at, a.lease_expires_at, a.completed_at, a.handoff_to, a.handoff_reason, \
         a.liveness, a.github_artifact_branch, a.github_artifact_pr_number \
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

/// Look up the actor_id recorded on the most recent active assignment for a
/// session. Returns `None` if the session has no active assignment — used by
/// spawn-time cross-user enforcement to derive the parent Hand's actor
/// without threading extra state through every call site.
pub async fn actor_id_for_session(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<String>, SparksError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT actor_id FROM assignments \
         WHERE session_id = ? AND status = 'active' \
         ORDER BY assigned_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?)
}

/// A spark that is still open in the workgraph but whose active owning
/// Hand has exited. Returned by [`find_orphaned_claims`] so a sweeper
/// can emit a visible signal (ember/comment) instead of letting the
/// spark sit `in_progress` indefinitely.
///
/// Motivation (sp-312b98ad): build Heads exit right after spawning the
/// Merger. If the Merger subprocess dies before it closes its merge
/// spark, the `assignments` row stays `active` even though
/// `agent_sessions` flips to `ended`. Nobody is polling, so the merge
/// stalls silently. This type makes that orphan queryable.
#[derive(Debug, Clone)]
pub struct OrphanedClaim {
    pub spark_id: String,
    pub spark_title: String,
    pub spark_status: String,
    pub role: String,
    pub session_id: String,
    pub assigned_at: String,
    pub last_heartbeat_at: Option<String>,
    pub session_ended_at: Option<String>,
}

/// Find every active assignment whose agent session has already ended
/// and whose spark is still open (not closed). Scoped to one workshop.
///
/// The workgraph invariant this protects: every open spark with an
/// `active` owner must be owned by a live session. When the session
/// dies without calling `assignment_repo::complete` or `abandon`, we
/// end up in a silent-stall state. This query is the authoritative
/// detector for that state.
pub async fn find_orphaned_claims(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<OrphanedClaim>, SparksError> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT a.spark_id, s.title, s.status, a.role, a.session_id, \
                a.assigned_at, a.last_heartbeat_at, ag.ended_at \
         FROM assignments a \
         INNER JOIN sparks s ON s.id = a.spark_id \
         INNER JOIN agent_sessions ag ON ag.id = a.session_id \
         WHERE a.status = 'active' \
           AND ag.status = 'ended' \
           AND s.status != 'closed' \
           AND s.workshop_id = ? \
         ORDER BY a.assigned_at ASC",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                spark_id,
                spark_title,
                spark_status,
                role,
                session_id,
                assigned_at,
                last_heartbeat_at,
                session_ended_at,
            )| OrphanedClaim {
                spark_id,
                spark_title,
                spark_status,
                role,
                session_id,
                assigned_at,
                last_heartbeat_at,
                session_ended_at,
            },
        )
        .collect())
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
                archetype_id: None,
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
    async fn find_orphaned_claims_flags_active_assignments_on_ended_sessions() {
        // [sp-312b98ad] Orphaned merge/PR handoff: the Merger session
        // ends without closing its spark, the assignment row is left
        // `active`, and nothing polls the crew anymore. This query is
        // the authoritative detector for that silent-stall state.
        let pool = fresh_pool().await;

        // Orphan: session will be ended, spark stays open.
        let orphan_sess = make_session(&pool, "ws-a").await;
        let orphan_spark = make_spark(&pool, "ws-a", "stalled merge").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: orphan_sess.clone(),
                spark_id: orphan_spark.clone(),
                role: AssignmentRole::Merger,
                actor_id: None,
            },
        )
        .await
        .unwrap();
        crate::sparks::agent_session_repo::end_session(&pool, &orphan_sess)
            .await
            .unwrap();

        // Healthy control: live session, open spark — must NOT be flagged.
        let live_sess = make_session(&pool, "ws-a").await;
        let live_spark = make_spark(&pool, "ws-a", "live work").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: live_sess,
                spark_id: live_spark.clone(),
                role: AssignmentRole::Owner,
                actor_id: None,
            },
        )
        .await
        .unwrap();

        // Cross-workshop control: session ended in ws-b, must not leak into ws-a.
        let other_sess = make_session(&pool, "ws-b").await;
        let other_spark = make_spark(&pool, "ws-b", "other workshop").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: other_sess.clone(),
                spark_id: other_spark,
                role: AssignmentRole::Owner,
                actor_id: None,
            },
        )
        .await
        .unwrap();
        crate::sparks::agent_session_repo::end_session(&pool, &other_sess)
            .await
            .unwrap();

        let orphans = find_orphaned_claims(&pool, "ws-a").await.unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "exactly one orphan expected in ws-a; got {orphans:?}"
        );
        assert_eq!(orphans[0].spark_id, orphan_spark);
        assert_eq!(orphans[0].session_id, orphan_sess);
        assert_eq!(orphans[0].role, "merger");
        assert!(
            orphans[0].session_ended_at.is_some(),
            "session_ended_at must be surfaced so the sweeper can render it"
        );
    }

    #[tokio::test]
    async fn find_orphaned_claims_ignores_closed_sparks_and_completed_assignments() {
        let pool = fresh_pool().await;

        // Assignment completed cleanly — not an orphan.
        let clean_sess = make_session(&pool, "ws-a").await;
        let clean_spark = make_spark(&pool, "ws-a", "done cleanly").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: clean_sess.clone(),
                spark_id: clean_spark.clone(),
                role: AssignmentRole::Owner,
                actor_id: None,
            },
        )
        .await
        .unwrap();
        complete(&pool, &clean_sess, &clean_spark).await.unwrap();
        crate::sparks::agent_session_repo::end_session(&pool, &clean_sess)
            .await
            .unwrap();

        // Spark closed, session ended — not an orphan (spark already terminal).
        let closed_sess = make_session(&pool, "ws-a").await;
        let closed_spark = make_spark(&pool, "ws-a", "closed spark").await;
        assign(
            &pool,
            NewHandAssignment {
                session_id: closed_sess.clone(),
                spark_id: closed_spark.clone(),
                role: AssignmentRole::Owner,
                actor_id: None,
            },
        )
        .await
        .unwrap();
        crate::sparks::spark_repo::close(&pool, &closed_spark, "completed", "test")
            .await
            .unwrap();
        crate::sparks::agent_session_repo::end_session(&pool, &closed_sess)
            .await
            .unwrap();

        let orphans = find_orphaned_claims(&pool, "ws-a").await.unwrap();
        assert!(
            orphans.is_empty(),
            "completed assignments and closed sparks must not register as orphans; got {orphans:?}"
        );
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
                actor_id: None,
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
                actor_id: None,
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

    async fn read_assignment_row(
        pool: &SqlitePool,
        session_id: &str,
        spark_id: &str,
    ) -> (Option<String>, i64, String) {
        sqlx::query_as::<_, (Option<String>, i64, String)>(
            "SELECT last_heartbeat_at, repair_cycle_count, liveness \
             FROM assignments WHERE session_id = ? AND spark_id = ?",
        )
        .bind(session_id)
        .bind(spark_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn record_heartbeat_stamps_active_claim_and_ignores_inactive() {
        let pool = fresh_pool().await;
        let sess = make_session(&pool, "ws-a").await;
        let spark = make_spark(&pool, "ws-a", "beating").await;

        // Freshly-assigned row: last_heartbeat_at starts equal to assigned_at,
        // liveness starts healthy, repair_cycle_count starts zero.
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
        let (before_heartbeat, cycles, liveness) = read_assignment_row(&pool, &sess, &spark).await;
        assert!(before_heartbeat.is_some(), "assign stamps an initial beat");
        assert_eq!(cycles, 0);
        assert_eq!(liveness, "healthy");

        // Sleep a little so the new timestamp is strictly greater than the
        // assign-time one; RFC3339 strings compare lexicographically.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let touched = record_heartbeat(&pool, &sess, &spark).await.unwrap();
        assert_eq!(touched, 1, "active claim must be updated exactly once");

        let (after_heartbeat, _, _) = read_assignment_row(&pool, &sess, &spark).await;
        assert!(
            after_heartbeat > before_heartbeat,
            "record_heartbeat must advance last_heartbeat_at \
             (before={before_heartbeat:?}, after={after_heartbeat:?})"
        );

        // Completed assignment: status!='active', so record_heartbeat is a no-op.
        complete(&pool, &sess, &spark).await.unwrap();
        let touched = record_heartbeat(&pool, &sess, &spark).await.unwrap();
        assert_eq!(
            touched, 0,
            "record_heartbeat must not touch non-active rows"
        );
    }

    #[tokio::test]
    async fn set_liveness_updates_only_the_liveness_column() {
        let pool = fresh_pool().await;
        let sess = make_session(&pool, "ws-a").await;
        let spark = make_spark(&pool, "ws-a", "liveness").await;

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

        let (hb_before, cycles_before, initial) = read_assignment_row(&pool, &sess, &spark).await;
        assert_eq!(initial, "healthy");

        // Healthy -> AtRisk -> Stuck must round-trip through storage.
        for target in [AssignmentLiveness::AtRisk, AssignmentLiveness::Stuck] {
            let touched = set_liveness(&pool, &sess, &spark, target).await.unwrap();
            assert_eq!(touched, 1);
            let (_, _, persisted) = read_assignment_row(&pool, &sess, &spark).await;
            assert_eq!(
                AssignmentLiveness::from_str(&persisted),
                Some(target),
                "persisted liveness must round-trip (got {persisted})",
            );
        }

        // Sibling columns must not move — liveness is orthogonal to heartbeat
        // and repair-cycle state.
        let (hb_after, cycles_after, _) = read_assignment_row(&pool, &sess, &spark).await;
        assert_eq!(
            hb_after, hb_before,
            "liveness write must not touch heartbeat"
        );
        assert_eq!(cycles_after, cycles_before);

        // Non-active row is ignored.
        complete(&pool, &sess, &spark).await.unwrap();
        let touched = set_liveness(&pool, &sess, &spark, AssignmentLiveness::Healthy)
            .await
            .unwrap();
        assert_eq!(touched, 0);
    }

    #[tokio::test]
    async fn increment_repair_cycle_bumps_counter_and_errors_when_missing() {
        let pool = fresh_pool().await;
        let sess = make_session(&pool, "ws-a").await;
        let spark = make_spark(&pool, "ws-a", "cycles").await;

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

        // Counter starts at zero and advances monotonically.
        for expected in 1..=3 {
            let n = increment_repair_cycle(&pool, &sess, &spark).await.unwrap();
            assert_eq!(n, expected, "repair_cycle_count must be monotonic");
        }
        let (_, persisted_cycles, _) = read_assignment_row(&pool, &sess, &spark).await;
        assert_eq!(persisted_cycles, 3);

        // A session with no active claim must surface NotFound rather than
        // silently succeeding — the caller is the repair-path transition,
        // which needs to know it missed.
        let err = increment_repair_cycle(&pool, "ghost-sess", "ghost-spark")
            .await
            .expect_err("missing claim must error");
        assert!(
            matches!(err, SparksError::NotFound(_)),
            "expected NotFound for ghost session, got {err:?}"
        );

        // Completing the assignment deactivates it; further increments also
        // surface NotFound (status guard in WHERE clause).
        complete(&pool, &sess, &spark).await.unwrap();
        let err = increment_repair_cycle(&pool, &sess, &spark)
            .await
            .expect_err("inactive claim must error");
        assert!(matches!(err, SparksError::NotFound(_)));
    }
}
