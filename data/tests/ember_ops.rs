use data::sparks::ember_repo;
use data::sparks::types::*;

#[sqlx::test]
async fn test_create_ember(pool: sqlx::SqlitePool) {
    let ember = ember_repo::create(
        &pool,
        NewEmber {
            ember_type: EmberType::Flash,
            content: "API changed".to_string(),
            source_agent: Some("agent-1".to_string()),
            workshop_id: "ws-ember".to_string(),
            ttl_seconds: Some(60),
        },
    )
    .await
    .unwrap();

    assert!(ember.id.starts_with("em-"));
    assert_eq!(ember.ember_type, "flash");
    assert_eq!(ember.ttl_seconds, 60);
}

#[sqlx::test]
async fn test_list_active_embers(pool: sqlx::SqlitePool) {
    // Create a fresh ember (should be active)
    ember_repo::create(
        &pool,
        NewEmber {
            ember_type: EmberType::Glow,
            content: "Working on auth".to_string(),
            source_agent: Some("agent-1".to_string()),
            workshop_id: "ws-ember".to_string(),
            ttl_seconds: Some(3600),
        },
    )
    .await
    .unwrap();

    let active = ember_repo::list_active(&pool, "ws-ember").await.unwrap();
    assert_eq!(active.len(), 1);
}

#[sqlx::test]
async fn test_list_by_type(pool: sqlx::SqlitePool) {
    ember_repo::create(
        &pool,
        NewEmber {
            ember_type: EmberType::Flash,
            content: "Flash 1".to_string(),
            source_agent: None,
            workshop_id: "ws-ember".to_string(),
            ttl_seconds: Some(3600),
        },
    )
    .await
    .unwrap();

    ember_repo::create(
        &pool,
        NewEmber {
            ember_type: EmberType::Flare,
            content: "Error!".to_string(),
            source_agent: None,
            workshop_id: "ws-ember".to_string(),
            ttl_seconds: Some(3600),
        },
    )
    .await
    .unwrap();

    let flashes = ember_repo::list_by_type(&pool, "ws-ember", EmberType::Flash)
        .await
        .unwrap();
    assert_eq!(flashes.len(), 1);
    assert_eq!(flashes[0].content, "Flash 1");
}

#[sqlx::test]
async fn test_delete_ember(pool: sqlx::SqlitePool) {
    // Dismissing a notification in the UI must remove the backing row so
    // the next 3-second poll does not resurrect it. Spark sp-ux0008.
    let ember = ember_repo::create(
        &pool,
        NewEmber {
            ember_type: EmberType::Glow,
            content: "Hand finished".to_string(),
            source_agent: Some("hands".to_string()),
            workshop_id: "ws-ember".to_string(),
            ttl_seconds: Some(3600),
        },
    )
    .await
    .unwrap();

    let before = ember_repo::list_active(&pool, "ws-ember").await.unwrap();
    assert_eq!(before.len(), 1);

    let removed = ember_repo::delete(&pool, &ember.id).await.unwrap();
    assert_eq!(removed, 1);

    let after = ember_repo::list_active(&pool, "ws-ember").await.unwrap();
    assert!(after.is_empty());

    // Deleting a non-existent id is a no-op and must not error.
    let noop = ember_repo::delete(&pool, "em-does-not-exist").await.unwrap();
    assert_eq!(noop, 0);
}

#[sqlx::test]
async fn test_sweep_expired(pool: sqlx::SqlitePool) {
    // Insert an ember that's already expired (TTL=0)
    sqlx::query(
        "INSERT INTO embers (id, ember_type, content, workshop_id, ttl_seconds, created_at) VALUES ('em-expired', 'ash', 'old', 'ws-ember', 0, '2020-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let removed = ember_repo::sweep_expired(&pool).await.unwrap();
    assert_eq!(removed, 1);
}
