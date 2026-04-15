use data::sparks::assign_repo;
use data::sparks::types::*;

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_create_assignment(pool: sqlx::SqlitePool) {
    let assignment = assign_repo::create_assignment(
        &pool,
        NewPhaseAssignment {
            spark_id: "sp-0001".to_string(),
            actor_id: "actor-1".to_string(),
            assignment_phase: AssignmentPhase::Assigned,
            source_branch: Some("hand/abc123".to_string()),
            target_branch: Some("main".to_string()),
        },
    )
    .await
    .unwrap();

    assert!(assignment.assignment_id.starts_with("asgn-"));
    assert_eq!(assignment.spark_id, "sp-0001");
    assert_eq!(assignment.actor_id, "actor-1");
    assert_eq!(assignment.assignment_phase, "assigned");
    assert_eq!(assignment.source_branch.as_deref(), Some("hand/abc123"));
    assert_eq!(assignment.target_branch.as_deref(), Some("main"));
    assert_eq!(assignment.event_version, 1);
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_get_assignment(pool: sqlx::SqlitePool) {
    let created = assign_repo::create_assignment(
        &pool,
        NewPhaseAssignment {
            spark_id: "sp-0001".to_string(),
            actor_id: "actor-1".to_string(),
            assignment_phase: AssignmentPhase::InProgress,
            source_branch: None,
            target_branch: None,
        },
    )
    .await
    .unwrap();

    let fetched = assign_repo::get_assignment(&pool, &created.assignment_id)
        .await
        .unwrap();
    assert_eq!(fetched.assignment_id, created.assignment_id);
    assert_eq!(fetched.assignment_phase, "in_progress");
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_get_assignment_not_found(pool: sqlx::SqlitePool) {
    let result = assign_repo::get_assignment(&pool, "asgn-nonexistent").await;
    assert!(result.is_err());
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_list_assignments_for_spark(pool: sqlx::SqlitePool) {
    // Create two assignments for the same spark
    assign_repo::create_assignment(
        &pool,
        NewPhaseAssignment {
            spark_id: "sp-0002".to_string(),
            actor_id: "actor-1".to_string(),
            assignment_phase: AssignmentPhase::Assigned,
            source_branch: None,
            target_branch: None,
        },
    )
    .await
    .unwrap();

    assign_repo::create_assignment(
        &pool,
        NewPhaseAssignment {
            spark_id: "sp-0002".to_string(),
            actor_id: "actor-2".to_string(),
            assignment_phase: AssignmentPhase::InProgress,
            source_branch: None,
            target_branch: None,
        },
    )
    .await
    .unwrap();

    let list = assign_repo::list_assignments_for_spark(&pool, "sp-0002")
        .await
        .unwrap();
    assert_eq!(list.len(), 2);

    // Empty list for spark with no assignments
    let empty = assign_repo::list_assignments_for_spark(&pool, "sp-0003")
        .await
        .unwrap();
    assert!(empty.is_empty());
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_update_assignment(pool: sqlx::SqlitePool) {
    let created = assign_repo::create_assignment(
        &pool,
        NewPhaseAssignment {
            spark_id: "sp-0001".to_string(),
            actor_id: "actor-1".to_string(),
            assignment_phase: AssignmentPhase::Assigned,
            source_branch: None,
            target_branch: None,
        },
    )
    .await
    .unwrap();

    let updated = assign_repo::update_assignment(
        &pool,
        &created.assignment_id,
        UpdatePhaseAssignment {
            event_version: Some(2),
            source_branch: Some(Some("hand/xyz".to_string())),
            target_branch: Some(Some("main".to_string())),
        },
    )
    .await
    .unwrap();

    assert_eq!(updated.event_version, 2);
    assert_eq!(updated.source_branch.as_deref(), Some("hand/xyz"));
    assert_eq!(updated.target_branch.as_deref(), Some("main"));
    // Phase should remain unchanged (raw update doesn't touch phase)
    assert_eq!(updated.assignment_phase, "assigned");
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_update_assignment_partial(pool: sqlx::SqlitePool) {
    let created = assign_repo::create_assignment(
        &pool,
        NewPhaseAssignment {
            spark_id: "sp-0001".to_string(),
            actor_id: "actor-1".to_string(),
            assignment_phase: AssignmentPhase::Assigned,
            source_branch: Some("hand/original".to_string()),
            target_branch: Some("main".to_string()),
        },
    )
    .await
    .unwrap();

    // Only update event_version, leave branches unchanged
    let updated = assign_repo::update_assignment(
        &pool,
        &created.assignment_id,
        UpdatePhaseAssignment {
            event_version: Some(5),
            source_branch: None,
            target_branch: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(updated.event_version, 5);
    assert_eq!(updated.source_branch.as_deref(), Some("hand/original"));
    assert_eq!(updated.target_branch.as_deref(), Some("main"));
}
