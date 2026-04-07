use data::sparks::bond_repo;
use data::sparks::types::*;

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_create_bond(pool: sqlx::SqlitePool) {
    let bond = bond_repo::create(&pool, "sp-0001", "sp-0003", BondType::Related)
        .await
        .unwrap();

    assert_eq!(bond.from_id, "sp-0001");
    assert_eq!(bond.to_id, "sp-0003");
    assert_eq!(bond.bond_type, "related");
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_list_for_spark(pool: sqlx::SqlitePool) {
    let bonds = bond_repo::list_for_spark(&pool, "sp-0002").await.unwrap();
    // sp-0002 has two bonds: blocks sp-0003, parent of sp-0004 (actually sp-0001 is parent)
    // sp-0002 blocks sp-0003 (from_id=sp-0002)
    assert!(bonds.iter().any(|b| b.to_id == "sp-0003"));
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_list_blockers(pool: sqlx::SqlitePool) {
    let blockers = bond_repo::list_blockers(&pool, "sp-0003").await.unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].from_id, "sp-0002");
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_delete_bond(pool: sqlx::SqlitePool) {
    let bonds = bond_repo::list_for_spark(&pool, "sp-0002").await.unwrap();
    let bond = bonds.iter().find(|b| b.bond_type == "blocks").unwrap();

    bond_repo::delete(&pool, bond.id).await.unwrap();

    let blockers = bond_repo::list_blockers(&pool, "sp-0003").await.unwrap();
    assert!(blockers.is_empty());
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_list_blocked_spark_ids(pool: sqlx::SqlitePool) {
    // sp-0003 is blocked by sp-0002 (open) → should appear.
    let blocked = bond_repo::list_blocked_spark_ids(&pool, "ws-test")
        .await
        .unwrap();
    assert!(blocked.contains("sp-0003"));
    assert!(!blocked.contains("sp-0002"));
    assert!(!blocked.contains("sp-0001"));

    // Once the blocker closes, the blocked spark must drop out of the set.
    sqlx::query("UPDATE sparks SET status = 'closed' WHERE id = 'sp-0002'")
        .execute(&pool)
        .await
        .unwrap();
    let blocked = bond_repo::list_blocked_spark_ids(&pool, "ws-test")
        .await
        .unwrap();
    assert!(!blocked.contains("sp-0003"));
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn test_cascade_delete_on_spark(pool: sqlx::SqlitePool) {
    // Deleting sp-0002 should cascade-delete its bonds
    data::sparks::spark_repo::delete(&pool, "sp-0002")
        .await
        .unwrap();

    let blockers = bond_repo::list_blockers(&pool, "sp-0003").await.unwrap();
    assert!(blockers.is_empty());
}
