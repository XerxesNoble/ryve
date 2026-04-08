// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for delegation traces.
//!
//! A delegation trace records one hop of a request travelling through Ryve's
//! agent hierarchy: Director (Atlas) → Head → Hand. The full chain for a
//! single user request is reconstructed by walking `parent_trace_id` upward
//! to the root, whose `origin_actor` identifies the Director (Atlas by
//! default — see [`ATLAS_ORIGIN`]).
//!
//! Spark ryve-1e3848b6.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::*;

/// Insert a new delegation trace. If `new.origin_actor` is `None` the row is
/// recorded as Atlas-originated, satisfying the spark's "Atlas visible as
/// delegation origin" acceptance criterion for the common case where the
/// caller is the Director itself or a downstream Head that doesn't override
/// the root identity.
pub async fn create(
    pool: &SqlitePool,
    new: NewDelegationTrace,
) -> Result<DelegationTrace, SparksError> {
    let id = generate_id("dt");
    let now = Utc::now().to_rfc3339();
    let origin_actor = new.origin_actor.unwrap_or_else(|| ATLAS_ORIGIN.to_string());

    sqlx::query(
        "INSERT INTO delegation_traces (
            id, workshop_id, spark_id, parent_trace_id,
            originating_request, origin_actor,
            delegating_actor, delegating_actor_kind,
            delegated_target, delegated_target_kind,
            status, created_at, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
    )
    .bind(&id)
    .bind(&new.workshop_id)
    .bind(&new.spark_id)
    .bind(&new.parent_trace_id)
    .bind(&new.originating_request)
    .bind(&origin_actor)
    .bind(&new.delegating_actor)
    .bind(new.delegating_actor_kind.as_str())
    .bind(&new.delegated_target)
    .bind(new.delegated_target_kind.as_str())
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    get(pool, &id).await
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<DelegationTrace, SparksError> {
    sqlx::query_as::<_, DelegationTrace>("SELECT * FROM delegation_traces WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("delegation_trace {id}")))
}

/// Update a hop's status. Setting it to `Completed` or `Failed` also stamps
/// `completed_at` so callers don't have to remember to do it themselves.
pub async fn update_status(
    pool: &SqlitePool,
    id: &str,
    status: DelegationStatus,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    let completed_at = matches!(
        status,
        DelegationStatus::Completed | DelegationStatus::Failed
    )
    .then(|| now.clone());

    let result = sqlx::query(
        "UPDATE delegation_traces
         SET status = ?, updated_at = ?, completed_at = COALESCE(?, completed_at)
         WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(&now)
    .bind(&completed_at)
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("delegation_trace {id}")));
    }
    Ok(())
}

/// Record the raw output produced by the delegated target. Does not change
/// status — call [`update_status`] separately if the hop is now finished.
pub async fn record_execution_result(
    pool: &SqlitePool,
    id: &str,
    result_text: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE delegation_traces
         SET execution_result = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(result_text)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("delegation_trace {id}")));
    }
    Ok(())
}

/// Record the Director's final synthesis back to the user. Should only be
/// called on the root hop of a delegation chain — `parent_trace_id` must be
/// NULL — but this is a soft expectation, not enforced at the schema level.
pub async fn record_final_synthesis(
    pool: &SqlitePool,
    id: &str,
    synthesis: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE delegation_traces
         SET final_synthesis = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(synthesis)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(SparksError::NotFound(format!("delegation_trace {id}")));
    }
    Ok(())
}

pub async fn list_for_workshop(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<DelegationTrace>, SparksError> {
    Ok(sqlx::query_as::<_, DelegationTrace>(
        "SELECT * FROM delegation_traces
         WHERE workshop_id = ?
         ORDER BY created_at ASC",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list_for_spark(
    pool: &SqlitePool,
    spark_id: &str,
) -> Result<Vec<DelegationTrace>, SparksError> {
    Ok(sqlx::query_as::<_, DelegationTrace>(
        "SELECT * FROM delegation_traces
         WHERE spark_id = ?
         ORDER BY created_at ASC",
    )
    .bind(spark_id)
    .fetch_all(pool)
    .await?)
}

/// All direct children of `parent_id` — one level of the delegation tree.
pub async fn list_children(
    pool: &SqlitePool,
    parent_id: &str,
) -> Result<Vec<DelegationTrace>, SparksError> {
    Ok(sqlx::query_as::<_, DelegationTrace>(
        "SELECT * FROM delegation_traces
         WHERE parent_trace_id = ?
         ORDER BY created_at ASC",
    )
    .bind(parent_id)
    .fetch_all(pool)
    .await?)
}

/// Walk parents upward from `id` to the root, returning the chain in
/// root-first order. Used by the Hands panel and trace viewer to render the
/// "Atlas → Head → Hand" breadcrumb for a single hop.
pub async fn ancestor_chain(
    pool: &SqlitePool,
    id: &str,
) -> Result<Vec<DelegationTrace>, SparksError> {
    let mut chain = Vec::new();
    let mut cursor = Some(id.to_string());
    while let Some(current) = cursor {
        let row = get(pool, &current).await?;
        cursor = row.parent_trace_id.clone();
        chain.push(row);
    }
    chain.reverse();
    Ok(chain)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<u64, SparksError> {
    let result = sqlx::query("DELETE FROM delegation_traces WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
