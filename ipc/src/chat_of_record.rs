// SPDX-License-Identifier: AGPL-3.0-or-later

//! Chat-of-record primitives: `post` + `tail`.
//!
//! Durable agent chat is backed by the `irc_messages` table (migration
//! `019_irc_messages.sql`). This module is the shared implementation behind
//! the `ryve post` / `ryve channel tail` CLI commands and the future
//! `chat.post` / `chat.tail` MCP tools — all three wrap the same two
//! functions here.
//!
//! ## Contract
//!
//! - [`post_message`] writes a row to `irc_messages` and returns the row's
//!   primary key. The database write is the ENTIRE contract — this
//!   module does not emit anything on the IRC wire. Agents read each
//!   other's posts via [`tail`] (DB-backed), not via an IRC client
//!   subscription.
//!
//!   PR #56 Copilot c3: earlier wording described IRC wire delivery as
//!   "best-effort via the outbox relay". That was aspirational — no
//!   call here enqueues an outbox row, and no code path in this
//!   module forwards to an `IrcClient`. IRC-wire emission for
//!   chat-of-record posts is tracked as a separate 0.4.0 candidate;
//!   until it lands, channel subscribers on the actual IRC server
//!   will NOT see chat-of-record posts in real time. Cross-agent
//!   coordination happens through `ryve channel tail` reading the DB.
//! - [`tail`] reads `irc_messages` scoped to a channel, optionally
//!   filtered by `since` / `author`, and caps the result at `limit` rows.
//!
//! ## Channel → epic mapping
//!
//! The `irc_messages` schema (migration 019) keys every row on an
//! `epic_id` that foreign-keys to `sparks.id`. The canonical channel
//! naming produced by [`crate::channel_manager::channel_name`] is
//! `#epic-<id>-<slug>`, so callers that have only a channel name can
//! recover the epic id via [`resolve_epic_id_for_channel`]. Workshop-
//! scoped well-known channels (`#atlas` and friends) are out of scope
//! for this foundation: a sibling spark in the chat-of-record epic adds
//! them with their own spark-id binding.

use chrono::Utc;
use data::sparks::error::SparksError;
use data::sparks::irc_repo;
use data::sparks::types::{IrcCommand, IrcMessage, NewIrcMessage};
use sqlx::SqlitePool;
use thiserror::Error;

use crate::channel_manager::{EpicRef, channel_name};

/// Input for [`post_message`].
#[derive(Debug, Clone)]
pub struct NewPost {
    /// IRC channel name, including the leading `#`. Must be the canonical
    /// name produced by [`crate::channel_manager::channel_name`] for the
    /// row's epic — the derived `epic_id` is extracted from the channel
    /// string.
    pub channel: String,
    /// Free-form message text. Trailing whitespace is preserved as-is;
    /// callers that render to IRC wire format should collapse newlines.
    pub body: String,
    /// Optional `agent_sessions.id` of the author. `None` for unattributed
    /// posts (human CLI use outside of any spawned Hand session).
    pub author_session_id: Option<String>,
    /// Optional explicit epic id override. When `None` the epic id is
    /// resolved from [`NewPost::channel`] via
    /// [`resolve_epic_id_for_channel`]; pass `Some` when the caller
    /// already knows the spark id (e.g. a future well-known-channel
    /// resolver).
    pub epic_id: Option<String>,
}

