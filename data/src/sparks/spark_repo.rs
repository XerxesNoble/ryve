// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD operations for Workgraph sparks.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_spark_id;
use super::types::*;

pub async fn create(pool: &SqlitePool, new: NewSpark) -> Result<Spark, SparksError> {
    // Invariant: only epics may be top-level. Every non-epic spark must live
    // under a parent so the workgraph always has a hierarchy Hands and the UI
    // can traverse — no orphan tasks, bugs, features, chores, spikes, or
    // milestones. This check is the sole enforcement point; callers on every
    // path (CLI, UI, GitHub sync, tests) must supply a parent_id for non-epics.
    if new.spark_type != SparkType::Epic && new.parent_id.is_none() {
        return Err(SparksError::OrphanChildRejected {
            spark_type: new.spark_type.as_str().to_string(),
        });
    }

    let id = generate_spark_id(&new.workshop_id);
    let now = Utc::now().to_rfc3339();
    let spark_type = new.spark_type.as_str();
    let metadata = new.metadata.unwrap_or_else(|| "{}".to_string());

    let risk_level = new
        .risk_level
        .map(|r| r.as_str().to_string())
        .unwrap_or_else(|| "normal".to_string());

    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, assignee, owner, parent_id, workshop_id, estimated_minutes, metadata, created_at, updated_at, due_at, risk_level, scope_boundary)
         VALUES (?, ?, ?, 'open', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.title)
    .bind(&new.description)
    .bind(new.priority)
    .bind(spark_type)
    .bind(&new.assignee)
    .bind(&new.owner)
    .bind(&new.parent_id)
    .bind(&new.workshop_id)
    .bind(new.estimated_minutes)
    .bind(&metadata)
    .bind(&now)
    .bind(&now)
    .bind(&new.due_at)
    .bind(&risk_level)
    .bind(&new.scope_boundary)
    .execute(pool)
    .await?;

    // Record creation event
    super::event_repo::record(
        pool,
        NewEvent {
            spark_id: id.clone(),
            actor: "system".to_string(),
            field_name: "status".to_string(),
            old_value: None,
            new_value: Some("open".to_string()),
            reason: Some("created".to_string()),
            actor_type: Some(ActorType::System),
            change_nature: None,
            session_id: None,
        },
    )
    .await?;

    get(pool, &id).await
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Spark, SparksError> {
    sqlx::query_as::<_, Spark>("SELECT * FROM sparks WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("spark {id}")))
}

pub async fn update(
    pool: &SqlitePool,
    id: &str,
    upd: UpdateSpark,
    actor: &str,
) -> Result<Spark, SparksError> {
    let existing = get(pool, id).await?;
    let now = Utc::now().to_rfc3339();

    let title = upd.title.unwrap_or(existing.title);
    let description = upd.description.unwrap_or(existing.description);
    let old_status = existing.status.clone();
    let status = upd
        .status
        .map(|s| s.as_str().to_string())
        .unwrap_or(existing.status);
    let priority = upd.priority.unwrap_or(existing.priority);
    let spark_type = upd
        .spark_type
        .map(|t| t.as_str().to_string())
        .unwrap_or(existing.spark_type);
    let assignee = upd.assignee.unwrap_or(existing.assignee);
    let owner = upd.owner.unwrap_or(existing.owner);
    let parent_id = upd.parent_id.unwrap_or(existing.parent_id);
    let due_at = upd.due_at.unwrap_or(existing.due_at);
    let defer_until = upd.defer_until.unwrap_or(existing.defer_until);
    let estimated_minutes = upd.estimated_minutes.unwrap_or(existing.estimated_minutes);
    let metadata = upd.metadata.unwrap_or(existing.metadata);
    let risk_level = upd
        .risk_level
        .map(|r| Some(r.as_str().to_string()))
        .unwrap_or(existing.risk_level);
    let scope_boundary = upd.scope_boundary.unwrap_or(existing.scope_boundary);

    // Record changed fields as events
    if status != old_status {
        super::event_repo::record(
            pool,
            NewEvent {
                spark_id: id.to_string(),
                actor: actor.to_string(),
                field_name: "status".to_string(),
                old_value: Some(old_status),
                new_value: Some(status.clone()),
                reason: None,
                actor_type: None,
                change_nature: None,
                session_id: None,
            },
        )
        .await?;
    }

    sqlx::query(
        "UPDATE sparks SET title=?, description=?, status=?, priority=?, spark_type=?, assignee=?, owner=?, parent_id=?, due_at=?, defer_until=?, estimated_minutes=?, metadata=?, updated_at=?, risk_level=?, scope_boundary=? WHERE id=?",
    )
    .bind(&title)
    .bind(&description)
    .bind(&status)
    .bind(priority)
    .bind(&spark_type)
    .bind(&assignee)
    .bind(&owner)
    .bind(&parent_id)
    .bind(&due_at)
    .bind(&defer_until)
    .bind(estimated_minutes)
    .bind(&metadata)
    .bind(&now)
    .bind(&risk_level)
    .bind(&scope_boundary)
    .bind(id)
    .execute(pool)
    .await?;

    get(pool, id).await
}

pub async fn close(
    pool: &SqlitePool,
    id: &str,
    reason: &str,
    actor: &str,
) -> Result<Spark, SparksError> {
    let existing = get(pool, id).await?;
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE sparks SET status='closed', closed_at=?, closed_reason=?, updated_at=? WHERE id=?",
    )
    .bind(&now)
    .bind(reason)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;

    super::event_repo::record(
        pool,
        NewEvent {
            spark_id: id.to_string(),
            actor: actor.to_string(),
            field_name: "status".to_string(),
            old_value: Some(existing.status),
            new_value: Some("closed".to_string()),
            reason: Some(reason.to_string()),
            actor_type: None,
            change_nature: None,
            session_id: None,
        },
    )
    .await?;

    get(pool, id).await
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), SparksError> {
    let result = sqlx::query("DELETE FROM sparks WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("spark {id}")));
    }
    Ok(())
}

