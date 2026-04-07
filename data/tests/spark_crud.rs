use data::sparks::spark_repo;
use data::sparks::types::*;

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_create_spark(pool: sqlx::SqlitePool) {
    let spark = spark_repo::create(
        &pool,
        NewSpark {
            title: "New feature".to_string(),
            description: "A cool feature".to_string(),
            spark_type: SparkType::Feature,
            priority: 1,
            workshop_id: "ws-test".to_string(),
            assignee: Some("alice".to_string()),
            owner: None,
            parent_id: None,
            due_at: None,
            estimated_minutes: Some(60),
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        },
    )
    .await
    .unwrap();

    assert!(spark.id.starts_with("ws-test-"));
    assert_eq!(spark.title, "New feature");
    assert_eq!(spark.status, "open");
    assert_eq!(spark.priority, 1);
    assert_eq!(spark.assignee.as_deref(), Some("alice"));
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_get_spark(pool: sqlx::SqlitePool) {
    let spark = spark_repo::get(&pool, "sp-0001").await.unwrap();
    assert_eq!(spark.title, "Setup CI pipeline");
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_get_not_found(pool: sqlx::SqlitePool) {
    let result = spark_repo::get(&pool, "sp-9999").await;
    assert!(result.is_err());
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_update_spark(pool: sqlx::SqlitePool) {
    let updated = spark_repo::update(
        &pool,
        "sp-0001",
        UpdateSpark {
            title: Some("Updated CI pipeline".to_string()),
            priority: Some(0),
            ..Default::default()
        },
        "alice",
    )
    .await
    .unwrap();

    assert_eq!(updated.title, "Updated CI pipeline");
    assert_eq!(updated.priority, 0);
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_close_spark(pool: sqlx::SqlitePool) {
    let closed = spark_repo::close(&pool, "sp-0001", "done", "alice")
        .await
        .unwrap();

    assert_eq!(closed.status, "closed");
    assert!(closed.closed_at.is_some());
    assert_eq!(closed.closed_reason.as_deref(), Some("done"));
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_delete_spark(pool: sqlx::SqlitePool) {
    spark_repo::delete(&pool, "sp-0005").await.unwrap();
    let result = spark_repo::get(&pool, "sp-0005").await;
    assert!(result.is_err());
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_list_with_filter(pool: sqlx::SqlitePool) {
    let open_sparks = spark_repo::list(
        &pool,
        SparkFilter {
            workshop_id: Some("ws-test".to_string()),
            status: Some(vec![SparkStatus::Open]),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // sp-0001, sp-0002, sp-0003 are open
    assert_eq!(open_sparks.len(), 3);
    // Should be ordered by priority (P0 first)
    assert_eq!(open_sparks[0].id, "sp-0002"); // P0
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_list_by_type(pool: sqlx::SqlitePool) {
    let bugs = spark_repo::list(
        &pool,
        SparkFilter {
            workshop_id: Some("ws-test".to_string()),
            spark_type: Some(SparkType::Bug),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(bugs.len(), 1);
    assert_eq!(bugs[0].id, "sp-0002");
}
