// SPDX-License-Identifier: AGPL-3.0-or-later

//! Durable log of IRC messages delivered to a channel.
//!
//! Backs the adversarial-review IRC facade (epic `ryve-5dcdf56e`): the relay
//! persists every accepted IRC event here, and the UI channel view /
//! replay tooling read it back by epic. Messages are append-only — see the
//! non-goal on spark `sp-ddf6fd7f`.
//!
//! Full-text search is served by the `irc_messages_fts` virtual table
//! kept in sync by the `irc_messages_ai` trigger emitted in migration
//! `019_irc_messages.sql`. The tokenizer is `unicode61`, SQLite's
//! language-agnostic equivalent of Postgres' `simple` dictionary.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::*;

/// Persist one IRC message and return the full row (including the
/// server-assigned `id` and `created_at`).
pub async fn insert_message(
    pool: &SqlitePool,
    new: NewIrcMessage,
) -> Result<IrcMessage, SparksError> {
    let now = Utc::now().to_rfc3339();

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO irc_messages \
         (epic_id, channel, irc_message_id, sender_actor_id, command, raw_text, \
          structured_event_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(&new.epic_id)
    .bind(&new.channel)
    .bind(&new.irc_message_id)
    .bind(&new.sender_actor_id)
    .bind(new.command.as_str())
    .bind(&new.raw_text)
    .bind(&new.structured_event_id)
    .bind(&now)
    .fetch_one(pool)
    .await?;

    Ok(IrcMessage {
        id,
        epic_id: new.epic_id,
        channel: new.channel,
        irc_message_id: new.irc_message_id,
        sender_actor_id: new.sender_actor_id,
        command: new.command.as_str().to_string(),
        raw_text: new.raw_text,
        structured_event_id: new.structured_event_id,
        created_at: now,
    })
}

/// List messages for an epic in chronological order. `since` filters to
/// messages strictly newer than the given RFC-3339 timestamp; pass `None`
/// to start from the beginning. `limit` caps the page size.
pub async fn list_by_epic(
    pool: &SqlitePool,
    epic_id: &str,
    since: Option<&str>,
    limit: i64,
) -> Result<Vec<IrcMessage>, SparksError> {
    let rows = match since {
        Some(since) => {
            sqlx::query_as::<_, IrcMessage>(
                "SELECT * FROM irc_messages \
                 WHERE epic_id = ? AND created_at > ? \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT ?",
            )
            .bind(epic_id)
            .bind(since)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, IrcMessage>(
                "SELECT * FROM irc_messages \
                 WHERE epic_id = ? \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT ?",
            )
            .bind(epic_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };

    Ok(rows)
}

/// Full-text search for `query` across `raw_text`, scoped to one epic.
/// Results are ordered by FTS rank (most relevant first), with ties
/// broken by `created_at`. `limit` caps the number of matches returned.
pub async fn search_text(
    pool: &SqlitePool,
    epic_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<IrcMessage>, SparksError> {
    let rows = sqlx::query_as::<_, IrcMessage>(
        "SELECT m.* FROM irc_messages m \
         JOIN irc_messages_fts fts ON fts.rowid = m.id \
         WHERE m.epic_id = ? AND irc_messages_fts MATCH ? \
         ORDER BY fts.rank, m.created_at ASC \
         LIMIT ?",
    )
    .bind(epic_id)
    .bind(query)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}
