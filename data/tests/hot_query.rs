use data::sparks::types::*;
use data::sparks::{bond_repo, graph, spark_repo};

async fn make_spark_with(
    pool: &sqlx::SqlitePool,
    title: &str,
    priority: i32,
    status: &str,
) -> String {
    let spark = spark_repo::create(
        pool,
        NewSpark {
            title: title.to_string(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority,
            workshop_id: "ws-hot".to_string(),
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
    .unwrap();

    if status != "open" {
        spark_repo::update(
            pool,
            &spark.id,
            UpdateSpark {
                status: SparkStatus::from_str(status).map(|s| s),
                ..Default::default()
            },
            "test",
        )
        .await
        .unwrap();
    }

    spark.id
}

#[sqlx::test]
async fn test_hot_excludes_blocked(pool: sqlx::SqlitePool) {
    let blocker = make_spark_with(&pool, "Blocker", 0, "open").await;
    let blocked = make_spark_with(&pool, "Blocked", 1, "open").await;

    bond_repo::create(&pool, &blocker, &blocked, BondType::Blocks)
        .await
        .unwrap();

    let hot = graph::hot_sparks(&pool, "ws-hot").await.unwrap();
    let hot_ids: Vec<&str> = hot.iter().map(|s| s.id.as_str()).collect();

    assert!(hot_ids.contains(&blocker.as_str()));
    assert!(!hot_ids.contains(&blocked.as_str()));
}

#[sqlx::test]
async fn test_hot_includes_unblocked_after_close(pool: sqlx::SqlitePool) {
    let blocker = make_spark_with(&pool, "Blocker", 0, "open").await;
    let blocked = make_spark_with(&pool, "Blocked", 1, "open").await;

    bond_repo::create(&pool, &blocker, &blocked, BondType::Blocks)
        .await
        .unwrap();

    // Close the blocker
    spark_repo::close(&pool, &blocker, "done", "test")
        .await
        .unwrap();

    let hot = graph::hot_sparks(&pool, "ws-hot").await.unwrap();
    let hot_ids: Vec<&str> = hot.iter().map(|s| s.id.as_str()).collect();

    assert!(hot_ids.contains(&blocked.as_str()));
}

#[sqlx::test]
async fn test_hot_excludes_deferred(pool: sqlx::SqlitePool) {
    let normal = make_spark_with(&pool, "Normal", 1, "open").await;
    let deferred = make_spark_with(&pool, "Deferred", 1, "open").await;

    // Defer far into the future
    spark_repo::update(
        &pool,
        &deferred,
        UpdateSpark {
            defer_until: Some(Some("2099-01-01T00:00:00Z".to_string())),
            ..Default::default()
        },
        "test",
    )
    .await
    .unwrap();

    let hot = graph::hot_sparks(&pool, "ws-hot").await.unwrap();
    let hot_ids: Vec<&str> = hot.iter().map(|s| s.id.as_str()).collect();

    assert!(hot_ids.contains(&normal.as_str()));
    assert!(!hot_ids.contains(&deferred.as_str()));
}

#[sqlx::test]
async fn test_hot_priority_ordering(pool: sqlx::SqlitePool) {
    let p2 = make_spark_with(&pool, "Low", 2, "open").await;
    let p0 = make_spark_with(&pool, "Critical", 0, "open").await;
    let p1 = make_spark_with(&pool, "High", 1, "open").await;

    let hot = graph::hot_sparks(&pool, "ws-hot").await.unwrap();

    assert_eq!(hot[0].id, p0);
    assert_eq!(hot[1].id, p1);
    assert_eq!(hot[2].id, p2);
}

#[sqlx::test]
async fn test_hot_excludes_closed(pool: sqlx::SqlitePool) {
    make_spark_with(&pool, "Open", 1, "open").await;
    make_spark_with(&pool, "Closed", 1, "closed").await;

    let hot = graph::hot_sparks(&pool, "ws-hot").await.unwrap();
    assert_eq!(hot.len(), 1);
    assert_eq!(hot[0].title, "Open");
}

#[sqlx::test]
async fn test_hot_complex_graph(pool: sqlx::SqlitePool) {
    // Build: A blocks B, B blocks C, D is independent, E deferred
    let a = make_spark_with(&pool, "A", 0, "open").await;
    let b = make_spark_with(&pool, "B", 1, "open").await;
    let c = make_spark_with(&pool, "C", 2, "open").await;
    let d = make_spark_with(&pool, "D", 1, "open").await;
    let e = make_spark_with(&pool, "E", 0, "open").await;

    bond_repo::create(&pool, &a, &b, BondType::Blocks)
        .await
        .unwrap();
    bond_repo::create(&pool, &b, &c, BondType::Blocks)
        .await
        .unwrap();

    spark_repo::update(
        &pool,
        &e,
        UpdateSpark {
            defer_until: Some(Some("2099-01-01T00:00:00Z".to_string())),
            ..Default::default()
        },
        "test",
    )
    .await
    .unwrap();

    let hot = graph::hot_sparks(&pool, "ws-hot").await.unwrap();
    let hot_ids: Vec<&str> = hot.iter().map(|s| s.id.as_str()).collect();

    // A is hot (no blockers), D is hot (independent)
    assert!(hot_ids.contains(&a.as_str()));
    assert!(hot_ids.contains(&d.as_str()));
    // B blocked by A, C blocked by B, E deferred
    assert!(!hot_ids.contains(&b.as_str()));
    assert!(!hot_ids.contains(&c.as_str()));
    assert!(!hot_ids.contains(&e.as_str()));
}
