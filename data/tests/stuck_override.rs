//! Integration tests for the Stuck assignment override flow introduced
//! by spark ryve-d649bb6f:
//!
//! - Head/Director can recover Stuck → InProgress and the override reason
//!   is audit-logged in the same transaction.
//! - Non-head (Hand / ReviewerHand / MergeHand) overrides are rejected.
//! - A Stuck assignment blocks its Epic merge with the pre-merge validator.

use data::pre_merge_validator::{AssignmentSnapshot, PreMergeError, validate_epic_assignments};
use data::sparks::error::{SparksError, TransitionError};
use data::sparks::types::{AssignmentPhase, TransitionActorRole};
use data::sparks::{assign_repo, transition};

async fn seed(pool: &sqlx::SqlitePool) -> (i64, String) {
    sqlx::query(
        "INSERT INTO agent_sessions (id, workshop_id, agent_name, agent_command, status, started_at)
         VALUES ('sess-stuck-01', 'ws-test', 'claude', 'claude', 'active', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed agent_session");

    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, workshop_id, metadata, created_at, updated_at)
         VALUES ('sp-stuck-1', 'stuck work', '', 'open', 2, 'task', 'ws-test', '{}', '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed spark");

    sqlx::query(
        "INSERT INTO assignments (assignment_id, spark_id, actor_id, session_id, status, role, event_version, assigned_at, assignment_phase, created_at, updated_at)
         VALUES ('asgn-stuck-01', 'sp-stuck-1', 'actor-hand', 'sess-stuck-01', 'active', 'owner', 0, '2026-04-18T00:00:00Z', 'assigned', '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed assignment");

    let id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM assignments WHERE assignment_id = 'asgn-stuck-01'",
    )
    .fetch_one(pool)
    .await
    .expect("get assignment id");

    (id, "asgn-stuck-01".to_string())
}

/// Walk an assignment from `assigned` to `stuck` using the legal
/// transition path: Hand moves it Assigned → InProgress, then marks
/// itself Stuck.
async fn drive_to_stuck(pool: &sqlx::SqlitePool, assignment_id: i64) {
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "actor-hand",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("assigned → in_progress");

    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "actor-hand",
        TransitionActorRole::Hand,
        AssignmentPhase::Stuck,
        AssignmentPhase::InProgress,
        2,
    )
    .await
    .expect("in_progress → stuck");
}

#[sqlx::test]
async fn head_override_restores_stuck_to_in_progress_and_logs_reason(pool: sqlx::SqlitePool) {
    let (assignment_id, asgn_ext_id) = seed(&pool).await;
    drive_to_stuck(&pool, assignment_id).await;

    let updated = assign_repo::override_stuck_to_in_progress(
        &pool,
        &asgn_ext_id,
        "head-session-A",
        TransitionActorRole::Head,
        "Hand reported library link failure; fixed upstream dep and unblocking",
    )
    .await
    .expect("head override must succeed");

    assert_eq!(
        updated.assignment_phase.as_deref(),
        Some("in_progress"),
        "override must land the assignment in in_progress"
    );
    assert_eq!(
        updated.phase_changed_by.as_deref(),
        Some("head-session-A"),
        "phase_changed_by must name the overriding session"
    );
    assert_eq!(
        updated.phase_actor_role.as_deref(),
        Some("head"),
        "phase_actor_role must record the override role"
    );

    // The override reason lives in a dedicated audit event so the
    // workgraph can surface *why* the phase was forced back.
    let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT field_name, old_value, new_value, reason \
         FROM events \
         WHERE spark_id = 'sp-stuck-1' AND field_name = 'assignment_phase_override'",
    )
    .fetch_all(&pool)
    .await
    .expect("query override events");
    assert_eq!(
        rows.len(),
        1,
        "override must write exactly one assignment_phase_override event"
    );
    let (field_name, old_value, new_value, reason) = &rows[0];
    assert_eq!(field_name, "assignment_phase_override");
    assert_eq!(old_value.as_deref(), Some("stuck"));
    assert_eq!(new_value.as_deref(), Some("in_progress"));
    assert!(
        reason
            .as_deref()
            .unwrap_or("")
            .contains("library link failure"),
        "reason text must be audit-logged verbatim, got {reason:?}"
    );
}

#[sqlx::test]
async fn director_override_also_recovers_stuck(pool: sqlx::SqlitePool) {
    let (assignment_id, asgn_ext_id) = seed(&pool).await;
    drive_to_stuck(&pool, assignment_id).await;

    let updated = assign_repo::override_stuck_to_in_progress(
        &pool,
        &asgn_ext_id,
        "atlas",
        TransitionActorRole::Director,
        "Atlas reassigning the scope after user clarification",
    )
    .await
    .expect("director override must succeed");

    assert_eq!(updated.assignment_phase.as_deref(), Some("in_progress"));
    assert_eq!(updated.phase_actor_role.as_deref(), Some("director"));
}

#[sqlx::test]
async fn non_head_override_is_rejected_with_unauthorized(pool: sqlx::SqlitePool) {
    let (assignment_id, asgn_ext_id) = seed(&pool).await;
    drive_to_stuck(&pool, assignment_id).await;

    for role in &[
        TransitionActorRole::Hand,
        TransitionActorRole::ReviewerHand,
        TransitionActorRole::MergeHand,
    ] {
        let err = assign_repo::override_stuck_to_in_progress(
            &pool,
            &asgn_ext_id,
            "impostor-session",
            *role,
            "unauthorised attempt",
        )
        .await
        .expect_err("non-head/director override must fail");
        match err {
            SparksError::Transition(TransitionError::Unauthorized { role: reported, .. }) => {
                assert_eq!(
                    reported,
                    role.as_str(),
                    "error must name the offending role"
                );
            }
            other => panic!("expected Unauthorized for {role:?}, got {other:?}"),
        }
    }

    // And the assignment must still be Stuck — no partial progress.
    let row = sqlx::query_scalar::<_, String>(
        "SELECT assignment_phase FROM assignments WHERE assignment_id = ?",
    )
    .bind(&asgn_ext_id)
    .fetch_one(&pool)
    .await
    .expect("read assignment phase");
    assert_eq!(
        row, "stuck",
        "rejected overrides must leave the phase untouched"
    );
}

#[sqlx::test]
async fn override_is_idempotent_guard_refuses_non_stuck_phase(pool: sqlx::SqlitePool) {
    let (assignment_id, asgn_ext_id) = seed(&pool).await;
    // Move to InProgress only — never enter Stuck. The override must
    // refuse because the from-phase check fails.
    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "actor-hand",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .unwrap();

    let err = assign_repo::override_stuck_to_in_progress(
        &pool,
        &asgn_ext_id,
        "head-session-A",
        TransitionActorRole::Head,
        "should fail — not stuck",
    )
    .await
    .expect_err("override must reject when from-phase is not Stuck");
    // Expected: PhaseMismatch — expected Stuck, actual InProgress.
    match err {
        SparksError::Transition(TransitionError::PhaseMismatch { expected, actual }) => {
            assert_eq!(expected, "stuck");
            assert_eq!(actual, "in_progress");
        }
        other => panic!("expected PhaseMismatch, got {other:?}"),
    }
}

// ── Pre-merge gate ────────────────────────────────────────

#[test]
fn stuck_assignment_blocks_epic_merge() {
    let snapshots = vec![
        AssignmentSnapshot::new("asgn-a", "approved"),
        AssignmentSnapshot::new("asgn-b", "stuck"),
    ];
    let err = validate_epic_assignments("ryve-epic-merge", &snapshots).unwrap_err();
    match err {
        PreMergeError::EpicHasStuckAssignment {
            epic_spark_id,
            assignment_id,
        } => {
            assert_eq!(epic_spark_id, "ryve-epic-merge");
            assert_eq!(assignment_id, "asgn-b");
        }
        other => panic!("expected EpicHasStuckAssignment, got {other:?}"),
    }
}

#[test]
fn epic_with_only_healthy_assignments_passes_gate() {
    let snapshots = vec![
        AssignmentSnapshot::new("asgn-a", "merged"),
        AssignmentSnapshot::new("asgn-b", "approved"),
        AssignmentSnapshot::new("asgn-c", "ready_for_merge"),
    ];
    validate_epic_assignments("ryve-epic-ok", &snapshots).unwrap();
}
