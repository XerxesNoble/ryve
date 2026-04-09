// Spark ryve-d5032784 [sp-2a82fee7]: covers the typed release_repo surface
// and the open-release invariant enforced by migration 011.

use data::sparks::error::SparksError;
use data::sparks::release_repo;
use data::sparks::types::*;

fn new_release(version: &str) -> NewRelease {
    NewRelease {
        version: version.to_string(),
        branch_name: Some(format!("release/{version}")),
        problem: Some("ship it".to_string()),
        acceptance: vec!["tests pass".to_string(), "tag pushed".to_string()],
        notes: None,
    }
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn create_roundtrips_and_parses_acceptance(pool: sqlx::SqlitePool) {
    let rel = release_repo::create(&pool, new_release("1.2.3"))
        .await
        .unwrap();

    assert!(rel.id.starts_with("rel-"));
    assert_eq!(rel.version, "1.2.3");
    assert_eq!(rel.status, "planning");
    assert_eq!(rel.branch_name.as_deref(), Some("release/1.2.3"));
    assert!(rel.cut_at.is_none());
    assert_eq!(rel.acceptance(), vec!["tests pass", "tag pushed"]);

    let fetched = release_repo::get(&pool, &rel.id).await.unwrap();
    assert_eq!(fetched.id, rel.id);
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn create_rejects_bad_semver(pool: sqlx::SqlitePool) {
    let err = release_repo::create(&pool, new_release("not-semver"))
        .await
        .unwrap_err();
    assert!(matches!(err, SparksError::InvalidSemver(_)), "got {err:?}");
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn add_and_list_member_epics(pool: sqlx::SqlitePool) {
    let rel = release_repo::create(&pool, new_release("0.1.0"))
        .await
        .unwrap();

    release_repo::add_epic(&pool, &rel.id, "sp-0001")
        .await
        .unwrap();
    release_repo::add_epic(&pool, &rel.id, "sp-0003")
        .await
        .unwrap();

    let members = release_repo::list_member_epics(&pool, &rel.id)
        .await
        .unwrap();
    assert_eq!(members, vec!["sp-0001".to_string(), "sp-0003".to_string()]);

    release_repo::remove_epic(&pool, &rel.id, "sp-0001")
        .await
        .unwrap();
    let members = release_repo::list_member_epics(&pool, &rel.id)
        .await
        .unwrap();
    assert_eq!(members, vec!["sp-0003".to_string()]);
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn epic_rejected_when_in_another_open_release(pool: sqlx::SqlitePool) {
    let a = release_repo::create(&pool, new_release("1.0.0"))
        .await
        .unwrap();
    let b = release_repo::create(&pool, new_release("1.1.0"))
        .await
        .unwrap();

    release_repo::add_epic(&pool, &a.id, "sp-0002")
        .await
        .unwrap();

    let err = release_repo::add_epic(&pool, &b.id, "sp-0002")
        .await
        .unwrap_err();
    assert!(
        matches!(err, SparksError::EpicAlreadyInOpenRelease { ref spark_id } if spark_id == "sp-0002"),
        "got {err:?}"
    );
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn epic_allowed_once_other_release_is_cut(pool: sqlx::SqlitePool) {
    let a = release_repo::create(&pool, new_release("2.0.0"))
        .await
        .unwrap();
    let b = release_repo::create(&pool, new_release("2.1.0"))
        .await
        .unwrap();

    release_repo::add_epic(&pool, &a.id, "sp-0004")
        .await
        .unwrap();

    // Closing A out of the open set frees sp-0004 for a new release (e.g.
    // a backport into 2.1.0).
    let a_cut = release_repo::set_status(&pool, &a.id, ReleaseStatus::Cut)
        .await
        .unwrap();
    assert_eq!(a_cut.status, "cut");
    assert!(a_cut.cut_at.is_some());

    release_repo::add_epic(&pool, &b.id, "sp-0004")
        .await
        .unwrap();
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn set_status_reopen_conflict_is_typed(pool: sqlx::SqlitePool) {
    let a = release_repo::create(&pool, new_release("3.0.0"))
        .await
        .unwrap();
    let b = release_repo::create(&pool, new_release("3.1.0"))
        .await
        .unwrap();

    release_repo::add_epic(&pool, &a.id, "sp-0001")
        .await
        .unwrap();
    release_repo::set_status(&pool, &a.id, ReleaseStatus::Cut)
        .await
        .unwrap();

    // sp-0001 now lives in B as the sole open release.
    release_repo::add_epic(&pool, &b.id, "sp-0001")
        .await
        .unwrap();

    // Trying to re-open A would mean two open releases share sp-0001.
    let err = release_repo::set_status(&pool, &a.id, ReleaseStatus::Planning)
        .await
        .unwrap_err();
    assert!(
        matches!(err, SparksError::EpicAlreadyInOpenRelease { .. }),
        "got {err:?}"
    );
}

#[sqlx::test(fixtures("seed_sparks"))]
async fn list_filters_by_status(pool: sqlx::SqlitePool) {
    let a = release_repo::create(&pool, new_release("4.0.0"))
        .await
        .unwrap();
    let _b = release_repo::create(&pool, new_release("4.1.0"))
        .await
        .unwrap();
    release_repo::set_status(&pool, &a.id, ReleaseStatus::InProgress)
        .await
        .unwrap();

    let all = release_repo::list(&pool, None).await.unwrap();
    assert_eq!(all.len(), 2);

    let planning = release_repo::list(&pool, Some(vec![ReleaseStatus::Planning]))
        .await
        .unwrap();
    assert_eq!(planning.len(), 1);
    assert_eq!(planning[0].version, "4.1.0");

    let in_progress = release_repo::list(&pool, Some(vec![ReleaseStatus::InProgress]))
        .await
        .unwrap();
    assert_eq!(in_progress.len(), 1);
    assert_eq!(in_progress[0].version, "4.0.0");
}
