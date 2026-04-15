use data::sparks::release_repo;
use data::sparks::types::*;

fn new_release(version: &str) -> NewRelease {
    NewRelease {
        version: version.to_string(),
        branch_name: None,
        problem: None,
        acceptance: Vec::new(),
        notes: None,
    }
}

#[sqlx::test]
async fn test_update_version(pool: sqlx::SqlitePool) {
    let r = release_repo::create(&pool, new_release("1.0.0"))
        .await
        .unwrap();
    assert_eq!(r.version, "1.0.0");

    let updated = release_repo::update(&pool, &r.id, "2.0.0").await.unwrap();
    assert_eq!(updated.version, "2.0.0");
    assert_eq!(updated.id, r.id);
}

#[sqlx::test]
async fn test_update_rejects_invalid_semver(pool: sqlx::SqlitePool) {
    let r = release_repo::create(&pool, new_release("1.0.0"))
        .await
        .unwrap();

    let err = release_repo::update(&pool, &r.id, "not-semver")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("semver"),
        "expected semver error, got: {err}"
    );
}

#[sqlx::test]
async fn test_update_missing_release(pool: sqlx::SqlitePool) {
    let err = release_repo::update(&pool, "rel-nonexistent", "1.0.0")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not found") || err.to_string().contains("Not found"),
        "expected not-found error, got: {err}"
    );
}
