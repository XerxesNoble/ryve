// SPDX-License-Identifier: AGPL-3.0-or-later

//! Channel projection — filtered views over `irc_messages` for UI surfaces.
//!
//! A projection is a query over one channel's durable IRC log that returns
//! the messages matching a combination of up-to-five structured filter
//! axes plus an optional FTS5 full-text match. The axes are ANDed; each
//! one is optional. Axes that reference structured event fields
//! (`assignment_id`, `actor_id`, `spark_id`, `pr_number`) join
//! [`irc_messages`] to [`event_outbox`] on `structured_event_id` and match
//! on exact column or JSON-extracted values — never on substrings of the
//! human-readable `raw_text`.
//!
//! ## Filter axes
//!
//! | Axis            | Source                                                        |
//! |-----------------|---------------------------------------------------------------|
//! | `epic_id`       | `irc_messages.epic_id` (direct column)                        |
//! | `spark_id`      | `json_extract(event_outbox.payload, '$.spark_id')`            |
//! | `assignment_id` | `event_outbox.assignment_id` (direct column)                  |
//! | `pr_number`     | `json_extract(event_outbox.payload, '$.pr_number')`           |
//! | `actor_id`      | `event_outbox.actor_id` (direct column)                       |
//!
//! Plus `fts_query`: passed straight to `irc_messages_fts MATCH ?`. The
//! query uses the existing `idx_irc_messages_epic_created` and the
//! `irc_messages_fts` virtual table — it never adds an index or a
//! materialised view to `irc_messages` (see migration 019 invariants).
//!
//! ## Mentions override
//!
//! If a [`ChannelProjectionQuery::current_actor_id`] is supplied, any
//! message whose `raw_text` contains `@<actor_id>` is surfaced in the
//! result even when it would otherwise be filtered out. The override is
//! additive: it never shrinks the match set, only grows it. Surfaced
//! mention rows are flagged on [`ProjectedMessage::matched_by_mention`]
//! so the UI can distinguish them from filter hits.
//!
//! ## Empty state
//!
//! A query that matches zero rows returns [`ProjectionOutput::Empty`] —
//! distinct from [`ProjectionError`], so the UI can render an explicit
//! "nothing to show" empty state instead of an error banner.
//!
//! ## Signal-discipline invariant
//!
//! The projection SELECTs only from `irc_messages` (with a LEFT JOIN on
//! `event_outbox` purely for filter and badge fields). It can never
//! surface an event that was not written to `irc_messages` by the relay,
//! which is the invariant from epic ryve-5dcdf56e / the signal-discipline
//! module.
//!
//! ## Presets
//!
//! Saved filter combinations live in `projection_presets` (migration
//! 023). The CRUD helpers at the bottom of this module are the only
//! writers; the UI's unread-badge query is [`preset_unread_count`], which
//! computes `max(m.id matching preset filters) - last_seen_message_id`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use thiserror::Error;

/// All errors surfaced by the projection module. Callers typically
/// surface [`ProjectionError::Database`] as an operator-visible banner and
/// [`ProjectionError::InvalidFtsQuery`] as an inline validation on the
/// search box.
#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid FTS query: {0}")]
    InvalidFtsQuery(String),
    #[error("preset not found: {0}")]
    PresetNotFound(i64),
    #[error("invalid preset filters: {0}")]
    InvalidPresetFilters(String),
}

/// One projected IRC message. `event_type` and `metadata` come from the
/// joined `event_outbox` row when a `structured_event_id` is present —
/// both are `None` for user chatter that did not originate from the
/// outbox (inbound PRIVMSGs, operator notices, topic changes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedMessage {
    pub id: i64,
    pub epic_id: String,
    pub channel: String,
    pub sender_actor_id: Option<String>,
    pub timestamp: String,
    /// Canonical event_type badge, e.g. `"assignment.created"`.
    pub event_type: Option<String>,
    pub raw_text: String,
    pub structured_event_id: Option<String>,
    pub metadata: Option<StructuredMetadata>,
    /// `true` when this message matched only because of the mentions
    /// override — the filter axes would have excluded it otherwise.
    pub matched_by_mention: bool,
}

