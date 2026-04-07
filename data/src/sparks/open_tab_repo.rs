// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Persistence for the bench's open tab list. The whole snapshot is
//! rewritten on every save so callers don't have to track diffs.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

use super::error::SparksError;

/// One persisted tab. The order of `Vec<PersistedTab>` returned from
/// [`list_for_workshop`] reflects the original tab order in the bench.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PersistedTab {
    pub workshop_id: String,
    pub position: i64,
    /// One of `terminal` or `file_viewer`. See migration 007 for the
    /// rationale on why coding-agent tabs are excluded.
    pub tab_kind: String,
    pub title: String,
    /// Kind-specific payload — see migration 007.
    pub payload: Option<String>,
}

/// Replace every open-tab row for a workshop with the given snapshot.
pub async fn save_snapshot(
    pool: &SqlitePool,
    workshop_id: &str,
    tabs: &[PersistedTab],
) -> Result<(), SparksError> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM open_tabs WHERE workshop_id = ?")
        .bind(workshop_id)
        .execute(&mut *tx)
        .await?;

    for tab in tabs {
        sqlx::query(
            "INSERT INTO open_tabs (workshop_id, position, tab_kind, title, payload)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(workshop_id)
        .bind(tab.position)
        .bind(&tab.tab_kind)
        .bind(&tab.title)
        .bind(&tab.payload)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Load every persisted tab for a workshop, ordered by position.
pub async fn list_for_workshop(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<PersistedTab>, SparksError> {
    let tabs = sqlx::query_as::<_, PersistedTab>(
        "SELECT workshop_id, position, tab_kind, title, payload
         FROM open_tabs
         WHERE workshop_id = ?
         ORDER BY position ASC",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?;
    Ok(tabs)
}
