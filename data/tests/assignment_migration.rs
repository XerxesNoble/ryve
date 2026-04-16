//! Tests for the consolidated assignments table (migrations 013–015).

async fn seed_session_and_spark(pool: &sqlx::SqlitePool, session_id: &str, spark_id: &str) {
    sqlx::query(
        "INSERT INTO agent_sessions (id, workshop_id, agent_name, agent_command, status, started_at)
         VALUES (?, 'ws-test', 'claude', 'claude', 'active', '2026-04-09T00:00:00Z')",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .expect("seed agent_session");

    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, workshop_id, metadata, created_at, updated_at)
         VALUES (?, ?, '', 'open', 2, 'task', 'ws-test', '{}', '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z')",
    )
    .bind(spark_id)
    .bind(spark_id)
    .execute(pool)
    .await
    .expect("seed spark");
}

#[sqlx::test]
async fn assignments_table_has_canonical_columns(pool: sqlx::SqlitePool) {
    seed_session_and_spark(&pool, "sess-1234abcd", "sp-test-1").await;

    sqlx::query(
        "INSERT INTO assignments (
            assignment_id, spark_id, actor_id, assignment_phase,
            source_branch, target_branch, event_version, created_at, updated_at
         ) VALUES (
            'asgn-1', 'sp-test-1', 'sess-1234abcd', 'assigned',
            'hand/sess-123', 'main', 0, '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .expect("insert canonical assignment");

    let row: (String, Option<String>, Option<String>, Option<String>, i64) = sqlx::query_as(
        "SELECT actor_id, assignment_phase, source_branch, target_branch, event_version
         FROM assignments WHERE assignment_id = 'asgn-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.0, "sess-1234abcd");
    assert_eq!(row.1.as_deref(), Some("assigned"));
    assert_eq!(row.2.as_deref(), Some("hand/sess-123"));
    assert_eq!(row.3.as_deref(), Some("main"));
    assert_eq!(row.4, 0);
}

#[sqlx::test]
async fn hand_assignments_view_exposes_assignments(pool: sqlx::SqlitePool) {
    seed_session_and_spark(&pool, "sess-viewtest1", "sp-view-1").await;

    sqlx::query(
        "INSERT INTO assignments (
            assignment_id, spark_id, actor_id, session_id, status, role,
            assigned_at, assignment_phase, created_at, updated_at
         ) VALUES (
            'asgn-view-1', 'sp-view-1', 'sess-viewtest1', 'sess-viewtest1',
            'active', 'owner', '2026-04-09T00:00:00Z', 'assigned',
            '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .expect("insert assignment");

    let row: (String, String, String, String) = sqlx::query_as(
        "SELECT session_id, spark_id, status, role
         FROM hand_assignments WHERE spark_id = 'sp-view-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.0, "sess-viewtest1");
    assert_eq!(row.1, "sp-view-1");
    assert_eq!(row.2, "active");
    assert_eq!(row.3, "owner");
}

#[sqlx::test]
async fn assignments_table_has_phase_tracking_columns(pool: sqlx::SqlitePool) {
    seed_session_and_spark(&pool, "sess-phase1", "sp-phase-1").await;

    sqlx::query(
        "INSERT INTO assignments (
            assignment_id, spark_id, actor_id, assignment_phase,
            phase_changed_at, phase_changed_by, phase_actor_role, phase_event_id,
            created_at, updated_at
         ) VALUES (
            'asgn-phase-1', 'sp-phase-1', 'sess-phase1', 'in_progress',
            '2026-04-09T01:00:00Z', 'hand-1', 'hand', 42,
            '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .expect("insert assignment with phase tracking");

    let row: (Option<String>, Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT phase_changed_at, phase_changed_by, phase_actor_role, phase_event_id
         FROM assignments WHERE assignment_id = 'asgn-phase-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.0.as_deref(), Some("2026-04-09T01:00:00Z"));
    assert_eq!(row.1.as_deref(), Some("hand-1"));
    assert_eq!(row.2.as_deref(), Some("hand"));
    assert_eq!(row.3, Some(42));
}
