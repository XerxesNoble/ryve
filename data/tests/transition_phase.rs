use data::sparks::transition;
use data::sparks::types::{AssignmentPhase, TransitionActorRole};

async fn seed_assignment(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query(
        "INSERT INTO agent_sessions (id, workshop_id, agent_name, agent_command, status, started_at)
         VALUES ('sess-trans-01', 'ws-test', 'claude', 'claude', 'active', '2026-04-09T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed agent_session");

    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, workshop_id, metadata, created_at, updated_at)
         VALUES ('sp-trans-1', 'test spark', '', 'open', 2, 'task', 'ws-test', '{}', '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed spark");

    sqlx::query(
        "INSERT INTO assignments (session_id, spark_id, status, role, phase, event_version, assigned_at, assignment_phase)
         VALUES ('sess-trans-01', 'sp-trans-1', 'active', 'owner', 'assigned', 0, '2026-04-09T00:00:00Z', 'assigned')",
    )
    .execute(pool)
    .await
    .expect("seed assignment");

    sqlx::query_scalar::<_, i64>("SELECT id FROM assignments WHERE spark_id = 'sp-trans-1'")
        .fetch_one(pool)
        .await
        .expect("get assignment id")
}

#[sqlx::test]
async fn transition_phase_assigned_to_in_progress(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    let updated = transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "actor-hand-1",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("transition should succeed");

    assert_eq!(
        updated.assignment_phase.as_deref(),
        Some("in_progress"),
        "assignment_phase should be updated to in_progress"
    );
    assert!(
        updated.phase_changed_at.is_some(),
        "phase_changed_at should be set"
    );
    assert_eq!(
        updated.phase_changed_by.as_deref(),
        Some("actor-hand-1"),
        "phase_changed_by should record the actor"
    );
    assert_eq!(
        updated.phase_actor_role.as_deref(),
        Some("hand"),
        "phase_actor_role should record the role"
    );
    assert_eq!(
        updated.phase_event_id,
        Some(1),
        "phase_event_id should record the event"
    );
}

#[sqlx::test]
async fn transition_phase_full_happy_path(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    let steps: &[(AssignmentPhase, AssignmentPhase, TransitionActorRole, &str)] = &[
        (
            AssignmentPhase::Assigned,
            AssignmentPhase::InProgress,
            TransitionActorRole::Hand,
            "hand-1",
        ),
        (
            AssignmentPhase::InProgress,
            AssignmentPhase::AwaitingReview,
            TransitionActorRole::Hand,
            "hand-1",
        ),
        (
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Approved,
            TransitionActorRole::ReviewerHand,
            "reviewer-1",
        ),
        (
            AssignmentPhase::Approved,
            AssignmentPhase::ReadyForMerge,
            TransitionActorRole::MergeHand,
            "merger-1",
        ),
        (
            AssignmentPhase::ReadyForMerge,
            AssignmentPhase::Merged,
            TransitionActorRole::MergeHand,
            "merger-1",
        ),
    ];

    for (i, (from, to, role, actor)) in steps.iter().enumerate() {
        let updated = transition::transition_assignment_phase(
            &pool,
            assignment_id,
            actor,
            *role,
            *to,
            *from,
            (i + 1) as i64,
        )
        .await
        .unwrap_or_else(|e| panic!("step {i}: {from:?} → {to:?} failed: {e}"));

        assert_eq!(
            updated.assignment_phase.as_deref(),
            Some(to.as_str()),
            "step {i}: phase should be {to:?}"
        );
    }
}