pub async fn list(pool: &SqlitePool, filter: SparkFilter) -> Result<Vec<Spark>, SparksError> {
    let mut sql = String::from("SELECT * FROM sparks WHERE 1=1");
    let mut args: Vec<String> = Vec::new();

    if let Some(ref wid) = filter.workshop_id {
        sql.push_str(" AND workshop_id = ?");
        args.push(wid.clone());
    }
    if let Some(ref statuses) = filter.status {
        let placeholders: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
        let ph = placeholders
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND status IN ({ph})"));
        for s in placeholders {
            args.push(s.to_string());
        }
    }
    if let Some(p) = filter.priority {
        sql.push_str(" AND priority = ?");
        args.push(p.to_string());
    }
    if let Some(ref a) = filter.assignee {
        sql.push_str(" AND assignee = ?");
        args.push(a.clone());
    }
    if let Some(ref t) = filter.spark_type {
        sql.push_str(" AND spark_type = ?");
        args.push(t.as_str().to_string());
    }
    if let Some(ref pid) = filter.parent_id {
        sql.push_str(" AND parent_id = ?");
        args.push(pid.clone());
    }
    if let Some(ref r) = filter.risk_level {
        sql.push_str(" AND risk_level = ?");
        args.push(r.as_str().to_string());
    }
    if let Some(ref s) = filter.stamp {
        sql.push_str(" AND id IN (SELECT spark_id FROM stamps WHERE name = ?)");
        args.push(s.clone());
    }

    sql.push_str(" ORDER BY priority ASC, created_at ASC");

    let mut query = sqlx::query_as::<_, Spark>(&sql);
    for arg in &args {
        query = query.bind(arg);
    }

    Ok(query.fetch_all(pool).await?)
}