/// Errors returned by [`post_message`] and [`tail`].
#[derive(Debug, Error)]
pub enum ChatError {
    /// The channel string could not be parsed into a `#epic-<id>-...`
    /// form and no explicit `epic_id` was provided.
    #[error(
        "cannot derive epic id from channel {0:?}; pass an explicit epic id or use the canonical #epic-<id>-<slug> form"
    )]
    UnresolvedEpic(String),
    /// A caller-supplied limit fell outside the accepted range.
    #[error("limit must be between 1 and {max}; got {got}")]
    InvalidLimit { got: i64, max: i64 },
    /// Underlying repo / sqlx failure.
    #[error("sparks: {0}")]
    Sparks(#[from] SparksError),
    /// Underlying sqlx failure outside the typed repo (direct queries).
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Upper bound on `limit` for [`tail`]. Keeps unbounded agent-driven
/// queries from loading the whole channel history into a single response.
pub const TAIL_MAX_LIMIT: i64 = 1_000;

/// Default `limit` used when the CLI caller does not supply one. Small
/// enough to fit an agent's reading window; callers that want more must
/// opt in explicitly.
pub const TAIL_DEFAULT_LIMIT: i64 = 50;

/// Resolve an epic id for a canonical `#epic-<id>-<slug>` channel name
/// by scanning epic sparks in the database.
///
/// Why a DB lookup instead of a pure parser: Ryve spark ids like
/// `ryve-a1b2c3d4` contain dashes, and the slug portion may also
/// contain dashes, so there is no syntactic way to split the channel
/// string back into `(id, slug)`. Instead we enumerate epic-type
/// sparks, recompute each one's canonical channel name via
/// [`crate::channel_manager::channel_name`], and return the spark whose
/// name matches exactly. That keeps the channel↔epic mapping tied to a
/// single source of truth — any drift in `channel_name` is automatically
/// reflected here.
///
/// Returns `None` when no epic's canonical channel matches. Well-known
/// workshop channels (`#atlas` and friends) intentionally do not resolve
/// here — they are handled by sibling sparks in the chat-of-record epic.
pub async fn resolve_epic_id_for_channel(
    pool: &SqlitePool,
    channel: &str,
) -> Result<Option<String>, ChatError> {
    if !channel.starts_with("#epic-") {
        return Ok(None);
    }
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT id, title FROM sparks WHERE spark_type = 'epic'",
    )
    .fetch_all(pool)
    .await?;
    for (id, title) in rows {
        let candidate = channel_name(&EpicRef {
            id: id.clone(),
            name: title,
        });
        if candidate == channel {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Filter for [`tail`]. `channel` is required; the rest are optional.
#[derive(Debug, Clone)]
pub struct TailFilter {
    /// Channel to read from, including the leading `#`.
    pub channel: String,
    /// RFC-3339 timestamp cutoff. Returns rows with `created_at > since`.
    /// `None` returns the first page from the channel's start.
    pub since: Option<String>,
    /// Maximum rows to return. [`tail`] rejects out-of-range values
    /// (anything outside `1..=TAIL_MAX_LIMIT`) with
    /// [`ChatError::InvalidLimit`] so the caller can surface the
    /// misuse rather than silently truncating. PR #54 Copilot c2:
    /// earlier wording said "clamped" which contradicted the actual
    /// reject-on-invalid behaviour.
    pub limit: i64,
    /// Optional author filter. Matches `irc_messages.sender_actor_id`
    /// exactly. `None` returns every author's posts.
    pub author_session_id: Option<String>,
}

impl TailFilter {
    /// Construct a filter with the module's default limit. Use the
    /// setter-style methods to attach `since` / `author` / a custom
    /// `limit` before handing it to [`tail`].
    pub fn for_channel(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            since: None,
            limit: TAIL_DEFAULT_LIMIT,
            author_session_id: None,
        }
    }

    pub fn with_since(mut self, since: Option<String>) -> Self {
        self.since = since;
        self
    }

    pub fn with_limit(mut self, limit: i64) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_author(mut self, author: Option<String>) -> Self {
        self.author_session_id = author;
        self
    }
}

/// Persist a chat post in `irc_messages` and return the inserted row's
/// primary key.
///
/// The `irc_message_id` column is populated with a fresh UUIDv4 so every
/// chat post has a stable wire id even without an originating outbox
/// event. The `command` is always `PRIVMSG` — NOTICE and TOPIC belong to
/// the relay, not to agent chat.
///
/// Outbox relay is best-effort: this function does not enqueue a row in
/// `event_outbox` because chat posts carry no assignment-lifecycle
/// meaning. Callers that want IRC wire delivery should run an
/// [`crate::irc_client::IrcClient`] alongside and route sends there; the
/// durable record here is the contract regardless.
pub async fn post_message(pool: &SqlitePool, new: NewPost) -> Result<i64, ChatError> {
    let NewPost {
        channel,
        body,
        author_session_id,
        epic_id,
    } = new;

    let epic_id = match epic_id {
        Some(id) => id,
        None => resolve_epic_id_for_channel(pool, &channel)
            .await?
            .ok_or_else(|| ChatError::UnresolvedEpic(channel.clone()))?,
    };

    let row = irc_repo::insert_message(
        pool,
        NewIrcMessage {
            epic_id,
            channel,
            irc_message_id: uuid::Uuid::new_v4().to_string(),
            sender_actor_id: author_session_id,
            command: IrcCommand::Privmsg,
            raw_text: body,
            structured_event_id: None,
        },
    )
    .await?;

    Ok(row.id)
}

/// Read a bounded window of chat posts from a channel.
///
/// Rows are returned in chronological order (`created_at ASC, id ASC`)
/// so pagination cursors can use the last row's `created_at` as the next
/// `since` without rewrites. A `limit` outside the valid range is
/// rejected with [`ChatError::InvalidLimit`] so the caller sees the
/// misuse instead of getting silently clamped.
pub async fn tail(pool: &SqlitePool, filter: TailFilter) -> Result<Vec<IrcMessage>, ChatError> {
    let TailFilter {
        channel,
        since,
        limit,
        author_session_id,
    } = filter;

    if !(1..=TAIL_MAX_LIMIT).contains(&limit) {
        return Err(ChatError::InvalidLimit {
            got: limit,
            max: TAIL_MAX_LIMIT,
        });
    }

    // Build the query dynamically: sqlx's QueryBuilder would work but
    // the filter combinations are small enough that four branches stay
    // readable and keep each SQL string greppable.
    let rows = match (since.as_deref(), author_session_id.as_deref()) {
        (None, None) => {
            sqlx::query_as::<_, IrcMessage>(
                "SELECT * FROM irc_messages \
                 WHERE channel = ? \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT ?",
            )
            .bind(&channel)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        (Some(since), None) => {
            sqlx::query_as::<_, IrcMessage>(
                "SELECT * FROM irc_messages \
                 WHERE channel = ? AND created_at > ? \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT ?",
            )
            .bind(&channel)
            .bind(since)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        (None, Some(author)) => {
            sqlx::query_as::<_, IrcMessage>(
                "SELECT * FROM irc_messages \
                 WHERE channel = ? AND sender_actor_id = ? \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT ?",
            )
            .bind(&channel)
            .bind(author)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        (Some(since), Some(author)) => {
            sqlx::query_as::<_, IrcMessage>(
                "SELECT * FROM irc_messages \
                 WHERE channel = ? AND created_at > ? AND sender_actor_id = ? \
                 ORDER BY created_at ASC, id ASC \
                 LIMIT ?",
            )
            .bind(&channel)
            .bind(since)
            .bind(author)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };

    Ok(rows)
}

/// RFC-3339 timestamp helper used by callers that want to mint a `since`
/// cursor from a [`chrono::DateTime`]. Kept here so CLI + MCP + test
/// plumbing share a single spelling.
pub fn rfc3339_now() -> String {
    Utc::now().to_rfc3339()
}

/// Count chat-of-record posts authored by `session_id` that target
/// `spark_id` since `since` (inclusive). Used by the `ryve assign close`
/// mandatory-post gate to enforce the "on handoff" posting boundary
/// defined on epic ryve-12f09190.
///
/// "Target" is mapped to `irc_messages.epic_id`: the column is a generic
/// spark FK (see migration 019), not epic-only. The gate counts posts
/// tagged with EITHER the closing Hand's own spark id OR its parent
/// epic id — IRC wire delivery is irrelevant because the durable DB
/// row is the contract.
///
/// PR #54 Copilot c3 — parent-epic fallback:
/// `ryve post --channel '#epic-<id>'` resolves `irc_messages.epic_id`
/// from the channel name, so a Hand working on a child task posts
/// with `epic_id = <parent epic id>`. If the gate only matched
/// `epic_id == spark_id`, every Hand on a non-epic spark would fail
/// the gate even after posting correctly. Counting `epic_id IN
/// (spark_id, parent_epic_of(spark_id))` matches both: posts that
/// reference the spark directly AND posts to the parent epic's
/// channel (the canonical channel for in-flight Hand chatter).
///
/// `since` is an RFC-3339 timestamp, typically the assignment's
/// `assigned_at`. Rows with `created_at >= since` are counted so a post
/// made in the same instant as the claim still satisfies the gate —
/// the acceptance criteria wants "since the claim timestamp", which we
/// read as "at or after", not "strictly after".
pub async fn count_posts_since_claim(
    pool: &SqlitePool,
    session_id: &str,
    spark_id: &str,
    since: &str,
) -> Result<i64, ChatError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM irc_messages \
         WHERE sender_actor_id = ? \
           AND epic_id IN ( \
             ?, \
             (SELECT from_id FROM bonds \
              WHERE to_id = ? AND bond_type = 'parent_child' \
              LIMIT 1) \
           ) \
           AND created_at >= ?",
    )
    .bind(session_id)
    .bind(spark_id)
    .bind(spark_id)
    .bind(since)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_filter_defaults_are_sensible() {
        let f = TailFilter::for_channel("#epic-1-x");
        assert_eq!(f.channel, "#epic-1-x");
        assert_eq!(f.limit, TAIL_DEFAULT_LIMIT);
        assert!(f.since.is_none());
        assert!(f.author_session_id.is_none());
    }

    #[test]
    fn tail_filter_builder_chains_carry_values() {
        let f = TailFilter::for_channel("#epic-1-x")
            .with_since(Some("2026-04-19T00:00:00Z".into()))
            .with_limit(25)
            .with_author(Some("sess-alice".into()));
        assert_eq!(f.since.as_deref(), Some("2026-04-19T00:00:00Z"));
        assert_eq!(f.limit, 25);
        assert_eq!(f.author_session_id.as_deref(), Some("sess-alice"));
    }
}
