// Integration test: create → add-epic → close a toy release end-to-end.
// Spark ryve-af58f359 [sp-2a82fee7].

use data::sparks::error::SparksError;
use data::sparks::types::*;
use data::sparks::{release_repo, spark_repo};

fn new_release(version: &str) -> NewRelease {
    NewRelease {
        version: version.to_string(),
        branch_name: Some(format!("release/{version}")),
        problem: Some("ship it".to_string()),
        acceptance: vec!["all epics closed".to_string()],
        notes: None,
    }
}

/// End-to-end lifecycle: create → add-epic → verify epic gate → close.
#[sqlx::test(fixtures("seed_sparks"))]
async fn release_lifecycle_create_add_epic_close(pool: sqlx::SqlitePool) {
    // 1. Create release
    let rel = release_repo::create(&pool, new_release("1.0.0"))
        .await
        .unwrap();
    assert_eq!(rel.status, "planning");
    assert!(rel.id.starts_with("rel-"));

    // 2. Add a closed epic (sp-0005 is closed in seed data)
    release_repo::add_epic(&pool, &rel.id, "sp-0005")
        .await
        .unwrap();
    let members = release_repo::list_member_epics(&pool, &rel.id)
        .await
        .unwrap();
    assert_eq!(members, vec!["sp-0005"]);

    // 3. Verify all epics are closed (the gate the CLI close checks)
    for eid in &members {
        let spark = spark_repo::get(&pool, eid).await.unwrap();
        assert_eq!(spark.status, "closed", "epic {eid} should be closed");
    }

    // 4. Transition through the lifecycle to closed
    let rel = release_repo::set_status(&pool, &rel.id, ReleaseStatus::InProgress)
        .await
        .unwrap();
    assert_eq!(rel.status, "in_progress");

    let rel = release_repo::set_status(&pool, &rel.id, ReleaseStatus::Ready)
        .await
        .unwrap();
    assert_eq!(rel.status, "ready");

    let rel = release_repo::set_status(&pool, &rel.id, ReleaseStatus::Cut)
        .await
        .unwrap();
    assert_eq!(rel.status, "cut");
    assert!(rel.cut_at.is_some(), "cut_at should be stamped");

    // Record close metadata (simulates what the CLI does after tagging + building)
    release_repo::record_close_metadata(&pool, &rel.id, "v1.0.0", "/path/to/artifact")
        .await
        .unwrap();

    let rel = release_repo::set_status(&pool, &rel.id, ReleaseStatus::Closed)
        .await
        .unwrap();
    assert_eq!(rel.status, "closed");

    // Verify metadata was persisted
    let final_rel = release_repo::get(&pool, &rel.id).await.unwrap();
    assert_eq!(final_rel.tag.as_deref(), Some("v1.0.0"));
    assert_eq!(
        final_rel.artifact_path.as_deref(),
        Some("/path/to/artifact")
    );
}

/// Adding an open epic to a release should succeed, but the close gate in the
/// CLI would reject it. Verify the gate logic works: an open epic blocks close.
#[sqlx::test(fixtures("seed_sparks"))]
async fn close_gate_rejects_open_epics(pool: sqlx::SqlitePool) {
    let rel = release_repo::create(&pool, new_release("2.0.0"))
        .await
        .unwrap();

    // sp-0001 is open in seed data
    release_repo::add_epic(&pool, &rel.id, "sp-0001")
        .await
        .unwrap();

    let members = release_repo::list_member_epics(&pool, &rel.id)
        .await
        .unwrap();
    let mut unclosed = Vec::new();
    for eid in &members {
        let spark = spark_repo::get(&pool, eid).await.unwrap();
        if spark.status != "closed" {
            unclosed.push(eid.clone());
        }
    }

    assert!(!unclosed.is_empty(), "should detect open epics");
    assert!(unclosed.contains(&"sp-0001".to_string()));
}

/// Removing an epic then re-adding to a different release works once the
/// first release is no longer open.
#[sqlx::test(fixtures("seed_sparks"))]
async fn remove_epic_allows_reassignment(pool: sqlx::SqlitePool) {
    let a = release_repo::create(&pool, new_release("3.0.0"))
        .await
        .unwrap();
    let b = release_repo::create(&pool, new_release("3.1.0"))
        .await
        .unwrap();

    release_repo::add_epic(&pool, &a.id, "sp-0005")
        .await
        .unwrap();

    // Can't add to b while a is open
    let err = release_repo::add_epic(&pool, &b.id, "sp-0005")
        .await
        .unwrap_err();
    assert!(matches!(err, SparksError::EpicAlreadyInOpenRelease { .. }));

    // Remove from a, then add to b succeeds
    release_repo::remove_epic(&pool, &a.id, "sp-0005")
        .await
        .unwrap();
    release_repo::add_epic(&pool, &b.id, "sp-0005")
        .await
        .unwrap();

    let b_members = release_repo::list_member_epics(&pool, &b.id).await.unwrap();
    assert_eq!(b_members, vec!["sp-0005"]);
}

/// Unknown release id returns a typed NotFound error.
#[sqlx::test(fixtures("seed_sparks"))]
async fn unknown_release_returns_not_found(pool: sqlx::SqlitePool) {
    let err = release_repo::get(&pool, "rel-nonexistent")
        .await
        .unwrap_err();
    assert!(matches!(err, SparksError::NotFound(_)), "got {err:?}");
}

/// Invalid version strings produce typed errors.
#[sqlx::test(fixtures("seed_sparks"))]
async fn invalid_version_is_rejected(pool: sqlx::SqlitePool) {
    let err = release_repo::create(&pool, new_release("not.valid"))
        .await
        .unwrap_err();
    assert!(matches!(err, SparksError::InvalidSemver(_)), "got {err:?}");
}