/// Return the id of the workshop's 'Unsorted' catch-all epic, creating it if
/// it doesn't already exist. Uses the same deterministic id scheme as
/// migration 011 (`<workshop_id>-unsorted-epic`) so that this helper and the
/// migration converge on a single epic per workshop.
///
/// This is the escape hatch for callers that import work from external
/// sources (e.g. GitHub sync) where no meaningful parent exists yet.
pub async fn ensure_unsorted_epic(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<String, SparksError> {
    let id = format!("{workshop_id}-unsorted-epic");
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT OR IGNORE INTO sparks (id, title, description, status, priority, spark_type, workshop_id, metadata, created_at, updated_at)
         VALUES (?, 'Unsorted', 'Catch-all epic for sparks that have no explicit parent. Reparent or re-home them as needed.', 'open', 4, 'epic', ?, '{}', ?, ?)",
    )
    .bind(&id)
    .bind(workshop_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(id)
}

#[cfg(test)]
mod orphan_rejection_tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn fresh_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        pool
    }

    fn new_spark(
        workshop: &str,
        title: &str,
        spark_type: SparkType,
        parent_id: Option<String>,
    ) -> NewSpark {
        NewSpark {
            title: title.to_string(),
            description: String::new(),
            spark_type,
            priority: 2,
            workshop_id: workshop.to_string(),
            assignee: None,
            owner: None,
            parent_id,
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        }
    }

    #[tokio::test]
    async fn rejects_orphan_task() {
        let pool = fresh_pool().await;
        let err = create(&pool, new_spark("ws", "orphan task", SparkType::Task, None))
            .await
            .expect_err("orphan non-epic must be rejected");
        match err {
            SparksError::OrphanChildRejected { spark_type } => {
                assert_eq!(spark_type, "task");
            }
            other => panic!("expected OrphanChildRejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_orphan_bug_and_feature() {
        let pool = fresh_pool().await;
        for kind in [SparkType::Bug, SparkType::Feature, SparkType::Chore, SparkType::Spike, SparkType::Milestone] {
            let err = create(&pool, new_spark("ws", "orphan", kind, None))
                .await
                .expect_err("orphan non-epic must be rejected");
            assert!(matches!(err, SparksError::OrphanChildRejected { .. }));
        }
    }

    #[tokio::test]
    async fn accepts_top_level_epic() {
        let pool = fresh_pool().await;
        let epic = create(
            &pool,
            new_spark("ws", "top level epic", SparkType::Epic, None),
        )
        .await
        .expect("top-level epic must be accepted");
        assert_eq!(epic.spark_type, "epic");
        assert!(epic.parent_id.is_none());
    }

    #[tokio::test]
    async fn accepts_nested_epic() {
        let pool = fresh_pool().await;
        let parent = create(
            &pool,
            new_spark("ws", "outer epic", SparkType::Epic, None),
        )
        .await
        .unwrap();
        let nested = create(
            &pool,
            new_spark("ws", "inner epic", SparkType::Epic, Some(parent.id.clone())),
        )
        .await
        .expect("nested epic must be accepted");
        assert_eq!(nested.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[tokio::test]
    async fn accepts_child_of_epic() {
        let pool = fresh_pool().await;
        let epic = create(&pool, new_spark("ws", "parent", SparkType::Epic, None))
            .await
            .unwrap();
        let child = create(
            &pool,
            new_spark("ws", "child task", SparkType::Task, Some(epic.id.clone())),
        )
        .await
        .expect("child of epic must be accepted");
        assert_eq!(child.parent_id.as_deref(), Some(epic.id.as_str()));
    }

    #[tokio::test]
    async fn ensure_unsorted_epic_is_idempotent() {
        let pool = fresh_pool().await;
        let id1 = ensure_unsorted_epic(&pool, "ws-a").await.unwrap();
        let id2 = ensure_unsorted_epic(&pool, "ws-a").await.unwrap();
        assert_eq!(id1, id2);
        // And a task can be parented under it.
        let child = create(
            &pool,
            new_spark("ws-a", "orphan import", SparkType::Task, Some(id1.clone())),
        )
        .await
        .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(id1.as_str()));
    }
}
