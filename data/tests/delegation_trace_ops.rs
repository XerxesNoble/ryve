// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix
//
// Tests for the delegation trace model — spark ryve-1e3848b6.
// Verifies that traces capture originating request, delegating actor,
// delegated target, execution result, and final synthesis, and that Atlas
// is visible as the default delegation origin.

use data::sparks::delegation_trace_repo as traces;
use data::sparks::types::*;

fn new_atlas_to_head(workshop_id: &str) -> NewDelegationTrace {
    NewDelegationTrace {
        workshop_id: workshop_id.to_string(),
        spark_id: None,
        parent_trace_id: None,
        originating_request: "Refactor the auth module".to_string(),
        // origin_actor omitted -> should default to Atlas.
        origin_actor: None,
        delegating_actor: ATLAS_ORIGIN.to_string(),
        delegating_actor_kind: ActorKind::Director,
        delegated_target: "head-session-abc".to_string(),
        delegated_target_kind: ActorKind::Head,
    }
}

#[sqlx::test]
async fn create_defaults_origin_to_atlas(pool: sqlx::SqlitePool) {
    let trace = traces::create(&pool, new_atlas_to_head("ws-trace"))
        .await
        .unwrap();

    assert!(trace.id.starts_with("dt-"));
    assert_eq!(trace.origin_actor, ATLAS_ORIGIN);
    assert!(trace.is_atlas_originated());
    assert_eq!(trace.delegating_actor, ATLAS_ORIGIN);
    assert_eq!(trace.delegating_actor_kind, "director");
    assert_eq!(trace.delegated_target, "head-session-abc");
    assert_eq!(trace.delegated_target_kind, "head");
    assert_eq!(trace.originating_request, "Refactor the auth module");
    assert_eq!(trace.status, "pending");
    assert!(trace.execution_result.is_none());
    assert!(trace.final_synthesis.is_none());
    assert!(trace.completed_at.is_none());
    assert!(trace.parent_trace_id.is_none());
}

#[sqlx::test]
async fn create_with_explicit_origin(pool: sqlx::SqlitePool) {
    // A non-Atlas Director should still be representable, even if Atlas is
    // the default — otherwise the model would be too rigid for future
    // experiments / personas.
    let mut new = new_atlas_to_head("ws-trace");
    new.origin_actor = Some("custom-director".to_string());
    let trace = traces::create(&pool, new).await.unwrap();
    assert_eq!(trace.origin_actor, "custom-director");
    assert!(!trace.is_atlas_originated());
}

#[sqlx::test]
async fn full_lifecycle_records_result_and_synthesis(pool: sqlx::SqlitePool) {
    let trace = traces::create(&pool, new_atlas_to_head("ws-trace"))
        .await
        .unwrap();

    traces::update_status(&pool, &trace.id, DelegationStatus::InProgress)
        .await
        .unwrap();
    traces::record_execution_result(&pool, &trace.id, "Head produced 3 sub-tasks")
        .await
        .unwrap();
    traces::update_status(&pool, &trace.id, DelegationStatus::Completed)
        .await
        .unwrap();
    traces::record_final_synthesis(&pool, &trace.id, "Auth refactor scoped and dispatched")
        .await
        .unwrap();

    let reloaded = traces::get(&pool, &trace.id).await.unwrap();
    assert_eq!(reloaded.status, "completed");
    assert_eq!(
        reloaded.execution_result.as_deref(),
        Some("Head produced 3 sub-tasks")
    );
    assert_eq!(
        reloaded.final_synthesis.as_deref(),
        Some("Auth refactor scoped and dispatched")
    );
    assert!(reloaded.completed_at.is_some());
}

