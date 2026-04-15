// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for the `assignments` table — actor-to-spark assignment with phase
//! tracking, branch metadata, and optimistic-concurrency event versioning.
//!
//! Every mutation (create, update) is wrapped in a transaction that atomically
//! appends an event to the `events` table. No code outside `data::sparks` may
//! mutate assignment state without producing an event.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::event_repo;
use super::id::generate_id;
use super::types::{Assignment, NewAssignment, NewEvent, UpdateAssignment};

/// Create a new assignment and return it. The INSERT and its corresponding
/// event are committed atomically.
pub async fn create_assignment(
    pool: &SqlitePool,
    new: NewAssignment,
) -> Result<Assignment, SparksError> {
    let id = generate_id("asgn");
    let now = Utc::now().to_rfc3339();

    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO assignments (assignment_id, spark_id, actor_id, assignment_phase, source_branch, target_branch, event_version, created_at, updated_at, session_id, assigned_at)
         VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.spark_id)
    .bind(&new.actor_id)
    .bind(new.assignment_phase.as_str())
    .bind(&new.source_branch)
    .bind(&new.target_branch)
    .bind(&now)
    .bind(&now)
    .bind(&new.actor_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    event_repo::record_in_tx(
        &mut tx,
        NewEvent {
            spark_id: new.spark_id.clone(),
            actor: new.actor_id.clone(),
            field_name: "assignment_phase".into(),
            old_value: None,
            new_value: Some(new.assignment_phase.as_str().to_string()),
            reason: Some("assignment created".into()),
            actor_type: None,
            change_nature: None,
            session_id: None,
        },
    )
    .await?;

    let assignment =
        sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE assignment_id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;

    Ok(assignment)
}

/// Fetch an assignment by its ID.
pub async fn get_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
) -> Result<Assignment, SparksError> {
    sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE assignment_id = ?")
        .bind(assignment_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("assignment {assignment_id}")))
}

/// List all assignments for a given spark, newest first.
pub async fn list_assignments_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<Assignment>, SparksError> {
    Ok(sqlx::query_as::<_, Assignment>(
        "SELECT * FROM assignments WHERE spark_id = ? ORDER BY created_at DESC",
    )
    .bind(spark_id)
    .fetch_all(pool)
    .await?)
}

/// Update mutable fields on an assignment: `event_version`, `source_branch`,
/// `target_branch`. Phase updates are intentionally excluded here — they go
/// through the phase-transition validator in `transition.rs`.
///
/// The UPDATE and its corresponding event are committed atomically.
pub async fn update_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
    update: UpdateAssignment,
) -> Result<Assignment, SparksError> {
    let mut tx = pool.begin().await?;

    let existing =
        sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE assignment_id = ?")
            .bind(assignment_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| SparksError::NotFound(format!("assignment {assignment_id}")))?;

    let now = Utc::now().to_rfc3339();

    let event_version = update.event_version.unwrap_or(existing.event_version);
    let source_branch = match update.source_branch {
        Some(v) => v,
        None => existing.source_branch,
    };
    let target_branch = match update.target_branch {
        Some(v) => v,
        None => existing.target_branch,
    };

    sqlx::query(
        "UPDATE assignments SET event_version = ?, source_branch = ?, target_branch = ?, updated_at = ? WHERE assignment_id = ?",
    )
    .bind(event_version)
    .bind(&source_branch)
    .bind(&target_branch)
    .bind(&now)
    .bind(assignment_id)
    .execute(&mut *tx)
    .await?;

    event_repo::record_in_tx(
        &mut tx,
        NewEvent {
            spark_id: existing.spark_id.clone(),
            actor: existing.actor_id.clone(),
            field_name: "assignment_metadata".into(),
            old_value: None,
            new_value: Some(format!("v{event_version}")),
            reason: Some("assignment updated".into()),
            actor_type: None,
            change_nature: None,
            session_id: None,
        },
    )
    .await?;

    let updated =
        sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE assignment_id = ?")
            .bind(assignment_id)
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;

    Ok(updated)
}
