// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for Crews — groups of Hands managed by a Head.
//!
//! A Crew rolls up several parallel Hand sessions working on related sparks
//! under a single Head's direction. The Head creates the Crew, adds members
//! as it spawns Hands, and (eventually) marks one member as the Merger so
//! that one Hand can integrate the Crew's worktree branches into a single
//! PR.
//!
//! All writes go through this module — never touch `crews` or `crew_members`
//! directly. The repo is the only thing that knows the table layout, which
//! keeps the schema swappable and lets us layer audit-trail writes here in
//! the future without touching callers.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::{Crew, CrewMember, NewCrew};

/// Create a new Crew row and return it.
pub async fn create(pool: &SqlitePool, new: NewCrew) -> Result<Crew, SparksError> {
    let id = generate_id("cr");
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO crews (id, workshop_id, name, purpose, status, head_session_id, parent_spark_id, created_at)
         VALUES (?, ?, ?, ?, 'active', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.workshop_id)
    .bind(&new.name)
    .bind(&new.purpose)
    .bind(&new.head_session_id)
    .bind(&new.parent_spark_id)
    .bind(&now)
    .execute(pool)
    .await?;

    get(pool, &id).await
}

/// Fetch a Crew by id.
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Crew, SparksError> {
    sqlx::query_as::<_, Crew>("SELECT * FROM crews WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("crew {id}")))
}

/// List all crews for a workshop, newest first.
pub async fn list_for_workshop(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<Crew>, SparksError> {
    Ok(sqlx::query_as::<_, Crew>(
        "SELECT * FROM crews WHERE workshop_id = ? ORDER BY created_at DESC",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?)
}

/// Update a crew's status. The caller is responsible for the lifecycle
/// transitions (active → merging → completed/abandoned) — the repo just
/// persists the value.
pub async fn set_status(pool: &SqlitePool, id: &str, status: &str) -> Result<(), SparksError> {
    let result = sqlx::query("UPDATE crews SET status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("crew {id}")));
    }
    Ok(())
}

/// Set (or clear) the head_session_id for a crew.
pub async fn set_head(
    pool: &SqlitePool,
    id: &str,
    head_session_id: Option<&str>,
) -> Result<(), SparksError> {
    let result = sqlx::query("UPDATE crews SET head_session_id = ? WHERE id = ?")
        .bind(head_session_id)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("crew {id}")));
    }
    Ok(())
}