#[sqlx::test]
async fn ancestor_chain_reconstructs_atlas_to_hand(pool: sqlx::SqlitePool) {
    // Atlas → Head → Hand. The Hand-level trace must be able to walk back
    // up to the Atlas-rooted hop so the UI can show "Atlas delegated this".
    let root = traces::create(&pool, new_atlas_to_head("ws-trace"))
        .await
        .unwrap();

    let head_to_hand = traces::create(
        &pool,
        NewDelegationTrace {
            workshop_id: "ws-trace".to_string(),
            spark_id: None,
            parent_trace_id: Some(root.id.clone()),
            originating_request: root.originating_request.clone(),
            origin_actor: None, // inherits Atlas as the default
            delegating_actor: "head-session-abc".to_string(),
            delegating_actor_kind: ActorKind::Head,
            delegated_target: "hand-session-xyz".to_string(),
            delegated_target_kind: ActorKind::Hand,
        },
    )
    .await
    .unwrap();

    let chain = traces::ancestor_chain(&pool, &head_to_hand.id)
        .await
        .unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].id, root.id);
    assert_eq!(chain[1].id, head_to_hand.id);
    // Atlas must be visible as the origin of the entire chain.
    assert!(chain[0].is_atlas_originated());
    assert!(chain[1].is_atlas_originated());

    let children = traces::list_children(&pool, &root.id).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, head_to_hand.id);
}

#[sqlx::test]
async fn list_for_workshop_and_spark(pool: sqlx::SqlitePool) {
    // Use a real spark id so the FK on spark_id is valid.
    sqlx::query(
        "INSERT INTO sparks (id, title, workshop_id, created_at, updated_at)
         VALUES ('ryve-trace01', 'trace test', 'ws-trace', '2026-04-08T00:00:00Z', '2026-04-08T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    traces::create(
        &pool,
        NewDelegationTrace {
            workshop_id: "ws-trace".to_string(),
            spark_id: Some("ryve-trace01".to_string()),
            parent_trace_id: None,
            originating_request: "do the thing".to_string(),
            origin_actor: None,
            delegating_actor: ATLAS_ORIGIN.to_string(),
            delegating_actor_kind: ActorKind::Director,
            delegated_target: "hand-1".to_string(),
            delegated_target_kind: ActorKind::Hand,
        },
    )
    .await
    .unwrap();

    traces::create(
        &pool,
        NewDelegationTrace {
            workshop_id: "ws-other".to_string(),
            spark_id: None,
            parent_trace_id: None,
            originating_request: "different workshop".to_string(),
            origin_actor: None,
            delegating_actor: ATLAS_ORIGIN.to_string(),
            delegating_actor_kind: ActorKind::Director,
            delegated_target: "hand-2".to_string(),
            delegated_target_kind: ActorKind::Hand,
        },
    )
    .await
    .unwrap();

    let in_workshop = traces::list_for_workshop(&pool, "ws-trace").await.unwrap();
    assert_eq!(in_workshop.len(), 1);
    assert_eq!(in_workshop[0].delegated_target, "hand-1");

    let on_spark = traces::list_for_spark(&pool, "ryve-trace01").await.unwrap();
    assert_eq!(on_spark.len(), 1);
    assert_eq!(on_spark[0].spark_id.as_deref(), Some("ryve-trace01"));
}

#[sqlx::test]
async fn update_status_on_missing_id_errors(pool: sqlx::SqlitePool) {
    let err = traces::update_status(&pool, "dt-nope0000", DelegationStatus::Completed)
        .await
        .unwrap_err();
    assert!(matches!(err, data::sparks::SparksError::NotFound(_)));
}

#[test]
fn actor_kind_round_trips() {
    for kind in [
        ActorKind::Director,
        ActorKind::Head,
        ActorKind::Hand,
        ActorKind::Tool,
        ActorKind::User,
    ] {
        assert_eq!(ActorKind::from_str(kind.as_str()), Some(kind));
    }
    assert_eq!(ActorKind::from_str("garbage"), None);
}

#[test]
fn delegation_status_round_trips() {
    for status in [
        DelegationStatus::Pending,
        DelegationStatus::InProgress,
        DelegationStatus::Completed,
        DelegationStatus::Failed,
    ] {
        assert_eq!(DelegationStatus::from_str(status.as_str()), Some(status));
    }
    assert_eq!(DelegationStatus::from_str("garbage"), None);
}
