// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for the `assignments` table — actor-to-spark assignment with phase
//! tracking, branch metadata, and optimistic-concurrency event versioning.
//!
//! All reads/writes to the `assignments` table go through this module.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::{NewPhaseAssignment, PhaseAssignment, UpdatePhaseAssignment};

/// Create a new assignment and return it.
pub async fn create_assignment(
    pool: &SqlitePool,
    new: NewPhaseAssignment,
) -> Result<PhaseAssignment, SparksError> {
    let id = generate_id("asgn");
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO assignments (assignment_id, spark_id, actor_id, assignment_phase, source_branch, target_branch, event_version, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&new.spark_id)
    .bind(&new.actor_id)
    .bind(new.assignment_phase.as_str())
    .bind(&new.source_branch)
    .bind(&new.target_branch)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    get_assignment(pool, &id).await
}

/// Fetch an assignment by its ID.
pub async fn get_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
) -> Result<PhaseAssignment, SparksError> {
    sqlx::query_as::<_, PhaseAssignment>("SELECT * FROM assignments WHERE assignment_id = ?")
        .bind(assignment_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("assignment {assignment_id}")))
}

/// List all assignments for a given spark, newest first.
pub async fn list_assignments_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<PhaseAssignment>, SparksError> {
    Ok(sqlx::query_as::<_, PhaseAssignment>(
        "SELECT * FROM assignments WHERE spark_id = ? ORDER BY created_at DESC",
    )
    .bind(spark_id)
    .fetch_all(pool)
    .await?)
}

/// Update mutable fields on an assignment: `event_version`, `source_branch`,
/// `target_branch`. Phase updates are intentionally excluded here — they go
/// through a phase-transition validator (sibling spark).
pub async fn update_assignment(
    pool: &SqlitePool,
    assignment_id: &str,
    update: UpdatePhaseAssignment,
) -> Result<PhaseAssignment, SparksError> {
    let existing = get_assignment(pool, assignment_id).await?;
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
    .execute(pool)
    .await?;

    get_assignment(pool, assignment_id).await
}