/// Add a session to a crew. Idempotent on (crew_id, session_id) thanks to the
/// UNIQUE constraint — re-inserts return the existing row.
pub async fn add_member(
    pool: &SqlitePool,
    crew_id: &str,
    session_id: &str,
    role: Option<&str>,
) -> Result<CrewMember, SparksError> {
    let now = Utc::now().to_rfc3339();
    // Try to insert; ignore the conflict and read back the (existing or new) row.
    sqlx::query(
        "INSERT OR IGNORE INTO crew_members (crew_id, session_id, role, joined_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(crew_id)
    .bind(session_id)
    .bind(role)
    .bind(&now)
    .execute(pool)
    .await?;

    sqlx::query_as::<_, CrewMember>(
        "SELECT * FROM crew_members WHERE crew_id = ? AND session_id = ? LIMIT 1",
    )
    .bind(crew_id)
    .bind(session_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| SparksError::NotFound(format!("crew member {crew_id}/{session_id}")))
}

/// Remove a session from a crew. No-op if the membership did not exist.
pub async fn remove_member(
    pool: &SqlitePool,
    crew_id: &str,
    session_id: &str,
) -> Result<(), SparksError> {
    sqlx::query("DELETE FROM crew_members WHERE crew_id = ? AND session_id = ?")
        .bind(crew_id)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// List members of a crew, oldest joiner first (Head usually first, Merger
/// usually last).
pub async fn members(pool: &SqlitePool, crew_id: &str) -> Result<Vec<CrewMember>, SparksError> {
    Ok(sqlx::query_as::<_, CrewMember>(
        "SELECT * FROM crew_members WHERE crew_id = ? ORDER BY joined_at ASC",
    )
    .bind(crew_id)
    .fetch_all(pool)
    .await?)
}

/// List crews that a given session belongs to.
pub async fn crews_for_session(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<Crew>, SparksError> {
    Ok(sqlx::query_as::<_, Crew>(
        "SELECT c.* FROM crews c
         INNER JOIN crew_members m ON m.crew_id = c.id
         WHERE m.session_id = ?
         ORDER BY c.created_at DESC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparks::types::{NewAgentSession, NewSpark, SparkType};

    async fn fresh_pool() -> SqlitePool {
        let dir = tempdir_unique();
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::open_sparks_db(&dir).await.unwrap()
    }

    fn tempdir_unique() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ryve-crew-test-{}", uuid::Uuid::new_v4()))
    }

    async fn make_session(pool: &SqlitePool, ws: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        crate::sparks::agent_session_repo::create(
            pool,
            &NewAgentSession {
                id: id.clone(),
                workshop_id: ws.into(),
                agent_name: "stub".into(),
                agent_command: "echo".into(),
                agent_args: vec![],
                session_label: None,
                child_pid: None,
                resume_id: None,
                log_path: None,
                parent_session_id: None,
            },
        )
        .await
        .unwrap();
        id
    }

    async fn make_spark(pool: &SqlitePool, ws: &str, title: &str) -> String {
        crate::sparks::spark_repo::create(
            pool,
            NewSpark {
                title: title.into(),
                description: String::new(),
                spark_type: SparkType::Task,
                priority: 2,
                workshop_id: ws.into(),
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

    #[tokio::test]
    async fn create_and_get_roundtrip() {
        let pool = fresh_pool().await;
        let parent = make_spark(&pool, "ws", "parent epic").await;
        let head_sid = make_session(&pool, "ws").await;
        let crew = create(
            &pool,
            NewCrew {
                name: "auth-crew".into(),
                purpose: Some("build login".into()),
                workshop_id: "ws".into(),
                head_session_id: Some(head_sid.clone()),
                parent_spark_id: Some(parent.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(crew.name, "auth-crew");
        assert_eq!(crew.status, "active");
        assert_eq!(crew.head_session_id.as_deref(), Some(head_sid.as_str()));
        assert_eq!(crew.parent_spark_id.as_deref(), Some(parent.as_str()));

        let fetched = get(&pool, &crew.id).await.unwrap();
        assert_eq!(fetched.id, crew.id);
    }

    #[tokio::test]
    async fn list_for_workshop_orders_newest_first() {
        let pool = fresh_pool().await;
        let a = create(
            &pool,
            NewCrew {
                name: "a".into(),
                purpose: None,
                workshop_id: "ws".into(),
                head_session_id: None,
                parent_spark_id: None,
            },
        )
        .await
        .unwrap();
        // Ensure the second row's created_at is strictly later.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let b = create(
            &pool,
            NewCrew {
                name: "b".into(),
                purpose: None,
                workshop_id: "ws".into(),
                head_session_id: None,
                parent_spark_id: None,
            },
        )
        .await
        .unwrap();
        let listed = list_for_workshop(&pool, "ws").await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, b.id);
        assert_eq!(listed[1].id, a.id);
    }

    #[tokio::test]
    async fn add_remove_members_and_role() {
        let pool = fresh_pool().await;
        let crew = create(
            &pool,
            NewCrew {
                name: "c".into(),
                purpose: None,
                workshop_id: "ws".into(),
                head_session_id: None,
                parent_spark_id: None,
            },
        )
        .await
        .unwrap();
        let s1 = make_session(&pool, "ws").await;
        let s2 = make_session(&pool, "ws").await;

        add_member(&pool, &crew.id, &s1, Some("hand"))
            .await
            .unwrap();
        add_member(&pool, &crew.id, &s2, Some("merger"))
            .await
            .unwrap();
        // Idempotent re-add returns existing row, no duplicate.
        add_member(&pool, &crew.id, &s1, Some("hand"))
            .await
            .unwrap();

        let listed = members(&pool, &crew.id).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|m| m.role.as_deref() == Some("merger")));

        remove_member(&pool, &crew.id, &s1).await.unwrap();
        let listed = members(&pool, &crew.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, s2);
    }

    #[tokio::test]
    async fn set_status_and_set_head() {
        let pool = fresh_pool().await;
        let crew = create(
            &pool,
            NewCrew {
                name: "c".into(),
                purpose: None,
                workshop_id: "ws".into(),
                head_session_id: None,
                parent_spark_id: None,
            },
        )
        .await
        .unwrap();
        set_status(&pool, &crew.id, "merging").await.unwrap();
        let h = make_session(&pool, "ws").await;
        set_head(&pool, &crew.id, Some(&h)).await.unwrap();
        let fetched = get(&pool, &crew.id).await.unwrap();
        assert_eq!(fetched.status, "merging");
        assert_eq!(fetched.head_session_id.as_deref(), Some(h.as_str()));

        // Errors on missing crew.
        assert!(set_status(&pool, "cr-nope", "merging").await.is_err());
    }

    #[tokio::test]
    async fn crews_for_session_finds_membership() {
        let pool = fresh_pool().await;
        let crew = create(
            &pool,
            NewCrew {
                name: "c".into(),
                purpose: None,
                workshop_id: "ws".into(),
                head_session_id: None,
                parent_spark_id: None,
            },
        )
        .await
        .unwrap();
        let sid = make_session(&pool, "ws").await;
        add_member(&pool, &crew.id, &sid, Some("hand"))
            .await
            .unwrap();
        let listed = crews_for_session(&pool, &sid).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, crew.id);
    }
}