/// Structured metadata copied from the joined `event_outbox` row. The
/// typed fields are the hot ones the UI wants without re-parsing JSON;
/// `payload` carries the full event body for richer surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredMetadata {
    pub assignment_id: Option<String>,
    pub actor_id: Option<String>,
    pub spark_id: Option<String>,
    pub pr_number: Option<u64>,
    pub payload: serde_json::Value,
}

/// Projection query. Every field is optional — an all-`None` query over
/// a channel returns every message on that channel, which is the UI's
/// "no filter" default. Axis fields are ANDed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelProjectionQuery {
    /// Scope to one epic (channel). Most UI surfaces set this; tests
    /// that want to exercise cross-channel behaviour can leave it `None`.
    pub epic_id: Option<String>,
    /// Scope to one literal channel name — orthogonal to `epic_id` so a
    /// preset keyed on a channel can still apply after a rename.
    pub channel: Option<String>,
    /// Axis: `json_extract(event_outbox.payload, '$.spark_id')`.
    pub spark_id: Option<String>,
    /// Axis: `event_outbox.assignment_id`.
    pub assignment_id: Option<String>,
    /// Axis: `json_extract(event_outbox.payload, '$.pr_number')`.
    pub pr_number: Option<u64>,
    /// Axis: `event_outbox.actor_id`.
    pub actor_id: Option<String>,
    /// FTS5 MATCH string applied to `irc_messages_fts.raw_text`.
    pub fts_query: Option<String>,
    /// If set, messages whose `raw_text` contains `@<actor_id>` bypass
    /// the filter axes and are always surfaced. This is the mention
    /// override — the UI sets it to the current session's actor so the
    /// user never misses an `@mention` hidden behind an active filter.
    pub current_actor_id: Option<String>,
    /// Upper bound on rows returned. Values `<= 0` fall back to
    /// [`DEFAULT_LIMIT`] (see [`Self::effective_limit`]) — there is no
    /// truly-unlimited path; callers that want pagination should set
    /// a real cap. PR #53 Copilot c1: earlier doc said "0 means
    /// unlimited" which contradicted the implementation.
    pub limit: i64,
}

impl ChannelProjectionQuery {
    /// Convenience constructor for callers that only care about one
    /// channel — the common UI case.
    pub fn for_channel(channel: impl Into<String>) -> Self {
        Self {
            channel: Some(channel.into()),
            limit: DEFAULT_LIMIT,
            ..Self::default()
        }
    }

    fn effective_limit(&self) -> i64 {
        if self.limit <= 0 {
            DEFAULT_LIMIT
        } else {
            self.limit
        }
    }
}

/// Reasonable default page size used when a caller leaves
/// [`ChannelProjectionQuery::limit`] at its zero default. Tuned so a
/// fresh channel view fits comfortably on a typical UI surface without
/// extra pagination.
pub const DEFAULT_LIMIT: i64 = 500;

/// Outcome of a projection query. [`ProjectionOutput::Empty`] is
/// deliberately separate from [`ProjectionError`] so the UI can render a
/// dedicated empty state ("no matches") distinct from an error banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionOutput {
    Empty,
    Messages(Vec<ProjectedMessage>),
}

impl ProjectionOutput {
    /// Returns the projected messages, collapsing [`ProjectionOutput::Empty`]
    /// to an empty `Vec`. Convenient for tests and callers that do not
    /// want to pattern-match.
    pub fn into_vec(self) -> Vec<ProjectedMessage> {
        match self {
            ProjectionOutput::Empty => Vec::new(),
            ProjectionOutput::Messages(msgs) => msgs,
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, ProjectionOutput::Empty)
    }
}

