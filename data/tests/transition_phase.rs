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
        "INSERT INTO assignments (assignment_id, spark_id, actor_id, session_id, status, role, event_version, assigned_at, assignment_phase, created_at, updated_at)
         VALUES ('asgn-trans-01', 'sp-trans-1', 'sess-trans-01', 'sess-trans-01', 'active', 'owner', 0, '2026-04-09T00:00:00Z', 'assigned', '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z')",
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
    assert!(
        updated.phase_event_id.is_some(),
        "phase_event_id should reference the appended event"
    );
}

#[sqlx::test]
async fn transition_phase_appends_event_atomically(pool: sqlx::SqlitePool) {
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

    let event_id = updated.phase_event_id.expect("event_id should be set");
    let event = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT field_name, old_value, new_value FROM events WHERE id = ?",
    )
    .bind(event_id)
    .fetch_one(&pool)
    .await
    .expect("event should exist");

    assert_eq!(event.0, "assignment_phase");
    assert_eq!(event.1.as_deref(), Some("assigned"));
    assert_eq!(event.2.as_deref(), Some("in_progress"));
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
        assert!(
            updated.phase_event_id.is_some(),
            "step {i}: event must be recorded"
        );
    }

    let events = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM events WHERE spark_id = 'sp-trans-1' AND field_name = 'assignment_phase'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 5, "all 5 transitions should produce events");
}
