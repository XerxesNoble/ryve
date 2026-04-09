use data::sparks::alloy_repo::AlloyMemberInput;
use data::sparks::types::*;
use data::sparks::{alloy_repo, spark_repo};

async fn make_spark(pool: &sqlx::SqlitePool, title: &str) -> String {
    spark_repo::create(
        pool,
        NewSpark {
            title: title.to_string(),
            description: String::new(),
            // Epic: top-level is the one case the invariant allows without
            // a parent, keeping this helper single-shot.
            spark_type: SparkType::Epic,
            priority: 2,
            workshop_id: "ws-alloy".to_string(),
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

#[sqlx::test]
async fn test_create_scatter_alloy(pool: sqlx::SqlitePool) {
    let s1 = make_spark(&pool, "Task 1").await;
    let s2 = make_spark(&pool, "Task 2").await;
    let s3 = make_spark(&pool, "Task 3").await;

    let alloy = alloy_repo::create(
        &pool,
        NewAlloy {
            name: "Refactor Auth".to_string(),
            alloy_type: AlloyType::Scatter,
            parent_spark_id: None,
            workshop_id: "ws-alloy".to_string(),
        },
        vec![
            AlloyMemberInput {
                spark_id: s1.clone(),
                bond_type: AlloyBondType::Parallel,
                position: 0,
            },
            AlloyMemberInput {
                spark_id: s2.clone(),
                bond_type: AlloyBondType::Parallel,
                position: 1,
            },
            AlloyMemberInput {
                spark_id: s3.clone(),
                bond_type: AlloyBondType::Parallel,
                position: 2,
            },
        ],
    )
    .await
    .unwrap();

    assert!(alloy.id.starts_with("al-"));
    assert_eq!(alloy.alloy_type, "scatter");

    let members = alloy_repo::get_members(&pool, &alloy.id).await.unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].position, 0);
    assert_eq!(members[2].position, 2);
}

#[sqlx::test]
async fn test_create_chain_alloy(pool: sqlx::SqlitePool) {
    let s1 = make_spark(&pool, "Design").await;
    let s2 = make_spark(&pool, "Implement").await;
    let s3 = make_spark(&pool, "Review").await;

    let alloy = alloy_repo::create(
        &pool,
        NewAlloy {
            name: "Feature Pipeline".to_string(),
            alloy_type: AlloyType::Chain,
            parent_spark_id: None,
            workshop_id: "ws-alloy".to_string(),
        },
        vec![
            AlloyMemberInput {
                spark_id: s1,
                bond_type: AlloyBondType::Sequential,
                position: 0,
            },
            AlloyMemberInput {
                spark_id: s2,
                bond_type: AlloyBondType::Sequential,
                position: 1,
            },
            AlloyMemberInput {
                spark_id: s3,
                bond_type: AlloyBondType::Sequential,
                position: 2,
            },
        ],
    )
    .await
    .unwrap();

    let members = alloy_repo::get_members(&pool, &alloy.id).await.unwrap();
    assert_eq!(members.len(), 3);
    // Verify sequential ordering
    assert!(members[0].position < members[1].position);
    assert!(members[1].position < members[2].position);
}

#[sqlx::test]
async fn test_delete_alloy_cascades(pool: sqlx::SqlitePool) {
    let s1 = make_spark(&pool, "Task").await;

    let alloy = alloy_repo::create(
        &pool,
        NewAlloy {
            name: "Test".to_string(),
            alloy_type: AlloyType::Scatter,
            parent_spark_id: None,
            workshop_id: "ws-alloy".to_string(),
        },
        vec![AlloyMemberInput {
            spark_id: s1,
            bond_type: AlloyBondType::Parallel,
            position: 0,
        }],
    )
    .await
    .unwrap();

    alloy_repo::delete(&pool, &alloy.id).await.unwrap();

    let members = alloy_repo::get_members(&pool, &alloy.id).await.unwrap();
    assert!(members.is_empty());
}