/// Run a projection query and return the matched messages in
/// chronological order (oldest first, then tie-broken by id).
pub async fn query(
    pool: &SqlitePool,
    q: &ChannelProjectionQuery,
) -> Result<ProjectionOutput, ProjectionError> {
    validate_fts(q.fts_query.as_deref())?;

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT m.id, m.epic_id, m.channel, m.sender_actor_id, m.created_at AS timestamp, \
         m.raw_text, m.structured_event_id, \
         e.event_type AS event_type, e.assignment_id AS e_assignment_id, \
         e.actor_id AS e_actor_id, e.payload AS e_payload, \
         CASE WHEN ",
    );
    // `matched_by_mention` is true when the filter part was false but the
    // mention predicate was true. We compute it inline so callers can
    // distinguish mention-surfaced rows from filter hits without a
    // second query.
    push_filter_predicate(&mut qb, q);
    qb.push(" THEN 0 ELSE 1 END AS matched_by_mention ");

    qb.push(
        " FROM irc_messages m \
         LEFT JOIN event_outbox e ON e.event_id = m.structured_event_id \
         WHERE 1=1",
    );

    if let Some(epic_id) = &q.epic_id {
        qb.push(" AND m.epic_id = ").push_bind(epic_id.clone());
    }
    if let Some(channel) = &q.channel {
        qb.push(" AND m.channel = ").push_bind(channel.clone());
    }

    qb.push(" AND (");
    push_filter_predicate(&mut qb, q);
    qb.push(" OR ");
    push_mention_predicate(&mut qb, q);
    qb.push(")");

    qb.push(" ORDER BY m.created_at ASC, m.id ASC LIMIT ")
        .push_bind(q.effective_limit());

    let rows = qb.build().fetch_all(pool).await?;

    if rows.is_empty() {
        return Ok(ProjectionOutput::Empty);
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let payload_str: Option<String> = row.try_get("e_payload")?;
        let payload: Option<serde_json::Value> = payload_str
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let metadata = match payload {
            Some(p) => Some(StructuredMetadata {
                assignment_id: row.try_get::<Option<String>, _>("e_assignment_id")?,
                actor_id: row.try_get::<Option<String>, _>("e_actor_id")?,
                spark_id: p
                    .get("spark_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                pr_number: p.get("pr_number").and_then(|v| v.as_u64()),
                payload: p,
            }),
            None => None,
        };
        out.push(ProjectedMessage {
            id: row.try_get("id")?,
            epic_id: row.try_get("epic_id")?,
            channel: row.try_get("channel")?,
            sender_actor_id: row.try_get("sender_actor_id")?,
            timestamp: row.try_get("timestamp")?,
            event_type: row.try_get("event_type")?,
            raw_text: row.try_get("raw_text")?,
            structured_event_id: row.try_get("structured_event_id")?,
            metadata,
            matched_by_mention: row.try_get::<i64, _>("matched_by_mention")? != 0,
        });
    }

    Ok(ProjectionOutput::Messages(out))
}

/// Emit the boolean SQL for "this row satisfies every axis filter".
/// Reused twice per query: once in the CASE that computes
/// `matched_by_mention`, once in the outer WHERE that ORs it with the
/// mention predicate.
fn push_filter_predicate(qb: &mut QueryBuilder<Sqlite>, q: &ChannelProjectionQuery) {
    qb.push("(1=1");
    if let Some(spark_id) = &q.spark_id {
        qb.push(" AND json_extract(e.payload, '$.spark_id') = ")
            .push_bind(spark_id.clone());
    }
    if let Some(assignment_id) = &q.assignment_id {
        qb.push(" AND e.assignment_id = ")
            .push_bind(assignment_id.clone());
    }
    if let Some(pr_number) = q.pr_number {
        // CAST so the json_extract numeric compares against the bound i64
        // regardless of whether sqlite's JSON lib returned a NUMERIC or
        // TEXT value.
        qb.push(" AND CAST(json_extract(e.payload, '$.pr_number') AS INTEGER) = ")
            .push_bind(pr_number as i64);
    }
    if let Some(actor_id) = &q.actor_id {
        qb.push(" AND e.actor_id = ").push_bind(actor_id.clone());
    }
    if let Some(fts) = &q.fts_query {
        qb.push(" AND m.id IN (SELECT rowid FROM irc_messages_fts WHERE irc_messages_fts MATCH ")
            .push_bind(fts.clone())
            .push(")");
    }
    qb.push(")");
}

/// Emit `m.raw_text` contains `@<current_actor_id>`, or `0` when no
/// actor id is supplied. `INSTR` keeps it cheap and avoids FTS
/// tokenizer-driven false negatives (the `@` sigil would be stripped by
/// unicode61, merging mention and casual mention of the same token).
fn push_mention_predicate(qb: &mut QueryBuilder<Sqlite>, q: &ChannelProjectionQuery) {
    match &q.current_actor_id {
        None => {
            qb.push("0");
        }
        Some(actor) => {
            qb.push("INSTR(m.raw_text, ")
                .push_bind(format!("@{actor}"))
                .push(") > 0");
        }
    }
}

