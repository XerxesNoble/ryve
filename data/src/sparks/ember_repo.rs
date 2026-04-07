// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for Embers (ephemeral inter-agent signals).

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::*;

pub async fn create(pool: &SqlitePool, new: NewEmber) -> Result<Ember, SparksError> {
    let id = generate_id("em");
    let now = Utc::now().to_rfc3339();
    let ttl = new.ttl_seconds.unwrap_or(3600);

    sqlx::query(
        "INSERT INTO embers (id, ember_type, content, source_agent, workshop_id, ttl_seconds, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(new.ember_type.as_str())
    .bind(&new.content)
    .bind(&new.source_agent)
    .bind(&new.workshop_id)
    .bind(ttl)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(Ember {
        id,
        ember_type: new.ember_type.as_str().to_string(),
        content: new.content,
        source_agent: new.source_agent,
        workshop_id: new.workshop_id,
        ttl_seconds: ttl,
        created_at: now,
    })
}

/// List embers that have not yet expired.
pub async fn list_active(pool: &SqlitePool, workshop_id: &str) -> Result<Vec<Ember>, SparksError> {
    let now = Utc::now().to_rfc3339();
    Ok(sqlx::query_as::<_, Ember>(
        "SELECT * FROM embers WHERE workshop_id = ? AND datetime(created_at, '+' || ttl_seconds || ' seconds') > datetime(?) ORDER BY created_at DESC",
    )
    .bind(workshop_id)
    .bind(&now)
    .fetch_all(pool)
    .await?)
}

/// List active embers of a specific type.
pub async fn list_by_type(
    pool: &SqlitePool,
    workshop_id: &str,
    ember_type: EmberType,
) -> Result<Vec<Ember>, SparksError> {
    let now = Utc::now().to_rfc3339();
    Ok(sqlx::query_as::<_, Ember>(
        "SELECT * FROM embers WHERE workshop_id = ? AND ember_type = ? AND datetime(created_at, '+' || ttl_seconds || ' seconds') > datetime(?) ORDER BY created_at DESC",
    )
    .bind(workshop_id)
    .bind(ember_type.as_str())
    .bind(&now)
    .fetch_all(pool)
    .await?)
}

/// Delete a single ember by id. Used by the UI dismiss-button flow —
/// when a user dismisses a notification it is removed from the backing
/// store so it does not reappear on the next poll.
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<u64, SparksError> {
    let result = sqlx::query("DELETE FROM embers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Delete all expired embers. Returns number removed.
pub async fn sweep_expired(pool: &SqlitePool) -> Result<u64, SparksError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "DELETE FROM embers WHERE datetime(created_at, '+' || ttl_seconds || ' seconds') <= datetime(?)",
    )
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}
