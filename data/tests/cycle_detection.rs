use data::sparks::types::*;
use data::sparks::{bond_repo, graph, spark_repo};

async fn make_spark(pool: &sqlx::SqlitePool, id_suffix: &str) -> String {
    let spark = spark_repo::create(
        pool,
        NewSpark {
            title: format!("Spark {id_suffix}"),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: "ws-test".to_string(),
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
    spark.id
}

#[sqlx::test]
async fn test_no_cycle_linear(pool: sqlx::SqlitePool) {
    let a = make_spark(&pool, "A").await;
    let b = make_spark(&pool, "B").await;
    let c = make_spark(&pool, "C").await;

    bond_repo::create(&pool, &a, &b, BondType::Blocks)
        .await
        .unwrap();
    bond_repo::create(&pool, &b, &c, BondType::Blocks)
        .await
        .unwrap();

    // A→B→C is fine, no cycle
    let has_cycle = graph::would_create_cycle(&pool, &a, &c).await.unwrap();
    assert!(!has_cycle); // a→c doesn't create a cycle
}

#[sqlx::test]
async fn test_cycle_detected(pool: sqlx::SqlitePool) {
    let a = make_spark(&pool, "A").await;
    let b = make_spark(&pool, "B").await;
    let c = make_spark(&pool, "C").await;

    bond_repo::create(&pool, &a, &b, BondType::Blocks)
        .await
        .unwrap();
    bond_repo::create(&pool, &b, &c, BondType::Blocks)
        .await
        .unwrap();

    // C→A would create A→B→C→A cycle
    let has_cycle = graph::would_create_cycle(&pool, &c, &a).await.unwrap();
    assert!(has_cycle);
}

#[sqlx::test]
async fn test_cycle_rejected_on_create(pool: sqlx::SqlitePool) {
    let a = make_spark(&pool, "A").await;
    let b = make_spark(&pool, "B").await;
    let c = make_spark(&pool, "C").await;

    bond_repo::create(&pool, &a, &b, BondType::Blocks)
        .await
        .unwrap();
    bond_repo::create(&pool, &b, &c, BondType::Blocks)
        .await
        .unwrap();

    // Creating C→A blocking bond should fail
    let result = bond_repo::create(&pool, &c, &a, BondType::Blocks).await;
    assert!(result.is_err());
}

#[sqlx::test]
async fn test_self_reference_cycle(pool: sqlx::SqlitePool) {
    let a = make_spark(&pool, "A").await;

    let has_cycle = graph::would_create_cycle(&pool, &a, &a).await.unwrap();
    assert!(has_cycle);
}

#[sqlx::test]
async fn test_non_blocking_bond_allows_cycle(pool: sqlx::SqlitePool) {
    let a = make_spark(&pool, "A").await;
    let b = make_spark(&pool, "B").await;

    bond_repo::create(&pool, &a, &b, BondType::Blocks)
        .await
        .unwrap();

    // Related bonds don't check for cycles
    let result = bond_repo::create(&pool, &b, &a, BondType::Related).await;
    assert!(result.is_ok());
}

#[sqlx::test]
async fn test_topological_order(_pool: sqlx::SqlitePool) {
    let edges = vec![
        ("A".to_string(), "B".to_string()),
        ("B".to_string(), "C".to_string()),
        ("A".to_string(), "C".to_string()),
    ];

    let order = graph::topological_order(&edges).unwrap();
    assert_eq!(order.len(), 3);

    let pos_a = order.iter().position(|x| x == "A").unwrap();
    let pos_b = order.iter().position(|x| x == "B").unwrap();
    let pos_c = order.iter().position(|x| x == "C").unwrap();

    assert!(pos_a < pos_b);
    assert!(pos_b < pos_c);
}