/// Cheap guardrail against obviously-broken FTS input. SQLite's FTS5
/// parser surfaces syntax errors at query time as a plain `SqlxError`,
/// which is fine for the operator log but useless to a UI author. This
/// pre-check catches the common cases (empty string, stray quotes).
fn validate_fts(q: Option<&str>) -> Result<(), ProjectionError> {
    let Some(q) = q else { return Ok(()) };
    if q.trim().is_empty() {
        return Err(ProjectionError::InvalidFtsQuery(
            "FTS query is empty".into(),
        ));
    }
    let quotes = q.chars().filter(|c| *c == '"').count();
    if quotes % 2 == 1 {
        return Err(ProjectionError::InvalidFtsQuery(
            "unbalanced quote in FTS query".into(),
        ));
    }
    Ok(())
}

// ── Presets ────────────────────────────────────────────
//
// Persisted filter combinations. Identity is
// `(workshop_id, channel, name)`; the same name can live on multiple
// channels and multiple workshops.

/// Serialised filter configuration stored in `projection_presets.filters_json`.
/// This is the persisted subset of [`ChannelProjectionQuery`] — the
/// per-session fields (`current_actor_id`, `limit`) are intentionally
/// absent because they change with the caller, not the preset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetFilters {
    pub epic_id: Option<String>,
    pub spark_id: Option<String>,
    pub assignment_id: Option<String>,
    pub pr_number: Option<u64>,
    pub actor_id: Option<String>,
    pub fts_query: Option<String>,
}

impl PresetFilters {
    /// Apply this preset's filters onto `base` in place. Axis fields
    /// overwrite; [`ChannelProjectionQuery::current_actor_id`] and
    /// `limit` are left untouched so the caller's session state wins.
    pub fn apply_to(&self, base: &mut ChannelProjectionQuery) {
        // PR #53 Copilot c6: assign every axis unconditionally so a
        // preset with `None` actually clears that axis on `base`. The
        // earlier `if self.epic_id.is_some()` made `epic_id` a
        // set-only field, inconsistent with the other axes and
        // surprising for users who built a preset to *narrow* off the
        // epic axis.
        base.epic_id = self.epic_id.clone();
        base.spark_id = self.spark_id.clone();
        base.assignment_id = self.assignment_id.clone();
        base.pr_number = self.pr_number;
        base.actor_id = self.actor_id.clone();
        base.fts_query = self.fts_query.clone();
    }
}

/// Persisted preset row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionPreset {
    pub id: i64,
    pub workshop_id: String,
    pub channel: String,
    pub name: String,
    pub filters: PresetFilters,
    pub last_seen_message_id: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Input payload for [`create_preset`]. `last_seen_message_id` defaults
/// to 0 (nothing seen yet); the caller can pre-seed it for imports.
#[derive(Debug, Clone)]
pub struct NewProjectionPreset {
    pub workshop_id: String,
    pub channel: String,
    pub name: String,
    pub filters: PresetFilters,
    pub last_seen_message_id: Option<i64>,
}

