//! Tests for migration `013_assignments.sql`.

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

    let row: (String, String, Option<String>, Option<String>, i64) = sqlx::query_as(
        "SELECT actor_id, assignment_phase, source_branch, target_branch, event_version
         FROM assignments WHERE assignment_id = 'asgn-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.0, "sess-1234abcd");
    assert_eq!(row.1, "assigned");
    assert_eq!(row.2.as_deref(), Some("hand/sess-123"));
    assert_eq!(row.3.as_deref(), Some("main"));
    assert_eq!(row.4, 0);
}

#[sqlx::test]
async fn legacy_hand_assignments_table_still_works(pool: sqlx::SqlitePool) {
    seed_session_and_spark(&pool, "sess-viewtest1", "sp-view-1").await;

    sqlx::query(
        "INSERT INTO hand_assignments (session_id, spark_id, status, role, assigned_at)
         VALUES ('sess-viewtest1', 'sp-view-1', 'active', 'owner', '2026-04-09T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("insert legacy hand assignment");

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
async fn migrated_rows_can_be_copied_from_hand_assignments(pool: sqlx::SqlitePool) {
    seed_session_and_spark(&pool, "sess-migrate1", "sp-migrate-1").await;

    sqlx::query(
        "INSERT INTO hand_assignments (
            session_id, spark_id, status, role, assigned_at, completed_at
         ) VALUES (
            'sess-migrate1', 'sp-migrate-1', 'completed', 'owner',
            '2026-04-09T00:00:00Z', '2026-04-09T01:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .expect("insert legacy row");

    sqlx::query(
        "INSERT INTO assignments (
            assignment_id, spark_id, actor_id, assignment_phase,
            source_branch, target_branch, event_version, created_at, updated_at
         )
         SELECT
            'asgn-migrated-' || id,
            spark_id,
            session_id,
            CASE status WHEN 'completed' THEN 'merged' ELSE 'assigned' END,
            'hand/' || substr(session_id, 1, 8),
            'main',
            0,
            assigned_at,
            COALESCE(completed_at, last_heartbeat_at, assigned_at)
         FROM hand_assignments
         WHERE spark_id = 'sp-migrate-1'",
    )
    .execute(&pool)
    .await
    .expect("copy legacy row into canonical assignments");

    let row: (String, String, String) = sqlx::query_as(
        "SELECT actor_id, assignment_phase, target_branch
         FROM assignments WHERE spark_id = 'sp-migrate-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.0, "sess-migrate1");
    assert_eq!(row.1, "merged");
    assert_eq!(row.2, "main");
}
