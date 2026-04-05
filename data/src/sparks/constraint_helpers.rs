// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Architectural constraints — stored as engravings with a `constraint:` key prefix.

use sqlx::SqlitePool;

use super::engraving_repo;
use super::error::SparksError;
use super::types::*;

const PREFIX: &str = "constraint:";

/// Create or update an architectural constraint.
pub async fn upsert(
    pool: &SqlitePool,
    name: &str,
    workshop_id: &str,
    constraint: &ArchConstraint,
    author: Option<&str>,
) -> Result<Engraving, SparksError> {
    let key = format!("{PREFIX}{name}");
    let value = serde_json::to_string(constraint)
        .map_err(|e| SparksError::Serialization(e.to_string()))?;

    engraving_repo::upsert(
        pool,
        NewEngraving {
            key,
            workshop_id: workshop_id.to_string(),
            value,
            author: author.map(String::from),
        },
    )
    .await
}

/// List all architectural constraints for a workshop.
/// Returns `(name, constraint)` pairs where `name` is the key without the prefix.
pub async fn list(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<(String, ArchConstraint)>, SparksError> {
    let all = engraving_repo::list_for_workshop(pool, workshop_id).await?;
    let mut constraints = Vec::new();

    for eng in all {
        if let Some(name) = eng.key.strip_prefix(PREFIX) {
            if let Ok(c) = serde_json::from_str::<ArchConstraint>(&eng.value) {
                constraints.push((name.to_string(), c));
            }
        }
    }

    Ok(constraints)
}

/// Delete an architectural constraint by name.
pub async fn delete(
    pool: &SqlitePool,
    name: &str,
    workshop_id: &str,
) -> Result<(), SparksError> {
    let key = format!("{PREFIX}{name}");
    engraving_repo::delete(pool, &key, workshop_id).await
}