/// Create a new preset. Returns the persisted row.
pub async fn create_preset(
    pool: &SqlitePool,
    new: NewProjectionPreset,
) -> Result<ProjectionPreset, ProjectionError> {
    let filters_json = serde_json::to_string(&new.filters)
        .map_err(|e| ProjectionError::InvalidPresetFilters(e.to_string()))?;
    let now = Utc::now().to_rfc3339();
    let last_seen = new.last_seen_message_id.unwrap_or(0);

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO projection_presets \
         (workshop_id, channel, name, filters_json, last_seen_message_id, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(&new.workshop_id)
    .bind(&new.channel)
    .bind(&new.name)
    .bind(&filters_json)
    .bind(last_seen)
    .bind(&now)
    .bind(&now)
    .fetch_one(pool)
    .await?;

    Ok(ProjectionPreset {
        id,
        workshop_id: new.workshop_id,
        channel: new.channel,
        name: new.name,
        filters: new.filters,
        last_seen_message_id: last_seen,
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Fetch one preset by primary key, or `None` if it has been deleted.
pub async fn get_preset(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<ProjectionPreset>, ProjectionError> {
    let row = sqlx::query(
        "SELECT id, workshop_id, channel, name, filters_json, last_seen_message_id, \
                created_at, updated_at \
         FROM projection_presets WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(preset_from_row).transpose()
}

/// List all presets for a (workshop, channel) in stable name order.
pub async fn list_presets(
    pool: &SqlitePool,
    workshop_id: &str,
    channel: &str,
) -> Result<Vec<ProjectionPreset>, ProjectionError> {
    let rows = sqlx::query(
        "SELECT id, workshop_id, channel, name, filters_json, last_seen_message_id, \
                created_at, updated_at \
         FROM projection_presets \
         WHERE workshop_id = ? AND channel = ? \
         ORDER BY name ASC",
    )
    .bind(workshop_id)
    .bind(channel)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(preset_from_row).collect()
}

/// Replace the filter config on a preset. Bumps `updated_at`.
pub async fn update_preset_filters(
    pool: &SqlitePool,
    id: i64,
    filters: PresetFilters,
) -> Result<ProjectionPreset, ProjectionError> {
    let filters_json = serde_json::to_string(&filters)
        .map_err(|e| ProjectionError::InvalidPresetFilters(e.to_string()))?;
    let now = Utc::now().to_rfc3339();
    let rows = sqlx::query(
        "UPDATE projection_presets \
         SET filters_json = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&filters_json)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;
    if rows.rows_affected() == 0 {
        return Err(ProjectionError::PresetNotFound(id));
    }
    get_preset(pool, id)
        .await?
        .ok_or(ProjectionError::PresetNotFound(id))
}

/// Bump `last_seen_message_id` to `message_id` iff `message_id` is
/// strictly greater than the current value. Monotonic by construction —
/// a stale client cannot unread a message.
pub async fn bump_last_seen(
    pool: &SqlitePool,
    id: i64,
    message_id: i64,
) -> Result<(), ProjectionError> {
    let now = Utc::now().to_rfc3339();
    let rows = sqlx::query(
        "UPDATE projection_presets \
         SET last_seen_message_id = ?, updated_at = ? \
         WHERE id = ? AND last_seen_message_id < ?",
    )
    .bind(message_id)
    .bind(&now)
    .bind(id)
    .bind(message_id)
    .execute(pool)
    .await?;
    // rows_affected == 0 is fine: either the preset does not exist, or
    // the supplied message_id is not newer than the stored high-water
    // mark. Both are no-ops by design.
    let _ = rows;
    Ok(())
}

/// Delete one preset. Returns `true` if a row was removed.
pub async fn delete_preset(pool: &SqlitePool, id: i64) -> Result<bool, ProjectionError> {
    let result = sqlx::query("DELETE FROM projection_presets WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Compute unread count for `preset_id`: number of messages matching the
/// preset's filters on its channel whose `id` is greater than
/// `last_seen_message_id`.
///
/// Implementation uses `COUNT(*) WHERE id > last_seen` rather than
/// the arithmetic `max(id) - last_seen` form in the original spark
/// description, because `irc_messages.id` is not gap-free within a
/// channel's filter slice (deletes, FTS-mismatches, cross-channel
/// inserts all break the assumption). PR #53 Copilot c3 + c7 — the
/// migration comment + this doc both used to imply the arithmetic
/// form was canonical; corrected.
pub async fn preset_unread_count(
    pool: &SqlitePool,
    preset_id: i64,
) -> Result<i64, ProjectionError> {
    let preset = get_preset(pool, preset_id)
        .await?
        .ok_or(ProjectionError::PresetNotFound(preset_id))?;

    let mut q = ChannelProjectionQuery {
        channel: Some(preset.channel.clone()),
        limit: i64::MAX,
        ..ChannelProjectionQuery::default()
    };
    preset.filters.apply_to(&mut q);

    // PR #53 Copilot c2: the preset's fts_query flows into
    // push_filter_predicate via apply_to above. Without an explicit
    // validate_fts call the unread-count query path could hit a
    // SQLite error on an empty/unbalanced preset, leaking that
    // error all the way to the caller. Validate up front so we
    // surface ProjectionError::InvalidFts cleanly instead.
    validate_fts(q.fts_query.as_deref())?;

    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT COUNT(*) AS unread FROM irc_messages m \
         LEFT JOIN event_outbox e ON e.event_id = m.structured_event_id \
         WHERE m.channel = ",
    );
    qb.push_bind(preset.channel.clone());
    if let Some(epic_id) = &q.epic_id {
        qb.push(" AND m.epic_id = ").push_bind(epic_id.clone());
    }
    qb.push(" AND m.id > ")
        .push_bind(preset.last_seen_message_id);
    qb.push(" AND ");
    push_filter_predicate(&mut qb, &q);

    let row = qb.build().fetch_one(pool).await?;
    Ok(row.try_get::<i64, _>("unread")?)
}

fn preset_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProjectionPreset, ProjectionError> {
    let filters_json: String = row.try_get("filters_json")?;
    let filters: PresetFilters = serde_json::from_str(&filters_json)
        .map_err(|e| ProjectionError::InvalidPresetFilters(e.to_string()))?;
    Ok(ProjectionPreset {
        id: row.try_get("id")?,
        workshop_id: row.try_get("workshop_id")?,
        channel: row.try_get("channel")?,
        name: row.try_get("name")?,
        filters,
        last_seen_message_id: row.try_get("last_seen_message_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_for_channel_sets_channel_and_default_limit() {
        let q = ChannelProjectionQuery::for_channel("#ryve:epic:1");
        assert_eq!(q.channel.as_deref(), Some("#ryve:epic:1"));
        assert_eq!(q.limit, DEFAULT_LIMIT);
        assert!(q.epic_id.is_none());
        assert!(q.current_actor_id.is_none());
    }

    #[test]
    fn effective_limit_defaults_when_zero_or_negative() {
        let mut q = ChannelProjectionQuery::default();
        assert_eq!(q.effective_limit(), DEFAULT_LIMIT);
        q.limit = -5;
        assert_eq!(q.effective_limit(), DEFAULT_LIMIT);
        q.limit = 17;
        assert_eq!(q.effective_limit(), 17);
    }

    #[test]
    fn validate_fts_rejects_empty_and_unbalanced_quotes() {
        assert!(validate_fts(None).is_ok());
        assert!(validate_fts(Some("approved")).is_ok());
        assert!(validate_fts(Some("\"quoted phrase\"")).is_ok());
        assert!(matches!(
            validate_fts(Some("")),
            Err(ProjectionError::InvalidFtsQuery(_))
        ));
        assert!(matches!(
            validate_fts(Some("   ")),
            Err(ProjectionError::InvalidFtsQuery(_))
        ));
        assert!(matches!(
            validate_fts(Some("\"unbalanced")),
            Err(ProjectionError::InvalidFtsQuery(_))
        ));
    }

    #[test]
    fn preset_filters_apply_overwrites_axes_not_session_state() {
        let filters = PresetFilters {
            epic_id: Some("e-1".into()),
            spark_id: Some("sp-1".into()),
            assignment_id: Some("asgn-1".into()),
            pr_number: Some(42),
            actor_id: Some("alice".into()),
            fts_query: Some("approved".into()),
        };
        let mut base = ChannelProjectionQuery {
            current_actor_id: Some("bob".into()),
            limit: 99,
            ..ChannelProjectionQuery::default()
        };
        filters.apply_to(&mut base);
        assert_eq!(base.epic_id.as_deref(), Some("e-1"));
        assert_eq!(base.spark_id.as_deref(), Some("sp-1"));
        assert_eq!(base.assignment_id.as_deref(), Some("asgn-1"));
        assert_eq!(base.pr_number, Some(42));
        assert_eq!(base.actor_id.as_deref(), Some("alice"));
        assert_eq!(base.fts_query.as_deref(), Some("approved"));
        // Session-scoped fields untouched.
        assert_eq!(base.current_actor_id.as_deref(), Some("bob"));
        assert_eq!(base.limit, 99);
    }

    #[test]
    fn projection_output_empty_vs_messages() {
        let empty = ProjectionOutput::Empty;
        assert!(empty.is_empty());
        assert_eq!(empty.into_vec(), Vec::new());

        let msgs = ProjectionOutput::Messages(vec![ProjectedMessage {
            id: 1,
            epic_id: "e".into(),
            channel: "#c".into(),
            sender_actor_id: None,
            timestamp: "t".into(),
            event_type: None,
            raw_text: "hi".into(),
            structured_event_id: None,
            metadata: None,
            matched_by_mention: false,
        }]);
        assert!(!msgs.is_empty());
        assert_eq!(msgs.into_vec().len(), 1);
    }
}
