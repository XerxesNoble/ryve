// SPDX-License-Identifier: AGPL-3.0-or-later

//! CRUD for the `assignments` table — actor-to-spark assignment with phase
//! tracking, branch metadata, and optimistic-concurrency event versioning.
//!
//! Every mutation (create, update) is wrapped in a transaction that atomically
//! appends an event to the `events` table. No code outside `data::sparks` may
//! mutate assignment state without producing an event.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::{SparksError, TransitionError};
use super::id::generate_id;
use super::types::{
    Assignment, AssignmentPhase, NewAssignment, NewEvent, TransitionActorRole, UpdateAssignment,
};
use super::{event_repo, transition};

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

/// Fetch the most recent assignment for a given spark (newest by
/// `created_at`). Used by the override recovery path to find the
/// assignment to transition when the caller only knows the spark id.
pub async fn latest_assignment_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Assignment, SparksError> {
    sqlx::query_as::<_, Assignment>(
        "SELECT * FROM assignments WHERE spark_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(spark_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| SparksError::NotFound(format!("no assignment for spark {spark_id}")))
}

/// Recover a `Stuck` assignment back to `InProgress` with an
/// audit-logged reason. Head/Director only.
///
/// This is the single supported exit from the `Stuck` phase. It layers
/// two things on top of
/// [`transition::transition_assignment_phase_override`]:
///
/// 1. A role gate that rejects every caller whose
///    [`TransitionActorRole`] is not Head or Director — even though the
///    underlying map already refuses non-override roles, we surface the
///    more specific "only Head/Director may override Stuck" error here
///    so the CLI can report it cleanly before the transition runs.
/// 2. A dedicated `assignment_phase_override` audit event carrying the
///    human-supplied `reason`, recorded immediately after the phase
///    transition so the override reason is coupled to the transition
///    it authorised. Note: the transition and the audit-event write
///    are currently two separate writes (not one transaction); a
///    failure between them would leave the assignment recovered with
///    no recorded reason. Atomicity is tracked as tech debt.
pub async fn override_stuck_to_in_progress(
    pool: &SqlitePool,
    assignment_id: &str,
    actor_id: &str,
    actor_role: TransitionActorRole,
    reason: &str,
) -> Result<Assignment, SparksError> {
    if !matches!(
        actor_role,
        TransitionActorRole::Head | TransitionActorRole::Director
    ) {
        return Err(SparksError::Transition(TransitionError::Unauthorized {
            role: actor_role.as_str(),
            from: AssignmentPhase::Stuck.as_str(),
            to: AssignmentPhase::InProgress.as_str(),
            authorized: "head, director".to_string(),
        }));
    }

    let existing =
        sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE assignment_id = ?")
            .bind(assignment_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| SparksError::NotFound(format!("assignment {assignment_id}")))?;

    let next_version = existing.event_version.saturating_add(1);

    let updated = transition::transition_assignment_phase_override(
        pool,
        existing.id,
        actor_id,
        actor_role,
        AssignmentPhase::InProgress,
        AssignmentPhase::Stuck,
        next_version,
    )
    .await?;

    // Record the override reason as its own audit event so the reason
    // text is queryable by spark (not just by assignment_id). The
    // `phase` field_name pattern matches the projector's existing
    // consumers — they ignore rows whose new_value is not a phase
    // literal.
    event_repo::record(
        pool,
        NewEvent {
            spark_id: updated.spark_id.clone(),
            actor: actor_id.to_string(),
            field_name: "assignment_phase_override".into(),
            old_value: Some(AssignmentPhase::Stuck.as_str().to_string()),
            new_value: Some(AssignmentPhase::InProgress.as_str().to_string()),
            reason: Some(reason.to_string()),
            actor_type: None,
            change_nature: None,
            session_id: None,
        },
    )
    .await?;

    Ok(updated)
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
