// SPDX-License-Identifier: AGPL-3.0-or-later

//! Append-only audit trail for spark changes.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::*;

pub async fn record(pool: &SqlitePool, new: NewEvent) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    let actor_type = new.actor_type.map(|a| a.as_str().to_string());
    let change_nature = new.change_nature.map(|c| c.as_str().to_string());

    sqlx::query(
        "INSERT INTO events (spark_id, actor, field_name, old_value, new_value, reason, timestamp, actor_type, change_nature, session_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&new.spark_id)
    .bind(&new.actor)
    .bind(&new.field_name)
    .bind(&new.old_value)
    .bind(&new.new_value)
    .bind(&new.reason)
    .bind(&now)
    .bind(&actor_type)
    .bind(&change_nature)
    .bind(&new.session_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    new: NewEvent,
) -> Result<i64, SparksError> {
    let now = Utc::now().to_rfc3339();
    let actor_type = new.actor_type.map(|a| a.as_str().to_string());
    let change_nature = new.change_nature.map(|c| c.as_str().to_string());

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO events (spark_id, actor, field_name, old_value, new_value, reason, timestamp, actor_type, change_nature, session_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&new.spark_id)
    .bind(&new.actor)
    .bind(&new.field_name)
    .bind(&new.old_value)
    .bind(&new.new_value)
    .bind(&new.reason)
    .bind(&now)
    .bind(&actor_type)
    .bind(&change_nature)
    .bind(&new.session_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(id)
}

pub async fn list_by_actor_type(
    pool: &SqlitePool,
    spark_id: &str,
    actor_type: &str,
) -> Result<Vec<Event>, SparksError> {
    Ok(sqlx::query_as::<_, Event>(
        "SELECT * FROM events WHERE spark_id = ? AND actor_type = ? ORDER BY timestamp ASC",
    )
    .bind(spark_id)
    .bind(actor_type)
    .fetch_all(pool)
    .await?)
}

pub async fn list_for_spark(pool: &SqlitePool, spark_id: &str) -> Result<Vec<Event>, SparksError> {
    Ok(
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events WHERE spark_id = ? ORDER BY timestamp ASC",
        )
        .bind(spark_id)
        .fetch_all(pool)
        .await?,
    )
}
